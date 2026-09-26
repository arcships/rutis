//! Default service drivers in actual Rust/Node children, loaded from a frozen
//! four-package plan. Host proxy publication and OS recovery remain separate.
#![cfg(target_os = "linux")]
#[path = "support/native_plan.rs"]
mod native_plan;
rutis_protocol::embed_runner_catalog!(include_bytes!(
    "../../../protocol/fixtures/native.runner.catalog.json"
));
#[allow(dead_code, unused_variables, non_snake_case)]
mod rpc {
    include!("../../../protocol/generated/rpc.rs");
}
use rpc::*;
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, PluginFactory, TypeKey};
use rutis_protocol::{
    deployment::DeploymentObjects,
    error::{ErrorCode, ProtocolError, Result},
    factories::{StaticFactories, StaticFactory},
    frame::{Handler, Peer},
    identity::{Activation, Sequence},
    lifecycle::{hello, serve, Hello, NativeDriver},
    runner_image::RunnerCatalog,
    sdk::*,
    services::{Bundles, NativePorts, ServiceTable},
    session::{HostObjects, RuntimeIdentity},
    snapshot::{Snapshot, SnapshotGroup},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    os::fd::{AsRawFd, FromRawFd},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::Duration,
};
const RPC: &[u8] = include_bytes!("../../../protocol/fixtures/rpc.bundle.json");
const CONFIG: &[u8] = include_bytes!("../../../protocol/fixtures/plugin.config.json");
const CATALOG: &[u8] = include_bytes!("../../../protocol/fixtures/native.runner.catalog.json");
static CLEANED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn owner(runtime: &str, n: u64) -> Activation {
    Activation {
        runtime: runtime.into(),
        epoch: Sequence(1),
        activation: Sequence(n),
    }
}
fn identity(runtime: &str) -> RuntimeIdentity {
    RuntimeIdentity {
        runtime: runtime.into(),
        epoch: Sequence(1),
    }
}
fn report(value: Value) {
    println!("\n{value}");
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
    creator: Ctx,
    session: Arc<Session>,
}
impl InterfaceConnectionService for Connection {
    fn session(&self) -> Result<Arc<dyn InterfaceSessionService>> {
        Ok(self.session.clone())
    }
    fn query(
        &self,
        context: CallContext,
        params: InterfaceConnectionMethod0Params,
    ) -> RpcFuture<Vec<BTreeMap<String, Value>>> {
        let key = TypeKey::of::<dyn InterfaceDatabaseService>();
        assert!(Arc::ptr_eq(
            &context
                .native()
                .unwrap()
                .get_as::<dyn InterfaceDatabaseService>(key.clone())
                .unwrap(),
            &self
                .creator
                .get_as::<dyn InterfaceDatabaseService>(key)
                .unwrap()
        ));
        Box::pin(async move {
            Ok(vec![BTreeMap::from([
                ("owner".into(), json!("rust")),
                ("sql".into(), json!(params.sql)),
            ])])
        })
    }
}
struct Database(Arc<Connection>);
impl InterfaceDatabaseService for Database {
    fn connect(
        &self,
        _: CallContext,
        _: InterfaceDatabaseMethod0Params,
    ) -> RpcFuture<Arc<dyn InterfaceConnectionService>> {
        let connection: Arc<dyn InterfaceConnectionService> = self.0.clone();
        Box::pin(async move { Ok(connection) })
    }
    fn inspect(&self, _: CallContext, value: InterfaceConnectionClient) -> RpcFuture<bool> {
        Box::pin(async move {
            Ok(value
                .query(InterfaceConnectionMethod0Params {
                    sql: "passback".into(),
                })
                .await?[0]["owner"]
                == "rust")
        })
    }
    fn withCallback(&self, context: CallContext, callback: BorrowCallback0Client) -> RpcFuture<()> {
        Box::pin(async move {
            callback.call("root".into()).await?;
            context.spawn(async move { callback.call("child".into()).await })?;
            Ok(())
        })
    }
}
struct Callback {
    creator: Ctx,
    connection: InterfaceConnectionClient,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}
