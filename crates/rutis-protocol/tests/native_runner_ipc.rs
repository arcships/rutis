//! Default service drivers and Host instance/native publication in actual
//! Rust/Node children loaded from a frozen plan. OS recovery remains separate.
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
        Arc, Mutex, Weak,
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
    cleanup: Option<Arc<std::sync::atomic::AtomicUsize>>,
    remove: Option<Arc<Mutex<Option<rutis::Disposer>>>>,
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
            let cleanup = self.cleanup.clone();
            ctx.effect(move || {
                Effect::Disposer(Box::new(move || {
                    if let Some(cleanup) = cleanup {
                        cleanup.fetch_add(1, Ordering::SeqCst);
                    } else {
                        CLEANED.fetch_add(1, Ordering::SeqCst);
                    }
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
                let registration =
                    ctx.provide_as(TypeKey::of::<dyn InterfaceDatabaseService>(), database)?;
                if let Some(remove) = &self.remove {
                    *remove.lock().unwrap() = Some(registration);
                }
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
            cleanup: None,
            remove: None,
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
/// Delay the real child's activate response without blocking its request pump
/// or changing frame contents. A stop can pass while that ACK is still held.
fn delayed_activate(stream: tokio::net::UnixStream, fence: Arc<Fence>) -> tokio::io::DuplexStream {
    let (peer, bridge) = tokio::io::duplex(65536);
    let (mut host_read, mut host_write) = tokio::io::split(bridge);
    let (mut child_read, mut child_write) = stream.into_split();
    let activations = Arc::new(Mutex::new(std::collections::BTreeSet::<String>::new()));
    let outbound = activations.clone();
    tokio::spawn(async move {
        while let Ok(Some(frame)) = rutis_protocol::frame::read(&mut host_read).await {
            if frame["type"] == "request" && frame["method"] == "plugin/activate" {
                outbound
                    .lock()
                    .unwrap()
                    .insert(frame["id"].as_str().unwrap().into());
            }
            if rutis_protocol::frame::write(&mut child_write, &frame)
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let (tx, mut frames) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            if rutis_protocol::frame::write(&mut host_write, &frame)
                .await
                .is_err()
            {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Ok(Some(frame)) = rutis_protocol::frame::read(&mut child_read).await {
            let held = frame["type"] == "response"
                && activations
                    .lock()
                    .unwrap()
                    .remove(frame["id"].as_str().unwrap())
                && fence.enabled.swap(false, Ordering::SeqCst);
            if held {
                let fence = fence.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    fence.entered.notify_one();
                    fence.resume.notified().await;
                    let _ = tx.send(frame);
                });
            } else if tx.send(frame).is_err() {
                break;
            }
        }
    });
    peer
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
        Self::launch_held(host, runtime, group, fence, None)
    }
    fn launch_held(
        host: &Arc<HostObjects>,
        runtime: &str,
        group: &SnapshotGroup,
        fence: Arc<Fence>,
        activate: Option<Arc<Fence>>,
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
        let stream = tokio::net::UnixStream::from_std(parent).unwrap();
        let peer = if let Some(activate) = activate {
            Peer::start(
                delayed_activate(stream, activate),
                handler(host, runtime, fence),
            )
        } else {
            Peer::start(stream, handler(host, runtime, fence))
        };
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
    async fn finish(self) {
        self.finish_count(2).await;
    }
    async fn finish_count(mut self, count: usize) {
        if !self.peer.is_closed() {
            self.peer.request("runtime/stop", json!({})).await.unwrap();
        }
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
        assert_eq!(cleaned.len(), count, "{cleaned:?}");
        assert!(cleaned.iter().any(|name| name.ends_with("provider")));
        assert_eq!(
            cleaned
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            count,
            "duplicate native cleanup: {cleaned:?}"
        );
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

#[tokio::test]
async fn host_proxy_stop_passes_a_loading_start_and_late_activate_ack_cannot_reopen_it() {
    use rutis::FiberState;
    use rutis_protocol::{host::HostGraph, lifecycle::publish_ready};
    tokio::time::timeout(Duration::from_secs(60), async {
        let plan = native_plan::prepare();
        let snapshot = Snapshot::materialize(&plan).unwrap();
        let objects = DeploymentObjects::new(plan.clone()).unwrap();
        let host = objects.host();
        let graph = HostGraph::new(objects, identity("host")).unwrap();
        let root = Ctx::root().unwrap();
        let rust_accept = Arc::new(Fence::default());
        let node_accept = Arc::new(Fence::default());
        let node_activate = Arc::new(Fence::default());
        let mut rust = Process::launch(
            &host,
            "rust",
            &snapshot.groups()["rust"],
            rust_accept.clone(),
        );
        let mut node = Process::launch_held(
            &host,
            "node",
            &snapshot.groups()["node"],
            node_accept.clone(),
            Some(node_activate.clone()),
        );
        let rust_launch = Hello::prepared(
            &plan,
            "rust",
            &snapshot.groups()["rust"],
            "rust".into(),
            Sequence(1),
        )
        .unwrap();
        let node_launch = Hello::prepared(
            &plan,
            "node",
            &snapshot.groups()["node"],
            "node".into(),
            Sequence(1),
        )
        .unwrap();
        hello(&rust.peer, &rust_launch).await.unwrap();
        hello(&node.peer, &node_launch).await.unwrap();
        let missing = graph
            .mount(&root, "rust-missing", rust_launch.identity.clone())
            .unwrap();
        let rust_provider = graph
            .mount(&root, "rust-provider", rust_launch.identity.clone())
            .unwrap();
        let node_provider = graph
            .mount(&root, "node-provider", node_launch.identity.clone())
            .unwrap();
        let rust_ready =
            publish_ready(&root, rust_launch.identity.clone(), rust.peer.clone()).unwrap();
        let node_ready =
            publish_ready(&root, node_launch.identity.clone(), node.peer.clone()).unwrap();
        rust_provider.ready().await.unwrap();
        node_provider.ready().await.unwrap();
        rust.record("constructed", "provider").await;
        node.record("loaded", "node-provider").await;
        assert_eq!(missing.native().view().state().state, FiberState::Pending);
        assert!(missing.activation().is_none());
        for (runtime, launch, process, fence) in [
            ("rust", &rust_launch, &mut rust, &rust_accept),
            ("node", &node_launch, &mut node, &node_accept),
        ] {
            fence.enabled.store(true, Ordering::SeqCst);
            let cancelled = graph
                .mount(&root, &format!("{runtime}-cancel"), launch.identity.clone())
                .unwrap();
            fence.entered.notified().await;
            assert_eq!(cancelled.native().view().state().state, FiberState::Loading);
            let ctx = cancelled.native().native_context().unwrap();
            let stopped = cancelled.stop();
            drop(stopped);
            let activation = cancelled.activation().unwrap();
            assert_eq!(
                process
                    .peer
                    .request("plugin/state", json!({"activation":activation}))
                    .await
                    .unwrap()["phase"],
                "closing",
                "remote stop is sent before Host native apply/effect drain can finish"
            );
            assert!(ctx
                .effect(|| panic!("stopped Loading context admitted a new effect"))
                .is_err());
            assert!(process.records.try_recv().is_err());
            fence.resume.notify_one();
            cancelled.stop().await.unwrap();
            assert!(cancelled.ready().await.is_err());
            assert!(
                process.records.try_recv().is_err(),
                "cancelled root admission entered business apply"
            );
            assert!(graph
                .mount(&root, &format!("{runtime}-cancel"), launch.identity.clone())
                .is_err());
        }
        node_activate.enabled.store(true, Ordering::SeqCst);
        let late = graph
            .mount(&root, "node-late", node_launch.identity.clone())
            .unwrap();
        node_activate.entered.notified().await;
        node.record("loaded", "node-consumer").await;
        node.record("exercised", "node-late").await;
        assert_eq!(late.native().view().state().state, FiberState::Active);
        assert!(!late.is_available());
        let old = late.native().native_context().unwrap();
        late.stop().await.unwrap();
        assert!(!late.is_available());
        assert!(old
            .effect(|| panic!("old late-ACK context entered effect"))
            .is_err());
        node_activate.resume.notify_one();
        assert!(late.ready().await.is_err());
        assert!(!late.is_available());
        assert!(node_provider.is_available());
        assert!(rust_provider.is_available());
        rust_provider.stop().await.unwrap();
        rust.peer.close(ProtocolError::new(
            ErrorCode::Unavailable,
            "test",
            "confirmed provider stop then epoch close",
        ));
        assert!(
            !missing.native().gate().is_open(),
            "Pending proxy also closes synchronously on epoch disconnect"
        );
        assert!(
            graph
                .mount(&root, "rust-late", rust_launch.identity)
                .is_err(),
            "closed epoch cannot admit an unentered instance"
        );
        graph.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
        drop(rust_ready);
        drop(node_ready);
        rust.finish_count(1).await;
        node.finish_count(2).await;
        drop(late);
        drop(missing);
        drop(rust_provider);
        drop(node_provider);
        drop(graph);
        snapshot.cleanup().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_host_graph_waits_for_activate_ack_refreshes_real_keys_and_closes_consumers_synchronously(
) {
    use rutis::FiberState;
    use rutis_protocol::{host::HostGraph, lifecycle::publish_ready};
    tokio::time::timeout(Duration::from_secs(50), async {
        let plan = native_plan::prepare();
        let snapshot = Snapshot::materialize(&plan).unwrap();
        let objects = DeploymentObjects::new(plan.clone()).unwrap();
        let host = objects.host();
        let graph = HostGraph::new(objects, identity("host")).unwrap();
        let root = Ctx::root().unwrap();
        let activate = Arc::new(Fence::default());
        activate.enabled.store(true, Ordering::SeqCst);
        let mut rust = Process::launch_held(
            &host,
            "rust",
            &snapshot.groups()["rust"],
            Arc::default(),
            Some(activate.clone()),
        );
        let mut node = Process::launch(&host, "node", &snapshot.groups()["node"], Arc::default());
        let rust_launch = Hello::prepared(
            &plan,
            "rust",
            &snapshot.groups()["rust"],
            "rust".into(),
            Sequence(1),
        )
        .unwrap();
        let node_launch = Hello::prepared(
            &plan,
            "node",
            &snapshot.groups()["node"],
            "node".into(),
            Sequence(1),
        )
        .unwrap();
        let rust_provider = graph
            .mount(&root, "rust-provider", rust_launch.identity.clone())
            .unwrap();
        let rust_consumer = graph
            .mount(&root, "rust-consumer", rust_launch.identity.clone())
            .unwrap();
        let node_provider = graph
            .mount(&root, "node-provider", node_launch.identity.clone())
            .unwrap();
        let node_consumer = graph
            .mount(&root, "node-consumer", node_launch.identity.clone())
            .unwrap();
        hello(&rust.peer, &rust_launch).await.unwrap();
        hello(&node.peer, &node_launch).await.unwrap();
        for proxy in [
            &rust_provider,
            &rust_consumer,
            &node_provider,
            &node_consumer,
        ] {
            assert_eq!(proxy.native().view().state().state, FiberState::Pending);
            assert!(proxy.activation().is_none());
        }
        assert!(rust.records.try_recv().is_err());
        assert!(node.records.try_recv().is_err());
        let rust_ready = publish_ready(&root, rust_launch.identity, rust.peer.clone()).unwrap();
        activate.entered.notified().await;
        assert_eq!(
            rust_provider.native().view().state().state,
            FiberState::Active
        );
        assert!(!rust_provider.is_available());
        let rust_key = plan.export_key("rust-provider", "rpc").unwrap();
        let raw = root
            .get_as::<rutis_protocol::imports::ObjectProxy>(rust_key.clone())
            .unwrap();
        assert_eq!(raw.delivery().unwrap_err().code, ErrorCode::Unavailable);
        assert_eq!(
            rust.peer
                .request(
                    "plugin/state",
                    json!({"activation":rust_provider.activation().unwrap()})
                )
                .await
                .unwrap()["phase"],
            "published",
            "real remote activate completed but its ACK remains held"
        );
        assert_eq!(
            node_consumer.native().view().state().state,
            FiberState::Pending
        );
        let node_ready = publish_ready(&root, node_launch.identity, node.peer.clone()).unwrap();
        node_provider.ready().await.unwrap();
        rust_consumer.ready().await.unwrap();
        assert_eq!(
            node_consumer.native().view().state().state,
            FiberState::Pending,
            "remote availability cannot precede activate ACK"
        );
        assert!(
            rust_consumer.is_available(),
            "a sibling in the same Rust runtime can publish while provider ACK is held"
        );
        rust.record("constructed", "provider").await;
        rust.record("constructed", "consumer").await;
        rust.record("exercised", "rust-consumer").await;
        node.record("loaded", "node-provider").await;
        assert!(node.records.try_recv().is_err());
        activate.resume.notify_one();
        rust_provider.ready().await.unwrap();
        node_consumer.ready().await.unwrap();
        node.record("loaded", "node-consumer").await;
        node.record("exercised", "node-consumer").await;
        let captured = node_consumer.captured();
        assert!(Arc::ptr_eq(
            &captured[0],
            &root
                .get_as::<rutis_protocol::imports::ObjectProxy>(rust_key.clone())
                .unwrap()
        ));
        assert_eq!(
            captured[0].delivery().unwrap().object.owner,
            rust_provider.activation().unwrap()
        );
        let client = rust_provider
            .service::<InterfaceDatabaseClient>("rpc")
            .unwrap();
        let connection = client
            .connect(InterfaceDatabaseMethod0Params {
                name: "actual-host-type-key".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            connection
                .query(InterfaceConnectionMethod0Params { sql: "host".into() })
                .await
                .unwrap()[0]["owner"],
            "rust"
        );
        let rust_ctx = rust_provider.native().native_context().unwrap();
        let node_ctx = node_consumer.native().native_context().unwrap();
        let revoked = host.close_member(&rust_provider.activation().unwrap());
        assert!(!rust_provider.is_available());
        assert!(!node_consumer.is_available());
        assert!(rust_ctx
            .effect(|| panic!("old provider effect factory entered"))
            .is_err());
        assert!(node_ctx
            .effect(|| panic!("old consumer effect factory entered"))
            .is_err());
        assert!(captured[0].delivery().is_err());
        if let Some(value) = root.get_as::<rutis_protocol::imports::ObjectProxy>(rust_key.clone()) {
            assert!(value.delivery().is_err());
        }
        assert!(rust_consumer.is_available());
        assert!(node_provider.is_available());
        assert!(connection
            .query(InterfaceConnectionMethod0Params {
                sql: "stale".into()
            })
            .await
            .is_err());
        revoked.await.unwrap();
        rust_provider.stop().await.unwrap();
        assert!(root
            .get_as::<rutis_protocol::imports::ObjectProxy>(rust_key)
            .is_none());
        node_consumer.stop().await.unwrap();
        graph.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
        drop(rust_ready);
        drop(node_ready);
        rust.finish().await;
        node.finish().await;
        drop(rust_provider);
        drop(rust_consumer);
        drop(node_provider);
        drop(node_consumer);
        drop(graph);
        snapshot.cleanup().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn frozen_native_adapter_installs_real_keys_and_service_loss_closes_both_runtime_consumers() {
    use rutis::FiberState;
    use rutis_protocol::{host::HostGraph, lifecycle::publish_ready};
    tokio::time::timeout(Duration::from_secs(60), async {
        let plan = native_plan::prepare_native();
        let snapshot = Snapshot::materialize(&plan).unwrap();
        let objects = DeploymentObjects::new(plan.clone()).unwrap();
        let host = objects.host();
        let graph = HostGraph::new(objects, identity("host")).unwrap();
        let root = Ctx::root().unwrap();
        let bundles = Bundles::admit([RPC.to_vec()]).unwrap();
        let ready_key = TypeKey::keyed::<String>("host-native-ready");
        let cleaned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let remove = Arc::new(Mutex::new(None));
        let plugin = || Native {
            role: "provider",
            keys: vec![ready_key.clone()],
            label: "host-native".into(),
            cleanup: Some(cleaned.clone()),
            remove: Some(remove.clone()),
        };
        assert_eq!(
            graph
                .mount_native(&root, Arc::new(NativePorts::default()), plugin())
                .err()
                .unwrap()
                .code,
            ErrorCode::InvalidParams
        );
        let mut wrong = NativePorts::default();
        wrong
            .provide::<dyn InterfaceDatabaseService, InterfaceConnectionClient>(
                "host-rpc",
                TypeKey::of::<dyn InterfaceDatabaseService>(),
                &bundles,
                exportInterfaceDatabase,
            )
            .unwrap();
        assert_eq!(
            graph
                .mount_native(&root, Arc::new(wrong), plugin())
                .err()
                .unwrap()
                .code,
            ErrorCode::InterfaceMismatch
        );
        assert_eq!(cleaned.load(Ordering::SeqCst), 0);
        assert!(remove.lock().unwrap().is_none());
        let mut ports = NativePorts::default();
        ports
            .provide::<dyn InterfaceDatabaseService, InterfaceDatabaseClient>(
                "host-rpc",
                TypeKey::of::<dyn InterfaceDatabaseService>(),
                &bundles,
                exportInterfaceDatabase,
            )
            .unwrap();
        let ports = Arc::new(ports);
        let adapter = graph.mount_native(&root, ports.clone(), plugin()).unwrap();
        assert_eq!(adapter.native().view().state().state, FiberState::Pending);
        assert_eq!(
            graph
                .mount_native(&root, ports.clone(), plugin())
                .err()
                .unwrap()
                .code,
            ErrorCode::ScopeClosed
        );
        assert!(plan.native_key("unknown-native").is_err());
        let key = plan.native_key("host-rpc").unwrap();
        for instance in ["rust-consumer", "node-consumer"] {
            assert_eq!(plan.instances()[instance].routes()["rpc"].key(), &key);
        }
        let mut rust = Process::launch(&host, "rust", &snapshot.groups()["rust"], Arc::default());
        let mut node = Process::launch(&host, "node", &snapshot.groups()["node"], Arc::default());
        let rust_launch = Hello::prepared(
            &plan,
            "rust",
            &snapshot.groups()["rust"],
            "rust".into(),
            Sequence(1),
        )
        .unwrap();
        let node_launch = Hello::prepared(
            &plan,
            "node",
            &snapshot.groups()["node"],
            "node".into(),
            Sequence(1),
        )
        .unwrap();
        let rust_provider = graph
            .mount(&root, "rust-provider", rust_launch.identity.clone())
            .unwrap();
        let node_provider = graph
            .mount(&root, "node-provider", node_launch.identity.clone())
            .unwrap();
        let rust_consumer = graph
            .mount(&root, "rust-consumer", rust_launch.identity.clone())
            .unwrap();
        let node_consumer = graph
            .mount(&root, "node-consumer", node_launch.identity.clone())
            .unwrap();
        hello(&rust.peer, &rust_launch).await.unwrap();
        hello(&node.peer, &node_launch).await.unwrap();
        let rust_ready = publish_ready(&root, rust_launch.identity, rust.peer.clone()).unwrap();
        let node_ready = publish_ready(&root, node_launch.identity, node.peer.clone()).unwrap();
        rust_provider.ready().await.unwrap();
        node_provider.ready().await.unwrap();
        rust.record("constructed", "provider").await;
        node.record("loaded", "node-provider").await;
        for consumer in [&rust_consumer, &node_consumer] {
            assert_eq!(consumer.native().view().state().state, FiberState::Pending);
            assert!(consumer.activation().is_none());
        }
        assert!(root
            .get_as::<rutis_protocol::imports::ObjectProxy>(key.clone())
            .is_none());
        let native_ready = root
            .provide_as(ready_key.clone(), Arc::new("ready".to_owned()))
            .unwrap();
        adapter.ready().await.unwrap();
        rust_consumer.ready().await.unwrap();
        node_consumer.ready().await.unwrap();
        rust.record("constructed", "consumer").await;
        rust.record("exercised", "rust-consumer").await;
        node.record("loaded", "node-consumer").await;
        node.record("exercised", "node-consumer").await;
        let slot = root
            .get_as::<rutis_protocol::imports::ObjectProxy>(key.clone())
            .unwrap();
        let rust_capture = rust_consumer.captured();
        let node_capture = node_consumer.captured();
        assert!(Arc::ptr_eq(&rust_capture[0], &slot));
        assert!(Arc::ptr_eq(&node_capture[0], &slot));
        assert_eq!(&slot.delivery().unwrap().object.owner, adapter.activation());
        let database = adapter
            .service::<InterfaceDatabaseClient>("host-rpc")
            .unwrap();
        let first = database
            .connect(InterfaceDatabaseMethod0Params {
                name: "host-sdk".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            first
                .query(InterfaceConnectionMethod0Params {
                    sql: "native-ctx".into()
                })
                .await
                .unwrap()[0]["sql"],
            "native-ctx"
        );
        assert!(database.inspect(first.clone()).await.unwrap());
        let failed_cleanup = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut missing_ports = NativePorts::default();
        missing_ports
            .provide::<dyn InterfaceDatabaseService, InterfaceDatabaseClient>(
                "host-missing",
                TypeKey::of::<dyn InterfaceDatabaseService>(),
                &bundles,
                exportInterfaceDatabase,
            )
            .unwrap();
        let missing = graph
            .mount_native(
                &root,
                Arc::new(missing_ports),
                Native {
                    role: "provider",
                    keys: vec![],
                    label: "missing".into(),
                    cleanup: Some(failed_cleanup.clone()),
                    remove: None,
                },
            )
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), missing.ready())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Unavailable);
        assert_eq!(failed_cleanup.load(Ordering::SeqCst), 1);
        assert_eq!(missing.native().view().state().state, FiberState::Disposed);
        assert!(root
            .get_as::<rutis_protocol::imports::ObjectProxy>(
                plan.native_key("host-missing").unwrap()
            )
            .is_none());
        assert!(adapter.is_available());
        let old_native = adapter.native().native_context().unwrap();
        let old_rust = rust_consumer.native().native_context().unwrap();
        let old_node = node_consumer.native().native_context().unwrap();
        let removing = remove.lock().unwrap().take().unwrap().dispose();
        removing.await.unwrap();
        assert!(
            !adapter.is_available(),
            "completed native removal closes publication at its admission check"
        );
        assert!(old_native
            .effect(|| panic!("old native owner entered effect"))
            .is_err());
        // The native gate's weak hook seals Host authority and captured
        // consumers synchronously; SDK scope draining is independent.
        assert!(slot.delivery().is_err());
        assert!(!rust_consumer.is_available());
        assert!(!node_consumer.is_available());
        assert!(old_rust
            .effect(|| panic!("native-root loss admitted a Rust Host effect"))
            .is_err());
        assert!(old_node
            .effect(|| panic!("native-root loss admitted a Node Host effect"))
            .is_err());
        adapter.stop().await.unwrap();
        for (consumer, old) in [(&rust_consumer, old_rust), (&node_consumer, old_node)] {
            assert!(!consumer.is_available());
            assert!(old
                .effect(|| panic!("native-route consumer entered effect"))
                .is_err());
            consumer.stop().await.unwrap();
        }
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
        assert!(first
            .query(InterfaceConnectionMethod0Params { sql: "old".into() })
            .await
            .is_err());
        assert!(root
            .get_as::<rutis_protocol::imports::ObjectProxy>(key)
            .is_none());
        assert!(rust_provider.is_available());
        assert!(node_provider.is_available());
        assert!(graph.mount_native(&root, ports, plugin()).is_err());
        graph.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
        drop(native_ready);
        drop(rust_ready);
        drop(node_ready);
        rust.finish().await;
        node.finish().await;
        drop(adapter);
        drop(missing);
        drop(rust_provider);
        drop(node_provider);
        drop(rust_consumer);
        drop(node_consumer);
        drop(graph);
        snapshot.cleanup().unwrap();
    })
    .await
    .unwrap();
}
