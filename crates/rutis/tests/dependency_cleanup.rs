use rutis::{
    BoxFuture, ConsumerCleanup, CordisError, Ctx, DependencyCleanup, Effect, Plugin, TypeKey,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::sync::{oneshot, Semaphore};

struct Service;
struct Provider {
    observation: Arc<Mutex<Option<DependencyCleanup>>>,
}
impl Plugin for Provider {
    fn name(&self) -> &str {
        "observed-provider"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            *self.observation.lock().unwrap() = Some(ctx.track_dependency_cleanup());
            // Services created by real internal children belong to this tree.
            let child = ctx.plugin(ServiceChild);
            (&child)
                .await
                .map_err(|error| CordisError::PluginFailed(Box::new(error)))?;
            Ok(Effect::Done)
        })
    }
}
struct ServiceChild;
impl Plugin for ServiceChild {
    fn name(&self) -> &str {
        "internal-service-child"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.provide(Service)?;
            Ok(Effect::Done)
        })
    }
}
struct SlowConsumer {
    injects: Vec<TypeKey>,
    entered: Mutex<Option<oneshot::Sender<()>>>,
    release: Arc<Semaphore>,
}
impl Plugin for SlowConsumer {
    fn name(&self) -> &str {
        "slow-external-consumer"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.require::<Service>()?;
            let entered = self.entered.lock().unwrap().take().unwrap();
            let release = self.release.clone();
            Ok(Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move {
                    entered.send(()).unwrap();
                    release.acquire().await.unwrap().forget();
                    Err(CordisError::ServiceNotFound("slow cleanup failure".into()))
                })
            })))
        })
    }
}
async fn provider(ctx: &Ctx) -> (rutis::FiberView, DependencyCleanup) {
    let observation = Arc::new(Mutex::new(None));
    let provider = ctx.plugin(Provider {
        observation: observation.clone(),
    });
    (&provider).await.unwrap();
    let observer = observation.lock().unwrap().take().unwrap();
    (provider, observer)
}
fn one(observation: &DependencyCleanup) -> ConsumerCleanup {
    let consumers = observation.consumers();
    assert_eq!(consumers.len(), 1);
    consumers.into_iter().next().unwrap()
}

#[tokio::test]
async fn actual_scoped_subtree_consumer_cleanup_survives_waiter_drop_and_native_edge_removal() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let root = Ctx::root().unwrap();
        let key = TypeKey::of::<Service>();
        let first = root.isolate(key.clone(), "first");
        let second = root.isolate(key.clone(), "second");
        let (source, observed) = provider(&first).await;
        let (other, unrelated) = provider(&second).await;
        let (entered, waiting) = oneshot::channel();
        let release = Arc::new(Semaphore::new(0));
        let consumer = first.plugin(SlowConsumer {
            injects: vec![key],
            entered: Mutex::new(Some(entered)),
            release: release.clone(),
        });
        (&consumer).await.unwrap();
        let stopped = source.shutdown();
        waiting.await.unwrap();
        let receipt = one(&observed);
        assert_eq!(receipt.id(), consumer.id);
        assert_eq!(receipt.generation(), 1);
        assert!(receipt.result().is_none());
        assert!(unrelated.consumers().is_empty());
        assert_eq!(other.state().state, rutis::FiberState::Active);
        let cloned = receipt.clone();
        let (entered, waiting) = oneshot::channel();
        let abandoned = tokio::spawn(async move {
            entered.send(()).unwrap();
            cloned.wait().await
        });
        waiting.await.unwrap();
        abandoned.abort();
        let _ = abandoned.await;
        release.add_permits(1);
        stopped.await.unwrap(); // Native provider eviction does not propagate this failure.
        observed.wait_provider().await.unwrap();
        consumer.dispose().await.unwrap();
        drop(consumer);
        drop(source);
        let failure = receipt.wait().await.unwrap_err();
        assert!(failure.to_string().contains("slow cleanup failure"));
        assert!(Arc::ptr_eq(&failure, &receipt.wait().await.unwrap_err()));
        assert!(one(&observed).result().unwrap().is_err());
        root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

struct RetriedConsumer {
    injects: Vec<TypeKey>,
    fail: Arc<AtomicBool>,
}
impl Plugin for RetriedConsumer {
    fn name(&self) -> &str {
        "retried-external-consumer"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.require::<Service>()?;
            if self.fail.load(Ordering::SeqCst) {
                ctx.effect(|| {
                    Effect::Disposer(Box::new(|| {
                        Err(CordisError::ServiceNotFound("failed apply cleanup".into()))
                    }))
                })?;
                return Err(CordisError::ServiceNotFound(
                    "business apply failure".into(),
                ));
            }
            Ok(Effect::Done)
        })
    }
}
#[tokio::test]
async fn failed_apply_cleanup_is_recorded_separately_and_a_new_generation_cannot_overwrite_it() {
    let root = Ctx::root().unwrap();
    let (source, observed) = provider(&root).await;
    let fail = Arc::new(AtomicBool::new(true));
    let consumer = root.plugin(RetriedConsumer {
        injects: vec![TypeKey::of::<Service>()],
        fail: fail.clone(),
    });
    assert!((&consumer).await.is_err());
    let old = one(&observed);
    let failure = old.wait().await.unwrap_err();
    assert!(failure.to_string().contains("failed apply cleanup"));
    assert!(!failure.to_string().contains("business apply failure"));
    fail.store(false, Ordering::SeqCst);
    consumer.restart().await.unwrap();
    assert_eq!(consumer.state().generation, 2);
    consumer.shutdown().await.unwrap();
    let receipts = observed.consumers();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[1].generation(), 2);
    receipts[1].wait().await.unwrap();
    assert!(Arc::ptr_eq(&failure, &old.wait().await.unwrap_err()));
    source.shutdown().await.unwrap();
    observed.wait_provider().await.unwrap();
    root.shutdown().await.unwrap();
}

