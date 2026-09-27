#[allow(dead_code, unused_variables, non_snake_case)]
mod database {
    include!("../../../protocol/generated/database.rs");
}
#[allow(dead_code, unused_variables, non_snake_case, non_camel_case_types)]
mod shapes {
    include!("../../../protocol/generated/binding-types.rs");
}
use database::*;
use rutis_protocol::{
    codegen,
    contract::AdmittedBundle,
    error::{ErrorCode, Result},
    exports::ObjectIds,
    identity::{Activation, Sequence},
    memory::{Endpoint, Network},
    sdk::*,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::Semaphore;
const BUNDLE: &[u8] = include_bytes!("../../../protocol/fixtures/database.bundle.json");
fn activation(name: &str) -> Activation {
    Activation {
        runtime: name.into(),
        epoch: Sequence(1),
        activation: Sequence(1),
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
struct Connection {
    session: Arc<Session>,
    queries: Arc<AtomicUsize>,
    entered: Arc<Semaphore>,
    resume: Arc<Semaphore>,
}
impl InterfaceConnectionService for Connection {
    fn session(&self) -> Result<Arc<dyn InterfaceSessionService>> {
        Ok(self.session.clone())
    }
    fn query(
        &self,
        _: CallContext,
        params: InterfaceConnectionMethod0Params,
    ) -> RpcFuture<Vec<BTreeMap<String, serde_json::Value>>> {
        let queries = self.queries.clone();
        let entered = self.entered.clone();
        let resume = self.resume.clone();
        Box::pin(async move {
            if params.sql == "slow" {
                entered.add_permits(1);
                resume.acquire().await.unwrap().forget();
            }
            let count = queries.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(vec![BTreeMap::from([
                ("count".into(), serde_json::json!(count)),
                ("sql".into(), serde_json::json!(params.sql)),
            ])])
        })
    }
}
struct Database {
    connection: Arc<Connection>,
    connects: Arc<AtomicUsize>,
    callback: Arc<Mutex<Option<BorrowCallback0Client>>>,
    child_started: Arc<Semaphore>,
    child_resume: Arc<Semaphore>,
}
impl InterfaceDatabaseService for Database {
    fn connect(
        &self,
        _: CallContext,
        _: InterfaceDatabaseMethod0Params,
    ) -> RpcFuture<Arc<dyn InterfaceConnectionService>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let value: Arc<dyn InterfaceConnectionService> = self.connection.clone();
        Box::pin(async move { Ok(value) })
    }
    fn inspect(&self, _: CallContext, params: InterfaceConnectionClient) -> RpcFuture<bool> {
        Box::pin(async move {
            let value = params
                .query(InterfaceConnectionMethod0Params {
                    sql: "owner-passback".into(),
                })
                .await?;
            Ok(value[0]["sql"] == "owner-passback")
        })
    }
    fn withCallback(&self, context: CallContext, params: BorrowCallback0Client) -> RpcFuture<()> {
        *self.callback.lock().unwrap() = Some(params.clone());
        let started = self.child_started.clone();
        let resume = self.child_resume.clone();
        Box::pin(async move {
            params.call("direct".into()).await?;
            let descendant = context.clone();
            context.spawn(async move {
                started.add_permits(1);
                resume.acquire().await.unwrap().forget();
                descendant.spawn(async move { params.call("grandchild".into()).await })?;
                Ok(())
            })?;
            Ok(())
        })
    }
}
struct Callback {
    database: InterfaceDatabaseClient,
    calls: Arc<AtomicUsize>,
}
impl BorrowCallback0Service for Callback {
    fn call(&self, _: CallContext, _: String) -> RpcFuture<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let database = self.database.clone();
        Box::pin(async move {
            let connection = database
                .connect(InterfaceDatabaseMethod0Params {
                    name: "reentrant".into(),
                })
                .await?;
            connection
                .query(InterfaceConnectionMethod0Params {
                    sql: "callback".into(),
                })
                .await?;
            Ok(())
        })
    }
}
struct Setup {
    network: Arc<Network>,
    provider: Arc<Endpoint>,
    consumer: Arc<Endpoint>,
    database: Arc<Database>,
    client: InterfaceDatabaseClient,
}
fn setup() -> Setup {
    let network = Network::new(AdmittedBundle::parse(BUNDLE).unwrap());
    let provider = network
        .endpoint(activation("provider"), ObjectIds::default(), None)
        .unwrap();
    let consumer = network
        .endpoint(activation("consumer"), ObjectIds::default(), None)
        .unwrap();
    let session = Arc::new_cyclic(|session| Session {
        agent: Arc::new(Agent {
            session: session.clone(),
        }),
    });
    let connection = Arc::new(Connection {
        session,
        queries: Arc::default(),
        entered: Arc::new(Semaphore::new(0)),
        resume: Arc::new(Semaphore::new(0)),
    });
    let database = Arc::new(Database {
        connection,
        connects: Arc::default(),
        callback: Arc::default(),
        child_started: Arc::new(Semaphore::new(0)),
        child_resume: Arc::new(Semaphore::new(0)),
    });
    let client = consumer
        .import(&provider, exportInterfaceDatabase(database.clone()))
        .unwrap();
    Setup {
        network,
        provider,
        consumer,
        database,
        client,
    }
}
async fn connect(client: &InterfaceDatabaseClient) -> InterfaceConnectionClient {
    client
        .connect(InterfaceDatabaseMethod0Params {
            name: "stateful".into(),
        })
        .await
        .unwrap()
}
async fn finish(setup: Setup) {
    setup.consumer.close();
    setup.provider.close();
    setup.consumer.exports().join().await.unwrap();
    setup.provider.exports().join().await.unwrap();
    assert_eq!(setup.consumer.imports().retained_objects(), 0);
}
#[test]
fn generated_sources_are_deterministic_and_match_checked_in_artifacts() {
    let bindings = codegen::generate(&AdmittedBundle::parse(BUNDLE).unwrap());
    assert_eq!(
        bindings.rust,
        include_str!("../../../protocol/generated/database.rs")
    );
    assert_eq!(
        bindings.typescript,
        include_str!("../../../protocol/ts/generated/database.ts")
    );
    let rpc = codegen::generate(
        &AdmittedBundle::parse(include_bytes!("../../../protocol/fixtures/rpc.bundle.json"))
            .unwrap(),
    );
    assert_eq!(rpc.rust, include_str!("../../../protocol/generated/rpc.rs"));
    assert_eq!(
        rpc.typescript,
        include_str!("../../../protocol/ts/generated/rpc.ts")
    );
    let settings = codegen::generate(
        &AdmittedBundle::parse(include_bytes!(
            "../../../protocol/fixtures/settings.bundle.json"
        ))
        .unwrap(),
    );
    assert_eq!(
        settings.rust,
        include_str!("../../../protocol/generated/settings.rs")
    );
    assert_eq!(
        settings.typescript,
        include_str!("../../../protocol/ts/generated/settings.ts")
    );
}
#[tokio::test]
async fn generated_clients_preserve_state_cycles_and_owner_passback_dispatch() {
    let setup = setup();
    let connection_native = Arc::downgrade(&setup.database.connection);
    let session_native = Arc::downgrade(&setup.database.connection.session);
    let agent_native = Arc::downgrade(&setup.database.connection.session.agent);
    let a = connect(&setup.client).await;
    let b = connect(&setup.client).await;
    assert!(same_wrapper(&a, &b));
    let session = a.session().unwrap();
    assert!(same_wrapper(
        &session,
        &session.agent().unwrap().session().unwrap()
    ));
    assert_eq!(
        a.query(InterfaceConnectionMethod0Params {
            sql: "first".into()
        })
        .await
        .unwrap()[0]["count"],
        1
    );
    assert!(setup.client.inspect(a.clone()).await.unwrap());
    assert_eq!(setup.database.connection.queries.load(Ordering::SeqCst), 2);
    assert_eq!(
        setup.provider.imports().retained_objects(),
        0,
        "owner passback borrows close, including properties"
    );
    let third = setup
        .network
        .endpoint(activation("third"), ObjectIds::default(), None)
        .unwrap();
    assert!(
        matches!(third.import::<InterfaceConnectionClient>(&setup.consumer, Outbound::Foreign(a.client().proxy().clone())), Err(e) if e.code == ErrorCode::UnsupportedCapability)
    );
    assert!(a
        .query(InterfaceConnectionMethod0Params {
            sql: "after rejection".into()
        })
        .await
        .is_ok());
    let error = setup
        .client
        .connect(InterfaceDatabaseMethod0Params { name: "".into() })
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidParams);
    assert_eq!(setup.database.connects.load(Ordering::SeqCst), 2);
    third.close();
    finish(setup).await;
    assert!(connection_native.upgrade().is_none());
    assert!(session_native.upgrade().is_none());
    assert!(
        agent_native.upgrade().is_none(),
        "closed graph proxies must not keep the native graph alive"
    );
}

