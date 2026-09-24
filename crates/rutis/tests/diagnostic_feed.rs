use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rutis::{
    BoxFuture, CordisError, Ctx, DiagnosticChange, DiagnosticChangeKind, Effect, FiberState,
    Plugin, ServiceReadFailure, TypeKey,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Barrier;

struct Capture(Arc<Mutex<Option<Ctx>>>);

impl Plugin for Capture {
    fn name(&self) -> &str {
        "diagnostic-feed-capture"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(ctx.clone());
            Ok(Effect::Done)
        })
    }
}

async fn child(root: &Ctx) -> (rutis::FiberView, Ctx) {
    let slot = Arc::new(Mutex::new(None));
    let view = root.plugin(Capture(slot.clone()));
    (&view).await.unwrap();
    let ctx = slot.lock().unwrap().take().unwrap();
    (view, ctx)
}

async fn collect_until_closed(
    receiver: &mut tokio::sync::broadcast::Receiver<DiagnosticChange>,
) -> Vec<DiagnosticChange> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut changes = Vec::new();
        loop {
            match receiver.recv().await {
                Ok(change) => changes.push(change),
                Err(RecvError::Closed) => return changes,
                Err(RecvError::Lagged(n)) => panic!("unexpected diagnostic lag: {n}"),
            }
        }
    })
    .await
    .expect("diagnostic feed did not close")
}

#[tokio::test]
async fn lifecycle_bindings_instances_and_strict_denial_share_a_root_sequence() {
    let root = Ctx::root().unwrap();
    let mut subscription = root.subscribe_diagnostics().unwrap();
    assert_eq!(subscription.initial.plugins.len(), 1);
    let (a, a_ctx) = child(&root).await;
    let (b, b_ctx) = child(&root).await;
    let key = TypeKey::instance::<u64>(a_ctx.instance());
    let _binding = a_ctx.provide_as(key.clone(), Arc::new(7_u64)).unwrap();
    let error = b_ctx.require_as::<u64>(key.clone()).unwrap_err();
    assert_eq!(error.reason, ServiceReadFailure::OutOfScope);
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
    root.shutdown().await.unwrap();

    let changes = collect_until_closed(&mut subscription.changes).await;
    assert!(changes
        .iter()
        .all(|change| change.seq > subscription.cursor));
    assert!(changes.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    let a_id = a.id;
    let b_id = b.id;
    let registered = changes
        .iter()
        .position(|change| {
            matches!(
                &change.kind,
                DiagnosticChangeKind::PluginRegistered { plugin, instance, parent: Some(_), .. }
                    if *plugin == a_id && *instance == a_ctx.instance()
            )
        })
        .unwrap();
    let active = changes.iter().position(|change| matches!(
        change.kind,
        DiagnosticChangeKind::StateChanged { plugin, to: FiberState::Active, .. } if plugin == a_id
    )).unwrap();
    let provided = changes.iter().position(|change| matches!(
        &change.kind,
        DiagnosticChangeKind::BindingRegistered(binding)
            if binding.provider == a_id && binding.instance == a_ctx.instance() && binding.key == key
    )).unwrap();
    let removing = changes.iter().position(|change| matches!(
        &change.kind,
        DiagnosticChangeKind::BindingRemoving(binding) if binding.provider == a_id && binding.key == key
    )).unwrap();
    let removed = changes.iter().position(|change| matches!(
        &change.kind,
        DiagnosticChangeKind::BindingRemoved(binding) if binding.provider == a_id && binding.key == key
    )).unwrap();
    let denied = changes.iter().position(|change| matches!(
        &change.kind,
        DiagnosticChangeKind::StrictReadDenied { plugin, instance, key: denied_key, reason: ServiceReadFailure::OutOfScope, .. }
            if *plugin == b_id && *instance == b_ctx.instance() && *denied_key == key
    )).unwrap();
    let terminated = changes
        .iter()
        .position(|change| {
            matches!(
                change.kind,
                DiagnosticChangeKind::PluginTerminated { plugin, .. } if plugin == a_id
            )
        })
        .unwrap();
    let root_terminated = changes
        .iter()
        .position(|change| {
            matches!(
                change.kind,
                DiagnosticChangeKind::PluginTerminated { parent: None, .. }
            )
        })
        .unwrap();
    assert!(registered < active);
    assert!(active < removing);
    assert!(provided < removing && removing < removed && removed < terminated);
    assert!(denied < root_terminated && terminated < root_terminated);
    assert!(matches!(
        root.subscribe_diagnostics(),
        Err(CordisError::Closed)
    ));
}

#[tokio::test]
async fn lag_is_explicit_and_a_failed_observer_cannot_stop_lifecycle() {
    let root = Ctx::root().unwrap();
    let mut slow = root.subscribe_diagnostics().unwrap();
    let mut failed = root.subscribe_diagnostics().unwrap().changes;
    let observer = tokio::spawn(async move {
        let _ = failed.recv().await;
        panic!("observer failure");
    });
    for _ in 0..300 {
        assert_eq!(
            root.require::<u64>().unwrap_err().reason,
            ServiceReadFailure::Undeclared
        );
    }
    assert!(observer.await.is_err());
    assert!(matches!(slow.changes.recv().await, Err(RecvError::Lagged(n)) if n > 0));
    let mut fresh = root.subscribe_diagnostics().unwrap();
    root.require::<u64>().unwrap_err();
    let next = fresh.changes.recv().await.unwrap();
    assert_eq!(next.seq, fresh.cursor + 1);
    assert!(matches!(
        next.kind,
        DiagnosticChangeKind::StrictReadDenied { .. }
    ));
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn repeated_subtrees_and_subscriptions_leave_only_root() {
    let root = Ctx::root().unwrap();
    for _ in 0..1000 {
        let subscription = root.subscribe_diagnostics().unwrap();
        let (view, ctx) = child(&root).await;
        drop(ctx);
        view.shutdown().await.unwrap();
        drop(view);
        drop(subscription);
    }
    assert_eq!(root.diagnostics().plugins.len(), 1);
    tokio::time::timeout(Duration::from_secs(3), root.shutdown())
        .await
        .expect("root shutdown waited on retired children")
        .unwrap();
}

#[tokio::test]
async fn root_shutdown_drains_child_termination_before_closing_the_feed() {
    let root = Ctx::root().unwrap();
    let mut subscription = root.subscribe_diagnostics().unwrap();
    let (parent, parent_ctx) = child(&root).await;
    let (grandchild, _) = child(&parent_ctx).await;
    let root_id = root.root_view().unwrap().id;
    root.shutdown().await.unwrap();
    // The public root shutdown task completes after the feed is closed.
    let mut changes = Vec::new();
    loop {
        match subscription.changes.try_recv() {
            Ok(change) => changes.push(change),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(error) => panic!("feed was not closed at root shutdown completion: {error}"),
        }
    }
    let terminated: Vec<_> = changes
        .iter()
        .filter_map(|change| match change.kind {
            DiagnosticChangeKind::PluginTerminated { plugin, .. } => Some(plugin),
            _ => None,
        })
        .collect();
    assert!(terminated.contains(&parent.id));
    assert!(terminated.contains(&grandchild.id));
    assert_eq!(terminated.last().copied(), Some(root_id));
}

struct ReloadingProvider {
    key: TypeKey,
    next: Arc<AtomicU64>,
}

impl Plugin for ReloadingProvider {
    fn name(&self) -> &str {
        "diagnostic-reloading-provider"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let value = self.next.fetch_add(1, Ordering::SeqCst);
            ctx.provide_as(self.key.clone(), Arc::new(value))?;
            Ok(Effect::Done)
        })
    }
}

