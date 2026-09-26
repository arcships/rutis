//! Host-owned runtime/member identities and a publication barrier around native
//! fibers. This controls protocol intent; the driver executes native lifecycle.
#[cfg(target_os = "linux")]
use crate::prepare::PreparedDeployment;
use crate::{
    contract::{identifier, FAMILY, VERSION},
    error::{ErrorCode, ProtocolError, Result},
    frame::{HandlerFuture, Peer},
    identity::{Activation, Sequence},
    managed::{ActivationGate, ManagedActivation},
    prepare::RuntimeKind,
    runner_image::{CatalogService, FactoryCatalog},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub runtime: String,
    pub epoch: Sequence,
    pub kind: RuntimeKind,
    pub framework_version: String,
    pub environment_sha256: String,
    pub code_sha256: String,
    pub capabilities: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entry {
    Rust { factory: String },
    Node { entry: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub entry: Entry,
    pub config: Value,
    pub config_schema: String,
    pub contracts: FactoryCatalog,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol_family: String,
    pub protocol_version: String,
    pub identity: RuntimeIdentity,
    pub members: BTreeMap<String, Member>,
}
/// Pure Node launch metadata, generated from the prepared group into its
/// private snapshot. Configuration travels only on the private hello frame.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCatalog {
    pub protocol_family: String,
    pub protocol_version: String,
    pub framework_version: String,
    pub environment_sha256: String,
    pub code_sha256: String,
    pub modules: BTreeMap<String, FactoryCatalog>,
}
pub(crate) fn member_contracts(
    package: &crate::prepare::PreparedPackage,
) -> Result<FactoryCatalog> {
    let services = |values: &BTreeMap<String, crate::prepare::PreparedService>| {
        values
            .iter()
            .map(|(name, service)| {
                (
                    name.clone(),
                    CatalogService {
                        interface: service.interface.clone(),
                        version: service.version.clone(),
                        bundle_sha256: service.bundle_sha256.clone(),
                    },
                )
            })
            .collect()
    };
    Ok(FactoryCatalog {
        config_sha256: package
            .file(&package.manifest().config_schema)?
            .sha256()
            .into(),
        provides: services(package.provides()),
        requires: services(package.requires()),
    })
}
impl Hello {
    #[cfg(target_os = "linux")]
    pub fn prepared(
        plan: &PreparedDeployment,
        group: &str,
        snapshot: &crate::snapshot::SnapshotGroup,
        runtime: String,
        epoch: Sequence,
    ) -> Result<Self> {
        let prepared = plan
            .groups()
            .get(group)
            .ok_or_else(|| invalid("unknown group"))?;
        if !identifier(&runtime)
            || epoch.0 == 0
            || snapshot.code_sha256() != prepared.code_sha256()
            || snapshot.members().keys().ne(prepared.members().iter())
        {
            return Err(invalid("runtime identity or snapshot members differ"));
        }
        let mut members = BTreeMap::new();
        for name in prepared.members() {
            let instance = &plan.instances()[name];
            let package = instance.package();
            let entry = match &package.manifest().plugin {
                crate::prepare::PluginEntry::Rust { factory } => Entry::Rust {
                    factory: factory.clone(),
                },
                crate::prepare::PluginEntry::Node { .. } => Entry::Node {
                    entry: snapshot.members()[name]
                        .entry()
                        .ok_or_else(|| invalid("Node entry missing"))?
                        .to_str()
                        .ok_or_else(|| invalid("Node entry is not UTF-8"))?
                        .into(),
                },
            };
            members.insert(
                name.clone(),
                Member {
                    entry,
                    config: instance.config().clone(),
                    config_schema: String::from_utf8(
                        package
                            .file(&package.manifest().config_schema)?
                            .bytes()
                            .to_vec(),
                    )
                    .map_err(|_| invalid("config schema is not UTF-8"))?,
                    contracts: member_contracts(package)?,
                },
            );
        }
        Ok(Self {
            protocol_family: FAMILY.into(),
            protocol_version: VERSION.into(),
            identity: RuntimeIdentity {
                runtime,
                epoch,
                kind: prepared.kind().clone(),
                framework_version: prepared.framework_version().into(),
                environment_sha256: prepared.environment_sha256().into(),
                code_sha256: prepared.code_sha256().into(),
                capabilities: prepared.capabilities().clone(),
            },
            members,
        })
    }
    fn validate(&self) -> Result<()> {
        if self.protocol_family != FAMILY || self.protocol_version != VERSION {
            return Err(ProtocolError::new(
                ErrorCode::InterfaceMismatch,
                "hello",
                "protocol identity differs",
            ));
        }
        if !identifier(&self.identity.runtime)
            || self.identity.epoch.0 == 0
            || self.members.is_empty()
            || !crate::prepare::version(&self.identity.framework_version)
            || !crate::prepare::sha256_field(&self.identity.code_sha256)
            || !crate::prepare::sha256_field(&self.identity.environment_sha256)
            || self.members.keys().any(|name| !identifier(name))
        {
            return Err(invalid("invalid runtime/member identity"));
        }
        for member in self.members.values() {
            match &member.entry {
                Entry::Rust { factory } if identifier(factory) => {}
                Entry::Node { entry } if std::path::Path::new(entry).is_absolute() => {}
                _ => return Err(invalid("invalid member entry")),
            }
            if !crate::prepare::sha256_field(&member.contracts.config_sha256) {
                return Err(invalid("invalid config schema digest"));
            }
            for (name, service) in member
                .contracts
                .provides
                .iter()
                .chain(&member.contracts.requires)
            {
                if !identifier(name)
                    || !identifier(&service.interface)
                    || !crate::prepare::version(&service.version)
                    || !crate::prepare::sha256_field(&service.bundle_sha256)
                {
                    return Err(invalid("invalid named service contract"));
                }
            }
            if crate::prepare::digest(member.config_schema.as_bytes())
                != member.contracts.config_sha256
            {
                return Err(ProtocolError::new(
                    ErrorCode::InterfaceMismatch,
                    "hello",
                    "config schema raw digest differs",
                ));
            }
            let schema = crate::json::decode(member.config_schema.as_bytes())?;
            crate::contract::validate_json(&schema, &member.config)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Staged,
    Published,
    Closing,
    Stopped,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub instance: String,
    pub activation: Activation,
    pub phase: Phase,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    instance: String,
    activation: Activation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Select {
    activation: Activation,
}

/// Implementations validate declarations without importing business code.
/// Mount returns a real native activation, plus its staged named service table.
/// Mount must forward admission to a gated native mounter. Object/event routing
/// must check `Runner::require_published` before admission.
pub trait Driver: Send + Sync + 'static {
    fn admit(&self, hello: &Hello) -> Result<()>;
    fn mount(&self, request: MountRequest, admission: ActivationGate) -> Result<Mounted>;
}
/// Identity comes from reserved host intent, never from plugin configuration.
/// Service adapters use it to bind native tables and object owner scopes.
pub struct MountRequest {
    pub instance: String,
    pub activation: Activation,
    pub member: Member,
}
pub type ServiceFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<BTreeMap<String, Value>>> + Send>>;
pub struct Mounted {
    pub native: ManagedActivation,
    pub services: ServiceFuture,
}
type Completion = std::result::Result<Value, ProtocolError>;
struct Slot {
    admission: ActivationGate,
    status: Status,
    native: Option<Arc<ManagedActivation>>,
    completion: watch::Receiver<Option<Completion>>,
    stop: Option<watch::Receiver<Option<Completion>>>,
}
#[derive(Default)]
struct State {
    hello: Option<Hello>,
    closed: bool,
    slots: BTreeMap<Activation, Slot>,
    current: BTreeMap<String, Activation>,
    close_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}
pub struct Runner {
    driver: Arc<dyn Driver>,
    state: Mutex<State>,
    handle: tokio::runtime::Handle,
}
impl Runner {
    pub fn new(driver: impl Driver) -> Arc<Self> {
        Arc::new(Self {
            driver: Arc::new(driver),
            state: Mutex::default(),
            handle: tokio::runtime::Handle::current(),
        })
    }
    pub fn handler(self: &Arc<Self>) -> crate::frame::Handler {
        let runner = self.clone();
        Arc::new(move |method, params| runner.handle(&method, params))
    }
    /// Admission and stop intent happen synchronously in stream order. No
    /// management lock spans native code, a peer request or a cleanup await.
    pub fn handle(self: &Arc<Self>, method: &str, params: Value) -> HandlerFuture {
        let result = match method {
            "runtime/hello" => self.hello(params),
            "plugin/start" => self.start(params),
            "plugin/activate" => self.activate(params),
            "plugin/stop" => self.stop(params),
            "plugin/state" => self.status(params),
            "runtime/stop" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Empty {}
                decode::<Empty>(params).map(|_| {
                    let stops = self.close();
                    Box::pin(async move {
                        let mut errors = Vec::new();
                        for stop in stops {
                            if let Err(error) = stop.await {
                                errors.push(error);
                            }
                        }
                        if errors.is_empty() {
                            Ok(json!({"stopped":true}))
                        } else {
                            Err(ProtocolError::new(
                                ErrorCode::Business,
                                "runtime_stop",
                                format!("{} member cleanup failures: {}", errors.len(), errors[0]),
                            ))
                        }
                    }) as HandlerFuture
                })
            }
            _ => Err(ProtocolError::new(
                ErrorCode::UnsupportedCapability,
                "lifecycle",
                "unknown lifecycle method",
            )),
        };
        match result {
            Ok(future) => future,
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    fn hello(&self, params: Value) -> Result<HandlerFuture> {
        check_capabilities(&params)?;
        let hello: Hello = decode(params)?;
        hello.validate()?;
        // Only this short admission section calls declarative driver checking.
        // No member constructor or native plugin may run here.
        let mut state = self.state.lock().unwrap();
        if state.closed || state.hello.is_some() {
            return Err(unavailable("runtime cannot hello twice"));
        }
        self.driver.admit(&hello)?;
        let identity = hello.identity.clone();
        state.hello = Some(hello);
        Ok(Box::pin(async move {
            Ok(json!({"protocol_family":FAMILY,"protocol_version":VERSION,"identity":identity}))
        }))
    }
    fn start(self: &Arc<Self>, params: Value) -> Result<HandlerFuture> {
        let request: Start = decode(params)?;
        let (tx, rx) = watch::channel(None);
        let member = {
            let mut state = self.state.lock().unwrap();
            let hello = state
                .hello
                .as_ref()
                .ok_or_else(|| unavailable("runtime is not ready"))?;
            if state.closed
                || request.activation.runtime != hello.identity.runtime
                || request.activation.epoch != hello.identity.epoch
                || state.slots.contains_key(&request.activation)
            {
                return Err(unavailable("activation is stale or runtime is closing"));
            }
            let member = hello
                .members
                .get(&request.instance)
                .cloned()
                .ok_or_else(|| invalid("unknown member"))?;
            if let Some(current) = state.current.get(&request.instance) {
                if request.activation.activation <= current.activation
                    || state.slots[current].status.phase != Phase::Stopped
                {
                    return Err(unavailable("previous activation cleanup is not confirmed"));
                }
            }
            state
                .current
                .insert(request.instance.clone(), request.activation.clone());
            state.slots.insert(
                request.activation.clone(),
                Slot {
                    admission: ActivationGate::default(),
                    status: Status {
                        instance: request.instance,
                        activation: request.activation.clone(),
                        phase: Phase::Starting,
                    },
                    native: None,
                    completion: rx.clone(),
                    stop: None,
                },
            );
            member
        };
        let runner = self.clone();
        let activation = request.activation;
        self.handle.spawn(async move {
            let executing = runner.clone();
            let selected = activation.clone();
            let result = tokio::spawn(async move { executing.mount(selected, member).await })
                .await
                .unwrap_or_else(|_| {
                    let mut error = ProtocolError::new(
                        ErrorCode::Business,
                        "start",
                        "native mount/staging panicked",
                    );
                    error.execution = crate::error::Execution::Unknown;
                    Err(error)
                });
            if result.is_err() {
                let native = runner.state.lock().unwrap().slots[&activation]
                    .native
                    .clone();
                let result = if let Some(native) = native {
                    native.gate().close();
                    match native.stop().await {
                        Ok(()) => result,
                        Err(error) => Err(cleanup_error(error)),
                    }
                } else {
                    result
                };
                let mut state = runner.state.lock().unwrap();
                let slot = state.slots.get_mut(&activation).unwrap();
                if slot.status.phase != Phase::Closing {
                    slot.status.phase = Phase::Failed;
                }
                tx.send_replace(Some(result));
                return;
            }
            tx.send_replace(Some(result));
        });
        Ok(wait(rx))
    }
    async fn mount(self: &Arc<Self>, activation: Activation, member: Member) -> Completion {
        if self.state.lock().unwrap().slots[&activation].status.phase == Phase::Closing {
            return Err(ProtocolError::new(
                ErrorCode::Cancelled,
                "start",
                "stop preceded native mount",
            ));
        }
        let (admission, instance) = {
            let state = self.state.lock().unwrap();
            let slot = &state.slots[&activation];
            (slot.admission.clone(), slot.status.instance.clone())
        };
        let mounted = self.driver.mount(
            MountRequest {
                instance,
                activation: activation.clone(),
                member: member.clone(),
            },
            admission.clone(),
        )?;
        if !admission.same_activation(&mounted.native.gate()) {
            mounted.native.stop().await.map_err(cleanup_error)?;
            return Err(unavailable(
                "driver did not use the host activation admission gate",
            ));
        }
        let native = Arc::new(mounted.native);
        let closing = {
            let mut state = self.state.lock().unwrap();
            let slot = state.slots.get_mut(&activation).unwrap();
            slot.native = Some(native.clone());
            slot.status.phase == Phase::Closing
        };
        let revoked = native.gate();
        let runner = Arc::downgrade(self);
        let invalidated = activation.clone();
        self.handle.spawn(async move {
            revoked.revoked().await;
            if let Some(runner) = runner.upgrade() {
                let _ = runner.stop_activation(&invalidated);
            }
        });
        if closing {
            native.stop().await.map_err(cleanup_error)?;
            return Err(unavailable("activation was stopped while mounting"));
        }
        if let Err(error) = native.view().await {
            native.stop().await.map_err(cleanup_error)?;
            return Err(ProtocolError::new(
                ErrorCode::Business,
                "start",
                error.to_string(),
            ));
        }
        if native.view().state().state != rutis::FiberState::Active || !native.gate().is_open() {
            native.stop().await.map_err(cleanup_error)?;
            return Err(unavailable("native member did not become Active"));
        }
        let gate = native.gate();
        let services = tokio::select! {
            result = mounted.services => result?,
            _ = gate.revoked() => return Err(unavailable("native member revoked before staging")),
        };
        if services.keys().ne(member.contracts.provides.keys()) {
            native.stop().await.map_err(cleanup_error)?;
            return Err(ProtocolError::new(
                ErrorCode::InterfaceMismatch,
                "ready",
                "named service table differs",
            ));
        }
        {
            let mut state = self.state.lock().unwrap();
            let slot = state.slots.get_mut(&activation).unwrap();
            if slot.status.phase == Phase::Closing {
                return Err(unavailable("activation was stopped before ready"));
            }
            slot.status.phase = Phase::Staged;
        }
        Ok(json!({"activation":activation,"services":services}))
    }
    fn activate(&self, params: Value) -> Result<HandlerFuture> {
        let request: Select = decode(params)?;
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(unavailable("runtime is closing"));
        }
        let slot = state
            .slots
            .get_mut(&request.activation)
            .ok_or_else(|| unavailable("unknown activation"))?;
        if !matches!(slot.status.phase, Phase::Staged | Phase::Published)
            || !slot.native.as_ref().is_some_and(|n| n.gate().is_open())
        {
            return Err(unavailable(
                "activation is not staged or native gate closed",
            ));
        }
        slot.status.phase = Phase::Published;
        let status = slot.status.clone();
        Ok(Box::pin(async move {
            Ok(serde_json::to_value(status).unwrap())
        }))
    }
    pub fn require_published(&self, activation: &Activation) -> Result<()> {
        let state = self.state.lock().unwrap();
        if state.closed {
            return Err(unavailable("runtime is closing"));
        }
        let slot = state
            .slots
            .get(activation)
            .ok_or_else(|| unavailable("unknown activation"))?;
        if slot.status.phase != Phase::Published
            || !slot.native.as_ref().is_some_and(|n| n.gate().is_open())
        {
            return Err(unavailable("activation has not been published"));
        }
        Ok(())
    }
    fn stop(self: &Arc<Self>, params: Value) -> Result<HandlerFuture> {
        let request: Select = decode(params)?;
        self.stop_activation(&request.activation)
    }
    fn stop_activation(self: &Arc<Self>, activation: &Activation) -> Result<HandlerFuture> {
        let (tx, rx) = watch::channel(None);
        let (native, started) = {
            let mut state = self.state.lock().unwrap();
            let slot = state
                .slots
                .get_mut(activation)
                .ok_or_else(|| unavailable("unknown activation"))?;
            if let Some(stop) = &slot.stop {
                return Ok(wait(stop.clone()));
            }
            slot.status.phase = Phase::Closing;
            slot.admission.close();
            // Closing intent precedes the cleanup task; native admission also
            // closes now, even if callers drop their stop waiters.
            if let Some(native) = &slot.native {
                native.gate().close();
            }
            slot.stop = Some(rx.clone());
            (slot.native.clone(), slot.completion.clone())
        };
        let runner = self.clone();
        let activation = activation.clone();
        self.handle.spawn(async move {
            if let Some(native) = native {
                drop(native.stop());
            }
            let _ = wait(started).await;
            let native = runner.state.lock().unwrap().slots[&activation]
                .native
                .clone();
            let result = match native {
                Some(native) => native.stop().await.map_err(cleanup_error),
                None => Ok(()),
            };
            let status = {
                let mut state = runner.state.lock().unwrap();
                let slot = state.slots.get_mut(&activation).unwrap();
                if result.is_ok() {
                    slot.status.phase = Phase::Stopped;
                    slot.native = None;
                }
                slot.status.clone()
            };
            tx.send_replace(Some(result.map(|_| serde_json::to_value(status).unwrap())));
        });
        Ok(wait(rx))
    }
    fn status(&self, params: Value) -> Result<HandlerFuture> {
        let request: Select = decode(params)?;
        let state = self.state.lock().unwrap();
        let slot = state
            .slots
            .get(&request.activation)
            .ok_or_else(|| unavailable("unknown activation"))?;
        let status = slot.status.clone();
        Ok(Box::pin(async move {
            Ok(serde_json::to_value(status).unwrap())
        }))
    }
    /// Disconnect closes publication before any cleanup await. An independent
    /// supervisor must still reap this process and its managed descendants.
    pub fn close(self: &Arc<Self>) -> Vec<HandlerFuture> {
        let activations = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            state.slots.keys().cloned().collect::<Vec<_>>()
        };
        activations
            .iter()
            .filter_map(|a| self.stop_activation(a).ok())
            .collect()
    }
    pub fn attach(self: &Arc<Self>, peer: &Peer) -> Result<()> {
        let weak = Arc::downgrade(self);
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(runner) = weak.upgrade() {
                drop(runner.close());
            }
        });
        {
            let mut state = self.state.lock().unwrap();
            if state.close_hook.is_some() {
                return Err(unavailable("runtime cannot rebind its private peer"));
            }
            state.close_hook = Some(hook.clone());
        }
        peer.on_close(&hook);
        Ok(())
    }
}

