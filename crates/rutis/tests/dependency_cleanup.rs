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
