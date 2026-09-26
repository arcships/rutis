use rutis_protocol::broker::{Broker, DeliveryState};
use rutis_protocol::error::ErrorCode;
use rutis_protocol::identity::{
    Activation, Delivery, InterfaceView, ObjectIdentity, Scope, Sequence,
};
use rutis_protocol::imports::{ImportControl, Imports};
use std::collections::{BTreeMap, BTreeSet};

fn activation(runtime: &str) -> Activation {
    Activation {
        runtime: runtime.into(),
        epoch: Sequence(1),
        activation: Sequence(1),
    }
}
fn scope(activation: &Activation, id: u64) -> Scope {
    Scope {
        activation: activation.clone(),
        scope: Sequence(id),
    }
}
fn view(source: &str) -> InterfaceView {
    InterfaceView {
        interface: "Connection".into(),
        bundle_sha256: "a".repeat(64),
        source: source.into(),
    }
}

struct Setup {
    broker: Broker,
    imports: Imports,
    owner: Activation,
    caller: Activation,
    scope: Scope,
    object: ObjectIdentity,
}
impl Setup {
    fn new() -> Self {
        let mut broker = Broker::default();
        let owner = activation("rust");
        let caller = activation("node");
        broker.start_activation(owner.clone()).unwrap();
        broker.start_activation(caller.clone()).unwrap();
        let scope = scope(&caller, 1);
        broker.open_scope(scope.clone(), None).unwrap();
        let imports = Imports::default();
        imports.open_scope(scope.clone(), None).unwrap();
        let views = BTreeMap::from([
            (view("route-a"), BTreeSet::from(["query".into()])),
            (
                view("route-b"),
                BTreeSet::from(["query".into(), "close".into()]),
            ),
        ]);
        let object = broker.register_object(&owner, views).unwrap();
        Self {
            broker,
            imports,
            owner,
            caller,
            scope,
            object,
        }
    }
    fn offer(&mut self, recipient: Option<&Scope>, source: &str) -> Delivery {
        self.broker
            .offer(
                &self.owner,
                &self.object,
                recipient.unwrap_or(&self.scope),
                &view(source),
            )
            .unwrap()
    }
    fn flush(&mut self) {
        for control in self.imports.take_controls() {
            match control {
                ImportControl::Accept { id, token } => {
                    self.broker.accept(&self.caller, id, &token).unwrap()
                }
                ImportControl::Release { id, token } => {
                    self.broker.release(&self.caller, id, &token).unwrap()
                }
            }
        }
    }
}

#[test]
fn t04_t07_proxy_identity_and_independent_delivery_pins() {
    let mut s = Setup::new();
    let d1 = s.offer(None, "route-a");
    let first = s.imports.receive(d1.clone()).unwrap();
    let d2 = s.offer(None, "route-a");
    assert_eq!(s.broker.pins(&s.object), (2, 0));
    first.release();
    s.flush();
    assert_eq!(s.broker.pins(&s.object), (1, 0));
    let second = s.imports.receive(d2.clone()).unwrap();
    assert!(!first.same_wrapper(&second));
    assert!(first.same_object(&second));
    assert!(first.delivery().is_err());
    assert!(second.delivery().is_ok());
    s.flush();
    s.broker.accept(&s.caller, d1.id, &d1.token).unwrap();
    assert_eq!(
        s.broker.state(&s.caller, d1.id),
        Some(DeliveryState::Released)
    );
    assert!(s.imports.receive(d1).is_err());
    let d3 = s.offer(None, "route-a");
    let alias = s.imports.receive(d3).unwrap();
    assert!(alias.same_wrapper(&second));
    second.release();
    assert!(alias.delivery().is_err());
    s.flush();
    assert_eq!(s.broker.pins(&s.object), (0, 0));
}