/// Serve one inherited private connection. The embedding binary owns its native
/// root and shuts it down after this function has joined all member cleanups.
pub async fn serve<S>(stream: S, driver: impl Driver) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let runner = Runner::new(driver);
    let peer = Peer::start(stream, runner.handler());
    runner.attach(&peer)?;
    peer.closed().await;
    let mut failure = None;
    for stopped in runner.close() {
        if let Err(error) = stopped.await {
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
fn wait(mut rx: watch::Receiver<Option<Completion>>) -> HandlerFuture {
    Box::pin(async move {
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                return result;
            }
            rx.changed()
                .await
                .map_err(|_| unavailable("lifecycle task ended without confirmation"))?;
        }
    })
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
}
fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "lifecycle", message)
}
fn unavailable(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Unavailable, "lifecycle", message)
}
fn cleanup_error(error: Arc<rutis::CordisError>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Business, "stop", error.to_string())
}

/// Hello completes independently of all business start requests. The caller
/// publishes its native RuntimeReady service only after this exact identity ACK.
pub async fn hello(peer: &Peer, plan: &Hello) -> Result<RuntimeIdentity> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ack {
        protocol_family: String,
        protocol_version: String,
        identity: RuntimeIdentity,
    }
    let value = peer
        .request(
            "runtime/hello",
            serde_json::to_value(plan).map_err(|e| invalid(e.to_string()))?,
        )
        .await?;
    check_capabilities(&value)?;
    let ack: Ack = decode(value)?;
    if ack.protocol_family != FAMILY
        || ack.protocol_version != VERSION
        || ack.identity != plan.identity
    {
        return Err(ProtocolError::new(
            ErrorCode::InterfaceMismatch,
            "hello",
            "runtime hello ACK differs from frozen plan",
        ));
    }
    Ok(ack.identity)
}
fn check_capabilities(params: &Value) -> Result<()> {
    let values = params
        .get("identity")
        .and_then(|identity| identity.get("capabilities"))
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("capabilities must be a list"))?;
    if values
        .iter()
        .map(crate::json::canonical)
        .collect::<BTreeSet<_>>()
        .len()
        != values.len()
    {
        return Err(invalid("duplicate runtime capability"));
    }
    for value in values {
        if !matches!(
            value.as_str(),
            Some("object.scope" | "callback.borrow" | "event.parallel" | "event.serial")
        ) {
            return Err(ProtocolError::new(
                ErrorCode::UnsupportedCapability,
                "hello",
                "unknown runtime capability",
            ));
        }
    }
    Ok(())
}