struct NativeProbe {
    context: Arc<Mutex<Option<rutis::Ctx>>>,
    injects: Vec<rutis::TypeKey>,
}
impl rutis::Plugin for NativeProbe {
    fn name(&self) -> &str {
        "generated-native-provider"
    }
    fn injects(&self) -> &[rutis::TypeKey] {
        &self.injects
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            *self.context.lock().unwrap() = Some(ctx.clone());
            Ok(rutis::Effect::Done)
        })
    }
}
struct NativeDatabase {
    database: Arc<Database>,
    called: Arc<AtomicUsize>,
}
impl InterfaceDatabaseService for NativeDatabase {
    fn connect(
        &self,
        context: CallContext,
        params: InterfaceDatabaseMethod0Params,
    ) -> RpcFuture<Arc<dyn InterfaceConnectionService>> {
        let native = context.native().unwrap();
        assert_eq!(*native.get::<u8>().unwrap(), 7);
        self.called.fetch_add(1, Ordering::SeqCst);
        self.database.connect(context, params)
    }
    fn inspect(&self, context: CallContext, params: InterfaceConnectionClient) -> RpcFuture<bool> {
        self.database.inspect(context, params)
    }
    fn withCallback(&self, context: CallContext, params: BorrowCallback0Client) -> RpcFuture<()> {
        self.database.withCallback(context, params)
    }
}
#[tokio::test]
async fn generated_dispatch_uses_the_original_native_ctx_and_unload_closes_its_exports() {
    let setup = setup();
    let root = rutis::Ctx::root().unwrap();
    let dependency = root.provide(7_u8).unwrap();
    let context = Arc::new(Mutex::new(None));
    let managed = rutis_protocol::managed::ManagedActivation::mount(
        &root,
        NativeProbe {
            context: context.clone(),
            injects: vec![rutis::TypeKey::of::<u8>()],
        },
    )
    .unwrap();
    managed.view().await.unwrap();
    let native = context.lock().unwrap().clone().unwrap();
    let owner = activation("native-provider");
    let exports = rutis_protocol::exports::Exports::managed(
        &native,
        owner.clone(),
        ObjectIds::default(),
        managed.gate(),
    )
    .unwrap();
    let endpoint = setup
        .network
        .endpoint_with_exports(owner, exports, Some(native))
        .unwrap();
    let called = Arc::new(AtomicUsize::new(0));
    let client: InterfaceDatabaseClient = setup
        .consumer
        .import(
            &endpoint,
            exportInterfaceDatabase(Arc::new(NativeDatabase {
                database: setup.database.clone(),
                called: called.clone(),
            })),
        )
        .unwrap();
    let connection = connect(&client).await;
    assert!(connection.session().is_ok());
    assert_eq!(called.load(Ordering::SeqCst), 1);
    dependency.dispose().await.unwrap();
    assert_eq!(
        client
            .connect(InterfaceDatabaseMethod0Params {
                name: "stale".into()
            })
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::ScopeClosed
    );
    assert_eq!(
        connection.session().err().unwrap().code,
        ErrorCode::ScopeClosed
    );
    assert_eq!(endpoint.imports().retained_objects(), 0);
    managed.stop().await.unwrap();
    root.shutdown().await.unwrap();
    finish(setup).await;
}
#[tokio::test]
async fn borrowed_callback_reenters_and_waits_for_registered_descendants() {
    let setup = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let callback: Arc<dyn BorrowCallback0Service> = Arc::new(Callback {
        database: setup.client.clone(),
        calls: calls.clone(),
    });
    let client = setup.client.clone();
    let waiter = tokio::spawn(async move { client.withCallback(callback).await });
    setup
        .database
        .child_started
        .acquire()
        .await
        .unwrap()
        .forget();
    assert!(!waiter.is_finished());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    setup.database.child_resume.add_permits(1);
    waiter.await.unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let saved = setup.database.callback.lock().unwrap().clone().unwrap();
    assert_eq!(
        saved.call("expired".into()).await.unwrap_err().code,
        ErrorCode::ScopeClosed
    );
    finish(setup).await;
}
#[tokio::test]
async fn dropping_a_waiter_and_releasing_aliases_keeps_actual_execution_pinned() {
    let setup = setup();
    let connection = connect(&setup.client).await;
    let identity = connection.client().proxy().delivery().unwrap().object;
    let alias = connection.clone();
    let waiter = tokio::spawn(async move {
        alias
            .query(InterfaceConnectionMethod0Params { sql: "slow".into() })
            .await
    });
    setup
        .database
        .connection
        .entered
        .acquire()
        .await
        .unwrap()
        .forget();
    waiter.abort();
    let _ = waiter.await;
    release(&connection);
    setup.consumer.flush();
    assert_eq!(setup.network.pins(&identity), (0, 1));
    setup.database.connection.resume.add_permits(1);
    setup.consumer.close();
    setup.provider.close();
    setup.provider.exports().join().await.unwrap();
    assert_eq!(setup.network.pins(&identity), (0, 0));
}

