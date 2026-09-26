#[allow(dead_code, unused_variables, non_snake_case)]
mod generated {
    include!("../../../protocol/generated/rpc.rs");
}
use generated::*;
use rutis_protocol::{
    broker::Broker,
    contract::{AdmittedBundle, Ownership, TypeExpr},
    draft::{DraftSource, GraphExporter},
    error::{ErrorCode, Result},
    exports::{Exports, ObjectIds},
    graph::{DecodedValue, GraphScopes},
    identity::{Activation, Scope, Sequence},
    imports::Imports,
    sdk::{NativeExport, Outbound, RegisteredExport},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
fn owner(name: &str) -> Activation {
    Activation {
        runtime: name.into(),
        epoch: Sequence(1),
        activation: Sequence(1),
    }
}
fn scope(owner: &Activation, id: u64) -> Scope {
    Scope {
        activation: owner.clone(),
        scope: Sequence(id),
    }
}
fn session_type(ownership: Ownership) -> TypeExpr {
    TypeExpr::Object {
        interface: "Session".into(),
        ownership,
    }
}
struct Session {
    agent: Arc<Agent>,
}
struct Agent {
    session: Weak<Session>,
}
impl InterfaceSessionService for Session {
    fn agent(&self) -> Result<Arc<dyn InterfaceAgentService>> {
        Ok(self.agent.clone())
    }
}
impl InterfaceAgentService for Agent {
    fn session(&self) -> Result<Arc<dyn InterfaceSessionService>> {
        Ok(self.session.upgrade().unwrap())
    }
}
fn cyclic() -> Arc<Session> {
    Arc::new_cyclic(|session| Session {
        agent: Arc::new(Agent {
            session: session.clone(),
        }),
    })
}
fn setup() -> (
    Arc<AdmittedBundle>,
    Exports,
    GraphExporter,
    Broker,
    GraphScopes,
) {
    let bundle = Arc::new(
        AdmittedBundle::parse(include_bytes!("../../../protocol/fixtures/rpc.bundle.json"))
            .unwrap(),
    );
    let exports = Exports::new(owner("author"), ObjectIds::default());
    let encoder = GraphExporter::new(bundle.clone(), exports.clone(), bundle.bundle().id.clone());
    let mut broker = Broker::default();
    for name in ["author", "receiver", "third"] {
        let owner = owner(name);
        broker.start_activation(owner.clone()).unwrap();
        broker.open_scope(scope(&owner, 1), None).unwrap();
        broker
            .open_scope(scope(&owner, 2), Some(scope(&owner, 1)))
            .unwrap();
    }
    (
        bundle,
        exports,
        encoder,
        broker,
        GraphScopes {
            scope: scope(&owner("receiver"), 1),
            borrow: scope(&owner("receiver"), 2),
        },
    )
}
#[tokio::test]
async fn draft_grants_are_atomic_and_staging_commit_preserves_cycles_and_inherited_borrows() {
    let (bundle, exports, encoder, mut broker, scopes) = setup();
    let object = cyclic();
    let ty = TypeExpr::Record {
        fields: BTreeMap::from([
            ("borrowed".into(), session_type(Ownership::Borrow)),
            ("scoped".into(), session_type(Ownership::Scope)),
        ]),
    };
    let mut staged = encoder
        .encode(
            &ty,
            Outbound::Record(BTreeMap::from([
                ("borrowed".into(), exportInterfaceSession(object.clone())),
                ("scoped".into(), exportInterfaceSession(object.clone())),
            ])),
        )
        .unwrap();
    assert_eq!(staged.draft.references.len(), 4);
    let mut invalid = staged.draft.clone();
    if let DraftSource::Own { object, .. } = &mut invalid.references[1].source {
        object.owner = owner("third");
    }
    assert_eq!(
        broker
            .offer_graph(
                &owner("author"),
                &bundle,
                &ty,
                &invalid,
                &scopes,
                &bundle.bundle().id
            )
            .err()
            .unwrap()
            .code,
        ErrorCode::CapabilityDenied
    );
    for reference in &staged.draft.references {
        assert_eq!(broker.pins(reference.source.object()), (0, 0));
    }
    let graph = broker
        .offer_graph(
            &owner("author"),
            &bundle,
            &ty,
            &staged.draft,
            &scopes,
            &bundle.bundle().id,
        )
        .unwrap();
    assert_eq!(
        graph.references[0].delivery.id,
        Sequence(1),
        "failed drafts cannot leave grant-sequence gaps"
    );
    let deliveries = graph
        .references
        .iter()
        .map(|r| r.delivery.clone())
        .collect::<Vec<_>>();
    for (reference, offered) in staged.draft.references.iter().zip(&graph.references) {
        assert_eq!(
            offered.delivery.recipient,
            if reference.ownership == Ownership::Borrow {
                scopes.borrow.clone()
            } else {
                scopes.scope.clone()
            }
        );
    }
    let committed = staged.commit(deliveries.clone(), &scopes).unwrap();
    staged.commit(deliveries.clone(), &scopes).unwrap();
    let mut changed = deliveries.clone();
    changed[0].token = "b".repeat(64);
    assert_eq!(
        staged.commit(changed, &scopes).err().unwrap().code,
        ErrorCode::CapabilityDenied
    );
    let imports = Imports::default();
    imports.open_scope(scopes.scope.clone(), None).unwrap();
    imports
        .open_scope(scopes.borrow.clone(), Some(scopes.scope.clone()))
        .unwrap();
    let DecodedValue::Record(mut values) = imports
        .receive_graph(&bundle, &ty, committed, &scopes)
        .unwrap()
    else {
        panic!()
    };
    let DecodedValue::Object(scoped) = values.remove("scoped").unwrap() else {
        panic!()
    };
    let DecodedValue::Object(borrowed) = values.remove("borrowed").unwrap() else {
        panic!()
    };
    assert!(scoped.same_object(&borrowed));
    assert!(!scoped.same_wrapper(&borrowed));
    imports.close_scope(&scopes.borrow);
    assert!(borrowed.property("agent").is_err());
    assert!(scoped.property("agent").is_ok());
    for d in &deliveries {
        exports.release(&rutis_protocol::exports::PinKey::Delivery {
            recipient: d.recipient.activation.clone(),
            id: d.id,
        });
    }
    for reference in &staged.draft.references {
        assert_eq!(exports.pins(reference.source.object()), 0);
    }
}
struct PanicSnapshot(Arc<dyn NativeExport>);
impl NativeExport for PanicSnapshot {
    fn register(&self, exports: &Exports) -> Result<RegisteredExport> {
        self.0.register(exports)
    }
    fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> {
        panic!("native snapshot panicked")
    }
}
#[tokio::test]
async fn panic_and_validation_failure_release_staging_pins_without_issuing_grants() {
    let (bundle, exports, encoder, _, _) = setup();
    let Outbound::Own(native) = exportInterfaceSession(cyclic()) else {
        panic!()
    };
    let identity = native.register(&exports).unwrap().identity;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        encoder.encode(
            &session_type(Ownership::Scope),
            Outbound::Own(Arc::new(PanicSnapshot(native.clone()))),
        )
    }));
    assert!(panic.is_err());
    assert_eq!(exports.pins(&identity), 0);
    let ty = TypeExpr::Record {
        fields: BTreeMap::from([
            ("first".into(), session_type(Ownership::Scope)),
            (
                "last".into(),
                TypeExpr::Value {
                    schema: serde_json::json!({"type":"boolean"}),
                },
            ),
        ]),
    };
    assert!(encoder
        .encode(
            &ty,
            Outbound::Record(BTreeMap::from([
                ("first".into(), Outbound::Own(native.clone())),
                ("last".into(), Outbound::Value(serde_json::json!("wrong")))
            ]))
        )
        .is_err());
    assert_eq!(exports.pins(&identity), 0);
    let other = GraphExporter::new(bundle.clone(), exports.clone(), bundle.bundle().id.clone());
    let first = encoder
        .encode(
            &session_type(Ownership::Scope),
            Outbound::Own(native.clone()),
        )
        .unwrap();
    let second = other
        .encode(&session_type(Ownership::Scope), Outbound::Own(native))
        .unwrap();
    assert!(
        exports.pins(&identity) >= 2,
        "independent bundle encoders cannot reuse staging keys"
    );
    drop(first);
    drop(second);
    assert_eq!(exports.pins(&identity), 0);
}
#[tokio::test]
async fn closed_owner_cannot_encode_even_a_reference_free_result() {
    let (_, exports, encoder, _, _) = setup();
    exports.close();
    let ty = TypeExpr::Value {
        schema: serde_json::json!({"type":"null"}),
    };
    assert_eq!(
        encoder
            .encode(&ty, Outbound::Value(serde_json::Value::Null))
            .err()
            .unwrap()
            .code,
        ErrorCode::ScopeClosed
    );
}
#[tokio::test]
async fn forged_foreign_proof_or_third_party_destination_cannot_grant_authority() {
    let (bundle, _, encoder, mut broker, scopes) = setup();
    let ty = session_type(Ownership::Scope);
    let staged = encoder
        .encode(&ty, exportInterfaceSession(cyclic()))
        .unwrap();
    let graph = broker
        .offer_graph(
            &owner("author"),
            &bundle,
            &ty,
            &staged.draft,
            &scopes,
            &bundle.bundle().id,
        )
        .unwrap();
    for r in &graph.references {
        broker
            .accept(&owner("receiver"), r.delivery.id, &r.delivery.token)
            .unwrap();
    }
    let mut draft = staged.draft.clone();
    for (reference, r) in draft.references.iter_mut().zip(&graph.references) {
        reference.source = DraftSource::Foreign {
            delivery: r.delivery.clone(),
        };
    }
    let author_scopes = GraphScopes::in_scope(scope(&owner("author"), 1));
    let valid = broker
        .offer_graph(
            &owner("receiver"),
            &bundle,
            &ty,
            &draft,
            &author_scopes,
            &bundle.bundle().id,
        )
        .unwrap();
    assert_eq!(valid.references.len(), 2);
    let third_scopes = GraphScopes::in_scope(scope(&owner("third"), 1));
    assert_eq!(
        broker
            .offer_graph(
                &owner("receiver"),
                &bundle,
                &ty,
                &draft,
                &third_scopes,
                &bundle.bundle().id
            )
            .err()
            .unwrap()
            .code,
        ErrorCode::UnsupportedCapability
    );
    if let DraftSource::Foreign { delivery } = &mut draft.references[1].source {
        delivery.token = "f".repeat(64);
    }
    assert!(broker
        .offer_graph(
            &owner("receiver"),
            &bundle,
            &ty,
            &draft,
            &author_scopes,
            &bundle.bundle().id
        )
        .is_err());
    for r in &valid.references {
        assert_eq!(
            broker.pins(&r.delivery.object),
            (2, 0),
            "forged draft must roll back earlier grants"
        );
    }
}
struct ClosingSnapshot {
    inner: Arc<dyn NativeExport>,
    exports: Exports,
}
impl NativeExport for ClosingSnapshot {
    fn register(&self, exports: &Exports) -> Result<RegisteredExport> {
        self.inner.register(exports)
    }
    fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> {
        let fields = self.inner.snapshot()?;
        self.exports.close();
        Ok(fields)
    }
}
#[tokio::test]
async fn failed_owner_pin_still_delivers_a_rejected_manifest_without_a_watermark_hole() {
    let bundle =
        AdmittedBundle::parse(include_bytes!("../../../protocol/fixtures/rpc.bundle.json"))
            .unwrap();
    let network = rutis_protocol::memory::Network::new(bundle);
    let author = network
        .endpoint(owner("author"), ObjectIds::default(), None)
        .unwrap();
    let receiver = network
        .endpoint(owner("receiver"), ObjectIds::default(), None)
        .unwrap();
    // A property-free Database snapshot can close native admission between
    // staging and broker grant commit, without interrupting graph traversal.
    struct Database;
    impl InterfaceDatabaseService for Database {
        fn connect(
            &self,
            _: rutis_protocol::sdk::CallContext,
            _: InterfaceDatabaseMethod0Params,
        ) -> rutis_protocol::sdk::RpcFuture<Arc<dyn InterfaceConnectionService>> {
            unreachable!()
        }
        fn inspect(
            &self,
            _: rutis_protocol::sdk::CallContext,
            _: InterfaceConnectionClient,
        ) -> rutis_protocol::sdk::RpcFuture<bool> {
            unreachable!()
        }
        fn withCallback(
            &self,
            _: rutis_protocol::sdk::CallContext,
            _: BorrowCallback0Client,
        ) -> rutis_protocol::sdk::RpcFuture<()> {
            unreachable!()
        }
    }
    let Outbound::Own(inner) = exportInterfaceDatabase(Arc::new(Database)) else {
        panic!()
    };
    let id = inner.register(author.exports()).unwrap().identity;
    let result = receiver.import::<InterfaceDatabaseClient>(
        &author,
        Outbound::Own(Arc::new(ClosingSnapshot {
            inner,
            exports: author.exports().clone(),
        })),
    );
    assert_eq!(result.err().unwrap().code, ErrorCode::ScopeClosed);
    assert_eq!(receiver.imports().retained_objects(), 0);
    let proposal = receiver.imports().retirement().unwrap();
    assert_eq!(proposal.received_through, Sequence(1));
    assert_eq!(proposal.terminal_through, Sequence(1));
    assert_eq!(network.pins(&id), (0, 0));
    assert_eq!(author.exports().pins(&id), 0);
    receiver.close();
    author.close();
    receiver.exports().join().await.unwrap();
    author.exports().join().await.unwrap();
}