/// The host's native dependency slot. Its check and synchronous close hook
/// invalidate dependent fibers independently of any member apply/cleanup.
pub struct RuntimeReady {
    identity: RuntimeIdentity,
    peer: Peer,
}
impl RuntimeReady {
    pub fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }
    pub fn peer(&self) -> &Peer {
        &self.peer
    }
    /// Host proxy ownership keeps this lease alive until that activation's
    /// cleanup finishes. Closing the connection pre-cancels its native fiber;
    /// a check refresh alone is asynchronous and cannot be this barrier.
    pub fn bind_consumer(&self, consumer: &ManagedActivation) -> Arc<dyn Fn() + Send + Sync> {
        let gate = consumer.gate();
        let view = consumer.view().clone();
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            gate.close();
            drop(view.shutdown());
        });
        self.peer.on_close(&hook);
        hook
    }
}
pub fn publish_ready(
    ctx: &rutis::Ctx,
    identity: RuntimeIdentity,
    peer: Peer,
) -> Result<rutis::Disposer> {
    let key = rutis::TypeKey::keyed_dynamic::<RuntimeReady>(crate::json::canonical(&json!([
        "protocol-runtime",
        identity.runtime,
        identity.epoch
    ])));
    let checked = peer.clone();
    let value = Arc::new(RuntimeReady {
        identity,
        peer: peer.clone(),
    });
    let refreshed = ctx.clone();
    let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || refreshed.refresh());
    let registration = ctx
        .provide_as_with_check(key, value, move || !checked.is_closed())
        .map_err(|e| ProtocolError::new(ErrorCode::Unavailable, "runtime_ready", e.to_string()))?;
    let lease = hook.clone();
    if let Err(error) = ctx.effect(move || {
        rutis::Effect::Disposer(Box::new(move || {
            drop(lease);
            Ok(())
        }))
    }) {
        ctx.handle().spawn(async move {
            let _ = registration.dispose().await;
        });
        return Err(ProtocolError::new(
            ErrorCode::Unavailable,
            "runtime_ready",
            error.to_string(),
        ));
    }
    peer.on_close(&hook);
    Ok(registration)
}
pub fn ready_key(identity: &RuntimeIdentity) -> rutis::TypeKey {
    rutis::TypeKey::keyed_dynamic::<RuntimeReady>(crate::json::canonical(&json!([
        "protocol-runtime",
        identity.runtime,
        identity.epoch
    ])))
}