#[test]
fn t05_permissions_do_not_merge_and_fake_delegation_is_rejected() {
    let mut s = Setup::new();
    let d1 = s.offer(None, "route-a");
    let first = s.imports.receive(d1.clone()).unwrap();
    let d2 = s.offer(None, "route-b");
    let second = s.imports.receive(d2.clone()).unwrap();
    assert!(first.same_object(&second));
    assert!(!first.same_wrapper(&second));
    s.flush();
    assert_eq!(
        s.broker
            .begin_call(&s.scope, Sequence(1), &d1, "close", &s.owner)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityDenied
    );
    let mut forged = d1.clone();
    forged.token = d2.token.clone();
    assert_eq!(
        s.broker
            .begin_call(&s.scope, Sequence(1), &forged, "query", &s.owner)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityDenied
    );
    assert_eq!(
        s.broker
            .offer(&s.caller, &s.object, &s.scope, &view("route-a"))
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    assert_eq!(
        s.broker
            .begin_call(
                &s.scope,
                Sequence(1),
                &d2,
                "query",
                &activation("third-party")
            )
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    first.release();
    s.flush();
    assert!(second.delivery().is_ok());
}

#[test]
fn t06_independent_child_scopes_and_parent_close() {
    let mut s = Setup::new();
    let a = scope(&s.caller, 2);
    let b = scope(&s.caller, 3);
    for child in [&a, &b] {
        s.broker
            .open_scope(child.clone(), Some(s.scope.clone()))
            .unwrap();
        s.imports
            .open_scope(child.clone(), Some(s.scope.clone()))
            .unwrap();
    }
    let d1 = s.offer(Some(&a), "route-a");
    let d2 = s.offer(Some(&b), "route-a");
    let first = s.imports.receive(d1).unwrap();
    let second = s.imports.receive(d2).unwrap();
    s.flush();
    s.imports.close_scope(&a);
    s.broker.close_scope(&a);
    s.flush();
    assert!(first.delivery().is_err());
    assert!(second.delivery().is_ok());
    assert_eq!(s.broker.pins(&s.object), (1, 0));
    s.imports.close_scope(&s.scope);
    s.broker.close_scope(&s.scope);
    s.flush();
    assert!(second.delivery().is_err());
    assert_eq!(s.broker.pins(&s.object), (0, 0));
    assert!(s.broker.open_scope(a.clone(), None).is_err());
    assert!(s.imports.open_scope(a, None).is_err());
    assert!(s
        .broker
        .offer(&s.owner, &s.object, &s.scope, &view("route-a"))
        .is_err());
}

#[test]
fn t07_retirement_requires_a_confirmed_contiguous_terminal_prefix() {
    let mut s = Setup::new();
    let d1 = s.offer(None, "route-a");
    let d2 = s.offer(None, "route-a");
    let second = s.imports.receive(d2.clone()).unwrap();
    second.release();
    assert!(
        s.imports.acknowledge_retirement(d2.id).is_err(),
        "missing d1 envelope prevents retirement"
    );
    assert!(
        s.broker.retire(&s.caller, d2.id, d2.id).is_err(),
        "live offered pins prevent retirement"
    );
    let first = s.imports.receive(d1.clone()).unwrap();
    first.release();
    s.flush();
    s.imports.acknowledge_retirement(d2.id).unwrap();
    s.broker.retire(&s.caller, d2.id, d2.id).unwrap();
    s.imports.acknowledge_retirement(d2.id).unwrap();
    s.broker.accept(&s.caller, d1.id, &d1.token).unwrap();
    s.broker.release(&s.caller, d2.id, &d2.token).unwrap();
    assert!(
        s.imports.receive(d1).is_err(),
        "late old envelope does not become a new object"
    );
    assert_eq!(s.broker.pins(&s.object), (0, 0));
    assert!(
        s.broker.accept(&s.caller, Sequence(99), "fake").is_err(),
        "unknown new token is rejected"
    );
    let d3 = s.offer(None, "route-a");
    assert!(d3.id > d2.id);
    assert!(s.imports.receive(d3).is_ok());
}

#[test]
fn t17_waiter_release_does_not_release_executing_borrow_pins() {
    let mut s = Setup::new();
    let delivery = s.offer(None, "route-a");
    let proxy = s.imports.receive(delivery.clone()).unwrap();
    s.flush();
    s.broker
        .begin_call(&s.scope, Sequence(1), &delivery, "query", &s.owner)
        .unwrap();
    proxy.release();
    s.flush();
    assert_eq!(s.broker.pins(&s.object), (0, 1));
    assert!(s
        .broker
        .finish_call(&activation("forged"), &s.caller, Sequence(1))
        .is_err());
    s.broker.close_activation(&s.owner);
    assert_eq!(s.broker.pins(&s.object), (0, 1));
    s.broker
        .finish_call(&s.owner, &s.caller, Sequence(1))
        .unwrap();
    s.broker
        .finish_call(&s.owner, &s.caller, Sequence(1))
        .unwrap();
    assert_eq!(s.broker.pins(&s.object), (0, 0));
}

#[test]
fn closed_epoch_and_activation_ids_are_never_reused() {
    let mut s = Setup::new();
    let delivery = s.offer(None, "route-a");
    s.broker.close_activation(&s.owner);
    assert!(s.broker.start_activation(s.owner.clone()).is_err());
    s.broker.close_activation(&s.caller);
    s.broker
        .accept(&s.caller, delivery.id, &delivery.token)
        .unwrap();
    s.broker
        .release(&s.caller, delivery.id, &delivery.token)
        .unwrap();
    s.broker.close_epoch(&s.caller.runtime, s.caller.epoch);
    s.broker
        .accept(&s.caller, delivery.id, &delivery.token)
        .unwrap();
    s.broker
        .release(&s.caller, delivery.id, &delivery.token)
        .unwrap();
    let mut same_epoch = s.caller.clone();
    same_epoch.activation = Sequence(2);
    assert!(s.broker.start_activation(same_epoch.clone()).is_err());
    same_epoch.epoch = Sequence(2);
    s.broker.start_activation(same_epoch).unwrap();
}

#[test]
fn reused_call_ids_and_new_epochs_cannot_overtake_execution_pins() {
    let mut s = Setup::new();
    let delivery = s.offer(None, "route-a");
    s.imports.receive(delivery.clone()).unwrap();
    s.flush();
    s.broker
        .begin_call(&s.scope, Sequence(1), &delivery, "query", &s.owner)
        .unwrap();
    s.broker
        .finish_call(&s.owner, &s.caller, Sequence(1))
        .unwrap();
    assert_eq!(
        s.broker
            .begin_call(&s.scope, Sequence(1), &delivery, "query", &s.owner)
            .unwrap_err()
            .code,
        ErrorCode::StaleObject
    );
    s.broker
        .begin_call(&s.scope, Sequence(2), &delivery, "query", &s.owner)
        .unwrap();
    s.broker
        .finish_call(&s.owner, &s.caller, Sequence(1))
        .unwrap();
    assert_eq!(s.broker.pins(&s.object), (1, 1));
    s.broker.close_epoch(&s.owner.runtime, s.owner.epoch);
    let mut next = s.owner.clone();
    next.epoch = Sequence(2);
    assert!(s.broker.start_activation(next.clone()).is_err());
    s.broker
        .finish_call(&s.owner, &s.caller, Sequence(2))
        .unwrap();
    s.broker.start_activation(next).unwrap();
}

#[test]
fn t07_receive_and_release_race_is_serialized_without_reviving_old_wrappers() {
    for _ in 0..32 {
        let mut s = Setup::new();
        let d1 = s.offer(None, "route-a");
        let first = s.imports.receive(d1).unwrap();
        let d2 = s.offer(None, "route-a");
        let barrier = std::sync::Barrier::new(2);
        let second = std::thread::scope(|threads| {
            let released = threads.spawn(|| {
                barrier.wait();
                first.release();
            });
            let received = threads.spawn(|| {
                barrier.wait();
                s.imports.receive(d2).unwrap()
            });
            released.join().unwrap();
            received.join().unwrap()
        });
        assert!(first.delivery().is_err());
        // If receive won, d2 was already attached when release took the token
        // snapshot; if release won, d2 belongs to a fresh wrapper.
        if first.same_wrapper(&second) {
            assert!(second.delivery().is_err());
        } else {
            assert!(second.delivery().is_ok());
            second.release();
        }
        s.flush();
        assert_eq!(s.broker.pins(&s.object), (0, 0));
    }
}
