use rutis::{BoxFuture, CordisError, Ctx, Effect, FiberState, Plugin, TypeKey};
use rutis::{Event, Listener};
use rutis_protocol::managed::{ActivationGate, ManagedActivation};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

struct Probe {
    injects: Vec<TypeKey>,
    seen: Arc<Mutex<Vec<Ctx>>>,
    loading: Option<Arc<Notify>>,
    entered: Arc<Notify>,
    cleanup: Arc<Mutex<usize>>,
}
impl Plugin for Probe {
    fn name(&self) -> &str {
        "native-probe"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(ctx.clone());
            let cleanup = self.cleanup.clone();
            ctx.effect(move || {
                Effect::Disposer(Box::new(move || {
                    *cleanup.lock().unwrap() += 1;
                    Ok(())
                }))
            })?;
            self.entered.notify_one();
            if let Some(gate) = &self.loading {
                gate.notified().await;
            }
            Ok(Effect::Done)
        })
    }
}

fn probe(injects: Vec<TypeKey>) -> Probe {
    Probe {
        injects,
        seen: Arc::default(),
        loading: None,
        entered: Arc::default(),
        cleanup: Arc::default(),
    }
}

#[tokio::test]
async fn dependency_loss_never_reapplies_the_old_activation() {
    let root = Ctx::root().unwrap();
    let provider = root.provide(1_u8).unwrap();
    let p = probe(vec![TypeKey::of::<u8>()]);
    let seen = p.seen.clone();
    let cleanups = p.cleanup.clone();
    let activation = ManagedActivation::mount(&root, p).unwrap();
    (activation.view()).await.unwrap();
    assert_eq!(activation.view().state().state, FiberState::Active);
    let old = seen.lock().unwrap()[0].clone();
    provider.dispose().await.unwrap();
    assert!(!activation.gate().is_open());
    let replacement = root.provide(2_u8).unwrap();
    (activation.view()).await.unwrap();
    assert_eq!(activation.view().state().state, FiberState::Pending);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(*cleanups.lock().unwrap(), 1);
    assert!(old.provide(99_u64).is_err());
    assert!(old.effect(|| Effect::Done).is_err());
    activation.stop().await.unwrap();
    let next = ManagedActivation::mount(&root, probe(vec![TypeKey::of::<u8>()])).unwrap();
    (next.view()).await.unwrap();
    assert_eq!(next.view().state().state, FiberState::Active);
    next.stop().await.unwrap();
    replacement.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_dependency_waits_for_the_first_host_authorized_load() {
    let root = Ctx::root().unwrap();
    let p = probe(vec![TypeKey::of::<u8>()]);
    let seen = p.seen.clone();
    let activation = ManagedActivation::mount(&root, p).unwrap();
    (activation.view()).await.unwrap();
    assert_eq!(activation.view().state().state, FiberState::Pending);
    assert!(seen.lock().unwrap().is_empty());
    root.provide(1_u8).unwrap();
    (activation.view()).await.unwrap();
    assert_eq!(activation.view().state().state, FiberState::Active);
    activation.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loading_dependency_loss_closes_admission_before_apply_finishes() {
    let root = Ctx::root().unwrap();
    let provider = root.provide(1_u8).unwrap();
    let mut p = probe(vec![TypeKey::of::<u8>()]);
    let finish = Arc::new(Notify::new());
    p.loading = Some(finish.clone());
    let entered = p.entered.clone();
    let seen = p.seen.clone();
    let activation = ManagedActivation::mount(&root, p).unwrap();
    entered.notified().await;
    let old = seen.lock().unwrap()[0].clone();
    let disposal = tokio::spawn(async move { provider.dispose().await });
    // Native cancellation is the causal barrier: provider teardown pre-cancels
    // its consumer before waiting for that consumer's Loading future.
    old.cancelled().await;
    assert!(!activation.gate().is_open());
    root.provide(2_u8).unwrap();
    finish.notify_one();
    disposal.await.unwrap().unwrap();
    (activation.view()).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(old.provide(99_u64).is_err());
    activation.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

struct Child {
    gate: ActivationGate,
    seen: Arc<Mutex<Option<Ctx>>>,
}
impl Plugin for Child {
    fn name(&self) -> &str {
        "necessary-child"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            self.gate.track_export(ctx)?;
            *self.seen.lock().unwrap() = Some(ctx.clone());
            ctx.provide(1_u32)?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn necessary_child_export_and_root_share_revocation() {
    let root = Ctx::root().unwrap();
    let p = probe(vec![]);
    let seen = p.seen.clone();
    let activation = ManagedActivation::mount(&root, p).unwrap();
    (activation.view()).await.unwrap();
    let child_seen = Arc::default();
    // M0 tests the native ownership/cancellation mechanism used by export
    // registration. Protocol object routing is a later milestone.
    let parent = seen.lock().unwrap()[0].clone();
    let child = parent.plugin(Child {
        gate: activation.gate(),
        seen: child_seen,
    });
    (&child).await.unwrap();
    assert!(activation.gate().is_open());
    child.shutdown().await.unwrap();
    assert!(!activation.gate().is_open());
    root.refresh();
    (activation.view()).await.unwrap();
    assert_eq!(activation.view().state().state, FiberState::Pending);
    activation.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_is_terminal_even_if_raw_restart_is_requested() {
    let root = Ctx::root().unwrap();
    let activation = ManagedActivation::mount(&root, probe(vec![])).unwrap();
    (activation.view()).await.unwrap();
    let stopping = activation.stop();
    assert!(!activation.gate().is_open());
    assert!(activation.view().restart().await.is_err());
    stopping.await.unwrap();
    assert_eq!(activation.view().state().state, FiberState::Disposed);
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn two_native_plugins_keep_same_type_injections_in_separate_scopes() {
    let root = Ctx::root().unwrap();
    let a = root.isolate(TypeKey::of::<u8>(), "activation-a");
    let b = root.isolate(TypeKey::of::<u8>(), "activation-b");
    a.provide(1_u8).unwrap();
    b.provide(2_u8).unwrap();
    let pa = probe(vec![TypeKey::of::<u8>()]);
    let pb = probe(vec![TypeKey::of::<u8>()]);
    let sa = pa.seen.clone();
    let sb = pb.seen.clone();
    let first = ManagedActivation::mount(&a, pa).unwrap();
    let second = ManagedActivation::mount(&b, pb).unwrap();
    tokio::try_join!(first.view(), second.view()).unwrap();
    assert_eq!(*sa.lock().unwrap()[0].require::<u8>().unwrap(), 1);
    assert_eq!(*sb.lock().unwrap()[0].require::<u8>().unwrap(), 2);
    first.stop().await.unwrap();
    assert_eq!(second.view().state().state, FiberState::Active);
    assert!(root.get::<u8>().is_none());
    second.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

struct Query;
impl Event for Query {
    const NAME: &'static str = "query";
    type Value = u8;
}
struct Respond(u8, Arc<Mutex<Vec<u8>>>);
impl Listener<Query> for Respond {
    fn call<'a>(
        &'a self,
        _: &'a Ctx,
        _: &'a Query,
    ) -> BoxFuture<'a, Result<Option<u8>, CordisError>> {
        Box::pin(async move {
            self.1.lock().unwrap().push(self.0);
            Ok((self.0 == 0).then_some(0))
        })
    }
}
struct EventPlugin(Arc<Mutex<Vec<u8>>>);
impl Plugin for EventPlugin {
    fn name(&self) -> &str {
        "native-events"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            for value in [1, 0, 2] {
                ctx.events().on(ctx, Respond(value, self.0.clone()))?;
            }
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn native_event_listeners_preserve_parallel_serial_and_unload_ownership() {
    let root = Ctx::root().unwrap();
    let calls = Arc::default();
    let activation = ManagedActivation::mount(&root, EventPlugin(calls)).unwrap();
    (activation.view()).await.unwrap();
    assert_eq!(root.events().serial(&root, &Query).await.unwrap(), Some(0));
    root.events()
        .parallel(&root, Arc::new(Query))
        .await
        .unwrap();
    activation.stop().await.unwrap();
    assert_eq!(root.events().serial(&root, &Query).await.unwrap(), None);
    root.shutdown().await.unwrap();
}

struct Factory;
impl rutis::PluginFactory<u8> for Factory {
    fn build(&self, config: &u8) -> Result<Box<dyn Plugin>, CordisError> {
        if *config == 0 {
            return Err(CordisError::Validation {
                issues: vec!["zero".into()],
            });
        }
        Ok(Box::new(probe(vec![])))
    }
}

#[tokio::test]
async fn native_factory_config_requires_a_new_host_activation() {
    let root = Ctx::root().unwrap();
    let activation = ManagedActivation::mount_factory(&root, Factory, 1_u8).unwrap();
    (activation.view()).await.unwrap();
    assert_eq!(activation.view().state().state, FiberState::Active);
    assert!(activation.view().update(2_u8).await.is_err());
    assert_eq!(activation.view().state().state, FiberState::Active);
    activation.stop().await.unwrap();
    let invalid = ManagedActivation::mount_factory(&root, Factory, 0_u8).unwrap();
    assert!((invalid.view()).await.is_err());
    assert!(!invalid.gate().is_open());
    let _ = invalid.view().restart().await;
    assert!(!invalid.gate().is_open());
    invalid.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

struct SlowCleanup {
    entered: Arc<Notify>,
    finish: Arc<Notify>,
}
impl Plugin for SlowCleanup {
    fn name(&self) -> &str {
        "slow-cleanup"
    }
    fn apply<'a>(&'a self, _: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        let entered = self.entered.clone();
        let finish = self.finish.clone();
        Box::pin(async move {
            Ok(Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move {
                    entered.notify_one();
                    finish.notified().await;
                    Ok(())
                })
            })))
        })
    }
}

#[tokio::test]
async fn dropping_stop_waiter_does_not_abandon_cleanup_and_repeated_stop_joins() {
    let root = Ctx::root().unwrap();
    let entered = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let activation = ManagedActivation::mount(
        &root,
        SlowCleanup {
            entered: entered.clone(),
            finish: finish.clone(),
        },
    )
    .unwrap();
    (activation.view()).await.unwrap();
    drop(activation.stop());
    entered.notified().await;
    assert_eq!(activation.view().state().state, FiberState::Unloading);
    finish.notify_one();
    tokio::try_join!(activation.stop(), activation.stop()).unwrap();
    assert_eq!(activation.view().state().state, FiberState::Disposed);
    root.shutdown().await.unwrap();
}