/// Statically linked native lifecycle driver. Protocol service codecs are a
/// separate driver integration: this constructor refuses service declarations
/// and capability claims it does not implement, before any factory executes.
pub struct NativeDriver {
    parent: rutis::Ctx,
    factories: crate::factories::StaticFactories,
}
impl NativeDriver {
    pub fn new(parent: rutis::Ctx, factories: crate::factories::StaticFactories) -> Self {
        Self { parent, factories }
    }
}
impl Driver for NativeDriver {
    fn admit(&self, hello: &Hello) -> Result<()> {
        let catalog = self.factories.catalog();
        if hello.identity.kind != RuntimeKind::RustRutis
            || hello.identity.framework_version != catalog.framework_version
            || hello.identity.environment_sha256 != catalog.environment_sha256
        {
            return Err(ProtocolError::new(
                ErrorCode::InterfaceMismatch,
                "hello",
                "native runner identity differs",
            ));
        }
        if !hello.identity.capabilities.is_empty() {
            return Err(ProtocolError::new(
                ErrorCode::UnsupportedCapability,
                "hello",
                "native lifecycle driver needs an object/event transport adapter",
            ));
        }
        for member in hello.members.values() {
            let Entry::Rust { factory } = &member.entry else {
                return Err(invalid("native member uses another runtime entry"));
            };
            if catalog.factories.get(factory) != Some(&member.contracts) {
                return Err(ProtocolError::new(
                    ErrorCode::InterfaceMismatch,
                    "hello",
                    "native factory contract differs",
                ));
            }
            if !member.contracts.provides.is_empty() || !member.contracts.requires.is_empty() {
                return Err(ProtocolError::new(
                    ErrorCode::UnsupportedCapability,
                    "hello",
                    "named native service transport adapter is not installed",
                ));
            }
        }
        Ok(())
    }
    fn mount(&self, request: MountRequest, admission: ActivationGate) -> Result<Mounted> {
        let member = request.member;
        let Entry::Rust { factory } = member.entry else {
            return Err(invalid("native member entry differs"));
        };
        Ok(Mounted {
            native: self
                .factories
                .mount_gated(&self.parent, &factory, member.config, admission)?,
            services: Box::pin(async { Ok(BTreeMap::new()) }),
        })
    }
}
