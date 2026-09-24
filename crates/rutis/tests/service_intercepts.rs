use std::future::IntoFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rutis::{
    BoxFuture, CordisError, Ctx, Effect, FiberState, Plugin, ServiceIntercept, ServiceReadFailure,
    ServiceWriteFailure, TypeKey,
};
use tokio::sync::oneshot;

struct Capture {
    injects: Vec<TypeKey>,
    slot: Arc<Mutex<Option<Ctx>>>,
}

impl Plugin for Capture {
    fn name(&self) -> &str {
        "service-intercept-capture"
    }

    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            *self.slot.lock().unwrap() = Some(ctx.clone());
            Ok(Effect::Done)
        })
    }
}

async fn child(parent: &Ctx, injects: Vec<TypeKey>) -> (rutis::FiberView, Ctx) {
    let slot = Arc::new(Mutex::new(None));
    let view = parent.plugin(Capture {
        injects,
        slot: slot.clone(),
    });
    (&view).into_future().await.unwrap();
    let ctx = slot.lock().unwrap().take().unwrap();
    (view, ctx)
}

#[tokio::test]
async fn strict_reads_chain_and_locator_bypass_with_recorded_denial() {
    let root = Ctx::root().unwrap();
    let key = TypeKey::keyed::<u64>("read");
    root.provide_as(key.clone(), Arc::new(10u64)).unwrap();
    let (view, reader) = child(&root, vec![key.clone()]).await;
    let seen = Arc::new(AtomicUsize::new(0));
    let seen_hook = seen.clone();
    let replace = root
        .intercept_require_as::<u64>(key.clone(), move |value| {
            seen_hook.fetch_add(1, Ordering::SeqCst);
            ServiceIntercept::Replace(Arc::new(*value + 1))
        })
        .unwrap();
    assert_eq!(*reader.get_as::<u64>(key.clone()).unwrap(), 10);
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    assert_eq!(*reader.require_as::<u64>(key.clone()).unwrap(), 11);

    let deny = root
        .intercept_require_as::<u64>(key.clone(), |_| ServiceIntercept::Deny)
        .unwrap();
    assert_eq!(
        reader.require_as::<u64>(key.clone()).unwrap_err().reason,
        ServiceReadFailure::InterceptDenied
    );
    let accesses = root
        .diagnostics()
        .plugins
        .into_iter()
        .find(|plugin| plugin.id == view.id)
        .unwrap()
        .accesses;
    assert!(accesses.iter().any(|access| {
        access.key == key
            && access.provider.is_some()
            && access.failure == Some(ServiceReadFailure::InterceptDenied)
    }));
    deny.dispose().await.unwrap();

    let panic_hook = root
        .intercept_require_as::<u64>(key.clone(), |_| panic!("read hook"))
        .unwrap();
    assert_eq!(
        reader.require_as::<u64>(key.clone()).unwrap_err().reason,
        ServiceReadFailure::InterceptPanicked
    );
    panic_hook.dispose().await.unwrap();

    let nested_reader = reader.clone();
    let nested_key = key.clone();
    let reentrant = root
        .intercept_require_as::<u64>(key.clone(), move |_| {
            assert_eq!(
                nested_reader
                    .require_as::<u64>(nested_key.clone())
                    .unwrap_err()
                    .reason,
                ServiceReadFailure::InterceptReentrant
            );
            ServiceIntercept::Continue
        })
        .unwrap();
    assert_eq!(*reader.require_as::<u64>(key.clone()).unwrap(), 11);
    reentrant.dispose().await.unwrap();
    replace.dispose().await.unwrap();
    assert_eq!(*reader.require_as::<u64>(key).unwrap(), 10);
}

