//! Real SDK conformance over a private inherited socket. The host owns grants;
//! this fixture deliberately does not substitute for a production manifest runner.
#![cfg(target_os = "linux")]
#[allow(dead_code, unused_variables, non_snake_case)]
mod generated {
    include!("../../../protocol/generated/rpc.rs");
}
use generated::*;
use rutis_protocol::{
    broker::Broker,
    contract::{AdmittedBundle, Method, Ownership, TypeExpr},
    draft::{DraftGraph, DraftSource, GraphExporter, StagedGraph},
    error::{ErrorCode, ProtocolError, Result},
    exports::{Exports, ObjectIds, PinKey},
    frame::{Peer, WeakPeer},
    graph::{DecodedValue, GraphScopes, WireGraph},
    identity::{Activation, Delivery, Scope, Sequence},
    imports::{ImportControl, Imports, ObjectProxy, Retirement},
    managed::ManagedActivation,
    sdk::*,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    os::fd::AsRawFd,
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
};

fn fail(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "conformance", message)
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| fail(e.to_string()))
}
fn activation(runtime: &str) -> Activation {
    Activation {
        runtime: runtime.into(),
        epoch: Sequence(1),
        activation: Sequence(1),
    }
}
fn root(owner: &Activation) -> Scope {
    Scope {
        activation: owner.clone(),
        scope: Sequence(1),
    }
}
fn database_type() -> TypeExpr {
    TypeExpr::Object {
        interface: "Database".into(),
        ownership: Ownership::Scope,
    }
}
struct State {
    broker: Broker,
    calls: BTreeMap<Activation, u64>,
    scopes: BTreeMap<Activation, u64>,
    deliveries: BTreeMap<(Activation, Sequence), Delivery>,
}
struct Host {
    bundle: Arc<AdmittedBundle>,
    state: Mutex<State>,
    peer: Mutex<Option<WeakPeer>>,
    rust: Activation,
    node: Activation,
    exports: Exports,
    imports: Imports,
    exporter: GraphExporter,
    native: rutis::Ctx,
}
enum Stage {
    Local(StagedGraph),
    Remote { stage: String, draft: DraftGraph },
}
impl Stage {
    fn draft(&self) -> &DraftGraph {
        match self {
            Self::Local(s) => &s.draft,
            Self::Remote { draft, .. } => draft,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteCall {
    target: Delivery,
    method: String,
    stage: String,
    draft: DraftGraph,
}
struct LocalCaller(Arc<Host>);
// Keep the execution and its handoff alive independently of the caller waiter.
impl Caller for LocalCaller {
    fn call(
        &self,
        target: ObjectProxy,
        method: String,
        params: Outbound,
    ) -> RpcFuture<DecodedValue> {
        let host = self.0.clone();
        Box::pin(async move {
            let target = target.delivery()?;
            let contract = host.contract(&target.view.interface, &method)?;
            let staged = host.exporter.encode(&contract.params, params)?;
            let (send, receive) = tokio::sync::oneshot::channel();
            let executing = host.clone();
            let output_type = contract.result.clone();
            let recipient = target.recipient.clone();
            tokio::spawn(async move {
                let result = executing
                    .route(executing.rust.clone(), target, method, Stage::Local(staged))
                    .await;
                let result = result.map(|graph| Envelope {
                    host: executing,
                    graph: Some(graph),
                });
                let _ = send.send(result);
            });
            let envelope = receive
                .await
                .map_err(|_| fail("execution task stopped"))??;
            envelope
                .consume(
                    &output_type,
                    &GraphScopes {
                        scope: recipient.clone(),
                        borrow: recipient,
                    },
                )
                .await
        })
    }
}
struct Envelope {
    host: Arc<Host>,
    graph: Option<WireGraph>,
}
impl Envelope {
    async fn consume(mut self, expr: &TypeExpr, scopes: &GraphScopes) -> Result<DecodedValue> {
        let result = self.host.imports.receive_graph(
            &self.host.bundle,
            expr,
            self.graph.take().unwrap(),
            scopes,
        );
        self.host.flush_local().await?;
        result
    }
}
impl Drop for Envelope {
    fn drop(&mut self) {
        if let Some(graph) = self.graph.take() {
            self.host.imports.reject(
                &graph
                    .references
                    .into_iter()
                    .map(|r| r.delivery)
                    .collect::<Vec<_>>(),
            );
            let host = self.host.clone();
            tokio::spawn(async move {
                host.flush_local().await.unwrap();
            });
        }
    }
}
impl Host {
    fn peer(&self) -> Result<Peer> {
        self.peer
            .lock()
            .unwrap()
            .as_ref()
            .and_then(WeakPeer::upgrade)
            .ok_or_else(|| fail("private peer closed"))
    }
    fn caller(self: &Arc<Self>) -> Arc<dyn Caller> {
        Arc::new(LocalCaller(self.clone()))
    }
    fn contract(&self, iface: &str, selector: &str) -> Result<Method> {
        if iface.starts_with("$callback:") {
            let callback =
                &self.bundle.bundle().interfaces["Database"].methods["withCallback"].params;
            if let TypeExpr::Callback { params, result, .. } = callback {
                if rutis_protocol::contract::callback_key(callback) == iface && selector == "call" {
                    return Ok(Method {
                        params: *params.clone(),
                        result: *result.clone(),
                    });
                }
            }
        }
        self.bundle
            .bundle()
            .interfaces
            .get(iface)
            .and_then(|i| i.methods.get(selector))
            .cloned()
            .ok_or_else(|| fail("unknown selector"))
    }
    async fn pin(
        &self,
        object: &rutis_protocol::identity::ObjectIdentity,
        key: PinKey,
    ) -> Result<()> {
        if object.owner == self.rust {
            self.exports.pin(object, key)
        } else {
            self.peer()?
                .request("pin", json!({"object": object, "key": key}))
                .await
                .map(|_| ())
        }
    }
    async fn controls(&self, sender: &Activation, controls: Vec<ImportControl>) -> Result<()> {
        for control in controls {
            let released = {
                let mut state = self.state.lock().unwrap();
                match control {
                    ImportControl::Accept { id, token } => {
                        state.broker.accept(sender, id, &token)?;
                        None
                    }
                    ImportControl::Release { id, token } => {
                        state.broker.release(sender, id, &token)?;
                        state.deliveries.get(&(sender.clone(), id)).map(|d| {
                            (
                                d.object.owner.clone(),
                                PinKey::Delivery {
                                    recipient: sender.clone(),
                                    id,
                                },
                            )
                        })
                    }
                }
            };
            if let Some((owner, key)) = released {
                if owner == self.rust {
                    self.exports.release(&key);
                } else {
                    self.peer()?.request("release", json!({"key": key})).await?;
                }
            }
        }
        Ok(())
    }
    async fn flush_local(&self) -> Result<()> {
        self.controls(&self.rust, self.imports.take_controls())
            .await
    }
    async fn retire(&self, sender: &Activation, proposal: Retirement) -> Result<Value> {
        self.state.lock().unwrap().broker.retire(
            sender,
            proposal.received_through,
            proposal.terminal_through,
        )?;
        self.exports
            .retire_deliveries(&sender.runtime, sender.epoch, proposal.terminal_through)?;
        self.peer()?.request("retire-owner", json!({"runtime": sender.runtime, "epoch": sender.epoch, "through": proposal.terminal_through})).await?;
        Ok(json!({"terminal_through": proposal.terminal_through}))
    }
    async fn remote_controls(&self, value: &Value) -> Result<()> {
        self.controls(&self.node, decode(value["controls"].clone())?)
            .await
    }
    async fn handoff(
        &self,
        sender: &Activation,
        expr: &TypeExpr,
        stage: &mut Stage,
        scopes: &GraphScopes,
    ) -> Result<WireGraph> {
        let graph = {
            let mut state = self.state.lock().unwrap();
            let graph = state.broker.offer_graph(
                sender,
                &self.bundle,
                expr,
                stage.draft(),
                scopes,
                &self.bundle.bundle().id,
            )?;
            for reference in &graph.references {
                state.deliveries.insert(
                    (
                        reference.delivery.recipient.activation.clone(),
                        reference.delivery.id,
                    ),
                    reference.delivery.clone(),
                );
            }
            graph
        };
        let deliveries: Vec<_> = graph
            .references
            .iter()
            .map(|r| r.delivery.clone())
            .collect();
        // The author cannot mint grants. Its staged natives must acknowledge
        // the broker's grants before the recipient can see this graph.
        let admitted = async {
            match &mut *stage {
                Stage::Local(staged) => {
                    staged.commit(deliveries.clone(), scopes)?;
                }
                Stage::Remote { stage, .. } => {
                    self.peer()?
                        .request(
                            "commit",
                            json!({"stage": stage, "deliveries": deliveries, "scopes": scopes}),
                        )
                        .await?;
                }
            }
            for (reference, delivery) in stage.draft().references.iter().zip(&deliveries) {
                if matches!(reference.source, DraftSource::Foreign { .. }) {
                    self.pin(
                        &delivery.object,
                        PinKey::Delivery {
                            recipient: delivery.recipient.activation.clone(),
                            id: delivery.id,
                        },
                    )
                    .await?;
                }
            }
            Ok::<_, ProtocolError>(())
        }
        .await;
        if let Err(error) = admitted {
            if scopes.scope.activation == self.rust {
                self.imports.reject(&deliveries);
                self.flush_local().await?;
            } else {
                let response = self
                    .peer()?
                    .request("reject", json!({"deliveries": deliveries}))
                    .await?;
                self.remote_controls(&response).await?;
            }
            if let Stage::Remote { stage, .. } = stage {
                self.peer()?
                    .request("abort", json!({"stage": stage}))
                    .await?;
            }
            return Err(error);
        }
        Ok(graph)
    }
    async fn route(
        self: &Arc<Self>,
        sender: Activation,
        target: Delivery,
        method: String,
        mut input: Stage,
    ) -> Result<WireGraph> {
        if target.recipient.activation != sender {
            return Err(fail("target is not held by authenticated sender"));
        }
        let contract = self.contract(&target.view.interface, &method)?;
        let owner = target.object.owner.clone();
        // Allocate and enqueue scope/pin admission while holding this brief
        // metadata lock. Async dispatch never holds it, including reentry.
        let (call, scopes, key, remote_open, remote_pin) = {
            let mut state = self.state.lock().unwrap();
            let counter = state.calls.entry(sender.clone()).or_default();
            *counter += 1;
            let call = Sequence(*counter);
            state
                .broker
                .begin_call(&target.recipient, call, &target, &method, &owner)?;
            let counter = state.scopes.entry(owner.clone()).or_insert(1);
            *counter += 1;
            let scope = Scope {
                activation: owner.clone(),
                scope: Sequence(*counter),
            };
            state.broker.open_scope(scope.clone(), Some(root(&owner)))?;
            let key = PinKey::Execution {
                caller: sender.clone(),
                call,
            };
            let (open, pin) = if owner == self.rust {
                self.imports.open_scope(scope.clone(), Some(root(&owner)))?;
                self.exports.pin(&target.object, key.clone())?;
                (None, None)
            } else {
                let peer = self.peer()?;
                (
                    Some(peer.start_request("open", json!({"scope": scope}))?),
                    Some(peer.start_request("pin", json!({"object": target.object, "key": key}))?),
                )
            };
            (
                call,
                GraphScopes {
                    scope: root(&owner),
                    borrow: scope,
                },
                key,
                open,
                pin,
            )
        };
        let mut result_stage = None;
        let result = async {
            if let Some(open) = remote_open { open.await.map_err(|_| fail("scope admission lost"))??; }
            if let Some(pin) = remote_pin { pin.await.map_err(|_| fail("execution admission lost"))??; }
            let graph = self.handoff(&sender, &contract.params, &mut input, &scopes).await?;
            let mut output = if owner == self.rust {
                let params = self.imports.receive_graph(&self.bundle, &contract.params, graph, &scopes)?;
                self.flush_local().await?;
                let context = CallContext::new(self.caller(), Some(self.native.clone()));
                let dispatched = self.exporter.dispatcher(&target.object, &target.view)?.dispatch(key.clone(), context.clone(), method, params).await;
                let children = context.finish().await;
                let result = dispatched?; children?;
                Stage::Local(self.exporter.encode(&contract.result, result)?)
            } else {
                let response = self.peer()?.request("execute", json!({"object": target.object, "view": target.view, "method": method, "key": key, "graph": graph, "scopes": scopes})).await?;
                self.remote_controls(&response).await?;
                if response.get("error").is_some() { return Err(decode(response["error"].clone())?); }
                let stage: String = decode(response["stage"].clone())?; result_stage = Some(stage.clone());
                Stage::Remote { stage, draft: decode(response["draft"].clone())? }
            };
            let recipient = target.recipient.clone();
            self.handoff(&owner, &contract.result, &mut output, &GraphScopes { scope: recipient.clone(), borrow: recipient }).await
        }.await;
        // This point follows actual handler + registered descendants, and
        // keeps borrowed parameter proofs alive through the result handoff.
        if owner == self.rust {
            self.imports.close_scope(&scopes.borrow);
            self.exports.release(&key);
            self.flush_local().await?;
        } else {
            let response = self
                .peer()?
                .request(
                    "end",
                    json!({"scope": scopes.borrow, "key": key, "stage": result_stage}),
                )
                .await?;
            self.remote_controls(&response).await?;
        }
        {
            let mut state = self.state.lock().unwrap();
            state.broker.close_scope(&scopes.borrow);
            state.broker.finish_call(&owner, &sender, call)?;
        }
        result
    }
    async fn incoming(self: Arc<Self>, method: String, value: Value) -> Result<Value> {
        match method.as_str() {
            "controls" => {
                self.remote_controls(&value).await?;
                Ok(Value::Null)
            }
            "retirement" => self.retire(&self.node, decode(value)?).await,
            "call" => {
                let request: RemoteCall = decode(value)?;
                let result = self
                    .route(
                        self.node.clone(),
                        request.target,
                        request.method,
                        Stage::Remote {
                            stage: request.stage,
                            draft: request.draft,
                        },
                    )
                    .await;
                Ok(match result {
                    Ok(graph) => json!({"graph": graph}),
                    Err(error) => json!({"error": error}),
                })
            }
            _ => Err(fail("unknown runtime request")),
        }
    }
}

struct Probe(Arc<Mutex<Option<rutis::Ctx>>>);
impl rutis::Plugin for Probe {
    fn name(&self) -> &str {
        "ipc-native"
    }
    fn injects(&self) -> &[rutis::TypeKey] {
        static KEYS: std::sync::OnceLock<Vec<rutis::TypeKey>> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| vec![rutis::TypeKey::of::<u8>()])
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(ctx.clone());
            Ok(rutis::Effect::Done)
        })
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
    count: AtomicUsize,
}
fn native(context: &CallContext) {
    assert_eq!(*context.native().unwrap().get::<u8>().unwrap(), 7);
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
        native(&context);
        let count = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            Ok(vec![BTreeMap::from([
                ("count".into(), json!(count)),
                ("sql".into(), json!(params.sql)),
                ("owner".into(), json!("rust")),
            ])])
        })
    }
}
struct Database(Arc<Connection>, Mutex<Option<BorrowCallback0Client>>);
impl InterfaceDatabaseService for Database {
    fn connect(
        &self,
        context: CallContext,
        _: InterfaceDatabaseMethod0Params,
    ) -> RpcFuture<Arc<dyn InterfaceConnectionService>> {
        native(&context);
        let value: Arc<dyn InterfaceConnectionService> = self.0.clone();
        Box::pin(async move { Ok(value) })
    }
    fn inspect(&self, context: CallContext, params: InterfaceConnectionClient) -> RpcFuture<bool> {
        native(&context);
        Box::pin(async move {
            Ok(params
                .query(InterfaceConnectionMethod0Params {
                    sql: "passback-rust".into(),
                })
                .await?[0]["owner"]
                == "rust")
        })
    }
    fn withCallback(&self, context: CallContext, callback: BorrowCallback0Client) -> RpcFuture<()> {
        native(&context);
        *self.1.lock().unwrap() = Some(callback.clone());
        Box::pin(async move {
            callback.call("rust-callback".into()).await?;
            context.spawn(async move { callback.call("rust-child".into()).await })?;
            Ok(())
        })
    }
}
struct Callback {
    database: InterfaceDatabaseClient,
    count: Arc<AtomicUsize>,
}
impl BorrowCallback0Service for Callback {
    fn call(&self, context: CallContext, text: String) -> RpcFuture<()> {
        native(&context);
        self.count.fetch_add(1, Ordering::SeqCst);
        let client = self.database.clone();
        Box::pin(async move {
            client
                .connect(InterfaceDatabaseMethod0Params { name: text })
                .await?
                .query(InterfaceConnectionMethod0Params {
                    sql: "reentrant-rust".into(),
                })
                .await?;
            Ok(())
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_node_peer_dispatches_native_objects_cycles_passback_and_reentrant_callbacks() {
    tokio::time::timeout(std::time::Duration::from_secs(30), scenario())
        .await
        .expect("private object conformance timed out");
}
async fn scenario() {
    let bundle = Arc::new(
        AdmittedBundle::parse(include_bytes!("../../../protocol/fixtures/rpc.bundle.json"))
            .unwrap(),
    );
    let native_root = rutis::Ctx::root().unwrap();
    let _dependency = native_root.provide(7_u8).unwrap();
    let seen: Arc<Mutex<Option<rutis::Ctx>>> = Arc::default();
    let managed = ManagedActivation::mount(&native_root, Probe(seen.clone())).unwrap();
    managed.view().await.unwrap();
    let native = seen.lock().unwrap().clone().unwrap();
    let rust = activation("rust");
    let node = activation("node");
    let exports =
        Exports::managed(&native, rust.clone(), ObjectIds::default(), managed.gate()).unwrap();
    let imports = Imports::default();
    imports.open_scope(root(&rust), None).unwrap();
    imports.bind_scope(&root(&rust), managed.gate()).unwrap();
    let mut broker = Broker::default();
    for owner in [&rust, &node] {
        broker.start_activation(owner.clone()).unwrap();
        broker.open_scope(root(owner), None).unwrap();
    }
    let host = Arc::new(Host {
        exporter: GraphExporter::new(bundle.clone(), exports.clone(), bundle.bundle().id.clone()),
        bundle,
        state: Mutex::new(State {
            broker,
            calls: BTreeMap::new(),
            scopes: BTreeMap::new(),
            deliveries: BTreeMap::new(),
        }),
        peer: Mutex::default(),
        rust,
        node,
        exports,
        imports,
        native,
    });
    let (parent, child_socket) = tokio::net::UnixStream::pair().unwrap();
    let child_socket = child_socket.into_std().unwrap();
    let fd = child_socket.as_raw_fd();
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = tokio::process::Command::new("node");
    command
        .current_dir(&workspace)
        .args([
            "--import",
            "./protocol/ts/node_modules/tsx/dist/loader.mjs",
            "protocol/ts/tests/fixtures/object-peer.ts",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Only this inherited socket carries frames. Child diagnostics retain
    // their ordinary pipes and no public socket pathname is created.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    drop(child_socket);
    let weak = Arc::downgrade(&host);
    let peer = Peer::start(
        parent,
        Arc::new(move |method, value| {
            let host = weak.upgrade().unwrap();
            Box::pin(host.incoming(method, value))
        }),
    );
    *host.peer.lock().unwrap() = Some(peer.downgrade());
    let hello = match peer.request("hello", Value::Null).await {
        Ok(value) => value,
        Err(error) => {
            let output = child.wait_with_output().await.unwrap();
            panic!(
                "hello failed: {error}; Node stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    assert_eq!(hello["bundle"], host.bundle.sha256());
    assert_eq!(hello["family"], "rutis-cordis-objects");
    assert_eq!(hello["version"], "0.experimental");
    assert_eq!(
        hello["capabilities"],
        json!(["object.scope", "callback.borrow"])
    );
    let session = Arc::new_cyclic(|session| Session {
        agent: Arc::new(Agent {
            session: session.clone(),
        }),
    });
    let database = Arc::new(Database(
        Arc::new(Connection {
            session,
            count: AtomicUsize::new(0),
        }),
        Mutex::default(),
    ));
    let mut staged = Stage::Local(
        host.exporter
            .encode(&database_type(), exportInterfaceDatabase(database.clone()))
            .unwrap(),
    );
    let graph = host
        .handoff(
            &host.rust,
            &database_type(),
            &mut staged,
            &GraphScopes {
                scope: root(&host.node),
                borrow: root(&host.node),
            },
        )
        .await
        .unwrap();
    let response = peer
        .request("attach", json!({"graph": graph}))
        .await
        .unwrap();
    host.remote_controls(&response).await.unwrap();
    let response = peer.request("activate", Value::Null).await.unwrap();
    let mut staged = Stage::Remote {
        stage: decode(response["stage"].clone()).unwrap(),
        draft: decode(response["draft"].clone()).unwrap(),
    };
    let scopes = GraphScopes {
        scope: root(&host.rust),
        borrow: root(&host.rust),
    };
    let graph = host
        .handoff(&host.node, &database_type(), &mut staged, &scopes)
        .await
        .unwrap();
    let client: InterfaceDatabaseClient = bind(
        host.imports
            .receive_graph(&host.bundle, &database_type(), graph, &scopes)
            .unwrap(),
        host.caller(),
    )
    .unwrap();
    host.flush_local().await.unwrap();
    let first = client
        .connect(InterfaceDatabaseMethod0Params {
            name: "from-rust".into(),
        })
        .await
        .unwrap();
    let second = client
        .connect(InterfaceDatabaseMethod0Params {
            name: "from-rust".into(),
        })
        .await
        .unwrap();
    assert!(same_wrapper(&first, &second));
    let session = first.session().unwrap();
    assert!(same_wrapper(
        &session,
        &session.agent().unwrap().session().unwrap()
    ));
    let rows = first
        .query(InterfaceConnectionMethod0Params {
            sql: "rust-to-node".into(),
        })
        .await
        .unwrap();
    assert_eq!(rows[0]["owner"], "node");
    assert_eq!(rows[0]["count"], 1);
    assert!(client.inspect(first.clone()).await.unwrap());
    let callbacks: Arc<AtomicUsize> = Arc::default();
    client
        .withCallback(Arc::new(Callback {
            database: client.clone(),
            count: callbacks.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(callbacks.load(Ordering::SeqCst), 2);
    assert_eq!(
        peer.request("expired", Value::Null).await.unwrap_err().code,
        ErrorCode::ScopeClosed
    );
    let response = peer.request("scenario", Value::Null).await.unwrap();
    assert_eq!(response["identity"], true);
    assert_eq!(response["cycle"], true);
    assert_eq!(response["inspected"], true);
    assert_eq!(response["callbacks"], 2);
    assert_eq!(response["rows"][0]["owner"], "rust");
    assert_eq!(database.0.count.load(Ordering::SeqCst), 4);
    let expired = database.1.lock().unwrap().clone().unwrap();
    assert_eq!(
        expired.call("expired".into()).await.unwrap_err().code,
        ErrorCode::ScopeClosed
    );
    let target = first.client().proxy().delivery().unwrap().object;
    let slow = first.clone();
    let waiter = tokio::spawn(async move {
        slow.query(InterfaceConnectionMethod0Params { sql: "slow".into() })
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if peer.request("slow-status", Value::Null).await.unwrap() == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(
        host.state.lock().unwrap().broker.pins(&target).1,
        1,
        "dropping a Rust waiter must retain the actual Node execution"
    );
    peer.request("resume", Value::Null).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if host.state.lock().unwrap().broker.pins(&target).1 == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        host.imports.retirement().is_none(),
        "a live first grant blocks retirement of later released borrows"
    );
    assert_eq!(
        peer.request("retire", Value::Null).await.unwrap_err().code,
        ErrorCode::InvalidParams
    );
    let response = peer.request("stop", Value::Null).await.unwrap();
    host.remote_controls(&response).await.unwrap();
    let node_prefix: Retirement =
        decode(peer.request("retire", Value::Null).await.unwrap()).unwrap();
    host.imports.close_scope(&root(&host.rust));
    host.flush_local().await.unwrap();
    let rust_prefix = host.imports.retirement().unwrap();
    let confirmed = host.retire(&host.rust, rust_prefix.clone()).await.unwrap();
    assert_eq!(
        confirmed["terminal_through"],
        json!(rust_prefix.terminal_through)
    );
    host.imports
        .acknowledge_retirement(rust_prefix.terminal_through)
        .unwrap();
    // Low-watermark replays cannot reopen the receiver or owner pins.
    let old_node_delivery = host
        .state
        .lock()
        .unwrap()
        .deliveries
        .values()
        .find(|d| d.recipient.activation == host.node)
        .unwrap()
        .clone();
    host.state
        .lock()
        .unwrap()
        .broker
        .accept(&host.node, old_node_delivery.id, &old_node_delivery.token)
        .unwrap();
    assert!(node_prefix.terminal_through >= old_node_delivery.id);
    assert!(host
        .exports
        .pin(
            &old_node_delivery.object,
            PinKey::Delivery {
                recipient: host.node.clone(),
                id: old_node_delivery.id
            }
        )
        .is_err());
    let old_rust_delivery = host
        .state
        .lock()
        .unwrap()
        .deliveries
        .values()
        .find(|d| d.recipient.activation == host.rust)
        .unwrap()
        .clone();
    assert!(host.imports.receive(old_rust_delivery).is_err());
    managed.stop().await.unwrap();
    native_root.shutdown().await.unwrap();
    assert_eq!(host.imports.retained_objects(), 0);
    assert!(first.session().is_err());
    {
        let mut state = host.state.lock().unwrap();
        state.broker.close_epoch("node", Sequence(1));
        state.broker.close_epoch("rust", Sequence(1));
        for delivery in state.deliveries.values() {
            assert_eq!(state.broker.pins(&delivery.object), (0, 0));
        }
    }
    peer.close(ProtocolError::new(
        ErrorCode::Unavailable,
        "test",
        "conformance complete",
    ));
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "Node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("diagnostics use stdout"));
}
