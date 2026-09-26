use rutis_protocol::error::ErrorCode;
use rutis_protocol::exports::{Exports, ObjectIds, PinKey};
use rutis_protocol::identity::{Activation, Sequence};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::oneshot;

fn activation(n: u64) -> Activation {
    Activation {
        runtime: "owner".into(),
        epoch: Sequence(1),
        activation: Sequence(n),
    }
}
fn execution(n: u64) -> PinKey {
    PinKey::Execution {
        caller: activation(9),
        call: Sequence(n),
    }
}
fn delivery(n: u64) -> PinKey {
    PinKey::Delivery {
        recipient: activation(9),
        id: Sequence(n),
    }
}
struct Resource(Arc<AtomicUsize>);
impl Drop for Resource {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn real_arc_identity_and_execution_pin_outlive_delivery_release() {
    let exports = Exports::new(activation(1), ObjectIds::default());
    let drops = Arc::new(AtomicUsize::new(0));
    let object = Arc::new(Resource(drops.clone()));
    let weak = Arc::downgrade(&object);
    let id = exports.register(&object).unwrap();
    assert_eq!(id, exports.register(&object).unwrap());
    assert_ne!(
        id,
        exports
            .register(&Arc::new(Resource(drops.clone())))
            .unwrap()
    );
    drops.store(0, Ordering::SeqCst);
    exports.pin(&id, delivery(1)).unwrap();
    exports.pin(&id, delivery(1)).unwrap(); // retransmission is not another pin
    exports.pin(&id, delivery(2)).unwrap();
    exports.pin(&id, execution(1)).unwrap();
    assert!(Arc::ptr_eq(
        &object,
        &exports.execution_object::<Resource>(&execution(1)).unwrap()
    ));
    drop(object);
    exports.release(&delivery(1));
    exports.release(&delivery(1));
    exports.release(&delivery(2));
    assert_eq!(exports.pins(&id), 1);
    assert!(weak.upgrade().is_some());
    exports.release(&execution(1));
    assert!(weak.upgrade().is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        exports.pin(&id, delivery(3)).unwrap_err().code,
        ErrorCode::StaleObject
    );
    exports.sweep();
    assert_eq!(
        exports.pin(&id, delivery(3)).unwrap_err().code,
        ErrorCode::StaleObject
    );
}

#[tokio::test]
async fn shared_local_ownership_survives_release_and_epoch_ids_span_activations() {
    let ids = ObjectIds::default();
    let first = Exports::new(activation(1), ids.clone());
    let second = Exports::new(activation(2), ids);
    let object = Arc::new(String::from("local"));
    let id = first.register(&object).unwrap();
    first.pin(&id, delivery(1)).unwrap();
    first.release(&delivery(1));
    assert_eq!(first.register(&object).unwrap(), id);
    first.pin(&id, delivery(2)).unwrap();
    assert!(second.register(&object).unwrap().object > id.object);
    first.release(&delivery(2));
    first.join().await.unwrap();
    assert_eq!(&*object, "local");
}

#[tokio::test]
async fn exclusive_disposer_runs_once_and_dropped_stop_waiter_does_not_cancel_it() {
    let exports = Exports::new(activation(1), ObjectIds::default());
    let object = Arc::new(String::from("connection"));
    let calls = Arc::new(AtomicUsize::new(0));
    let (started, entered) = oneshot::channel();
    let (finish, waiting) = oneshot::channel();
    let count = calls.clone();
    let original = Arc::downgrade(&object);
    let id = exports
        .register_exclusive(&object, move |actual| async move {
            assert!(Arc::ptr_eq(&actual, &original.upgrade().unwrap()));
            count.fetch_add(1, Ordering::SeqCst);
            started.send(()).unwrap();
            waiting.await.unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(exports.register(&object).unwrap(), id);
    exports.pin(&id, delivery(1)).unwrap();
    exports.pin(&id, execution(1)).unwrap();
    exports.close();
    assert_eq!(exports.pins(&id), 1);
    assert_eq!(
        exports.pin(&id, delivery(2)).unwrap_err().code,
        ErrorCode::ScopeClosed
    );
    let waiter = tokio::spawn({
        let exports = exports.clone();
        async move { exports.join().await }
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    waiter.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    exports.release(&execution(1));
    entered.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    exports.release(&execution(1));
    exports.close();
    let waiter = tokio::spawn({
        let exports = exports.clone();
        async move { exports.join().await }
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    finish.send(()).unwrap();
    waiter.await.unwrap().unwrap();
    exports.join().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cleanup_failure_is_confirmed_and_reported_to_all_joiners() {
    let exports = Exports::new(activation(1), ObjectIds::default());
    let object = Arc::new(());
    let id = exports
        .register_exclusive(&object, |_| async { panic!("failed disposer") })
        .unwrap();
    exports.pin(&id, delivery(1)).unwrap();
    exports.release(&delivery(1));
    assert_eq!(exports.join().await.unwrap_err().code, ErrorCode::Business);
    assert_eq!(exports.join().await.unwrap_err().code, ErrorCode::Business);
    assert_eq!(
        exports.register(&object).unwrap_err().code,
        ErrorCode::StaleObject
    );
}

#[tokio::test]
async fn terminal_pins_do_not_revive_after_release_or_confirmed_retirement() {
    let exports = Exports::new(activation(1), ObjectIds::default());
    let object = Arc::new(());
    let id = exports.register(&object).unwrap();
    exports.pin(&id, delivery(1)).unwrap();
    exports.pin(&id, delivery(2)).unwrap();
    exports.release(&delivery(2));
    assert_eq!(
        exports.pin(&id, delivery(2)).unwrap_err().code,
        ErrorCode::StaleObject
    );
    assert_eq!(
        exports
            .retire_deliveries("owner", Sequence(1), Sequence(2))
            .unwrap_err()
            .code,
        ErrorCode::InvalidParams
    );
    exports.release(&delivery(1));
    exports
        .retire_deliveries("owner", Sequence(1), Sequence(2))
        .unwrap();
    exports
        .retire_deliveries("owner", Sequence(1), Sequence(2))
        .unwrap();
    assert_eq!(
        exports.pin(&id, delivery(1)).unwrap_err().code,
        ErrorCode::StaleObject
    );
    exports.release(&delivery(1));
    exports.pin(&id, delivery(3)).unwrap();
    assert_eq!(exports.pins(&id), 1);
    exports.close();
    exports.join().await.unwrap();
}

#[tokio::test]
async fn closed_recipient_epoch_discards_delivery_state_without_releasing_execution() {
    let exports = Exports::new(activation(1), ObjectIds::default());
    let object = Arc::new(());
    let id = exports.register(&object).unwrap();
    exports.pin(&id, delivery(1)).unwrap();
    exports.pin(&id, execution(1)).unwrap();
    exports.close_recipient_epoch("owner", Sequence(1));
    exports.close_recipient_epoch("owner", Sequence(1));
    assert_eq!(exports.pins(&id), 1);
    assert_eq!(
        exports.pin(&id, delivery(2)).unwrap_err().code,
        ErrorCode::StaleObject
    );
    exports.release(&delivery(1));
    assert_eq!(exports.pins(&id), 1);
    exports.release(&execution(1));
    exports.join().await.unwrap();
    let next = PinKey::Delivery {
        recipient: Activation {
            epoch: Sequence(2),
            ..activation(9)
        },
        id: Sequence(1),
    };
    exports.pin(&id, next.clone()).unwrap();
    exports.release(&next);
    exports.join().await.unwrap();
}

#[tokio::test]
async fn unpublished_exclusive_registration_is_owned_and_cleaned_on_rollback() {
    let exports = Exports::new(activation(1), ObjectIds::default());
    let object = Arc::new(());
    let weak = Arc::downgrade(&object);
    let disposed = Arc::new(AtomicUsize::new(0));
    let count = disposed.clone();
    let id = exports
        .register_exclusive(&object, move |_| async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    assert_eq!(exports.pins(&id), 0);
    drop(object);
    assert!(weak.upgrade().is_some());
    exports.close();
    exports.close();
    exports.join().await.unwrap();
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
    assert!(weak.upgrade().is_none());
}