impl BorrowCallback0Service for Callback {
    fn call(&self, context: CallContext, text: String) -> RpcFuture<()> {
        assert!(Arc::ptr_eq(
            &context
                .native()
                .unwrap()
                .require::<InterfaceDatabaseClient>()
                .unwrap(),
            &self.creator.require::<InterfaceDatabaseClient>().unwrap()
        ));
        self.calls.lock().unwrap().push(text.clone());
        let connection = self.connection.clone();
        Box::pin(async move {
            connection
                .query(InterfaceConnectionMethod0Params { sql: text })
                .await?;
            Ok(())
        })
    }
}
struct Native {
    role: &'static str,
    keys: Vec<TypeKey>,
    label: String,
}
impl Plugin for Native {
    fn name(&self) -> &str {
        self.role
    }
    fn injects(&self) -> &[TypeKey] {
        &self.keys
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let label = self.label.clone();
            ctx.effect(move || {
                Effect::Disposer(Box::new(move || {
                    CLEANED.fetch_add(1, Ordering::SeqCst);
                    report(json!({"cleaned":label}));
                    Ok(())
                }))
            })?;
            if self.role == "provider" && self.label != "missing" {
                let session = Arc::new_cyclic(|session| Session {
                    agent: Arc::new(Agent {
                        session: session.clone(),
                    }),
                });
                let database: Arc<dyn InterfaceDatabaseService> =
                    Arc::new(Database(Arc::new(Connection {
                        creator: ctx.clone(),
                        session,
                    })));
                ctx.provide_as(TypeKey::of::<dyn InterfaceDatabaseService>(), database)?;
            } else if self.role == "consumer" {
                let database = ctx.require::<InterfaceDatabaseClient>()?;
                let first = database
                    .connect(InterfaceDatabaseMethod0Params {
                        name: "first".into(),
                    })
                    .await
                    .unwrap();
                assert!(same_wrapper(
                    &first,
                    &database
                        .connect(InterfaceDatabaseMethod0Params {
                            name: "second".into()
                        })
                        .await
                        .unwrap()
                ));
                let session = first.session().unwrap();
                assert!(same_wrapper(
                    &session,
                    &session.agent().unwrap().session().unwrap()
                ));
                assert!(database.inspect(first.clone()).await.unwrap());
                let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
                database
                    .withCallback(Arc::new(Callback {
                        creator: ctx.clone(),
                        connection: first.clone(),
                        calls: calls.clone(),
                    }))
                    .await
                    .unwrap();
                assert_eq!(*calls.lock().unwrap(), ["root", "child"]);
                let row = first
                    .query(InterfaceConnectionMethod0Params {
                        sql: "ordinary".into(),
                    })
                    .await
                    .unwrap();
                report(
                    json!({"exercised":self.label,"owner":row[0]["owner"],"callbacks":calls.lock().unwrap().len()}),
                );
            }
            Ok(Effect::Done)
        })
    }
}
struct Factory {
    role: &'static str,
    keys: Vec<TypeKey>,
}
impl PluginFactory<Value> for Factory {
    fn injects(&self) -> &[TypeKey] {
        &self.keys
    }
    fn build(&self, config: &Value) -> std::result::Result<Box<dyn Plugin>, CordisError> {
        Ok(Box::new(Native {
            role: self.role,
            keys: self.keys.clone(),
            label: config["label"].as_str().unwrap().into(),
        }))
    }
}
fn driver(root: Ctx) -> NativeDriver {
    let bundles = Bundles::admit([RPC.to_vec()]).unwrap();
    let catalog = RunnerCatalog::parse(CATALOG).unwrap();
    let mut ports = BTreeMap::new();
    let mut entries = Vec::new();
    for role in ["provider", "consumer"] {
        let mut port = NativePorts::default();
        if role == "provider" {
            port.provide::<dyn InterfaceDatabaseService, InterfaceDatabaseClient>(
                "rpc",
                TypeKey::of::<dyn InterfaceDatabaseService>(),
                &bundles,
                exportInterfaceDatabase,
            )
            .unwrap();
        } else {
            port.require::<InterfaceDatabaseClient>(
                "rpc",
                TypeKey::of::<InterfaceDatabaseClient>(),
                &bundles,
            )
            .unwrap();
        }
        let keys = port.required_keys();
        ports.insert(role.into(), Arc::new(port));
        entries.push(
            StaticFactory::native(role, catalog.factories[role].clone(), CONFIG, move || {
                report(json!({"constructed":role}));
                Factory {
                    role,
                    keys: keys.clone(),
                }
            })
            .unwrap(),
        );
    }
    NativeDriver::with_services(
        root,
        StaticFactories::admit(CATALOG, entries).unwrap(),
        bundles,
        ports,
    )
    .unwrap()
}
#[tokio::test]
async fn native_image_entry() {
    if std::env::var_os("RUTIS_NATIVE_IMAGE_ENTRY").is_none() {
        return;
    }
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(3) };
    stream.set_nonblocking(true).unwrap();
    let root = Ctx::root().unwrap();
    serve(
        tokio::net::UnixStream::from_std(stream).unwrap(),
        driver(root.clone()),
    )
    .await
    .unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn default_driver_missing_declared_export_joins_cleanup_without_a_pending_guard() {
    use rutis_protocol::{
        contract::{FAMILY, VERSION},
        lifecycle::{Entry, Member, RuntimeIdentity as LifecycleIdentity},
        prepare::{digest, RuntimeKind},
    };
    let root = Ctx::root().unwrap();
    let catalog = RunnerCatalog::parse(CATALOG).unwrap();
    let (client, server) = tokio::io::duplex(65536);
    let native = driver(root.clone());
    let served = tokio::spawn(serve(server, native));
    let peer = Peer::start(client, Arc::new(|_, _| Box::pin(async { Ok(Value::Null) })));
    let launch = Hello {
        protocol_family: FAMILY.into(),
        protocol_version: VERSION.into(),
        identity: LifecycleIdentity {
            runtime: "rust".into(),
            epoch: Sequence(1),
            kind: RuntimeKind::RustRutis,
            framework_version: catalog.framework_version,
            environment_sha256: catalog.environment_sha256,
            code_sha256: digest(b"local-native-driver"),
            capabilities: catalog.capabilities,
        },
        members: BTreeMap::from([(
            "missing".into(),
            Member {
                entry: Entry::Rust {
                    factory: "provider".into(),
                },
                config: json!({"label":"missing"}),
                config_schema: String::from_utf8(CONFIG.to_vec()).unwrap(),
                contracts: catalog.factories["provider"].clone(),
            },
        )]),
    };
    let before = CLEANED.load(Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), async {
        hello(&peer, &launch).await.unwrap();
        assert_eq!(
            peer.request(
                "plugin/start",
                json!({"instance":"missing","activation":owner("rust",1)})
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::Unavailable
        );
        assert_eq!(CLEANED.load(Ordering::SeqCst), before + 1);
        let phase = peer
            .request("plugin/state", json!({"activation":owner("rust",1)}))
            .await
            .unwrap()["phase"]
            .as_str()
            .unwrap()
            .to_owned();
        // Gate revocation also requests stop independently of the failed start.
        assert!(matches!(phase.as_str(), "failed" | "closing" | "stopped"));
        peer.request("runtime/stop", json!({})).await.unwrap();
        peer.close(ProtocolError::new(
            ErrorCode::Unavailable,
            "test",
            "joined native rollback",
        ));
        served.await.unwrap().unwrap();
        root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}
#[derive(Default)]
struct Fence {
    enabled: AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
fn handler(host: &Arc<HostObjects>, runtime: &str, fence: Arc<Fence>) -> Handler {
    let fallback: Handler = Arc::new(|_, _| {
        Box::pin(async {
            Err(ProtocolError::new(
                ErrorCode::UnsupportedCapability,
                "test",
                "unknown Host request",
            ))
        })
    });
    let handler = host.handler(identity(runtime), fallback);
    Arc::new(move |method, value| {
        let held = method == "object/controls"
            && value
                .as_array()
                .is_some_and(|controls| controls.iter().any(|control| control["type"] == "accept"))
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
struct Process {
    child: tokio::process::Child,
    peer: Peer,
    records: tokio::sync::mpsc::UnboundedReceiver<Value>,
    stderr: tokio::task::JoinHandle<String>,
}
impl Process {
    fn launch(
        host: &Arc<HostObjects>,
        runtime: &str,
        group: &SnapshotGroup,
        fence: Arc<Fence>,
    ) -> Self {
        let (parent, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
        parent.set_nonblocking(true).unwrap();
        let fd = child_socket.as_raw_fd();
        let argv = group.argv();
        let mut command = tokio::process::Command::new(&argv[0]);
        command.args(&argv[1..]);
        if runtime == "rust" {
            command.env("RUTIS_NATIVE_IMAGE_ENTRY", "1").args([
                "--exact",
                "native_image_entry",
                "--nocapture",
            ]);
        } else {
            command.arg(group.node_catalog().unwrap());
        }
        command
            .stdin(Stdio::null())
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
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let (tx, records) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(line) = lines.next_line().await.unwrap() {
                if line.starts_with('{') {
                    tx.send(serde_json::from_str(&line).unwrap()).unwrap();
                }
            }
        });
        let mut error = child.stderr.take().unwrap();
        let stderr = tokio::spawn(async move {
            let mut text = String::new();
            error.read_to_string(&mut text).await.unwrap();
            if !text.is_empty() {
                eprintln!("{text}");
            }
            text
        });
        let peer = Peer::start(
            tokio::net::UnixStream::from_std(parent).unwrap(),
            handler(host, runtime, fence),
        );
        host.attach(identity(runtime), &peer).unwrap();
        Self {
            child,
            peer,
            records,
            stderr,
        }
    }
    async fn record(&mut self, key: &str, expected: &str) -> Value {
        let record = self.records.recv().await.unwrap();
        assert_eq!(record[key], expected, "{record}");
        record
    }
    async fn finish(mut self) {
        self.peer.request("runtime/stop", json!({})).await.unwrap();
        self.peer.close(ProtocolError::new(
            ErrorCode::Unavailable,
            "test",
            "native cleanup confirmed",
        ));
        let status = self.child.wait().await.unwrap();
        let stderr = self.stderr.await.unwrap();
        assert!(status.success(), "{status}: {stderr}");
        let mut cleaned = Vec::new();
        while let Some(record) = self.records.recv().await {
            if let Some(value) = record["cleaned"].as_str() {
                cleaned.push(value.to_owned());
            } else {
                panic!("unexpected late business code: {record}");
            }
        }
        cleaned.sort();
        assert_eq!(cleaned.len(), 2);
        assert!(cleaned[0].ends_with("consumer"));
        assert!(cleaned[1].ends_with("provider"));
    }
}
#[tokio::test]
async fn frozen_default_drivers_accept_before_construction_and_route_bidirectionally() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let plan = native_plan::prepare();
        let snapshot = Snapshot::materialize(&plan).unwrap();
        assert!(
            plan.packages()
                .values()
                .all(|package| !package.root().exists()),
            "original package paths were deleted before launch"
        );
        let objects = DeploymentObjects::new(plan.clone()).unwrap();
        let host = objects.host();
        for runtime in ["rust", "node"] {
            for (suffix, n) in [("provider", 1), ("consumer", 2), ("cancel", 3)] {
                objects
                    .reserve(&format!("{runtime}-{suffix}"), owner(runtime, n))
                    .unwrap();
            }
        }
        let rust_fence = Arc::new(Fence::default());
        let node_fence = Arc::new(Fence::default());
        let mut rust = Process::launch(
            &host,
            "rust",
            &snapshot.groups()["rust"],
            rust_fence.clone(),
        );
        let mut node = Process::launch(
            &host,
            "node",
            &snapshot.groups()["node"],
            node_fence.clone(),
        );
        for (runtime, process) in [("rust", &rust), ("node", &node)] {
            let launch = Hello::prepared(
                &plan,
                runtime,
                &snapshot.groups()[runtime],
                runtime.into(),
                Sequence(1),
            )
            .unwrap();
            let mut unsupported = launch.clone();
            unsupported
                .identity
                .capabilities
                .insert("event.parallel".into());
            assert_eq!(
                hello(&process.peer, &unsupported).await.unwrap_err().code,
                ErrorCode::UnsupportedCapability
            );
            hello(&process.peer, &launch).await.unwrap();
            let instance = format!("{runtime}-provider");
            let table: ServiceTable = serde_json::from_value(
                objects
                    .offer_required(&instance)
                    .await
                    .unwrap()
                    .start(&instance)
                    .await
                    .unwrap(),
            )
            .unwrap();
            objects.stage(&instance, table).unwrap();
            process
                .peer
                .request("plugin/activate", json!({"activation":owner(runtime, 1)}))
                .await
                .unwrap();
            host.publish(&owner(runtime, 1)).unwrap();
        }
        rust.record("constructed", "provider").await;
        node.record("loaded", "node-provider").await;
        for (runtime, process, fence) in [
            ("rust", &mut rust, &rust_fence),
            ("node", &mut node, &node_fence),
        ] {
            let instance = format!("{runtime}-consumer");
            let delivery = objects.offer_required(&instance).await.unwrap();
            fence.enabled.store(true, Ordering::SeqCst);
            let starting = tokio::spawn(async move { delivery.start(&instance).await });
            fence.entered.notified().await;
            assert_eq!(
                process
                    .peer
                    .request("plugin/state", json!({"activation":owner(runtime, 2)}))
                    .await
                    .unwrap()["phase"],
                "starting"
            );
            assert!(
                process.records.try_recv().is_err(),
                "business construction/import preceded Accept ACK"
            );
            fence.resume.notify_one();
            let table = serde_json::from_value(starting.await.unwrap().unwrap()).unwrap();
            objects
                .stage(&format!("{runtime}-consumer"), table)
                .unwrap();
            process
                .record(
                    if runtime == "rust" {
                        "constructed"
                    } else {
                        "loaded"
                    },
                    if runtime == "rust" {
                        "consumer"
                    } else {
                        "node-consumer"
                    },
                )
                .await;
            let exercised = process
                .record("exercised", &format!("{runtime}-consumer"))
                .await;
            assert_eq!(exercised["callbacks"], 2);
            assert_eq!(
                exercised["owner"],
                if runtime == "rust" { "node" } else { "rust" }
            );
            assert_eq!(
                process
                    .peer
                    .request("plugin/state", json!({"activation":owner(runtime, 2)}))
                    .await
                    .unwrap()["phase"],
                "staged"
            );
            process
                .peer
                .request("plugin/activate", json!({"activation":owner(runtime, 2)}))
                .await
                .unwrap();
            host.publish(&owner(runtime, 2)).unwrap();
            let instance = format!("{runtime}-cancel");
            let delivery = objects.offer_required(&instance).await.unwrap();
            fence.enabled.store(true, Ordering::SeqCst);
            let starting = tokio::spawn(async move { delivery.start(&instance).await });
            fence.entered.notified().await;
            let stopped = process
                .peer
                .start_request("plugin/stop", json!({"activation":owner(runtime, 3)}))
                .unwrap();
            assert_eq!(
                process
                    .peer
                    .request("plugin/state", json!({"activation":owner(runtime, 3)}))
                    .await
                    .unwrap()["phase"],
                "closing"
            );
            fence.resume.notify_one();
            let error = starting.await.unwrap().unwrap_err();
            assert!(matches!(
                error.code,
                ErrorCode::Cancelled | ErrorCode::Unavailable
            ));
            assert_eq!(stopped.await.unwrap().unwrap()["phase"], "stopped");
            assert!(
                process.records.try_recv().is_err(),
                "cancelled admission ran business code"
            );
        }
        rust.finish().await;
        node.finish().await;
        assert!(snapshot.root().exists());
        snapshot.cleanup().unwrap();
    })
    .await
    .unwrap();
}
