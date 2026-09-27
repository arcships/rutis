//! Shared runtime actors and authoritative Host routing over private Unix
//! streams, including an actual Node child. This is not the frozen-plan loader.
#![cfg(target_os = "linux")]
#[path = "support/session_plan.rs"]
mod session_plan;
rutis_protocol::embed_runner_catalog!(include_bytes!(
    "../../../protocol/fixtures/session.runner.catalog.json"
));
#[allow(dead_code, unused_variables, non_snake_case)]
mod rpc {
    include!("../../../protocol/generated/rpc.rs");
}
#[allow(dead_code, unused_variables, non_snake_case)]
mod data {
    include!("../../../protocol/generated/database.rs");
}
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};
use rutis_protocol::{
    deployment::DeploymentObjects,
    error::{ErrorCode, Execution, ProtocolError, Result},
    exports::Exports,
    frame::{Handler, Peer},
    identity::{Activation, Sequence},
    managed::{ActivationGate, ManagedActivation},
    sdk::*,
    services::{Bundles, NativePorts, ServiceTable},
    session::{HostObjects, RuntimeIdentity, RuntimeObjects},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    os::fd::AsRawFd,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
};
fn owner(runtime: &str, activation: u64) -> Activation {
    Activation {
        runtime: runtime.into(),
        epoch: Sequence(1),
        activation: Sequence(activation),
    }
}
fn identity(runtime: &str) -> RuntimeIdentity {
    RuntimeIdentity {
        runtime: runtime.into(),
        epoch: Sequence(1),
    }
}
fn bundles() -> Bundles {
    Bundles::admit([
        include_bytes!("../../../protocol/fixtures/rpc.bundle.json").to_vec(),
        include_bytes!("../../../protocol/fixtures/database.bundle.json").to_vec(),
    ])
    .unwrap()
}
fn reject() -> Handler {
    Arc::new(|_, _| {
        Box::pin(async {
            Err(ProtocolError::new(
                ErrorCode::UnsupportedCapability,
                "fixture",
                "unknown fixture request",
            ))
        })
    })
}
#[derive(Default)]
struct Fence {
    enabled: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
fn controlled(host: &Arc<HostObjects>, identity: RuntimeIdentity, fence: Arc<Fence>) -> Handler {
    let handler = host.handler(identity, reject());
    Arc::new(move |method, value| {
        let held = method == "object/controls"
            && value
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["type"] == "release"))
            && fence.enabled.swap(false, Ordering::SeqCst);
        let future = handler(method, value);
        let fence = fence.clone();
        Box::pin(async move {
            if held {
                fence.entered.notify_one();
                fence.resume.notified().await;
            }
            future.await
        })
    })
}
struct Native {
    injects: Vec<TypeKey>,
    run: Arc<
        dyn Fn(Ctx) -> BoxFuture<'static, std::result::Result<Effect, CordisError>> + Send + Sync,
    >,
}
impl Plugin for Native {
    fn name(&self) -> &str {
        "session-native"
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
struct Session {
    agent: Arc<Agent>,
}
struct Agent {
    session: Weak<Session>,
}
struct Connection {
    session: Arc<Session>,
    creator: Ctx,
    count: AtomicUsize,
    child_entered: Arc<AtomicBool>,
    child_resume: Arc<tokio::sync::Notify>,
}
struct Database(Arc<Connection>);
struct Callback<T> {
    creator: Ctx,
    connection: T,
    count: Arc<AtomicUsize>,
}
macro_rules! implement {
    ($api:ident, $exercise:ident) => {
        impl $api::InterfaceSessionService for Session {
            fn agent(&self) -> Result<Arc<dyn $api::InterfaceAgentService>> {
                Ok(self.agent.clone())
            }
        }
        impl $api::InterfaceAgentService for Agent {
            fn session(&self) -> Result<Arc<dyn $api::InterfaceSessionService>> {
                Ok(self.session.upgrade().unwrap())
            }
        }
        impl $api::InterfaceConnectionService for Connection {
            fn session(&self) -> Result<Arc<dyn $api::InterfaceSessionService>> {
                Ok(self.session.clone())
            }
            fn query(
                &self,
                context: CallContext,
                params: $api::InterfaceConnectionMethod0Params,
            ) -> RpcFuture<Vec<BTreeMap<String, Value>>> {
                assert!(Arc::ptr_eq(
                    &context
                        .native()
                        .unwrap()
                        .get_as::<dyn $api::InterfaceDatabaseService>(TypeKey::of::<
                            dyn $api::InterfaceDatabaseService,
                        >())
                        .unwrap(),
                    &self
                        .creator
                        .get_as::<dyn $api::InterfaceDatabaseService>(TypeKey::of::<
                            dyn $api::InterfaceDatabaseService,
                        >())
                        .unwrap()
                ));
                if params.sql == "panic-child" {
                    let resume = self.child_resume.clone();
                    let entered = self.child_entered.clone();
                    context
                        .spawn(async move {
                            entered.store(true, Ordering::SeqCst);
                            resume.notified().await;
                            Ok(())
                        })
                        .unwrap();
                    panic!("native handler panic before registered child completion");
                }
                let count = self.count.fetch_add(1, Ordering::SeqCst) + 1;
                Box::pin(async move {
                    Ok(vec![BTreeMap::from([
                        ("owner".into(), json!("rust")),
                        ("sql".into(), json!(params.sql)),
                        ("count".into(), json!(count)),
                    ])])
                })
            }
        }
        impl $api::InterfaceDatabaseService for Database {
            fn connect(
                &self,
                _: CallContext,
                _: $api::InterfaceDatabaseMethod0Params,
            ) -> RpcFuture<Arc<dyn $api::InterfaceConnectionService>> {
                let connection: Arc<dyn $api::InterfaceConnectionService> = self.0.clone();
                Box::pin(async move { Ok(connection) })
            }
            fn inspect(
                &self,
                _: CallContext,
                params: $api::InterfaceConnectionClient,
            ) -> RpcFuture<bool> {
                Box::pin(async move {
                    Ok(params
                        .query($api::InterfaceConnectionMethod0Params {
                            sql: "passback".into(),
                        })
                        .await?[0]["owner"]
                        == "rust")
                })
            }
            fn withCallback(
                &self,
                context: CallContext,
                callback: $api::BorrowCallback0Client,
            ) -> RpcFuture<()> {
                Box::pin(async move {
                    callback.call("root".into()).await?;
                    context.spawn(async move { callback.call("child".into()).await })?;
                    Ok(())
                })
            }
        }
        impl $api::BorrowCallback0Service for Callback<$api::InterfaceConnectionClient> {
            fn call(&self, context: CallContext, text: String) -> RpcFuture<()> {
                assert!(Arc::ptr_eq(
                    &context
                        .native()
                        .unwrap()
                        .require::<$api::InterfaceDatabaseClient>()
                        .unwrap(),
                    &self
                        .creator
                        .require::<$api::InterfaceDatabaseClient>()
                        .unwrap()
                ));
                self.count.fetch_add(1, Ordering::SeqCst);
                let connection = self.connection.clone();
                Box::pin(async move {
                    connection
                        .query($api::InterfaceConnectionMethod0Params { sql: text })
                        .await?;
                    Ok(())
                })
            }
        }
        async fn $exercise(ctx: &Ctx) -> Value {
            let client = ctx.require::<$api::InterfaceDatabaseClient>().unwrap();
            let first = client
                .connect($api::InterfaceDatabaseMethod0Params {
                    name: "native-rust-consumer".into(),
                })
                .await
                .unwrap();
            let second = client
                .connect($api::InterfaceDatabaseMethod0Params {
                    name: "same".into(),
                })
                .await
                .unwrap();
            assert!(same_wrapper(&first, &second));
            let session = first.session().unwrap();
            assert!(same_wrapper(
                &session,
                &session.agent().unwrap().session().unwrap()
            ));
            assert!(client.inspect(first.clone()).await.unwrap());
            let count = Arc::new(AtomicUsize::new(0));
            client
                .withCallback(Arc::new(Callback {
                    creator: ctx.clone(),
                    connection: first.clone(),
                    count: count.clone(),
                }))
                .await
                .unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 2);
            json!(first
                .query($api::InterfaceConnectionMethod0Params {
                    sql: "native-rust".into()
                })
                .await
                .unwrap())
        }
    };
}
implement!(rpc, exercise_rpc);
implement!(data, exercise_data);
async fn eventually(mut condition: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !condition().await {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("observable actor barrier timed out");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn named_native_services_route_between_two_rust_and_two_cordis_members() {
    tokio::time::timeout(std::time::Duration::from_secs(30), scenario())
        .await
        .expect("shared session conformance timed out");
}
async fn scenario() {
    let bundles = bundles();
    let deployment = DeploymentObjects::new(session_plan::prepare()).unwrap();
    let host = deployment.host();
    let runtime = RuntimeObjects::new(identity("rust"), bundles.clone());
    for runtime in ["rust", "node"] {
        for index in 1..=2 {
            deployment
                .reserve(
                    &format!(
                        "{runtime}-{}",
                        if index == 1 { "provider" } else { "consumer" }
                    ),
                    owner(runtime, index),
                )
                .unwrap();
        }
    }
    let (host_stream, runtime_stream) = tokio::net::UnixStream::pair().unwrap();
    let rust_fence = Arc::new(Fence::default());
    let rust_peer = Peer::start(
        host_stream,
        controlled(&host, identity("rust"), rust_fence.clone()),
    );
    host.attach(identity("rust"), &rust_peer).unwrap();
    let rust_runtime_peer = Peer::start(runtime_stream, runtime.handler(reject()));
    runtime.attach(&rust_runtime_peer).unwrap();
    let (parent, child_socket) = tokio::net::UnixStream::pair().unwrap();
    let child_socket = child_socket.into_std().unwrap();
    let fd = child_socket.as_raw_fd();
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = tokio::process::Command::new("node");
    command
        .current_dir(workspace)
        .args([
            "--import",
            "./protocol/ts/node_modules/tsx/dist/loader.mjs",
            "protocol/ts/tests/fixtures/session-peer.ts",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(child_socket);
    // Drain diagnostics concurrently so a failing child cannot block on pipes.
    let mut stderr = child.stderr.take().unwrap();
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut output = String::new();
        stderr.read_to_string(&mut output).await.unwrap();
        if !output.is_empty() {
            eprintln!("Node session diagnostics: {output}");
        }
        output
    });
    let node_fence = Arc::new(Fence::default());
    let node_peer = Peer::start(
        parent,
        controlled(&host, identity("node"), node_fence.clone()),
    );
    host.attach(identity("node"), &node_peer).unwrap();
    let started = match node_peer.request("fixture/start", Value::Null).await {
        Ok(value) => value,
        Err(error) => {
            let _ = child.kill().await;
            panic!(
                "Node native start failed: {error}: {}",
                stderr_task.await.unwrap()
            );
        }
    };
    let node_table: ServiceTable = serde_json::from_value(started["table"].clone()).unwrap();
    let contracts = serde_json::from_value(started["contracts"].clone()).unwrap();
    deployment
        .stage("node-provider", node_table.clone())
        .unwrap();
    assert_eq!(
        deployment
            .offer_required("rust-consumer")
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::Unavailable
    );
    deployment.publish("node-provider").unwrap();
    node_peer
        .request("fixture/publish", json!({"activation":"1"}))
        .await
        .unwrap();

    let native_root = Ctx::root().unwrap();
    let cleanup = Arc::new(AtomicUsize::new(0));
    let rust_provider = owner("rust", 1);
    let provider_gate = ActivationGate::default();
    runtime
        .reserve(rust_provider.clone(), provider_gate.clone())
        .unwrap();
    let mut ports = NativePorts::default();
    ports
        .provide::<dyn rpc::InterfaceDatabaseService, rpc::InterfaceDatabaseClient>(
            "rpc",
            TypeKey::of::<dyn rpc::InterfaceDatabaseService>(),
            &bundles,
            rpc::exportInterfaceDatabase,
        )
        .unwrap();
    ports
        .provide::<dyn data::InterfaceDatabaseService, data::InterfaceDatabaseClient>(
            "data",
            TypeKey::of::<dyn data::InterfaceDatabaseService>(),
            &bundles,
            data::exportInterfaceDatabase,
        )
        .unwrap();
    let provider_bindings = ports.scope(&native_root, rust_provider.clone(), provider_gate.clone());
    let child_entered = Arc::new(AtomicBool::new(false));
    let child_resume = Arc::new(tokio::sync::Notify::new());
    let captured_connection = Arc::new(Mutex::new(None));
    let connection_capture = captured_connection.clone();
    let entered = child_entered.clone();
    let resume = child_resume.clone();
    let removal = Arc::new(Mutex::new(None));
    let remove_rpc = removal.clone();
    let captured_exports = Arc::new(Mutex::new(None));
    let captured = captured_exports.clone();
    let actor = runtime.clone();
    let provider_owner = rust_provider.clone();
    let gate = provider_gate.clone();
    let disposed = cleanup.clone();
    let provider = ManagedActivation::mount_gated(
        provider_bindings.ctx(),
        Native {
            injects: vec![],
            run: Arc::new(move |ctx| {
                let actor = actor.clone();
                let owner = provider_owner.clone();
                let gate = gate.clone();
                let disposed = disposed.clone();
                let captured = captured.clone();
                let remove_rpc = remove_rpc.clone();
                let connection_capture = connection_capture.clone();
                let entered = entered.clone();
                let resume = resume.clone();
                Box::pin(async move {
                    let exports = Exports::managed(&ctx, owner.clone(), actor.ids(), gate)?;
                    actor.bind(&owner, ctx.clone(), exports.clone()).unwrap();
                    *captured.lock().unwrap() = Some(exports);
                    ctx.effect(move || {
                        Effect::Disposer(Box::new(move || {
                            disposed.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }))
                    })?;
                    let database = Arc::new(Database(Arc::new(Connection {
                        session: Arc::new_cyclic(|session| Session {
                            agent: Arc::new(Agent {
                                session: session.clone(),
                            }),
                        }),
                        creator: ctx.clone(),
                        count: AtomicUsize::new(0),
                        child_entered: entered,
                        child_resume: resume,
                    })));
                    *connection_capture.lock().unwrap() = Some(database.0.clone());
                    let provided = ctx.provide_as::<dyn rpc::InterfaceDatabaseService>(
                        TypeKey::of::<dyn rpc::InterfaceDatabaseService>(),
                        database.clone(),
                    )?;
                    *remove_rpc.lock().unwrap() = Some(provided);
                    ctx.provide_as::<dyn data::InterfaceDatabaseService>(
                        TypeKey::of::<dyn data::InterfaceDatabaseService>(),
                        database,
                    )?;
                    Ok(Effect::Done)
                })
            }),
        },
        provider_gate,
    )
    .unwrap();
    provider.view().await.unwrap();
    let exports = captured_exports.lock().unwrap().take().unwrap();
    let guards = ports.guard_exports(&provider_bindings, &provider).unwrap();
    for guard in &guards {
        guard.await.unwrap();
    }
    let provider_exports = exports.clone();
    let staged = ports
        .stage(&provider_bindings, bundles.clone(), exports)
        .unwrap();
    let rust_table = runtime.stage_services(staged).unwrap();
    assert_eq!(
        deployment
            .stage_native(rust_table.clone())
            .unwrap_err()
            .code,
        ErrorCode::CapabilityDenied
    );
    deployment
        .stage_native(ServiceTable {
            activation: rust_provider.clone(),
            services: BTreeMap::from([("data".into(), rust_table.services["data"].clone())]),
        })
        .unwrap();
    deployment.stage("rust-provider", rust_table).unwrap();
    deployment.publish("rust-provider").unwrap();
    runtime.publish(&rust_provider).unwrap();

    node_peer
        .request("fixture/reserve", Value::Null)
        .await
        .unwrap();
    let node_roots = deployment.offer_required("node-consumer").await.unwrap();
    let node_results = node_roots.send("fixture/consume").await.unwrap();
    assert_eq!(node_results["distinct"], true);
    for result in node_results["results"].as_array().unwrap() {
        assert_eq!(result["rows"][0]["owner"], "rust");
        for field in ["identity", "cycle", "passback"] {
            assert_eq!(result[field], true);
        }
        assert_eq!(result["callbacks"], 2);
    }
    host.publish(&owner("node", 2)).unwrap();
    node_peer
        .request("fixture/publish", json!({"activation":"2"}))
        .await
        .unwrap();

    let consumer_owner = owner("rust", 2);
    let consumer_gate = ActivationGate::default();
    runtime
        .reserve(consumer_owner.clone(), consumer_gate.clone())
        .unwrap();
    let values = deployment
        .offer_required("rust-consumer")
        .await
        .unwrap()
        .receive(&runtime)
        .await
        .unwrap();
    for (name, value) in &values {
        let proof = value.clone().into_object().unwrap().delivery().unwrap();
        assert_eq!(
            proof.view.source,
            deployment.plan().instances()["rust-consumer"].routes()[name].source()
        );
    }
    let mut requires = NativePorts::default();
    requires
        .require::<rpc::InterfaceDatabaseClient>(
            "rpc",
            TypeKey::of::<rpc::InterfaceDatabaseClient>(),
            &bundles,
        )
        .unwrap();
    requires
        .require::<data::InterfaceDatabaseClient>(
            "data",
            TypeKey::of::<data::InterfaceDatabaseClient>(),
            &bundles,
        )
        .unwrap();
    let mut consumer_bindings =
        requires.scope(&native_root, consumer_owner.clone(), consumer_gate.clone());
    requires
        .install(
            &mut consumer_bindings,
            values,
            runtime.caller(&consumer_owner),
        )
        .unwrap();
    let actor = runtime.clone();
    let captured_owner = consumer_owner.clone();
    let gate = consumer_gate.clone();
    let disposed = cleanup.clone();
    let consumer = ManagedActivation::mount_gated(
        consumer_bindings.ctx(),
        Native {
            injects: requires.required_keys(),
            run: Arc::new(move |ctx| {
                let actor = actor.clone();
                let owner = captured_owner.clone();
                let gate = gate.clone();
                let disposed = disposed.clone();
                Box::pin(async move {
                    let exports = Exports::managed(&ctx, owner.clone(), actor.ids(), gate)?;
                    actor.bind(&owner, ctx.clone(), exports).unwrap();
                    ctx.effect(move || {
                        Effect::Disposer(Box::new(move || {
                            disposed.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }))
                    })?;
                    assert_eq!(exercise_rpc(&ctx).await[0]["owner"], "node");
                    assert_eq!(exercise_data(&ctx).await[0]["owner"], "node");
                    Ok(Effect::Done)
                })
            }),
        },
        consumer_gate,
    )
    .unwrap();
    consumer_bindings.adopt(&consumer).unwrap();
    consumer.view().await.unwrap();
    assert!(native_root.get::<rpc::InterfaceDatabaseClient>().is_none());
    host.publish(&consumer_owner).unwrap();
    runtime.publish(&consumer_owner).unwrap();
    let original = consumer.native_context().unwrap();
    assert_eq!(
        deployment
            .offer_required("rust-consumer")
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::ScopeClosed
    );
    assert!(consumer.gate().is_open());
    let db = original.require::<rpc::InterfaceDatabaseClient>().unwrap();
    let connection = db
        .connect(rpc::InterfaceDatabaseMethod0Params {
            name: "cancellation".into(),
        })
        .await
        .unwrap();
    let object = connection.client().proxy().identity();
    assert_eq!(host.pins(&object).1, 0);
    // An ordinary native child owns returned objects, not the required root.
    // Its stop must await both the remote Release ACK and an actual execution.
    let optional = db
        .connect(rpc::InterfaceDatabaseMethod0Params {
            name: "optional-child".into(),
        })
        .await
        .unwrap();
    let optional_object = optional.client().proxy().identity();
    let cached_session = optional.session().unwrap();
    let cached_agent = cached_session.agent().unwrap();
    let before = host.pins(&optional_object);
    assert_eq!(
        host.handle(
            &identity("rust"),
            "object/closed",
            json!({"owner": owner("node", 1), "objects": [&optional_object]})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::CapabilityDenied
    );
    assert_eq!(host.pins(&optional_object), before);
    assert_eq!(
        optional
            .query(rpc::InterfaceConnectionMethod0Params {
                sql: "original-child".into()
            })
            .await
            .unwrap()[0]["owner"],
        "optional-node-child"
    );
    let called = optional.clone();
    let execution = tokio::spawn(async move {
        called
            .query(rpc::InterfaceConnectionMethod0Params {
                sql: "optional-slow".into(),
            })
            .await
    });
    eventually(async || {
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["optionalEntered"]
            == true
    })
    .await;
    rust_fence.enabled.store(true, Ordering::SeqCst);
    let peer = node_peer.clone();
    let child_stop = tokio::spawn(async move {
        peer.request("fixture/stop-optional", json!({"activation":"1"}))
            .await
    });
    rust_fence.entered.notified().await;
    assert!(!child_stop.is_finished());
    assert_eq!(host.pins(&optional_object), (0, 1));
    assert!(optional.client().proxy().delivery().is_err());
    assert!(cached_session.agent().is_err());
    assert!(cached_agent.session().is_err());
    assert!(consumer.gate().is_open());
    rust_fence.resume.notify_one();
    assert_eq!(
        connection
            .query(rpc::InterfaceConnectionMethod0Params {
                sql: "root-still-active".into()
            })
            .await
            .unwrap()[0]["owner"],
        "node"
    );
    assert!(!child_stop.is_finished());
    node_peer
        .request("fixture/resume-optional", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        execution.await.unwrap().unwrap_err().code,
        ErrorCode::ScopeClosed
    );
    eventually(async || {
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["optionalCleaning"]
            == true
    })
    .await;
    assert!(!child_stop.is_finished());
    assert_eq!(host.pins(&optional_object), (0, 0));
    assert_eq!(
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["optionalCleanups"],
        1
    );
    node_peer
        .request("fixture/resume-optional-cleanup", Value::Null)
        .await
        .unwrap();
    child_stop.await.unwrap().unwrap();
    assert_eq!(host.pins(&optional_object), (0, 0));
    assert!(consumer.gate().is_open());
    assert_eq!(
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["providerOpen"],
        true
    );
    assert!(db
        .connect(rpc::InterfaceDatabaseMethod0Params {
            name: "optional-child".into()
        })
        .await
        .is_err());
    assert_eq!(
        node_peer
            .request("fixture/expired", Value::Null)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ScopeClosed
    );

    // The real handler throws, but its registered child still owns execution.
    let c = connection.clone();
    let failure = tokio::spawn(async move {
        c.query(rpc::InterfaceConnectionMethod0Params {
            sql: "fail-child".into(),
        })
        .await
    });
    eventually(async || {
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["childEntered"]
            == true
    })
    .await;
    assert!(!failure.is_finished());
    assert_eq!(host.pins(&object).1, 1);
    node_peer
        .request("fixture/resume", Value::Null)
        .await
        .unwrap();
    let failed = failure.await.unwrap().unwrap_err();
    assert_eq!(failed.code, ErrorCode::Business);
    assert_eq!(failed.execution, Execution::Unknown);
    assert_eq!(host.pins(&object).1, 0);

    let c = connection.clone();
    let abandoned = tokio::spawn(async move {
        c.query(rpc::InterfaceConnectionMethod0Params { sql: "slow".into() })
            .await
    });
    eventually(async || {
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["slowEntered"]
            == true
    })
    .await;
    abandoned.abort();
    let _ = abandoned.await;
    assert_eq!(host.pins(&object).1, 1);
    node_peer
        .request("fixture/resume", Value::Null)
        .await
        .unwrap();
    eventually(async || host.pins(&object).1 == 0).await;

    // An abandoned object-valued result must be received as a rejected
    // envelope, so its new graph pins and retirement prefix do not leak.
    let before = host.pins(&object).0;
    let client = db.clone();
    let abandoned = tokio::spawn(async move {
        client
            .connect(rpc::InterfaceDatabaseMethod0Params {
                name: "slow-connect".into(),
            })
            .await
    });
    eventually(async || {
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["connectEntered"]
            == true
    })
    .await;
    abandoned.abort();
    let _ = abandoned.await;
    node_peer
        .request("fixture/resume", Value::Null)
        .await
        .unwrap();
    eventually(async || {
        host.pins(&object).0 == before && host.pins(&db.client().proxy().identity()).1 == 0
    })
    .await;

    // Rust panics are caught inside the SDK execution task. The outer barrier
    // remains alive and does not ACK finished while the registered child runs.
    let held = captured_connection.lock().unwrap().take().unwrap();
    let native: Arc<dyn rpc::InterfaceConnectionService> = held.clone();
    let rust_object = provider_exports.register_trait(&native).unwrap();
    let peer = node_peer.clone();
    let panicked = tokio::spawn(async move {
        peer.request("fixture/query", json!({"sql":"panic-child"}))
            .await
    });
    eventually(async || child_entered.load(Ordering::SeqCst)).await;
    assert!(!panicked.is_finished());
    assert_eq!(host.pins(&rust_object).1, 1);
    child_resume.notify_one();
    let error = panicked.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Business);
    assert_eq!(error.execution, Execution::Unknown);
    assert_eq!(host.pins(&rust_object).1, 0);
    drop(held);

    // Fresh service roots retain an owned receipt until native imports accept
    // all of them. Dropping it retires both bundles without starting a plugin.
    host.reserve(owner("node", 3)).unwrap();
    host.reserve(owner("rust", 3)).unwrap();
    let spare_gate = ActivationGate::default();
    runtime
        .reserve(owner("rust", 3), spare_gate.clone())
        .unwrap();
    let spare = node_peer
        .request("fixture/start", json!({"activation":"3"}))
        .await
        .unwrap();
    let spare_table: ServiceTable = serde_json::from_value(spare["table"].clone()).unwrap();
    let roots = host
        .offer_services(
            &owner("node", 3),
            &spare_table,
            &contracts,
            &owner("rust", 3),
        )
        .await
        .unwrap();
    assert_eq!(
        node_peer
            .request("fixture/pins", json!({"activation":"3"}))
            .await
            .unwrap(),
        2
    );
    drop(roots);
    eventually(async || {
        !spare_gate.is_open()
            && node_peer
                .request("fixture/pins", json!({"activation":"3"}))
                .await
                .unwrap()
                == 0
    })
    .await;

    // A second native consumer captures a distinct frozen source view of the
    // same RPC owner. Its other bundle comes from a different actual provider.
    assert_eq!(
        deployment
            .reserve("rust-extra", owner("node", 4))
            .unwrap_err()
            .code,
        ErrorCode::CapabilityDenied
    );
    deployment.reserve("rust-extra", owner("rust", 4)).unwrap();
    deployment.reserve("node-extra", owner("node", 4)).unwrap();
    let extra_owner = owner("rust", 4);
    let extra_gate = ActivationGate::default();
    runtime
        .reserve(extra_owner.clone(), extra_gate.clone())
        .unwrap();
    let values = deployment
        .offer_required("rust-extra")
        .await
        .unwrap()
        .receive(&runtime)
        .await
        .unwrap();
    for (name, value) in &values {
        let proof = value.clone().into_object().unwrap().delivery().unwrap();
        assert_eq!(
            proof.view.source,
            deployment.plan().instances()["rust-extra"].routes()[name].source()
        );
        if name == "rpc" {
            assert_eq!(proof.object, db.client().proxy().identity());
            assert_ne!(proof.view, db.client().proxy().delivery().unwrap().view);
        }
    }
    let mut extra_bindings = requires.scope(&native_root, extra_owner.clone(), extra_gate.clone());
    requires
        .install(&mut extra_bindings, values, runtime.caller(&extra_owner))
        .unwrap();
    let actor = runtime.clone();
    let actual_owner = extra_owner.clone();
    let gate = extra_gate.clone();
    let disposed = cleanup.clone();
    let extra_consumer = ManagedActivation::mount_gated(
        extra_bindings.ctx(),
        Native {
            injects: requires.required_keys(),
            run: Arc::new(move |ctx| {
                let actor = actor.clone();
                let owner = actual_owner.clone();
                let gate = gate.clone();
                let disposed = disposed.clone();
                Box::pin(async move {
                    let exports = Exports::managed(&ctx, owner.clone(), actor.ids(), gate)?;
                    actor.bind(&owner, ctx.clone(), exports).unwrap();
                    ctx.effect(move || {
                        Effect::Disposer(Box::new(move || {
                            disposed.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }))
                    })?;
                    assert_eq!(exercise_rpc(&ctx).await[0]["owner"], "node");
                    assert_eq!(exercise_data(&ctx).await[0]["owner"], "rust");
                    Ok(Effect::Done)
                })
            }),
        },
        extra_gate,
    )
    .unwrap();
    extra_bindings.adopt(&extra_consumer).unwrap();
    extra_consumer.view().await.unwrap();
    let extra_original = extra_consumer.native_context().unwrap();
    host.publish(&extra_owner).unwrap();
    runtime.publish(&extra_owner).unwrap();
    node_peer
        .request("fixture/reserve", json!({"activation":"4"}))
        .await
        .unwrap();
    let extra_results = deployment
        .offer_required("node-extra")
        .await
        .unwrap()
        .send("fixture/consume")
        .await
        .unwrap();
    let rows = extra_results["results"].as_array().unwrap();
    assert_eq!(rows[rows.len() - 2]["rows"][0]["owner"], "rust");
    assert_eq!(rows[rows.len() - 1]["rows"][0]["owner"], "node");
    host.publish(&owner("node", 4)).unwrap();
    node_peer
        .request("fixture/publish", json!({"activation":"4"}))
        .await
        .unwrap();

    // The SDK owner creates actual staging pins, then the fixture corrupts the
    // last returned source. The Host rejects the entire frozen root manifest
    // before issuing any grant; all fresh stage pins return to baseline.
    deployment.reserve("rust-failed", owner("rust", 5)).unwrap();
    let failed_gate = ActivationGate::default();
    runtime
        .reserve(owner("rust", 5), failed_gate.clone())
        .unwrap();
    let before = node_peer
        .request("fixture/pins", json!({"activation":"1"}))
        .await
        .unwrap();
    node_peer
        .request("fixture/tamper-route", Value::Null)
        .await
        .unwrap();
    let failure = deployment.offer_required("rust-failed").await;
    assert_eq!(failure.err().unwrap().code, ErrorCode::CapabilityDenied);
    eventually(async || !failed_gate.is_open()).await;
    assert_eq!(
        node_peer
            .request("fixture/pins", json!({"activation":"1"}))
            .await
            .unwrap(),
        before
    );
    assert!(consumer.gate().is_open());
    assert!(extra_consumer.gate().is_open());
    assert_eq!(
        connection
            .query(rpc::InterfaceConnectionMethod0Params {
                sql: "after-route-rollback".into()
            })
            .await
            .unwrap()[0]["owner"],
        "node"
    );
    assert_eq!(
        deployment
            .reserve("rust-failed", owner("rust", 6))
            .unwrap_err()
            .code,
        ErrorCode::ScopeClosed
    );

    // The final native commit fails after the first root has acquired its
    // actual delivery pin. Reject every issued envelope, remove the earlier
    // pin and abort the uncommitted stage; existing bindings remain live.
    deployment
        .reserve("rust-commit-failed", owner("rust", 6))
        .unwrap();
    let commit_failed = ActivationGate::default();
    runtime
        .reserve(owner("rust", 6), commit_failed.clone())
        .unwrap();
    node_peer
        .request("fixture/fail-route-commit", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        deployment
            .offer_required("rust-commit-failed")
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::CapabilityDenied
    );
    eventually(async || !commit_failed.is_open()).await;
    assert_eq!(
        node_peer
            .request("fixture/pins", json!({"activation":"1"}))
            .await
            .unwrap(),
        before
    );
    assert!(consumer.gate().is_open());
    assert!(extra_consumer.gate().is_open());

    // A valid token cannot be substituted into another consumer's full view,
    // even when owner, interface and exact bundle SHA are all identical.
    let mut forged_view = db.client().proxy().delivery().unwrap();
    forged_view.view.source = deployment.plan().instances()["rust-extra"].routes()["rpc"]
        .source()
        .into();
    let source = runtime
        .encode(
            &consumer_owner,
            &forged_view.view.bundle_sha256,
            &bundles.method(&forged_view.view, "connect").unwrap().params,
            Outbound::json(rpc::InterfaceDatabaseMethod0Params {
                name: "source-substitution".into(),
            })
            .unwrap(),
        )
        .unwrap();
    let stage = source.stage;
    assert_eq!(
        host.handle(
            &identity("rust"),
            "object/call",
            json!({"target":forged_view,"method":"connect","input":source})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::StaleObject
    );
    runtime
        .handle(
            "object/abort",
            json!({"activation":consumer_owner,"stage":stage}),
        )
        .await
        .unwrap();

    // Connection-bound sender checks reject a fabricated native identity before
    // allocating an execution pin, without closing healthy sibling members.
    let proof = connection.client().proxy().delivery().unwrap();
    let offered = runtime
        .encode(
            &consumer_owner,
            &proof.view.bundle_sha256,
            &bundles.method(&proof.view, "query").unwrap().params,
            Outbound::json(rpc::InterfaceConnectionMethod0Params {
                sql: "forged".into(),
            })
            .unwrap(),
        )
        .unwrap();
    let forged = host
        .handle(
            &identity("node"),
            "object/call",
            json!({"target":proof,"method":"query","input":offered}),
        )
        .await
        .unwrap_err();
    assert_eq!(forged.code, ErrorCode::CapabilityDenied);
    assert_eq!(host.pins(&object).1, 0);

    // Empty control queues still wait for the previous ACK. Both peers use
    // actual broker grants; the wrapper only delays real control processing.
    node_fence.enabled.store(true, Ordering::SeqCst);
    let peer = node_peer.clone();
    let fenced = tokio::spawn(async move { peer.request("fixture/fence", Value::Null).await });
    node_fence.entered.notified().await;
    assert_eq!(
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["fenceSecondFinished"],
        false
    );
    node_fence.resume.notify_one();
    fenced.await.unwrap().unwrap();

    rust_fence.enabled.store(true, Ordering::SeqCst);
    connection.client().proxy().release();
    let actor = runtime.clone();
    let flush = tokio::spawn(async move { actor.flush().await });
    rust_fence.entered.notified().await;
    flush.abort();
    let _ = flush.await;
    let actor = runtime.clone();
    let second = tokio::spawn(async move { actor.flush().await });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    rust_fence.resume.notify_one();
    second.await.unwrap().unwrap();

    // Revocation closes the dependent original Ctx before ACK. Another pair in
    // this same connection remains live until it is independently stopped.
    // Losing only the declared root closes its captured native consumers,
    // while the actual provider activation remains open until its own stop.
    host.close_objects(&owner("node", 1), &[db.client().proxy().identity()])
        .unwrap()
        .await
        .unwrap();
    assert!(!consumer.gate().is_open());
    assert!(!extra_consumer.gate().is_open());
    let status = node_peer
        .request("fixture/status", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["providerOpen"], true);
    assert_eq!(status["consumerOpen"], true);
    assert_eq!(status["extraOpen"], false);
    host.close_member(&owner("node", 1)).await.unwrap();
    assert!(!consumer.gate().is_open());
    assert!(!extra_consumer.gate().is_open());
    assert!(extra_original.effect(|| Effect::Done).is_err());
    let called = Arc::new(AtomicUsize::new(0));
    let entered = called.clone();
    assert!(original
        .effect(move || {
            entered.fetch_add(1, Ordering::SeqCst);
            Effect::Done
        })
        .is_err());
    assert_eq!(called.load(Ordering::SeqCst), 0);
    consumer.stop().await.unwrap();
    extra_consumer.stop().await.unwrap();
    node_peer
        .request("fixture/stop", json!({"activation":"1"}))
        .await
        .unwrap();
    assert!(provider.gate().is_open());
    assert!(runtime.retire().await.unwrap().is_some());
    // Native necessary-service loss propagates on the same private stream;
    // Host sends no initial stop and the old activation cannot reload.
    let remove = removal.lock().unwrap().take().unwrap();
    remove.dispose().await.unwrap();
    assert!(!provider.gate().is_open());
    eventually(async || {
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["consumerOpen"]
            == false
    })
    .await;
    assert_eq!(
        node_peer
            .request("fixture/status", Value::Null)
            .await
            .unwrap()["extraOpen"],
        false
    );
    runtime.close_member(&rust_provider);
    provider.stop().await.unwrap();
    // Runtime-wide receipt evidence remains valid after all members close.
    assert!(runtime.retire().await.unwrap().is_some());
    node_peer
        .request("fixture/retire", Value::Null)
        .await
        .unwrap();
    let closed = node_peer
        .request("fixture/close", Value::Null)
        .await
        .unwrap();
    assert_eq!(closed["cleanup"], 4);
    assert_eq!(cleanup.load(Ordering::SeqCst), 3);
    native_root.shutdown().await.unwrap();
    node_peer.close(ProtocolError::new(
        ErrorCode::Unavailable,
        "fixture",
        "finished",
    ));
    let status = child.wait().await.unwrap();
    let diagnostics = stderr_task.await.unwrap();
    assert!(status.success(), "Node exit: {status}: {diagnostics}");
    rust_peer.close(ProtocolError::new(
        ErrorCode::Unavailable,
        "fixture",
        "finished",
    ));
}

#[tokio::test]
async fn disconnected_native_admission_without_dispatch_does_not_hold_shutdown_open() {
    let root = Ctx::root().unwrap();
    let runtime = RuntimeObjects::new(identity("native"), bundles());
    let activation = owner("native", 1);
    let gate = ActivationGate::default();
    runtime.reserve(activation.clone(), gate.clone()).unwrap();
    let native = ManagedActivation::mount_gated(
        &root,
        Native {
            injects: vec![],
            run: Arc::new(|_| Box::pin(async { Ok(Effect::Done) })),
        },
        gate.clone(),
    )
    .unwrap();
    native.view().await.unwrap();
    let exports = Exports::managed(
        &native.native_context().unwrap(),
        activation.clone(),
        runtime.ids(),
        gate,
    )
    .unwrap();
    runtime
        .bind(
            &activation,
            native.native_context().unwrap(),
            exports.clone(),
        )
        .unwrap();
    let session: Arc<dyn rpc::InterfaceSessionService> = Arc::new_cyclic(|session| Session {
        agent: Arc::new(Agent {
            session: session.clone(),
        }),
    });
    let contract = bundles().client::<rpc::InterfaceSessionClient>().unwrap();
    let staged = runtime
        .encode(
            &activation,
            &contract.bundle_sha256,
            &rutis_protocol::services::service_type(&contract),
            rpc::exportInterfaceSession(session),
        )
        .unwrap();
    let object = staged.draft.references[0].source.object().clone();
    // Authoritative Host pin control with a real SDK-owned identity, before
    // execute arrives. No grant or object number is invented by a plugin.
    let key = rutis_protocol::exports::PinKey::Execution {
        caller: owner("host", 1),
        call: Sequence(1),
    };
    runtime
        .handle("object/pin", json!({"object":object,"key":key}))
        .await
        .unwrap();
    assert!(exports.pins(&object) >= 2);
    runtime.close();
    assert_eq!(exports.pins(&object), 0);
    tokio::time::timeout(std::time::Duration::from_secs(2), native.stop())
        .await
        .unwrap()
        .unwrap();
    root.shutdown().await.unwrap();
}
