#[allow(dead_code, unused_variables, non_snake_case)]
mod database {
    include!("../../../protocol/generated/database.rs");
}
#[allow(dead_code, unused_variables, non_snake_case)]
mod rpc {
    include!("../../../protocol/generated/rpc.rs");
}
use database::*;
use rutis::{BoxFuture, CordisError, Ctx, Effect, FiberState, Plugin, PluginFactory, TypeKey};
use rutis_protocol::{
    broker::Broker,
    contract::{AdmittedBundle, FAMILY, VERSION},
    draft::DraftSource,
    error::{ErrorCode, Result},
    exports::{Exports, ObjectIds},
    factories::{StaticFactories, StaticFactory},
    graph::GraphScopes,
    identity::{Activation, Delivery, Scope, Sequence},
    managed::{ActivationGate, ManagedActivation},
    memory::Network,
    prepare::digest,
    runner_image::{FactoryCatalog, RunnerCatalog},
    sdk::*,
    services::{offer_table, validate_table, Bundles, NativePorts},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
};

const BUNDLE: &[u8] = include_bytes!("../../../protocol/fixtures/database.bundle.json");
const RPC: &[u8] = include_bytes!("../../../protocol/fixtures/rpc.bundle.json");
const CONFIG: &[u8] = br#"{"type":"object","additionalProperties":false}"#;
fn owner(runtime: &str, activation: u64) -> Activation {
    Activation {
        runtime: runtime.into(),
        epoch: Sequence(1),
        activation: Sequence(activation),
    }
}
fn scope(activation: Activation) -> Scope {
    Scope {
        activation,
        scope: Sequence(1),
    }
}
fn bundles() -> Bundles {
    Bundles::admit([BUNDLE.to_vec(), RPC.to_vec()]).unwrap()
}
fn metadata() -> FactoryCatalog {
    FactoryCatalog {
        config_sha256: digest(CONFIG),
        provides: BTreeMap::new(),
        requires: BTreeMap::new(),
    }
}
#[derive(Clone)]
struct Native {
    injects: Vec<TypeKey>,
    run: Arc<
        dyn Fn(Ctx) -> BoxFuture<'static, std::result::Result<Effect, CordisError>> + Send + Sync,
    >,
}
impl Plugin for Native {
    fn name(&self) -> &str {
        "native-service"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        (self.run)(ctx.clone())
    }
}
#[derive(Clone)]
struct Factory(Native);
impl PluginFactory<serde_json::Value> for Factory {
    fn injects(&self) -> &[TypeKey] {
        &self.0.injects
    }
    fn build(&self, _: &serde_json::Value) -> std::result::Result<Box<dyn Plugin>, CordisError> {
        Ok(Box::new(self.0.clone()))
    }
}
fn registry(plugin: Native, definition: FactoryCatalog) -> StaticFactories {
    let catalog = RunnerCatalog {
        protocol_family: FAMILY.into(),
        protocol_version: VERSION.into(),
        framework_version: "0.3.0".into(),
        environment_sha256: digest(b"service-test"),
        capabilities: BTreeSet::new(),
        factories: BTreeMap::from([("service".into(), definition.clone())]),
    };
    let entry = StaticFactory::native("service", definition, CONFIG, move || {
        Factory(plugin.clone())
    })
    .unwrap();
    StaticFactories::admit(&serde_json::to_vec(&catalog).unwrap(), [entry]).unwrap()
}
fn native(
    run: impl Fn(Ctx) -> BoxFuture<'static, std::result::Result<Effect, CordisError>>
        + Send
        + Sync
        + 'static,
) -> Native {
    Native {
        injects: vec![],
        run: Arc::new(run),
    }
}
fn assert_creator(context: &CallContext, creator: &Ctx) {
    let key = TypeKey::of::<dyn InterfaceDatabaseService>();
    assert!(Arc::ptr_eq(
        &context
            .native()
            .unwrap()
            .get_as::<dyn InterfaceDatabaseService>(key.clone())
            .unwrap(),
        &creator.get_as::<dyn InterfaceDatabaseService>(key).unwrap()
    ));
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
fn session() -> Arc<Session> {
    Arc::new_cyclic(|session| Session {
        agent: Arc::new(Agent {
            session: session.clone(),
        }),
    })
}
struct RpcSession {
    agent: Arc<RpcAgent>,
}
struct RpcAgent {
    session: Weak<RpcSession>,
}
impl rpc::InterfaceSessionService for RpcSession {
    fn agent(&self) -> Result<Arc<dyn rpc::InterfaceAgentService>> {
        Ok(self.agent.clone())
    }
}
impl rpc::InterfaceAgentService for RpcAgent {
    fn session(&self) -> Result<Arc<dyn rpc::InterfaceSessionService>> {
        Ok(self.session.upgrade().unwrap())
    }
}
fn rpc_session() -> Arc<RpcSession> {
    Arc::new_cyclic(|session| RpcSession {
        agent: Arc::new(RpcAgent {
            session: session.clone(),
        }),
    })
}
struct Connection {
    session: Arc<Session>,
    queries: AtomicUsize,
    creator: Ctx,
}
impl InterfaceConnectionService for Connection {
    fn session(&self) -> Result<Arc<dyn InterfaceSessionService>> {
        Ok(self.session.clone())
    }
    fn query(
        &self,
        context: CallContext,
        params: InterfaceConnectionMethod0Params,
    ) -> RpcFuture<Vec<BTreeMap<String, serde_json::Value>>> {
        assert_creator(&context, &self.creator);
        let count = self.queries.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            Ok(vec![BTreeMap::from([
                ("count".into(), serde_json::json!(count)),
                ("sql".into(), serde_json::json!(params.sql)),
            ])])
        })
    }
}
struct Database {
    connection: Arc<Connection>,
}
impl InterfaceDatabaseService for Database {
    fn connect(
        &self,
        context: CallContext,
        _: InterfaceDatabaseMethod0Params,
    ) -> RpcFuture<Arc<dyn InterfaceConnectionService>> {
        assert_creator(&context, &self.connection.creator);
        let value: Arc<dyn InterfaceConnectionService> = self.connection.clone();
        Box::pin(async move { Ok(value) })
    }
    fn inspect(&self, _: CallContext, params: InterfaceConnectionClient) -> RpcFuture<bool> {
        Box::pin(async move {
            Ok(params
                .query(InterfaceConnectionMethod0Params {
                    sql: "returned".into(),
                })
                .await?[0]["sql"]
                == "returned")
        })
    }
    fn withCallback(&self, _: CallContext, _: BorrowCallback0Client) -> RpcFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn named_roots_bind_real_generated_clients_in_independent_native_scopes() {
    let root = Ctx::root().unwrap();
    let bundles = bundles();
    let network = Network::new(AdmittedBundle::parse(BUNDLE).unwrap());
    let ids = ObjectIds::default();
    let consumer_ids = ObjectIds::default();
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut members = Vec::new();
    for index in 1..=2 {
        let mut ports = NativePorts::default();
        ports
            .provide::<dyn InterfaceDatabaseService, InterfaceDatabaseClient>(
                "db",
                TypeKey::of::<dyn InterfaceDatabaseService>(),
                &bundles,
                exportInterfaceDatabase,
            )
            .unwrap();
        let gate = ActivationGate::default();
        let bindings = ports.scope(&root, owner("provider", index), gate.clone());
        let provide = native(|ctx| {
            Box::pin(async move {
                let service: Arc<dyn InterfaceDatabaseService> = Arc::new(Database {
                    connection: Arc::new(Connection {
                        session: session(),
                        queries: AtomicUsize::new(0),
                        creator: ctx.clone(),
                    }),
                });
                ctx.provide_as(TypeKey::of::<dyn InterfaceDatabaseService>(), service)?;
                Ok(Effect::Done)
            })
        });
        let provider =
            ManagedActivation::mount_gated(bindings.ctx(), provide, gate.clone()).unwrap();
        provider.view().await.unwrap();
        let original = provider.native_context().unwrap();
        let exports =
            Exports::managed(&original, bindings.owner().clone(), ids.clone(), gate).unwrap();
        let endpoint = network
            .endpoint_with_exports(bindings.owner().clone(), exports.clone(), Some(original))
            .unwrap();
        let guards = ports.guard_exports(&bindings, &provider).unwrap();
        for guard in &guards {
            guard.await.unwrap();
            assert_eq!(guard.state().state, FiberState::Active);
        }
        let mut staged = ports.stage(&bindings, bundles.clone(), exports).unwrap();
        assert_eq!(
            ports
                .stage(&bindings, bundles.clone(), endpoint.exports().clone())
                .err()
                .unwrap()
                .code,
            ErrorCode::Unavailable
        );

        let recipient = owner("consumer", index);
        let target = network
            .endpoint(recipient.clone(), consumer_ids.clone(), None)
            .unwrap();
        let values = target.import_services(&endpoint, &mut staged).unwrap();
        let mut requires = NativePorts::default();
        requires
            .require::<InterfaceDatabaseClient>(
                "db",
                TypeKey::of::<InterfaceDatabaseClient>(),
                &bundles,
            )
            .unwrap();
        let gate = ActivationGate::default();
        let mut consumer_bindings = requires.scope(&root, recipient.clone(), gate.clone());
        target
            .imports()
            .bind_scope(&scope(recipient), gate.clone())
            .unwrap();
        requires
            .install(&mut consumer_bindings, values, target.caller())
            .unwrap();
        let captured = results.clone();
        let mut consume = native(move |ctx| {
            let captured = captured.clone();
            Box::pin(async move {
                let db = ctx.require::<InterfaceDatabaseClient>()?;
                let work = async {
                    let connection = db
                        .connect(InterfaceDatabaseMethod0Params {
                            name: "native".into(),
                        })
                        .await?;
                    let same = db
                        .connect(InterfaceDatabaseMethod0Params {
                            name: "again".into(),
                        })
                        .await?;
                    assert!(same_wrapper(&connection, &same));
                    assert!(same_wrapper(
                        &connection.session()?.agent()?.session()?,
                        &connection.session()?
                    ));
                    let row = connection
                        .query(InterfaceConnectionMethod0Params {
                            sql: format!("member-{index}"),
                        })
                        .await?;
                    assert!(db.inspect(connection).await?);
                    captured.lock().unwrap().push(row[0].clone());
                    Ok::<_, rutis_protocol::error::ProtocolError>(())
                };
                work.await
                    .map_err(|e| CordisError::PluginFailed(Box::new(e)))?;
                Ok(Effect::Done)
            })
        });
        consume.injects = requires.required_keys();
        let mut definition = metadata();
        definition.requires.insert(
            "db".into(),
            bundles.client::<InterfaceDatabaseClient>().unwrap(),
        );
        requires.check(&definition).unwrap();
        let linked = registry(consume, definition);
        let consumer = linked
            .mount_bound(
                consumer_bindings.ctx(),
                "service",
                serde_json::json!({}),
                gate,
                &requires.required_keys(),
            )
            .unwrap();
        consumer_bindings.adopt(&consumer).unwrap();
        consumer.view().await.unwrap();
        assert_eq!(consumer.view().state().state, FiberState::Active);
        assert!(root.get::<InterfaceDatabaseClient>().is_none());
        members.push((
            provider,
            consumer,
            bindings,
            consumer_bindings,
            endpoint,
            target,
            guards,
        ));
    }
    assert_eq!(results.lock().unwrap().len(), 2);
    assert!(results.lock().unwrap().iter().all(|row| row["count"] == 1));
    let old = members[0].1.native_context().unwrap();
    drop(members[0].1.stop());
    assert!(old.effect(|| Effect::Done).is_err());
    members[0].1.stop().await.unwrap();
    assert!(members[0]
        .3
        .ctx()
        .get::<InterfaceDatabaseClient>()
        .is_none());
    assert_eq!(members[1].1.view().state().state, FiberState::Active);
    assert!(members[1]
        .3
        .ctx()
        .get::<InterfaceDatabaseClient>()
        .is_some());
    for (provider, consumer, _, _, endpoint, target, guards) in members {
        consumer.stop().await.unwrap();
        target.close();
        endpoint.close();
        provider.stop().await.unwrap();
        for guard in guards {
            assert_eq!(guard.state().state, FiberState::Disposed);
        }
    }
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn complete_multi_bundle_offer_and_native_commit_have_no_partial_authority() {
    let root = Ctx::root().unwrap();
    let bundles = bundles();
    let activation = owner("provider", 1);
    let mut ports = NativePorts::default();
    ports
        .provide::<dyn InterfaceSessionService, InterfaceSessionClient>(
            "a",
            TypeKey::of::<dyn InterfaceSessionService>(),
            &bundles,
            exportInterfaceSession,
        )
        .unwrap();
    ports
        .provide::<dyn rpc::InterfaceSessionService, rpc::InterfaceSessionClient>(
            "z",
            TypeKey::of::<dyn rpc::InterfaceSessionService>(),
            &bundles,
            rpc::exportInterfaceSession,
        )
        .unwrap();
    let gate = ActivationGate::default();
    let bindings = ports.scope(&root, activation.clone(), gate.clone());
    let plugin = native(|ctx| {
        Box::pin(async move {
            ctx.provide_as(
                TypeKey::of::<dyn InterfaceSessionService>(),
                session() as Arc<dyn InterfaceSessionService>,
            )?;
            ctx.provide_as(
                TypeKey::of::<dyn rpc::InterfaceSessionService>(),
                rpc_session() as Arc<dyn rpc::InterfaceSessionService>,
            )?;
            Ok(Effect::Done)
        })
    });
    let native =
        ManagedActivation::mount_gated(bindings.ctx(), plugin.clone(), gate.clone()).unwrap();
    native.view().await.unwrap();
    let exports = Exports::managed(
        &native.native_context().unwrap(),
        activation.clone(),
        ObjectIds::default(),
        gate,
    )
    .unwrap();
    let mut staged = ports
        .stage(&bindings, bundles.clone(), exports.clone())
        .unwrap();
    let mut broker = Broker::default();
    let recipient = owner("host", 1);
    broker.start_activation(activation.clone()).unwrap();
    broker.start_activation(recipient.clone()).unwrap();
    broker.open_scope(scope(activation.clone()), None).unwrap();
    broker.open_scope(scope(recipient.clone()), None).unwrap();
    let scopes = GraphScopes::in_scope(scope(recipient.clone()));
    let mut forged = staged.table().clone();
    let reference = &mut forged.services.get_mut("z").unwrap().graph.references[1];
    reference.source = DraftSource::Foreign {
        delivery: Delivery {
            id: Sequence(900),
            token: "not-a-grant".into(),
            object: reference.source.object().clone(),
            view: reference.source.view().clone(),
            recipient: scope(activation.clone()),
        },
    };
    // This is a well-formed graph; rejection happens in the second broker
    // transaction after the first named graph would otherwise allocate grants.
    validate_table(&forged, staged.contracts(), &bundles).unwrap();
    assert!(offer_table(
        &mut broker,
        &activation,
        &forged,
        staged.contracts(),
        &bundles,
        &scopes
    )
    .is_err());
    let first = staged.table().services["a"].graph.references[0]
        .source
        .object()
        .clone();
    assert_eq!(broker.pins(&first), (0, 0));
    let grants = offer_table(
        &mut broker,
        &activation,
        staged.table(),
        staged.contracts(),
        &bundles,
        &scopes,
    )
    .unwrap();
    assert_eq!(grants["a"].references[0].delivery.id, Sequence(1));
    let mut changed = grants.clone();
    changed.get_mut("z").unwrap().references[0]
        .delivery
        .object
        .object = Sequence(700);
    let before = exports.pins(&first);
    assert_eq!(
        staged.commit(&changed, &scopes).unwrap_err().code,
        ErrorCode::CapabilityDenied
    );
    assert_eq!(exports.pins(&first), before);
    staged.commit(&grants, &scopes).unwrap();
    let committed = exports.pins(&first);
    staged.commit(&grants, &scopes).unwrap();
    assert_eq!(exports.pins(&first), committed);
    let mut changed = grants.clone();
    changed.get_mut("a").unwrap().references[0].delivery.token = "replay-changed".into();
    assert_eq!(
        staged.commit(&changed, &scopes).unwrap_err().code,
        ErrorCode::CapabilityDenied
    );
    assert_eq!(exports.pins(&first), committed);
    let mut missing = staged.table().clone();
    missing.services.remove("z");
    assert_eq!(
        validate_table(&missing, staged.contracts(), &bundles)
            .unwrap_err()
            .code,
        ErrorCode::InterfaceMismatch
    );
    let mut foreign_root = staged.table().clone();
    foreign_root.services.get_mut("a").unwrap().graph.references[0].source =
        forged.services["z"].graph.references[1].source.clone();
    assert!(validate_table(&foreign_root, staged.contracts(), &bundles).is_err());
    let next = owner("abort", 1);
    let next_gate = ActivationGate::default();
    let next_bindings = ports.scope(&root, next.clone(), next_gate.clone());
    let next_native =
        ManagedActivation::mount_gated(next_bindings.ctx(), plugin, next_gate.clone()).unwrap();
    next_native.view().await.unwrap();
    let next_exports = Exports::managed(
        &next_native.native_context().unwrap(),
        next.clone(),
        ObjectIds::default(),
        next_gate,
    )
    .unwrap();
    let mut failed = ports
        .stage(&next_bindings, bundles.clone(), next_exports.clone())
        .unwrap();
    broker.start_activation(next.clone()).unwrap();
    broker.open_scope(scope(next.clone()), None).unwrap();
    let proposed = offer_table(
        &mut broker,
        &next,
        failed.table(),
        failed.contracts(),
        &bundles,
        &scopes,
    )
    .unwrap();
    let second = &proposed["z"].references[0].delivery;
    next_exports.release(&rutis_protocol::exports::PinKey::Delivery {
        recipient: recipient.clone(),
        id: second.id,
    });
    assert_eq!(
        failed.commit(&proposed, &scopes).unwrap_err().code,
        ErrorCode::StaleObject
    );
    // Failure in the last graph releases converted delivery pins and all
    // remaining staging pins, including earlier successful native graphs.
    for graph in proposed.values() {
        for reference in &graph.references {
            assert_eq!(next_exports.pins(&reference.delivery.object), 0);
            broker
                .release(&recipient, reference.delivery.id, &reference.delivery.token)
                .unwrap();
        }
    }
    assert_eq!(
        failed.commit(&proposed, &scopes).unwrap_err().code,
        ErrorCode::ScopeClosed
    );
    next_native.stop().await.unwrap();
    native.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

#[test]
fn shared_named_service_table_corpus_matches_both_sdks() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/services-corpus.json"
    ))
    .unwrap();
    let bundles = bundles();
    for case in cases {
        let contracts: BTreeMap<String, rutis_protocol::runner_image::CatalogService> =
            serde_json::from_value(case["contracts"].clone()).unwrap();
        let checked = serde_json::from_value(case["table"].clone())
            .map_err(|e| {
                rutis_protocol::error::ProtocolError::new(
                    ErrorCode::InvalidParams,
                    "services",
                    e.to_string(),
                )
            })
            .and_then(|table| validate_table(&table, &contracts, &bundles));
        if case["error"].is_null() {
            assert!(checked.is_ok(), "{}: {checked:?}", case["name"]);
        } else {
            assert_eq!(
                serde_json::to_value(checked.unwrap_err().code).unwrap(),
                case["error"],
                "{}",
                case["name"]
            );
        }
    }
}