struct ReloadingConsumer(TypeKey);

impl Plugin for ReloadingConsumer {
    fn name(&self) -> &str {
        "diagnostic-reloading-consumer"
    }

    fn injects(&self) -> &[TypeKey] {
        std::slice::from_ref(&self.0)
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let _ = ctx.require_as::<u64>(self.0.clone())?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn reload_reports_old_binding_removal_then_new_generation() {
    let root = Ctx::root().unwrap();
    let key = TypeKey::keyed::<u64>("feed-reload");
    let provider = root.plugin(ReloadingProvider {
        key: key.clone(),
        next: Arc::new(AtomicU64::new(1)),
    });
    (&provider).await.unwrap();
    let consumer = root.plugin(ReloadingConsumer(key.clone()));
    (&consumer).await.unwrap();
    let initial_generation = consumer.state().generation;
    let mut subscription = root.subscribe_diagnostics().unwrap();
    provider.restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut state = consumer.watch();
        loop {
            if state.borrow().state == FiberState::Active
                && state.borrow().generation > initial_generation
            {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("consumer did not reload");
    root.shutdown().await.unwrap();
    let changes = collect_until_closed(&mut subscription.changes).await;
    let old_removing = changes.iter().position(|change| matches!(
        &change.kind,
        DiagnosticChangeKind::BindingRemoving(binding) if binding.provider == provider.id && binding.key == key
    )).unwrap();
    let old_gen = match &changes[old_removing].kind {
        DiagnosticChangeKind::BindingRemoving(binding) => binding.generation,
        _ => unreachable!(),
    };
    let old_removed = changes.iter().position(|change| matches!(
        &change.kind,
        DiagnosticChangeKind::BindingRemoved(binding) if binding.provider == provider.id && binding.generation == old_gen
    )).unwrap();
    let new_registered = changes.iter().position(|change| matches!(
        &change.kind,
        DiagnosticChangeKind::BindingRegistered(binding) if binding.provider == provider.id && binding.generation > old_gen
    )).unwrap();
    assert!(old_removing < old_removed && old_removed < new_registered);
    assert!(changes.iter().any(|change| matches!(
        change.kind,
        DiagnosticChangeKind::StateChanged { plugin, from: FiberState::Active, to: FiberState::Unloading, .. } if plugin == consumer.id
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_subscribe_and_mount_cannot_miss_a_live_plugin() {
    let root = Ctx::root().unwrap();
    for _ in 0..32 {
        let barrier = Arc::new(Barrier::new(2));
        let subscribing_root = root.clone();
        let subscribing_barrier = barrier.clone();
        let subscriber = tokio::spawn(async move {
            subscribing_barrier.wait().await;
            subscribing_root.subscribe_diagnostics().unwrap()
        });
        let mounting_root = root.clone();
        let mounting_barrier = barrier;
        let mounted = tokio::spawn(async move {
            mounting_barrier.wait().await;
            mounting_root.plugin(Capture(Arc::new(Mutex::new(None))))
        });
        let mut subscription = subscriber.await.unwrap();
        let view = mounted.await.unwrap();
        let in_snapshot = subscription
            .initial
            .plugins
            .iter()
            .any(|plugin| plugin.id == view.id);
        let mut in_stream = false;
        loop {
            match subscription.changes.try_recv() {
                Ok(change) => {
                    in_stream |= matches!(
                        change.kind,
                        DiagnosticChangeKind::PluginRegistered { plugin, .. } if plugin == view.id
                    );
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(error) => panic!("unexpected diagnostic receive error: {error}"),
            }
        }
        assert!(in_snapshot || in_stream);
        view.shutdown().await.unwrap();
    }
    root.shutdown().await.unwrap();
}