struct Forwarded;
struct Forwarder {
    injects: Vec<TypeKey>,
    service: Arc<Mutex<Option<rutis::Disposer>>>,
}
impl Plugin for Forwarder {
    fn name(&self) -> &str {
        "external-forwarder"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.require::<Service>()?;
            let child = ctx.plugin(ForwardedChild(self.service.clone()));
            (&child)
                .await
                .map_err(|error| CordisError::PluginFailed(Box::new(error)))?;
            Ok(Effect::Done)
        })
    }
}
struct ForwardedChild(Arc<Mutex<Option<rutis::Disposer>>>);
impl Plugin for ForwardedChild {
    fn name(&self) -> &str {
        "forwarded-internal-child"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(ctx.provide(Forwarded)?);
            Ok(Effect::Done)
        })
    }
}
struct IndirectConsumer {
    injects: Vec<TypeKey>,
    entered: Mutex<Option<oneshot::Sender<()>>>,
    release: Arc<Semaphore>,
}
impl Plugin for IndirectConsumer {
    fn name(&self) -> &str {
        "indirect-external-consumer"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.require::<Forwarded>()?;
            let entered = self.entered.lock().unwrap().take().unwrap();
            let release = self.release.clone();
            Ok(Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move {
                    entered.send(()).unwrap();
                    release.acquire().await.unwrap().forget();
                    Err(CordisError::ServiceNotFound(
                        "indirect cleanup failure".into(),
                    ))
                })
            })))
        })
    }
}
#[tokio::test]
async fn indirect_consumer_failure_is_captured_before_its_forwarder_starts_draining() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let root = Ctx::root().unwrap();
        let (source, observed) = provider(&root).await;
        let service = Arc::new(Mutex::new(None));
        let forwarder = root.plugin(Forwarder {
            injects: vec![TypeKey::of::<Service>()],
            service: service.clone(),
        });
        (&forwarder).await.unwrap();
        let (entered, waiting) = oneshot::channel();
        let release = Arc::new(Semaphore::new(0));
        let leaf = root.plugin(IndirectConsumer {
            injects: vec![TypeKey::of::<Forwarded>()],
            entered: Mutex::new(Some(entered)),
            release: release.clone(),
        });
        (&leaf).await.unwrap();
        let released = service.lock().unwrap().take().unwrap().dispose();
        let released = tokio::spawn(released);
        waiting.await.unwrap();
        let leaf_cleanup = observed
            .consumers()
            .into_iter()
            .find(|consumer| consumer.id() == leaf.id)
            .expect("the indirect consumer is tracked before its forwarder drains");
        assert!(leaf_cleanup.result().is_none());
        assert!(observed
            .consumers()
            .iter()
            .all(|consumer| consumer.id() != forwarder.id));
        assert_eq!(forwarder.state().state, rutis::FiberState::Active);
        release.add_permits(1);
        released.await.unwrap().unwrap(); // Native eviction ignores leaf failure.
        source.shutdown().await.unwrap();
        observed.wait_provider().await.unwrap();
        let consumers = observed.consumers();
        assert_eq!(consumers.len(), 2);
        consumers
            .iter()
            .find(|consumer| consumer.id() == forwarder.id)
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert!(leaf_cleanup
            .wait()
            .await
            .unwrap_err()
            .to_string()
            .contains("indirect cleanup failure"));
        root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

struct LoadingConsumer {
    injects: Vec<TypeKey>,
    context: Mutex<Option<oneshot::Sender<Ctx>>>,
    resume: Mutex<Option<oneshot::Receiver<()>>>,
}
impl Plugin for LoadingConsumer {
    fn name(&self) -> &str {
        "loading-external-consumer"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.require::<Service>()?;
            assert!(self
                .context
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(ctx.clone())
                .is_ok());
            let resume = self.resume.lock().unwrap().take().unwrap();
            resume.await.unwrap();
            Ok(Effect::Done)
        })
    }
}
#[tokio::test]
async fn evicted_loading_consumer_has_a_pending_receipt_before_apply_can_exit() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let root = Ctx::root().unwrap();
        let (source, observed) = provider(&root).await;
        let (context, entered) = oneshot::channel();
        let (resume, waiting) = oneshot::channel();
        let consumer = root.plugin(LoadingConsumer {
            injects: vec![TypeKey::of::<Service>()],
            context: Mutex::new(Some(context)),
            resume: Mutex::new(Some(waiting)),
        });
        let ctx = entered.await.unwrap();
        let stopped = source.shutdown();
        ctx.cancellation_token().cancelled().await;
        assert_eq!(consumer.state().state, rutis::FiberState::Loading);
        let receipt = one(&observed);
        assert_eq!(receipt.id(), consumer.id);
        assert_eq!(receipt.generation(), 1);
        assert!(receipt.result().is_none());
        resume.send(()).unwrap();
        stopped.await.unwrap();
        observed.wait_provider().await.unwrap();
        receipt.wait().await.unwrap();
        assert_eq!(one(&observed).generation(), 1);
        root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}