#[test]
fn ports_bind_exact_raw_contracts_and_reject_duplicate_native_keys() {
    let bundles = bundles();
    let mut service = bundles.client::<InterfaceDatabaseClient>().unwrap();
    service.version = "2.0.0".into();
    assert_eq!(
        bundles.service(&service).err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    service = bundles.client::<InterfaceDatabaseClient>().unwrap();
    service.bundle_sha256 = digest(b"other bytes");
    assert_eq!(
        bundles.service(&service).err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    let mut raw = BUNDLE.to_vec();
    raw.push(b'\n');
    assert_eq!(
        Bundles::admit([BUNDLE.to_vec(), raw]).err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    let mut ports = NativePorts::default();
    ports
        .require::<InterfaceDatabaseClient>(
            "db",
            TypeKey::of::<InterfaceDatabaseClient>(),
            &bundles,
        )
        .unwrap();
    assert_eq!(
        ports
            .require::<InterfaceDatabaseClient>(
                "other",
                TypeKey::of::<InterfaceDatabaseClient>(),
                &bundles
            )
            .unwrap_err()
            .code,
        ErrorCode::InvalidParams
    );
    assert_eq!(
        ports.check(&metadata()).unwrap_err().code,
        ErrorCode::InterfaceMismatch
    );
}

#[tokio::test]
async fn bound_factory_metadata_is_checked_before_native_build() {
    let root = Ctx::root().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let linked = registry(
        native(move |_| {
            captured.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(Effect::Done) })
        }),
        metadata(),
    );
    assert_eq!(
        linked
            .mount_bound(
                &root,
                "service",
                serde_json::json!({}),
                ActivationGate::default(),
                &[TypeKey::of::<u8>()]
            )
            .err()
            .unwrap()
            .code,
        ErrorCode::InterfaceMismatch
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn empty_service_tables_still_require_a_live_native_owner_and_broker_scope() {
    let root = Ctx::root().unwrap();
    let ports = NativePorts::default();
    let owner = owner("empty", 1);
    let gate = ActivationGate::default();
    let bindings = ports.scope(&root, owner.clone(), gate.clone());
    let member = ManagedActivation::mount_gated(
        bindings.ctx(),
        native(|_| Box::pin(async { Ok(Effect::Done) })),
        gate.clone(),
    )
    .unwrap();
    member.view().await.unwrap();
    let exports = Exports::managed(
        &member.native_context().unwrap(),
        owner.clone(),
        ObjectIds::default(),
        gate,
    )
    .unwrap();
    let mut staged = ports.stage(&bindings, bundles(), exports).unwrap();
    let scopes = GraphScopes::in_scope(scope(owner.clone()));
    let mut broker = Broker::default();
    assert_eq!(
        offer_table(
            &mut broker,
            &owner,
            staged.table(),
            staged.contracts(),
            &bundles(),
            &scopes
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::ScopeClosed
    );
    broker.start_activation(owner.clone()).unwrap();
    assert_eq!(
        offer_table(
            &mut broker,
            &owner,
            staged.table(),
            staged.contracts(),
            &bundles(),
            &scopes
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::ScopeClosed
    );
    broker.open_scope(scope(owner), None).unwrap();
    let offered = offer_table(
        &mut broker,
        bindings.owner(),
        staged.table(),
        staged.contracts(),
        &bundles(),
        &scopes,
    )
    .unwrap();
    member.stop().await.unwrap();
    assert_eq!(
        staged.commit(&offered, &scopes).err().unwrap().code,
        ErrorCode::ScopeClosed
    );
    root.shutdown().await.unwrap();
}

#[test]
fn method_registry_resolves_nested_callbacks_by_exact_bundle() {
    use rutis_protocol::{
        contract::{callback_key, TypeExpr},
        identity::InterfaceView,
    };
    let mut raw: serde_json::Value = serde_json::from_slice(RPC).unwrap();
    let callback = raw["interfaces"]["Database"]["methods"]["withCallback"]["params"].clone();
    let nested = serde_json::json!({"kind":"callback","params":callback,"result":{"kind":"value","schema":{"type":"null"}},"ownership":"borrow"});
    raw["id"] = serde_json::json!("lookup.bundle");
    raw["interfaces"]["Database"]["methods"]["nested"] = serde_json::json!({"params":{"kind":"record","fields":{"callbacks":{"kind":"list","item":{"kind":"optional","item":callback}}}},"result":{"kind":"value","schema":{"type":"null"}}});
    raw["interfaces"]["Database"]["methods"]["nestedCallback"] =
        serde_json::json!({"params":nested,"result":{"kind":"value","schema":{"type":"null"}}});
    let bytes = serde_json::to_vec(&raw).unwrap();
    let admitted = AdmittedBundle::parse(&bytes).unwrap();
    let registry = Bundles::admit([bytes]).unwrap();
    let view = |interface: String| InterfaceView {
        interface,
        bundle_sha256: admitted.sha256().into(),
        source: "full-view-is-authorized-by-broker".into(),
    };
    for callback in [callback, nested] {
        let expr: TypeExpr = serde_json::from_value(callback.clone()).unwrap();
        let method = registry.method(&view(callback_key(&expr)), "call").unwrap();
        assert_eq!(
            serde_json::to_value(method).unwrap(),
            serde_json::json!({"params":callback["params"],"result":callback["result"]})
        );
    }
    for name in ["toString", "constructor", "missing"] {
        assert_eq!(
            registry
                .method(&view("Database".into()), name)
                .unwrap_err()
                .code,
            ErrorCode::CapabilityDenied
        );
    }
    assert_eq!(
        registry
            .method(&view("$callback:unprepared".into()), "call")
            .unwrap_err()
            .code,
        ErrorCode::CapabilityDenied
    );
    let mut other = view("Database".into());
    other.bundle_sha256 = "0".repeat(64);
    assert_eq!(
        registry.method(&other, "connect").unwrap_err().code,
        ErrorCode::InterfaceMismatch
    );
}