#[tokio::test]
async fn strict_read_checks_reject_before_interceptor_runs() {
    let root = Ctx::root().unwrap();
    let key = TypeKey::keyed::<u64>("precheck");
    let service = root.provide_as(key.clone(), Arc::new(1u64)).unwrap();
    let (_view, undeclared) = child(&root, vec![]).await;
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_hook = hits.clone();
    root.intercept_require_as::<u64>(key.clone(), move |_| {
        hits_hook.fetch_add(1, Ordering::SeqCst);
        ServiceIntercept::Continue
    })
    .unwrap();
    assert_eq!(
        undeclared
            .require_as::<u64>(key.clone())
            .unwrap_err()
            .reason,
        ServiceReadFailure::Undeclared
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let (_view, declared) = child(&root, vec![key.clone()]).await;
    service.dispose().await.unwrap();
    assert!(matches!(
        declared.require_as::<u64>(key).unwrap_err().reason,
        ServiceReadFailure::Unavailable(_)
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn hooks_match_full_key_scope_and_reader_ancestry() {
    let root = Ctx::root().unwrap();
    let key = TypeKey::keyed::<u64>("shared");
    root.provide_as(key.clone(), Arc::new(1u64)).unwrap();
    let (_a_view, a) = child(&root, vec![key.clone()]).await;
    let (_b_view, b) = child(&root, vec![key.clone()]).await;
    a.intercept_require_as::<u64>(key.clone(), |_| ServiceIntercept::Replace(Arc::new(2)))
        .unwrap();
    assert_eq!(*a.require_as::<u64>(key.clone()).unwrap(), 2);
    assert_eq!(*b.require_as::<u64>(key.clone()).unwrap(), 1);

    let write_key = TypeKey::keyed::<u32>("write-ancestry");
    let (_binding, root_writer) = root
        .provide_mut_as(write_key.clone(), Arc::new(1u32))
        .unwrap();
    let write_hits = Arc::new(AtomicUsize::new(0));
    let write_hits_hook = write_hits.clone();
    a.intercept_set_as::<u32>(write_key, move |_| {
        write_hits_hook.fetch_add(1, Ordering::SeqCst);
        ServiceIntercept::Continue
    })
    .unwrap();
    root_writer.set(&root, Arc::new(2)).unwrap();
    assert_eq!(write_hits.load(Ordering::SeqCst), 0);

    let instance_key = TypeKey::instance::<u64>(a.instance());
    a.provide_as(instance_key.clone(), Arc::new(5u64)).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_hook = hits.clone();
    a.intercept_require_as::<u64>(instance_key.clone(), move |_| {
        hits_hook.fetch_add(1, Ordering::SeqCst);
        ServiceIntercept::Continue
    })
    .unwrap();
    assert_eq!(*a.require_as::<u64>(instance_key.clone()).unwrap(), 5);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        b.require_as::<u64>(instance_key.clone())
            .unwrap_err()
            .reason,
        ServiceReadFailure::OutOfScope
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(matches!(
        root.intercept_require_as::<u64>(instance_key, |_| ServiceIntercept::Continue),
        Err(CordisError::InstanceOutOfScope { .. })
    ));

    let mutable_instance = TypeKey::instance::<u32>(a.instance());
    let (_binding, instance_writer) = a.provide_mut_as(mutable_instance, Arc::new(1u32)).unwrap();
    assert_eq!(
        instance_writer.set(&b, Arc::new(2)).unwrap_err().reason,
        ServiceWriteFailure::WrongOwner
    );
    instance_writer.set(&a, Arc::new(3)).unwrap();

    let isolated_key = TypeKey::keyed::<u32>("isolated");
    let left = root.isolate(isolated_key.clone(), "left");
    let right = root.isolate(isolated_key.clone(), "right");
    left.provide_as(isolated_key.clone(), Arc::new(10u32))
        .unwrap();
    right
        .provide_as(isolated_key.clone(), Arc::new(20u32))
        .unwrap();
    left.intercept_require_as::<u32>(isolated_key.clone(), |_| {
        ServiceIntercept::Replace(Arc::new(11))
    })
    .unwrap();
    assert_eq!(*left.require_as::<u32>(isolated_key.clone()).unwrap(), 11);
    assert_eq!(*right.require_as::<u32>(isolated_key).unwrap(), 20);
}

#[tokio::test]
async fn mutable_writer_respects_owner_generation_and_write_hooks() {
    let root = Ctx::root().unwrap();
    let key = TypeKey::keyed::<u64>("mutable");
    let (binding, writer) = root.provide_mut_as(key.clone(), Arc::new(1u64)).unwrap();
    let (view, reader) = child(&root, vec![key.clone()]).await;
    let old = reader.require_as::<u64>(key.clone()).unwrap();
    assert_eq!(
        writer.set(&reader, Arc::new(2)).unwrap_err().reason,
        ServiceWriteFailure::WrongOwner
    );
    writer.set(&root, Arc::new(2)).unwrap();
    assert_eq!(*old, 1);
    assert_eq!(*reader.require_as::<u64>(key.clone()).unwrap(), 2);
    assert_eq!(view.state().state, FiberState::Active);

    let replace = root
        .intercept_set_as::<u64>(key.clone(), |value| {
            ServiceIntercept::Replace(Arc::new(*value + 10))
        })
        .unwrap();
    writer.set(&root, Arc::new(3)).unwrap();
    assert_eq!(*reader.require_as::<u64>(key.clone()).unwrap(), 13);
    let deny = root
        .intercept_set_as::<u64>(key.clone(), |_| ServiceIntercept::Deny)
        .unwrap();
    assert_eq!(
        writer.set(&root, Arc::new(4)).unwrap_err().reason,
        ServiceWriteFailure::InterceptDenied
    );
    assert_eq!(*reader.require_as::<u64>(key.clone()).unwrap(), 13);
    deny.dispose().await.unwrap();

    let panic_hook = root
        .intercept_set_as::<u64>(key.clone(), |_| panic!("write hook"))
        .unwrap();
    assert_eq!(
        writer.set(&root, Arc::new(4)).unwrap_err().reason,
        ServiceWriteFailure::InterceptPanicked
    );
    panic_hook.dispose().await.unwrap();
    replace.dispose().await.unwrap();

    let writer = Arc::new(writer);
    let nested_writer = writer.clone();
    let nested_root = root.clone();
    let reentrant = root
        .intercept_set_as::<u64>(key.clone(), move |_| {
            assert_eq!(
                nested_writer
                    .set(&nested_root, Arc::new(99))
                    .unwrap_err()
                    .reason,
                ServiceWriteFailure::InterceptReentrant
            );
            ServiceIntercept::Continue
        })
        .unwrap();
    writer.set(&root, Arc::new(14)).unwrap();
    assert_eq!(*reader.require_as::<u64>(key.clone()).unwrap(), 14);
    reentrant.dispose().await.unwrap();

    binding.dispose().await.unwrap();
    let (_new_binding, current) = root.provide_mut_as(key.clone(), Arc::new(50u64)).unwrap();
    assert_eq!(
        writer.set(&root, Arc::new(99)).unwrap_err().reason,
        ServiceWriteFailure::Stale
    );
    current.set(&root, Arc::new(51)).unwrap();
    assert_eq!(*reader.require_as::<u64>(key).unwrap(), 51);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_writer_cannot_commit_after_binding_is_replaced_during_hook() {
    let root = Ctx::root().unwrap();
    let key = TypeKey::keyed::<u64>("race");
    let (old_binding, old_writer) = root.provide_mut_as(key.clone(), Arc::new(1u64)).unwrap();
    let (entered_tx, entered_rx) = oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new(Mutex::new(release_rx));
    let entered_hook = entered.clone();
    let release_hook = release.clone();
    let _hook = root
        .intercept_set_as::<u64>(key.clone(), move |_| {
            if let Some(tx) = entered_hook.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            release_hook.lock().unwrap().recv().unwrap();
            ServiceIntercept::Continue
        })
        .unwrap();
    let root_writer = root.clone();
    let write = tokio::task::spawn_blocking(move || old_writer.set(&root_writer, Arc::new(2)));
    entered_rx.await.unwrap();
    old_binding.dispose().await.unwrap();
    let (_new_binding, current) = root.provide_mut_as(key.clone(), Arc::new(10u64)).unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(
        write.await.unwrap().unwrap_err().reason,
        ServiceWriteFailure::Stale
    );
    assert_eq!(*root.get_as::<u64>(key).unwrap(), 10);
    drop(current);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subtree_shutdown_waits_for_selected_read_hook() {
    let root = Ctx::root().unwrap();
    let key = TypeKey::keyed::<u64>("shutdown");
    root.provide_as(key.clone(), Arc::new(1u64)).unwrap();
    let (view, reader) = child(&root, vec![key.clone()]).await;
    let (entered_tx, entered_rx) = oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new(Mutex::new(release_rx));
    let entered_hook = entered.clone();
    let release_hook = release.clone();
    reader
        .intercept_require_as::<u64>(key.clone(), move |_| {
            if let Some(tx) = entered_hook.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            release_hook.lock().unwrap().recv().unwrap();
            ServiceIntercept::Continue
        })
        .unwrap();
    let reader_task = reader.clone();
    let read = tokio::task::spawn_blocking(move || reader_task.require_as::<u64>(key));
    entered_rx.await.unwrap();
    let shutdown = view.shutdown();
    tokio::pin!(shutdown);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    assert_eq!(*read.await.unwrap().unwrap(), 1);
    shutdown.await.unwrap();
}