#[tokio::test]
async fn closing_the_owner_rejects_a_late_value_only_result_without_aborting_execution() {
    let setup = setup();
    let connection = connect(&setup.client).await;
    let identity = connection.client().proxy().delivery().unwrap().object;
    let waiter = tokio::spawn(async move {
        connection
            .query(InterfaceConnectionMethod0Params { sql: "slow".into() })
            .await
    });
    setup
        .database
        .connection
        .entered
        .acquire()
        .await
        .unwrap()
        .forget();
    setup.provider.close();
    assert_eq!(setup.network.pins(&identity), (0, 1));
    setup.database.connection.resume.add_permits(1);
    let error = waiter.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::ScopeClosed);
    assert_eq!(error.execution, rutis_protocol::error::Execution::Unknown);
    assert_eq!(setup.database.connection.queries.load(Ordering::SeqCst), 1);
    setup.provider.exports().join().await.unwrap();
    assert_eq!(setup.network.pins(&identity), (0, 0));
    setup.consumer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_generated_calls_allocate_and_open_scopes_in_broker_order() {
    let setup = setup();
    for _ in 0..8 {
        let barrier = Arc::new(tokio::sync::Barrier::new(17));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let client = setup.client.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let connection = connect(&client).await;
                connection
                    .query(InterfaceConnectionMethod0Params {
                        sql: "concurrent".into(),
                    })
                    .await
                    .unwrap()
            }));
        }
        barrier.wait().await;
        for task in tasks {
            task.await.unwrap();
        }
    }
    assert_eq!(
        setup.database.connection.queries.load(Ordering::SeqCst),
        128
    );
    finish(setup).await;
}

#[test]
fn generated_shapes_preserve_absence_null_discriminants_and_raw_descriptor_hashes() {
    let bytes = include_bytes!("../../../protocol/fixtures/binding-types.bundle.json");
    let bindings = codegen::generate(&AdmittedBundle::parse(bytes).unwrap());
    assert_eq!(
        bindings.rust,
        include_str!("../../../protocol/generated/binding-types.rs")
    );
    assert_eq!(
        bindings.typescript,
        include_str!("../../../protocol/ts/generated/binding-types.ts")
    );
    let value = serde_json::json!({"nullable": null, "choice": {"tag": "first", "text": "hi"}, "labels": ["red"]});
    let dto: shapes::InterfaceValuesMethod1Params = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(dto.nullable, OptionalField::Present(())));
    assert_eq!(serde_json::to_value(dto).unwrap(), value);
    let missing = serde_json::json!({"choice": {"tag": "second", "count": 7}, "labels": ["green"]});
    let dto: shapes::InterfaceValuesMethod1Params =
        serde_json::from_value(missing.clone()).unwrap();
    assert!(matches!(dto.nullable, OptionalField::Missing));
    assert_eq!(serde_json::to_value(dto).unwrap(), missing);
}
