use rutis::Ctx;
use rutis_protocol::{
    contract::{AdmittedBundle, EventMode},
    error::{ErrorCode, ProtocolError},
    events::{EventKey, HostEvents, ListenerResult},
    exports::{Exports, ObjectIds},
    identity::{Activation, Sequence},
    managed::ActivationGate,
    sdk::Outbound,
    services::Bundles,
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::Semaphore;
fn setup() -> (Ctx, Arc<HostEvents>, Activation, EventKey) {
    let mut bundle: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../protocol/fixtures/settings.bundle.json"
    ))
    .unwrap();
    bundle["events"]["changed"]["params"] = json!({"kind":"value","schema":{"type":"null"}});
    let bytes = serde_json::to_vec(&bundle).unwrap();
    let hash = AdmittedBundle::parse(&bytes).unwrap().sha256().to_owned();
    let hub = HostEvents::new(Bundles::admit([bytes]).unwrap());
    let root = Ctx::root().unwrap();
    let owner = Activation {
        runtime: "events".into(),
        epoch: Sequence(1),
        activation: Sequence(1),
    };
    let key = EventKey {
        scope: "application".into(),
        bundle: hash,
        event: "changed".into(),
    };
    let exports = Exports::managed(
        &root,
        owner.clone(),
        ObjectIds::default(),
        ActivationGate::default(),
    )
    .unwrap();
    hub.bind_publisher(&root, exports, BTreeSet::from([key.clone()]))
        .unwrap();
    (root, hub, owner, key)
}
#[tokio::test]
async fn serial_preserves_zero_false_and_null_and_once_survives_reentrancy() {
    let (root, hub, owner, key) = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let reentrant = hub.clone();
    let emitter = owner.clone();
    let event = key.clone();
    let observed = calls.clone();
    let once = hub
        .subscribe(
            &root,
            key.clone(),
            true,
            Arc::new(move |_| {
                let hub = reentrant.clone();
                let owner = emitter.clone();
                let key = event.clone();
                observed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    assert!(hub
                        .dispatch(
                            &owner,
                            &key,
                            EventMode::Serial,
                            Outbound::Value(json!(null))
                        )
                        .await?
                        .returned
                        .is_none());
                    Ok(ListenerResult::Continue)
                })
            }),
        )
        .unwrap();
    hub.dispatch(
        &owner,
        &key,
        EventMode::Serial,
        Outbound::Value(json!(null)),
    )
    .await
    .unwrap();
    hub.dispatch(
        &owner,
        &key,
        EventMode::Serial,
        Outbound::Value(json!(null)),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    once.unsubscribe().await.unwrap();
    for value in [json!(0), json!(false), json!(null)] {
        let expected = value.clone();
        let first = hub
            .subscribe(
                &root,
                key.clone(),
                false,
                Arc::new(move |_| {
                    let value = value.clone();
                    Box::pin(async move { Ok(ListenerResult::Return(value)) })
                }),
            )
            .unwrap();
        let skipped = hub
            .subscribe(
                &root,
                key.clone(),
                false,
                Arc::new(|_| Box::pin(async { panic!("serial short circuit lost") })),
            )
            .unwrap();
        assert_eq!(
            hub.dispatch(
                &owner,
                &key,
                EventMode::Serial,
                Outbound::Value(json!(null))
            )
            .await
            .unwrap()
            .returned,
            Some(expected)
        );
        first.unsubscribe().await.unwrap();
        skipped.unsubscribe().await.unwrap();
    }
    assert_eq!(
        hub.dispatch(&owner, &key, EventMode::Emit, Outbound::Value(json!(null)))
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::UnsupportedCapability
    );
    assert_eq!(
        hub.dispatch(
            &owner,
            &key,
            EventMode::Parallel,
            Outbound::Value(json!("wrong payload"))
        )
        .await
        .err()
        .unwrap()
        .code,
        ErrorCode::InvalidParams
    );
    let mut foreign = key.clone();
    foreign.scope = "other".into();
    assert_eq!(
        hub.dispatch(
            &owner,
            &foreign,
            EventMode::Parallel,
            Outbound::Value(json!(null))
        )
        .await
        .err()
        .unwrap()
        .code,
        ErrorCode::CapabilityDenied
    );
    root.shutdown().await.unwrap();
}
#[tokio::test]
async fn abandoned_parallel_waiter_and_unsubscribe_still_join_running_work_and_errors() {
    let (root, hub, owner, key) = setup();
    let entered = Arc::new(Semaphore::new(0));
    let resume = Arc::new(Semaphore::new(0));
    let arrived = entered.clone();
    let paused = resume.clone();
    let slow = hub
        .subscribe(
            &root,
            key.clone(),
            false,
            Arc::new(move |_| {
                let entered = arrived.clone();
                let resume = paused.clone();
                Box::pin(async move {
                    entered.add_permits(1);
                    resume.acquire().await.unwrap().forget();
                    Ok(ListenerResult::Continue)
                })
            }),
        )
        .unwrap();
    let failure = hub
        .subscribe(
            &root,
            key.clone(),
            false,
            Arc::new(|_| {
                Box::pin(async {
                    Err(ProtocolError::new(
                        ErrorCode::Business,
                        "listener",
                        "actual failure",
                    ))
                })
            }),
        )
        .unwrap();
    let dispatch = tokio::spawn(hub.dispatch(
        &owner,
        &key,
        EventMode::Parallel,
        Outbound::Value(json!(null)),
    ));
    entered.acquire().await.unwrap().forget();
    dispatch.abort();
    let _ = dispatch.await;
    let mut removing = slow.unsubscribe();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut removing)
            .await
            .is_err()
    );
    resume.add_permits(1);
    removing.await.unwrap();
    let report = hub
        .dispatch(
            &owner,
            &key,
            EventMode::Parallel,
            Outbound::Value(json!(null)),
        )
        .await
        .unwrap();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].message, "actual failure");
    failure.unsubscribe().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_unload_closes_admission_joins_work_and_releases_listener_captures() {
    let (root, hub, owner, key) = setup();
    let native = Ctx::root().unwrap();
    let entered = Arc::new(Semaphore::new(0));
    let resume = Arc::new(Semaphore::new(0));
    let capture = Arc::new(());
    let released = Arc::downgrade(&capture);
    let arrived = entered.clone();
    let paused = resume.clone();
    let subscription = hub
        .subscribe(
            &native,
            key.clone(),
            false,
            Arc::new(move |_| {
                let entered = arrived.clone();
                let resume = paused.clone();
                let capture = capture.clone();
                Box::pin(async move {
                    entered.add_permits(1);
                    resume.acquire().await.unwrap().forget();
                    drop(capture);
                    Ok(ListenerResult::Continue)
                })
            }),
        )
        .unwrap();
    let dispatch = tokio::spawn(hub.dispatch(
        &owner,
        &key,
        EventMode::Parallel,
        Outbound::Value(json!(null)),
    ));
    entered.acquire().await.unwrap().forget();
    let mut shutdown = tokio::spawn(async move { native.shutdown().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut shutdown)
            .await
            .is_err()
    );
    // Native cleanup removed the callback even while the handle remains held.
    assert!(hub
        .dispatch(
            &owner,
            &key,
            EventMode::Parallel,
            Outbound::Value(json!(null))
        )
        .await
        .unwrap()
        .errors
        .is_empty());
    assert_eq!(entered.available_permits(), 0);
    resume.add_permits(1);
    dispatch.await.unwrap().unwrap();
    shutdown.await.unwrap().unwrap();
    assert!(released.upgrade().is_none());
    subscription.unsubscribe().await.unwrap();
    root.shutdown().await.unwrap();
    assert_eq!(
        hub.dispatch(
            &owner,
            &key,
            EventMode::Parallel,
            Outbound::Value(json!(null))
        )
        .await
        .err()
        .unwrap()
        .code,
        ErrorCode::ScopeClosed
    );
}
