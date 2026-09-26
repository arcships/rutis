//! Object routing on the same private stream as lifecycle control. The Host
//! owns grants; runtime actors own only their native tables and staged values.
use crate::{
    broker::{Broker, DeliveryState},
    contract::TypeExpr,
    draft::{DraftGraph, DraftSource, GraphExporter, StagedGraph},
    error::{ErrorCode, Execution, ProtocolError, Result},
    exports::{Exports, ObjectIds, PinKey},
    frame::{Handler, HandlerFuture, Peer, WeakPeer},
    graph::{DecodedValue, GraphScopes, WireGraph},
    identity::{Activation, Delivery, InterfaceView, ObjectIdentity, Scope, Sequence},
    imports::{ImportControl, Imports, ObjectProxy, Retirement},
    managed::ActivationGate,
    runner_image::CatalogService,
    sdk::{CallContext, Caller, Outbound, RpcFuture},
    services::{offer_table, Bundles, ServiceTable, StagedServices},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::{oneshot, watch};

fn fail(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, "object_session", message)
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| fail(ErrorCode::InvalidParams, e.to_string()))
}
fn wire<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap()
}
pub fn root(activation: &Activation) -> Scope {
    Scope {
        activation: activation.clone(),
        scope: Sequence(1),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub runtime: String,
    pub epoch: Sequence,
}
impl RuntimeIdentity {
    pub fn contains(&self, activation: &Activation) -> bool {
        activation.runtime == self.runtime && activation.epoch == self.epoch
    }
    fn require(&self, activation: &Activation) -> Result<()> {
        if !crate::contract::identifier(&self.runtime)
            || self.epoch.0 == 0
            || activation.activation.0 == 0
        {
            return Err(fail(
                ErrorCode::InvalidParams,
                "invalid native session identity",
            ));
        }
        if self.contains(activation) {
            Ok(())
        } else {
            Err(fail(
                ErrorCode::CapabilityDenied,
                "activation belongs to another private session",
            ))
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageOffer {
    pub activation: Activation,
    pub stage: Sequence,
    pub source: String,
    pub draft: DraftGraph,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    target: Delivery,
    method: String,
    input: StageOffer,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    activation: Activation,
    stage: Sequence,
    deliveries: Vec<Delivery>,
    scopes: GraphScopes,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Execute {
    object: ObjectIdentity,
    view: InterfaceView,
    method: String,
    key: PinKey,
    graph: WireGraph,
    scopes: GraphScopes,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct End {
    activation: Activation,
    scope: Scope,
    key: PinKey,
    stage: Option<Sequence>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    object: ObjectIdentity,
    key: PinKey,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    activation: Activation,
    key: PinKey,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectStage {
    activation: Activation,
    stage: Sequence,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceCommit {
    activation: Activation,
    graphs: BTreeMap<String, WireGraph>,
    scopes: GraphScopes,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteStage {
    activation: Activation,
    service: String,
    source: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerRetire {
    runtime: RuntimeIdentity,
    through: Sequence,
}
/// Frozen Host selections, never accepted from runtime payloads.
pub(crate) struct RoutedRoot {
    pub name: String,
    pub service: String,
    pub owner: Activation,
    pub source: String,
    pub contract: CatalogService,
    pub original: DraftGraph,
}

struct NativeMember {
    gate: ActivationGate,
    native: Option<rutis::Ctx>,
    exports: Option<Exports>,
    exporters: BTreeMap<String, Arc<GraphExporter>>,
    services: Option<StagedServices>,
    published: bool,
    upstream: BTreeSet<Activation>,
    executing: BTreeSet<PinKey>,
    admitted: BTreeSet<PinKey>,
    running: BTreeSet<PinKey>,
}
struct RuntimeState {
    peer: Option<WeakPeer>,
    close_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    closed: bool,
    next_stage: u64,
    members: BTreeMap<Activation, NativeMember>,
    stages: BTreeMap<Sequence, (Activation, StagedGraph)>,
}
/// One actor per runtime epoch, with shared object/delivery numbering across
/// all members. Reserve before construction, bind the actual apply Ctx, then
/// stage services and publish only after the lifecycle/HostActive barrier.
pub struct RuntimeObjects {
    identity: RuntimeIdentity,
    bundles: Bundles,
    ids: ObjectIds,
    imports: Imports,
    state: Mutex<RuntimeState>,
    control_tail: Mutex<watch::Receiver<Option<Result<()>>>>,
}
impl RuntimeObjects {
    pub fn new(identity: RuntimeIdentity, bundles: Bundles) -> Arc<Self> {
        Arc::new(Self {
            identity,
            bundles,
            ids: ObjectIds::default(),
            imports: Imports::default(),
            control_tail: Mutex::new(watch::channel(Some(Ok(()))).1),
            state: Mutex::new(RuntimeState {
                peer: None,
                close_hook: None,
                closed: false,
                next_stage: 0,
                members: BTreeMap::new(),
                stages: BTreeMap::new(),
            }),
        })
    }
    pub fn imports(&self) -> Imports {
        self.imports.clone()
    }
    pub fn ids(&self) -> ObjectIds {
        self.ids.clone()
    }
    pub fn reserve(self: &Arc<Self>, activation: Activation, gate: ActivationGate) -> Result<()> {
        self.identity.require(&activation)?;
        let mut state = self.state.lock().unwrap();
        if state.closed || state.members.contains_key(&activation) || !gate.is_open() {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "closed or reused native member",
            ));
        }
        self.imports.open_scope(root(&activation), None)?;
        self.imports.bind_scope(&root(&activation), gate.clone())?;
        let revoked = gate.clone();
        state.members.insert(
            activation.clone(),
            NativeMember {
                gate,
                native: None,
                exports: None,
                exporters: BTreeMap::new(),
                services: None,
                published: false,
                upstream: BTreeSet::new(),
                executing: BTreeSet::new(),
                admitted: BTreeSet::new(),
                running: BTreeSet::new(),
            },
        );
        drop(state);
        let runtime = Arc::downgrade(self);
        tokio::spawn(async move {
            revoked.revoked().await;
            if let Some(runtime) = runtime.upgrade() {
                runtime.close_member(&activation);
                if let Ok(peer) = runtime.peer() {
                    if !peer.is_closed() {
                        let _ = peer.request("object/closing", wire(activation)).await;
                    }
                }
            }
        });
        Ok(())
    }
    pub fn bind(
        &self,
        activation: &Activation,
        native: rutis::Ctx,
        exports: Exports,
    ) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let member = state
            .members
            .get_mut(activation)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native member was not reserved"))?;
        if !member.gate.is_open() || member.native.is_some() || exports.owner() != activation {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "closed, rebound or mismatched native table",
            ));
        }
        member.native = Some(native);
        member.exports = Some(exports);
        Ok(())
    }
    pub fn stage_services(&self, services: StagedServices) -> Result<ServiceTable> {
        let mut state = self.state.lock().unwrap();
        let owner = &services.table().activation;
        let member = state
            .members
            .get_mut(owner)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native member was not reserved"))?;
        if !member.gate.is_open()
            || member.services.is_some()
            || !member
                .exports
                .as_ref()
                .is_some_and(|e| e.same_table(services.exports()))
        {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "service table belongs to another native member",
            ));
        }
        // Merge only each exact bundle, preserving every full source view.
        for name in services.table().services.keys() {
            let named = services.exporter(name)?;
            let hash = services.contracts()[name].bundle_sha256.clone();
            let bundle = self.bundles.exact(&hash)?;
            let exporter = member.exporters.entry(hash.clone()).or_insert_with(|| {
                Arc::new(GraphExporter::new(
                    bundle.clone(),
                    services.exports().clone(),
                    call_source(owner, &hash),
                ))
            });
            exporter.merge_registered(&named)?;
        }
        let table = services.table().clone();
        member.services = Some(services);
        Ok(table)
    }
    pub fn publish(&self, activation: &Activation) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let member = state
            .members
            .get_mut(activation)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "unknown native member"))?;
        if !member.gate.is_open() || member.native.is_none() {
            return Err(fail(ErrorCode::ScopeClosed, "native member is not ready"));
        }
        member.published = true;
        Ok(())
    }
    /// Capture accepted root bindings before native construction. Revocation
    /// seals each dependent activation synchronously before its ACK.
    pub fn track_imports(
        &self,
        owner: &Activation,
        values: &BTreeMap<String, DecodedValue>,
    ) -> Result<()> {
        let mut upstream = BTreeSet::new();
        for value in values.values() {
            let DecodedValue::Object(proxy) = value else {
                return Err(fail(
                    ErrorCode::InterfaceMismatch,
                    "native import is not an object",
                ));
            };
            let delivery = proxy.delivery()?;
            if delivery.recipient != root(owner) {
                return Err(fail(
                    ErrorCode::CapabilityDenied,
                    "native import belongs to another root",
                ));
            }
            upstream.insert(delivery.object.owner);
        }
        let mut state = self.state.lock().unwrap();
        let member = state
            .members
            .get_mut(owner)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "unknown native member"))?;
        if member.native.is_some() || !member.gate.is_open() {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "native imports already mounted",
            ));
        }
        member.upstream = upstream;
        Ok(())
    }
    pub async fn receive_services(
        self: &Arc<Self>,
        owner: &Activation,
        contracts: &BTreeMap<String, CatalogService>,
        graphs: BTreeMap<String, WireGraph>,
    ) -> Result<BTreeMap<String, DecodedValue>> {
        self.identity.require(owner)?;
        let deliveries = graphs
            .values()
            .flat_map(|g| g.references.iter().map(|r| r.delivery.clone()))
            .collect::<Vec<_>>();
        let result = async {
            if graphs.keys().ne(contracts.keys()) {
                return Err(fail(
                    ErrorCode::InterfaceMismatch,
                    "complete required roots differ from declaration",
                ));
            }
            let scopes = GraphScopes::in_scope(root(owner));
            for (name, graph) in &graphs {
                crate::graph::validate(
                    self.bundles.service(&contracts[name])?.as_ref(),
                    &crate::services::service_type(&contracts[name]),
                    graph,
                    &scopes,
                )?;
            }
            let mut values = BTreeMap::new();
            for (name, graph) in graphs {
                values.insert(
                    name.clone(),
                    self.imports.receive_graph(
                        self.bundles.service(&contracts[&name])?.as_ref(),
                        &crate::services::service_type(&contracts[&name]),
                        graph,
                        &scopes,
                    )?,
                );
            }
            self.track_imports(owner, &values)?;
            self.flush().await?;
            Ok(values)
        }
        .await;
        if result.is_err() {
            self.close_member(owner);
            self.imports.reject(&deliveries);
            if let Err(cleanup) = self.flush().await {
                return Err(fail(
                    ErrorCode::Unavailable,
                    format!(
                        "required root rollback: {cleanup}; admission: {:?}",
                        result.err()
                    ),
                ));
            }
        }
        result
    }
    pub(crate) fn native_context(&self, owner: &Activation) -> Result<rutis::Ctx> {
        self.state
            .lock()
            .unwrap()
            .members
            .get(owner)
            .and_then(|m| m.native.clone())
            .ok_or_else(|| {
                fail(
                    ErrorCode::Unavailable,
                    "original native context is not bound",
                )
            })
    }
    pub fn close_member(&self, activation: &Activation) {
        let mut state = self.state.lock().unwrap();
        if let Some(member) = state.members.get_mut(activation) {
            member.published = false;
            member.gate.close();
            if let Some(exports) = &member.exports {
                // Admissions never dispatched, and completed work awaiting an
                // end ACK, cannot hold native shutdown open after disconnect.
                for key in member.admitted.difference(&member.running) {
                    exports.release(key);
                }
                exports.close();
            }
        }
        state.stages.retain(|_, (owner, _)| owner != activation);
        self.imports.close_scope(&root(activation));
    }
    pub fn close(&self) {
        let owners = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            state.members.keys().cloned().collect::<Vec<_>>()
        };
        for owner in owners {
            self.close_member(&owner);
        }
    }
    pub fn attach(self: &Arc<Self>, peer: &Peer) -> Result<()> {
        let weak = Arc::downgrade(self);
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.close();
            }
        });
        {
            let mut state = self.state.lock().unwrap();
            if state.peer.is_some() || state.closed {
                return Err(fail(
                    ErrorCode::Unavailable,
                    "runtime cannot rebind a private session",
                ));
            }
            state.peer = Some(peer.downgrade());
            state.close_hook = Some(hook.clone());
        }
        peer.on_close(&hook);
        Ok(())
    }
    fn peer(&self) -> Result<Peer> {
        self.state
            .lock()
            .unwrap()
            .peer
            .as_ref()
            .and_then(WeakPeer::upgrade)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "private session unavailable"))
    }
    pub fn handler(self: &Arc<Self>, fallback: Handler) -> Handler {
        let runtime = self.clone();
        Arc::new(move |method, value| {
            if method.starts_with("object/") {
                runtime.handle(&method, value)
            } else {
                fallback(method, value)
            }
        })
    }
    /// Compose lifecycle and object traffic on one authenticated pump. A
    /// service-capable Driver reserves/binds this actor using MountRequest's
    /// original gate, and installs its complete table before start resolves.
    pub fn lifecycle_handler(self: &Arc<Self>, runner: Arc<crate::lifecycle::Runner>) -> Handler {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Select {
            activation: Activation,
        }
        let objects = self.clone();
        self.handler(Arc::new(move |method, value| {
            let selected = if matches!(method.as_str(), "plugin/activate" | "plugin/stop") {
                match decode::<Select>(value.clone()) {
                    Ok(selected) => Some(selected.activation),
                    Err(error) => return Box::pin(async move { Err(error) }),
                }
            } else {
                None
            };
            if method == "runtime/stop" && value.as_object().is_some_and(|v| v.is_empty()) {
                objects.close();
            }
            let future = runner.handle(&method, value);
            if method == "plugin/stop" {
                objects.close_member(selected.as_ref().unwrap());
            }
            let objects = objects.clone();
            Box::pin(async move {
                let result = future.await?;
                if method == "plugin/activate" {
                    objects.publish(&selected.unwrap())?;
                }
                Ok(result)
            })
        }))
    }
    pub fn caller(self: &Arc<Self>, activation: &Activation) -> Arc<dyn Caller> {
        Arc::new(RuntimeCaller {
            runtime: self.clone(),
            activation: activation.clone(),
        })
    }
    fn exporter(&self, activation: &Activation, hash: &str) -> Result<Arc<GraphExporter>> {
        let bundle = self.bundles.exact(hash)?;
        let mut state = self.state.lock().unwrap();
        let member = state
            .members
            .get_mut(activation)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "unknown native member"))?;
        if !member.gate.is_open() {
            return Err(fail(ErrorCode::ScopeClosed, "native member closed"));
        }
        let exports = member
            .exports
            .as_ref()
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native table has not been bound"))?
            .clone();
        Ok(member
            .exporters
            .entry(hash.into())
            .or_insert_with(|| {
                Arc::new(GraphExporter::new(
                    bundle,
                    exports,
                    call_source(activation, hash),
                ))
            })
            .clone())
    }
    pub fn encode(
        &self,
        activation: &Activation,
        hash: &str,
        expr: &TypeExpr,
        value: Outbound,
    ) -> Result<StageOffer> {
        let staged = self.exporter(activation, hash)?.encode(expr, value)?;
        let mut state = self.state.lock().unwrap();
        // Stop racing encoding aborts its temporary staging pins.
        if state.closed || !state.members[activation].gate.is_open() {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "native member closed during encoding",
            ));
        }
        state.next_stage = state
            .next_stage
            .checked_add(1)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "stage sequence exhausted"))?;
        let stage = Sequence(state.next_stage);
        let offer = StageOffer {
            activation: activation.clone(),
            stage,
            source: call_source(activation, hash),
            draft: staged.draft.clone(),
        };
        state.stages.insert(stage, (activation.clone(), staged));
        Ok(offer)
    }
    fn abort(&self, owner: &Activation, stage: Sequence) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state
            .stages
            .get(&stage)
            .is_some_and(|(activation, _)| activation != owner)
        {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "staging graph belongs to another member",
            ));
        }
        state.stages.remove(&stage);
        Ok(())
    }
    fn stage_route(&self, request: RouteStage) -> Result<StageOffer> {
        self.identity.require(&request.activation)?;
        if !request
            .source
            .strip_prefix("route.")
            .is_some_and(crate::prepare::sha256_field)
        {
            return Err(fail(
                ErrorCode::InvalidParams,
                "invalid prepared route source",
            ));
        }
        let mut state = self.state.lock().unwrap();
        let member = state
            .members
            .get_mut(&request.activation)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "unknown route owner"))?;
        if !member.gate.is_open() {
            return Err(fail(ErrorCode::ScopeClosed, "route owner closed"));
        }
        let services = member
            .services
            .as_ref()
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native service table is not staged"))?;
        let staged = services.stage_route(&request.service, &request.source)?;
        let named = services.exporter(&request.service)?;
        member.exporters[&services.contracts()[&request.service].bundle_sha256]
            .merge_registered(&named)?;
        state.next_stage = state
            .next_stage
            .checked_add(1)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "stage sequence exhausted"))?;
        let stage = Sequence(state.next_stage);
        let offer = StageOffer {
            activation: request.activation.clone(),
            stage,
            source: request.source,
            draft: staged.draft.clone(),
        };
        state.stages.insert(stage, (request.activation, staged));
        Ok(offer)
    }
    pub async fn flush(self: &Arc<Self>) -> Result<()> {
        let completion = {
            let mut tail = self.control_tail.lock().unwrap();
            let controls = self.imports.take_controls();
            if controls.is_empty() {
                tail.clone()
            } else {
                let previous = tail.clone();
                let (tx, rx) = watch::channel(None);
                *tail = rx.clone();
                let runtime = self.clone();
                // The ACK fence survives abandonment of any flushing waiter.
                tokio::spawn(async move {
                    let prior = control_complete(previous).await;
                    let current = async {
                        runtime
                            .peer()?
                            .request("object/controls", wire(controls))
                            .await?;
                        Ok(())
                    }
                    .await;
                    tx.send_replace(Some(prior.and(current)));
                });
                rx
            }
        };
        control_complete(completion).await
    }
    pub async fn retire(self: &Arc<Self>) -> Result<Option<Retirement>> {
        self.flush().await?;
        let Some(proposal) = self.imports.retirement() else {
            return Ok(None);
        };
        let through: Sequence = decode(
            self.peer()?
                .request("object/retire", wire(proposal.clone()))
                .await?,
        )?;
        if through != proposal.terminal_through {
            return Err(fail(
                ErrorCode::InvalidParams,
                "retirement ACK differs from proposal",
            ));
        }
        self.imports.acknowledge_retirement(through)?;
        Ok(Some(proposal))
    }
    pub fn handle(self: &Arc<Self>, method: &str, value: Value) -> HandlerFuture {
        let rejected = if method == "object/execute" {
            decode::<Execute>(value.clone()).ok().map(|request| {
                request
                    .graph
                    .references
                    .into_iter()
                    .map(|r| r.delivery)
                    .filter(|d| self.identity.contains(&d.recipient.activation))
                    .collect::<Vec<_>>()
            })
        } else {
            None
        };
        let result = self.admit(method, value);
        match result {
            Ok(future) => future,
            Err(error) => {
                if let Some(deliveries) = rejected {
                    self.imports.reject(&deliveries);
                    let runtime = self.clone();
                    Box::pin(async move {
                        runtime.flush().await?;
                        Err(error)
                    })
                } else {
                    Box::pin(async move { Err(error) })
                }
            }
        }
    }
    fn admit(self: &Arc<Self>, method: &str, value: Value) -> Result<HandlerFuture> {
        let ready = |value| Ok(Box::pin(async move { Ok(value) }) as HandlerFuture);
        match method {
            "object/open" => {
                let scope: Scope = decode(value)?;
                self.identity.require(&scope.activation)?;
                self.imports
                    .open_scope(scope.clone(), Some(root(&scope.activation)))?;
                ready(Value::Null)
            }
            "object/pin" => {
                let pin: Pin = decode(value)?;
                self.identity.require(&pin.object.owner)?;
                self.exports(&pin.object.owner)?
                    .pin(&pin.object, pin.key.clone())?;
                if matches!(pin.key, PinKey::Execution { .. }) {
                    self.state
                        .lock()
                        .unwrap()
                        .members
                        .get_mut(&pin.object.owner)
                        .unwrap()
                        .admitted
                        .insert(pin.key);
                }
                ready(Value::Null)
            }
            "object/release" => {
                let release: Release = decode(value)?;
                self.exports(&release.activation)?.release(&release.key);
                ready(Value::Null)
            }
            "object/abort" => {
                let request: SelectStage = decode(value)?;
                self.abort(&request.activation, request.stage)?;
                ready(Value::Null)
            }
            "object/commit" => {
                let request: Commit = decode(value)?;
                let mut state = self.state.lock().unwrap();
                let (owner, staged) = state
                    .stages
                    .get_mut(&request.stage)
                    .ok_or_else(|| fail(ErrorCode::StaleObject, "unknown staging graph"))?;
                if owner != &request.activation {
                    return Err(fail(
                        ErrorCode::CapabilityDenied,
                        "staging graph belongs to another member",
                    ));
                }
                staged.commit(request.deliveries, &request.scopes)?;
                ready(Value::Null)
            }
            "object/services-commit" => {
                let request: ServiceCommit = decode(value)?;
                let mut state = self.state.lock().unwrap();
                let staged = state
                    .members
                    .get_mut(&request.activation)
                    .and_then(|m| m.services.as_mut())
                    .ok_or_else(|| fail(ErrorCode::StaleObject, "no staged service table"))?;
                staged.commit(&request.graphs, &request.scopes)?;
                ready(Value::Null)
            }
            "object/route" => ready(wire(self.stage_route(decode(value)?)?)),
            "object/reject" => {
                let deliveries: Vec<Delivery> = decode(value)?;
                self.imports.reject(&deliveries);
                self.flush_reply()
            }
            "object/revoke" => {
                let owner: Activation = decode(value)?;
                self.imports.revoke_owner(&owner);
                let dependents = self
                    .state
                    .lock()
                    .unwrap()
                    .members
                    .iter()
                    .filter(|(_, m)| m.upstream.contains(&owner))
                    .map(|(a, _)| a.clone())
                    .collect::<Vec<_>>();
                self.close_member(&owner);
                for dependent in dependents {
                    self.close_member(&dependent);
                }
                self.flush_reply()
            }
            "object/retire-owner" => {
                let request: OwnerRetire = decode(value)?;
                let state = self.state.lock().unwrap();
                for member in state.members.values() {
                    if let Some(exports) = &member.exports {
                        exports.retire_deliveries(
                            &request.runtime.runtime,
                            request.runtime.epoch,
                            request.through,
                        )?;
                    }
                }
                ready(Value::Null)
            }
            "object/end" => {
                let end: End = decode(value)?;
                self.identity.require(&end.activation)?;
                if end.scope.activation != end.activation || end.scope.scope == Sequence(1) {
                    return Err(fail(ErrorCode::CapabilityDenied, "invalid execution scope"));
                }
                self.imports.close_scope(&end.scope);
                self.exports(&end.activation)?.release(&end.key);
                if let Some(member) = self.state.lock().unwrap().members.get_mut(&end.activation) {
                    member.executing.remove(&end.key);
                    member.running.remove(&end.key);
                    member.admitted.remove(&end.key);
                }
                if let Some(stage) = end.stage {
                    self.abort(&end.activation, stage)?;
                }
                self.flush_reply()
            }
            "object/execute" => {
                let request: Execute = decode(value)?;
                self.identity.require(&request.object.owner)?;
                if request.scopes.scope != root(&request.object.owner)
                    || request.scopes.borrow.activation != request.object.owner
                    || request.scopes.borrow.scope == Sequence(1)
                {
                    return Err(fail(ErrorCode::CapabilityDenied, "invalid dispatch scopes"));
                }
                let mut state = self.state.lock().unwrap();
                let member = state
                    .members
                    .get_mut(&request.object.owner)
                    .ok_or_else(|| fail(ErrorCode::Unavailable, "unknown native member"))?;
                if (!member.published && !request.view.interface.starts_with("$callback:"))
                    || !member.gate.is_open()
                {
                    return Err(fail(
                        ErrorCode::Unavailable,
                        "native member has not been published",
                    ));
                }
                let native = member
                    .exports
                    .as_ref()
                    .ok_or_else(|| fail(ErrorCode::Unavailable, "native exports unavailable"))?
                    .execution_context(&request.key)?
                    .or_else(|| member.native.clone())
                    .ok_or_else(|| fail(ErrorCode::Unavailable, "native context unavailable"))?;
                let contract = self.bundles.method(&request.view, &request.method)?;
                let bundle = self.bundles.exact(&request.view.bundle_sha256)?;
                if !matches!(request.key, PinKey::Execution { .. })
                    || !member.executing.insert(request.key.clone())
                {
                    return Err(fail(
                        ErrorCode::InvalidParams,
                        "duplicate or invalid native execution",
                    ));
                }
                member.running.insert(request.key.clone());
                let runtime = self.clone();
                Ok(Box::pin(async move {
                    let context =
                        CallContext::new(runtime.caller(&request.object.owner), Some(native));
                    let dispatched = runtime.clone();
                    let dispatch_context = context.clone();
                    let dispatch = request.clone();
                    let params_type = contract.params.clone();
                    let result = tokio::spawn(async move {
                        let params = dispatched.imports.receive_graph(
                            &bundle,
                            &params_type,
                            dispatch.graph,
                            &dispatch.scopes,
                        )?;
                        // ACK parameters before user code can reenter via them.
                        dispatched.flush().await?;
                        dispatched
                            .exporter(&dispatch.object.owner, &dispatch.view.bundle_sha256)?
                            .dispatcher(&dispatch.object, &dispatch.view)?
                            .dispatch(dispatch.key, dispatch_context, dispatch.method, params)
                            .await
                    })
                    .await
                    .unwrap_or_else(|error| {
                        Err(fail(
                            ErrorCode::Business,
                            format!("native handler panicked: {error}"),
                        ))
                    });
                    let children = context.finish().await;
                    {
                        let mut state = runtime.state.lock().unwrap();
                        let closed = state.closed;
                        let member = state.members.get_mut(&request.object.owner).unwrap();
                        member.running.remove(&request.key);
                        if closed || !member.gate.is_open() {
                            member.exports.as_ref().unwrap().release(&request.key);
                            member.admitted.remove(&request.key);
                            runtime.imports.close_scope(&request.scopes.borrow);
                        }
                    }
                    let output = result
                        .and_then(|value| children.map(|_| value))
                        .and_then(|value| context.outbound(value))
                        .map_err(|mut error| {
                            error.execution = Execution::Unknown;
                            error
                        })?;
                    Ok(wire(
                        runtime
                            .encode(
                                &request.object.owner,
                                &request.view.bundle_sha256,
                                &contract.result,
                                output,
                            )
                            .map_err(|mut error| {
                                error.execution = Execution::Unknown;
                                error
                            })?,
                    ))
                }))
            }
            _ => Err(fail(
                ErrorCode::UnsupportedCapability,
                "unknown runtime object operation",
            )),
        }
    }
    fn flush_reply(self: &Arc<Self>) -> Result<HandlerFuture> {
        let runtime = self.clone();
        Ok(Box::pin(async move {
            runtime.flush().await?;
            Ok(wire(Vec::<ImportControl>::new()))
        }))
    }
    pub(crate) fn exports(&self, owner: &Activation) -> Result<Exports> {
        self.state
            .lock()
            .unwrap()
            .members
            .get(owner)
            .and_then(|m| m.exports.clone())
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native table unavailable"))
    }
}
async fn control_complete(mut completion: watch::Receiver<Option<Result<()>>>) -> Result<()> {
    loop {
        if let Some(result) = completion.borrow_and_update().clone() {
            return result;
        }
        completion
            .changed()
            .await
            .map_err(|_| fail(ErrorCode::Unavailable, "control ACK task lost"))?;
    }
}
pub fn call_source(owner: &Activation, hash: &str) -> String {
    crate::prepare::digest(
        crate::json::canonical(&json!(["protocol-call", owner, hash])).as_bytes(),
    )
}
struct RuntimeCaller {
    runtime: Arc<RuntimeObjects>,
    activation: Activation,
}
struct Received {
    runtime: Arc<RuntimeObjects>,
    graph: Option<WireGraph>,
    bundle: String,
    expr: TypeExpr,
    scope: Scope,
}
impl Received {
    async fn consume(mut self) -> Result<DecodedValue> {
        let result = self.runtime.imports.receive_graph(
            self.runtime.bundles.exact(&self.bundle)?.as_ref(),
            &self.expr,
            self.graph.take().unwrap(),
            &GraphScopes::in_scope(self.scope.clone()),
        );
        self.runtime.flush().await?;
        result
    }
}
impl Drop for Received {
    fn drop(&mut self) {
        if let Some(graph) = self.graph.take() {
            self.runtime.imports.reject(
                &graph
                    .references
                    .into_iter()
                    .map(|r| r.delivery)
                    .collect::<Vec<_>>(),
            );
            let runtime = self.runtime.clone();
            tokio::spawn(async move {
                let _ = runtime.flush().await;
            });
        }
    }
}
impl Caller for RuntimeCaller {
    fn bind_native(&self, ctx: &rutis::Ctx, value: Outbound) -> Result<()> {
        crate::sdk::register_native(&self.runtime.exports(&self.activation)?, ctx, value)
    }
    fn call(
        &self,
        target: ObjectProxy,
        method: String,
        params: Outbound,
    ) -> RpcFuture<DecodedValue> {
        let runtime = self.runtime.clone();
        let activation = self.activation.clone();
        Box::pin(async move {
            let delivery = target.delivery()?;
            if delivery.recipient.activation != activation {
                return Err(fail(
                    ErrorCode::CapabilityDenied,
                    "client belongs to another native member",
                ));
            }
            let contract = runtime.bundles.method(&delivery.view, &method)?;
            let input = runtime.encode(
                &activation,
                &delivery.view.bundle_sha256,
                &contract.params,
                params,
            )?;
            let (tx, rx) = oneshot::channel();
            // Owner work and envelope rejection survive abandonment of this waiter.
            let scope = delivery.recipient.clone();
            let hash = delivery.view.bundle_sha256.clone();
            tokio::spawn(async move {
                let stage = input.stage;
                let result = async {
                    let graph = decode(
                        runtime
                            .peer()?
                            .request(
                                "object/call",
                                wire(Call {
                                    target: delivery,
                                    method,
                                    input,
                                }),
                            )
                            .await?,
                    )?;
                    Ok(Received {
                        runtime: runtime.clone(),
                        graph: Some(graph),
                        bundle: hash,
                        expr: contract.result,
                        scope,
                    })
                }
                .await;
                let _ = runtime.abort(&activation, stage);
                let _ = tx.send(result);
            });
            rx.await
                .map_err(|_| fail(ErrorCode::Unavailable, "object call task lost"))??
                .consume()
                .await
        })
    }
}

/// Owns fresh service envelopes until they are accepted by the actual runtime
/// SDK. Abandonment closes the reserved recipient and rejects every root; raw
/// graphs are never returned without an owner for their delivery pins.
pub struct RootDelivery {
    host: Arc<HostObjects>,
    recipient: Activation,
    contracts: BTreeMap<String, CatalogService>,
    graphs: Option<BTreeMap<String, WireGraph>>,
    local: Option<Arc<RuntimeObjects>>,
}
impl RootDelivery {
    fn accepted(&self) -> Result<()> {
        let state = self.host.state.lock().unwrap();
        if !state
            .members
            .get(&self.recipient)
            .is_some_and(|m| !m.closed)
            || self
                .graphs
                .as_ref()
                .unwrap()
                .values()
                .flat_map(|g| &g.references)
                .any(|r| {
                    state.broker.state(&self.recipient, r.delivery.id)
                        != Some(DeliveryState::Accepted)
                })
        {
            return Err(fail(
                ErrorCode::Unavailable,
                "complete service roots were not accepted by the recipient",
            ));
        }
        Ok(())
    }

    pub async fn send(mut self, method: &str) -> Result<Value> {
        let result = self.host.peer(&self.recipient)?.request(method, json!({
            "activation": self.recipient, "contracts": self.contracts, "graphs": self.graphs.as_ref().unwrap(),
        })).await?;
        self.accepted()?;
        self.graphs.take();
        Ok(result)
    }
    pub async fn start(self, instance: &str) -> Result<Value> {
        self.begin_start(instance)?.await
    }
    /// Queue start synchronously so a paired stop cannot overtake it while the
    /// Host native apply is awaiting the response. The receipt owns all roots.
    pub(crate) fn begin_start(self, instance: &str) -> Result<HandlerFuture> {
        let response = self.queue_start(instance)?;
        Ok(self.start_response(response))
    }
    pub(crate) fn queue_start(&self, instance: &str) -> Result<oneshot::Receiver<Result<Value>>> {
        self.host.peer(&self.recipient)?.start_request("plugin/start", json!({
            "instance": instance, "activation": self.recipient, "required": self.graphs.as_ref().unwrap(),
        }))
    }
    pub(crate) fn start_response(
        mut self,
        response: oneshot::Receiver<Result<Value>>,
    ) -> HandlerFuture {
        Box::pin(async move {
            let result = response
                .await
                .map_err(|_| fail(ErrorCode::Unavailable, "start ACK lost"))??;
            self.accepted()?;
            self.graphs.take();
            Ok(result)
        })
    }
    pub async fn receive(
        mut self,
        runtime: &Arc<RuntimeObjects>,
    ) -> Result<BTreeMap<String, DecodedValue>> {
        runtime.identity.require(&self.recipient)?;
        self.local = Some(runtime.clone());
        let scopes = GraphScopes::in_scope(root(&self.recipient));
        // Validate every bundle and root before attaching the first delivery.
        for (name, graph) in self.graphs.as_ref().unwrap() {
            let contract = &self.contracts[name];
            crate::graph::validate(
                runtime.bundles.service(contract)?.as_ref(),
                &crate::services::service_type(contract),
                graph,
                &scopes,
            )?;
        }
        let mut values = BTreeMap::new();
        for (name, graph) in self.graphs.as_ref().unwrap() {
            let contract = &self.contracts[name];
            let value = runtime.imports.receive_graph(
                runtime.bundles.service(contract)?.as_ref(),
                &crate::services::service_type(contract),
                graph.clone(),
                &scopes,
            )?;
            values.insert(name.clone(), value);
        }
        runtime.track_imports(&self.recipient, &values)?;
        runtime.flush().await?;
        self.accepted()?;
        self.graphs.take();
        Ok(values)
    }
}
impl Drop for RootDelivery {
    fn drop(&mut self) {
        if let Some(graphs) = self.graphs.take() {
            if let Some(runtime) = &self.local {
                runtime.close_member(&self.recipient);
            }
            drop(self.host.close_member(&self.recipient));
            let host = self.host.clone();
            tokio::spawn(async move {
                for graph in graphs.values() {
                    let _ = host.reject(graph).await;
                }
            });
        }
    }
}

struct HostMember {
    published: bool,
    closed: bool,
    close_hooks: Vec<std::sync::Weak<dyn Fn() + Send + Sync>>,
}
struct Connection {
    peer: WeakPeer,
    _close_hook: Arc<dyn Fn() + Send + Sync>,
}
#[derive(Default)]
struct HostState {
    broker: Broker,
    peers: BTreeMap<RuntimeIdentity, Connection>,
    members: BTreeMap<Activation, HostMember>,
    calls: BTreeMap<Activation, u64>,
    scopes: BTreeMap<Activation, u64>,
    deliveries: BTreeMap<(RuntimeIdentity, Sequence), Delivery>,
    retired: BTreeMap<RuntimeIdentity, Sequence>,
    runtime_hooks: BTreeMap<RuntimeIdentity, Vec<std::sync::Weak<dyn Fn() + Send + Sync>>>,
    disconnected: BTreeSet<RuntimeIdentity>,
}
/// Authoritative multi-runtime router. Every handler is bound to the inherited
/// stream's identity, never an identity supplied by business configuration.
pub struct HostObjects {
    bundles: Bundles,
    state: Mutex<HostState>,
}
impl HostObjects {
    pub fn new(bundles: Bundles) -> Arc<Self> {
        Arc::new(Self {
            bundles,
            state: Mutex::default(),
        })
    }
    /// Pending proxies belong to their epoch before member apply can capture
    /// RuntimeReady. Disconnect seals them synchronously as well.
    pub(crate) fn on_runtime_close(
        &self,
        identity: &RuntimeIdentity,
        hook: &Arc<dyn Fn() + Send + Sync>,
    ) {
        let closed = {
            let mut state = self.state.lock().unwrap();
            let hooks = state.runtime_hooks.entry(identity.clone()).or_default();
            hooks.retain(|hook| hook.strong_count() != 0);
            hooks.push(Arc::downgrade(hook));
            state.disconnected.contains(identity)
        };
        if closed {
            hook();
        }
    }
    /// Reserve IDs in Host order before independently sending member starts.
    pub fn reserve(&self, activation: Activation) -> Result<()> {
        RuntimeIdentity {
            runtime: activation.runtime.clone(),
            epoch: activation.epoch,
        }
        .require(&activation)?;
        let mut state = self.state.lock().unwrap();
        state.broker.start_activation(activation.clone())?;
        state.broker.open_scope(root(&activation), None)?;
        state.members.insert(
            activation,
            HostMember {
                published: false,
                closed: false,
                close_hooks: Vec::new(),
            },
        );
        Ok(())
    }
    /// Native Host proxies keep this lease until cleanup. Revoke/disconnect
    /// calls it synchronously outside the broker lock, before remote ACKs.
    pub(crate) fn on_close(
        &self,
        owner: &Activation,
        hook: &Arc<dyn Fn() + Send + Sync>,
    ) -> Result<()> {
        let closed = {
            let mut state = self.state.lock().unwrap();
            let member = state
                .members
                .get_mut(owner)
                .ok_or_else(|| fail(ErrorCode::Unavailable, "unknown Host member"))?;
            member.close_hooks.retain(|hook| hook.strong_count() != 0);
            member.close_hooks.push(Arc::downgrade(hook));
            member.closed
        };
        if closed {
            hook();
        }
        Ok(())
    }
    pub fn publish(&self, activation: &Activation) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let member = state
            .members
            .get_mut(activation)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "unknown Host member"))?;
        if member.closed {
            return Err(fail(ErrorCode::ScopeClosed, "Host member closed"));
        }
        member.published = true;
        Ok(())
    }
    pub(crate) fn require_published(&self, activation: &Activation) -> Result<()> {
        if self
            .state
            .lock()
            .unwrap()
            .members
            .get(activation)
            .is_some_and(|m| m.published && !m.closed)
        {
            Ok(())
        } else {
            Err(fail(
                ErrorCode::Unavailable,
                "prepared provider has not been published",
            ))
        }
    }
    pub fn attach(self: &Arc<Self>, identity: RuntimeIdentity, peer: &Peer) -> Result<()> {
        let weak = Arc::downgrade(self);
        let closed_identity = identity.clone();
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(host) = weak.upgrade() {
                host.disconnect(&closed_identity);
            }
        });
        {
            let mut state = self.state.lock().unwrap();
            if state.peers.contains_key(&identity) {
                return Err(fail(ErrorCode::Unavailable, "Host session cannot rebind"));
            }
            state.peers.insert(
                identity,
                Connection {
                    peer: peer.downgrade(),
                    _close_hook: hook.clone(),
                },
            );
        }
        peer.on_close(&hook);
        Ok(())
    }
    fn peer(&self, owner: &Activation) -> Result<Peer> {
        self.peer_identity(&RuntimeIdentity {
            runtime: owner.runtime.clone(),
            epoch: owner.epoch,
        })
    }
    fn peer_identity(&self, identity: &RuntimeIdentity) -> Result<Peer> {
        self.state
            .lock()
            .unwrap()
            .peers
            .get(identity)
            .and_then(|p| p.peer.upgrade())
            .ok_or_else(|| fail(ErrorCode::Unavailable, "owner private session unavailable"))
    }
    pub fn handler(self: &Arc<Self>, identity: RuntimeIdentity, fallback: Handler) -> Handler {
        let host = self.clone();
        Arc::new(move |method, value| {
            if method.starts_with("object/") {
                host.handle(&identity, &method, value)
            } else {
                fallback(method, value)
            }
        })
    }
    pub fn handle(
        self: &Arc<Self>,
        identity: &RuntimeIdentity,
        method: &str,
        value: Value,
    ) -> HandlerFuture {
        let admitted = match method {
            "object/call" => {
                decode::<Call>(value).and_then(|request| self.admit_call(identity, request))
            }
            "object/closing" => decode::<Activation>(value).and_then(|activation| {
                identity.require(&activation)?;
                if !self.state.lock().unwrap().members.contains_key(&activation) {
                    return Err(fail(ErrorCode::Unavailable, "unknown closing member"));
                }
                Ok(self.close_member(&activation))
            }),
            "object/controls" => decode::<Vec<ImportControl>>(value).map(|controls| {
                let host = self.clone();
                let identity = identity.clone();
                Box::pin(async move {
                    host.controls(&identity, controls).await?;
                    Ok(Value::Null)
                }) as HandlerFuture
            }),
            "object/retire" => decode::<Retirement>(value).and_then(|proposal| {
                self.state.lock().unwrap().broker.retire_epoch(
                    &identity.runtime,
                    identity.epoch,
                    proposal.received_through,
                    proposal.terminal_through,
                )?;
                let host = self.clone();
                let identity = identity.clone();
                Ok(Box::pin(async move {
                    let peers = host
                        .state
                        .lock()
                        .unwrap()
                        .peers
                        .values()
                        .filter_map(|p| p.peer.upgrade())
                        .collect::<Vec<_>>();
                    for peer in peers {
                        peer.request(
                            "object/retire-owner",
                            wire(OwnerRetire {
                                runtime: identity.clone(),
                                through: proposal.terminal_through,
                            }),
                        )
                        .await?;
                    }
                    {
                        let mut state = host.state.lock().unwrap();
                        state.deliveries.retain(|(recipient, id), _| {
                            recipient != &identity || *id > proposal.terminal_through
                        });
                        state.retired.insert(identity, proposal.terminal_through);
                    }
                    Ok(wire(proposal.terminal_through))
                }) as HandlerFuture)
            }),
            _ => Err(fail(
                ErrorCode::UnsupportedCapability,
                "unknown Host object operation",
            )),
        };
        match admitted {
            Ok(future) => future,
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    async fn controls(
        &self,
        identity: &RuntimeIdentity,
        controls: Vec<ImportControl>,
    ) -> Result<()> {
        for control in controls {
            let released = {
                let mut state = self.state.lock().unwrap();
                let (id, token) = match &control {
                    ImportControl::Accept { id, token } | ImportControl::Release { id, token } => {
                        (*id, token)
                    }
                };
                if state
                    .retired
                    .get(identity)
                    .is_some_and(|through| id <= *through)
                {
                    continue;
                }
                let delivery = state
                    .deliveries
                    .get(&(identity.clone(), id))
                    .cloned()
                    .ok_or_else(|| {
                        fail(
                            ErrorCode::CapabilityDenied,
                            "delivery not issued to this private session",
                        )
                    })?;
                match control {
                    ImportControl::Accept { .. } => {
                        state
                            .broker
                            .accept(&delivery.recipient.activation, id, token)?;
                        None
                    }
                    ImportControl::Release { .. } => {
                        state
                            .broker
                            .release(&delivery.recipient.activation, id, token)?;
                        Some((
                            delivery.object.owner,
                            PinKey::Delivery {
                                recipient: delivery.recipient.activation,
                                id,
                            },
                        ))
                    }
                }
            };
            if let Some((owner, key)) = released {
                let peer = self.peer(&owner)?;
                if peer.is_closed() {
                    continue;
                }
                peer.request(
                    "object/release",
                    wire(Release {
                        activation: owner,
                        key,
                    }),
                )
                .await?;
            }
        }
        Ok(())
    }
    fn remember(state: &mut HostState, graph: &WireGraph) {
        for reference in &graph.references {
            let recipient = &reference.delivery.recipient.activation;
            state.deliveries.insert(
                (
                    RuntimeIdentity {
                        runtime: recipient.runtime.clone(),
                        epoch: recipient.epoch,
                    },
                    reference.delivery.id,
                ),
                reference.delivery.clone(),
            );
        }
    }
    async fn reject(&self, graph: &WireGraph) -> Result<()> {
        if graph.references.is_empty() {
            return Ok(());
        }
        let recipient = &graph.references[0].delivery.recipient.activation;
        let controls = decode(
            self.peer(recipient)?
                .request(
                    "object/reject",
                    wire(
                        graph
                            .references
                            .iter()
                            .map(|r| &r.delivery)
                            .collect::<Vec<_>>(),
                    ),
                )
                .await?,
        )?;
        self.controls(
            &RuntimeIdentity {
                runtime: recipient.runtime.clone(),
                epoch: recipient.epoch,
            },
            controls,
        )
        .await
    }
    async fn handoff(
        &self,
        sender: &Activation,
        hash: &str,
        expr: &TypeExpr,
        offer: &StageOffer,
        scopes: &GraphScopes,
    ) -> Result<WireGraph> {
        if offer.activation != *sender {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "staging graph belongs to another sender",
            ));
        }
        let graph = {
            let mut state = self.state.lock().unwrap();
            let graph = state.broker.offer_graph(
                sender,
                self.bundles.exact(hash)?.as_ref(),
                expr,
                &offer.draft,
                scopes,
                &offer.source,
            )?;
            Self::remember(&mut state, &graph);
            graph
        };
        let result = async {
            self.peer(sender)?
                .request(
                    "object/commit",
                    wire(Commit {
                        activation: sender.clone(),
                        stage: offer.stage,
                        deliveries: graph
                            .references
                            .iter()
                            .map(|r| r.delivery.clone())
                            .collect(),
                        scopes: scopes.clone(),
                    }),
                )
                .await?;
            self.pin_foreign(&offer.draft, &graph).await
        }
        .await;
        if let Err(error) = result {
            self.reject(&graph).await?;
            return Err(error);
        }
        Ok(graph)
    }
    async fn pin_foreign(&self, draft: &DraftGraph, graph: &WireGraph) -> Result<()> {
        for (reference, wire) in draft.references.iter().zip(&graph.references) {
            if matches!(reference.source, DraftSource::Foreign { .. }) {
                let delivery = &wire.delivery;
                self.peer(&delivery.object.owner)?
                    .request("object/pin", wire_value_pin(delivery))
                    .await?;
            }
        }
        Ok(())
    }
    /// Commit all service roots to the requested recipient before exposing any
    /// grant. Typed native provider installation remains the Host adapter's job.
    pub async fn offer_services(
        self: &Arc<Self>,
        owner: &Activation,
        table: &ServiceTable,
        contracts: &BTreeMap<String, CatalogService>,
        recipient: &Activation,
    ) -> Result<RootDelivery> {
        let host = self.clone();
        let owner = owner.clone();
        let table = table.clone();
        let contracts = contracts.clone();
        let recipient = recipient.clone();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let result = host
                .handoff_services(&owner, &table, &contracts, &recipient)
                .await
                .map(|graphs| RootDelivery {
                    host,
                    recipient,
                    contracts,
                    graphs: Some(graphs),
                    local: None,
                });
            // Failed send drops the owned receipt and rejects every fresh root.
            let _ = tx.send(result);
        });
        rx.await
            .map_err(|_| fail(ErrorCode::Unavailable, "service handoff task lost"))?
    }
    pub(crate) async fn offer_routes(
        self: &Arc<Self>,
        roots: Vec<RoutedRoot>,
        recipient: Activation,
    ) -> Result<RootDelivery> {
        self.offer_routes_mode(roots, recipient, true).await
    }
    /// Only the Host native mirror uses this mode after a validated ready
    /// table. Its real SDK scope accepts roots while availability stays false.
    pub(crate) async fn mirror_routes(
        self: &Arc<Self>,
        roots: Vec<RoutedRoot>,
        recipient: Activation,
    ) -> Result<RootDelivery> {
        self.offer_routes_mode(roots, recipient, false).await
    }
    async fn offer_routes_mode(
        self: &Arc<Self>,
        roots: Vec<RoutedRoot>,
        recipient: Activation,
        published: bool,
    ) -> Result<RootDelivery> {
        let host = self.clone();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let contracts = roots
                .iter()
                .map(|r| (r.name.clone(), r.contract.clone()))
                .collect();
            let result = host.handoff_routes(&roots, &recipient, published).await;
            let result = match result {
                Ok(graphs) => Ok(RootDelivery {
                    host,
                    recipient,
                    contracts,
                    graphs: Some(graphs),
                    local: None,
                }),
                Err(error) => {
                    drop(host.close_member(&recipient));
                    Err(error)
                }
            };
            // Issuance and rollback survive abandonment of a Host apply waiter.
            let _ = tx.send(result);
        });
        rx.await
            .map_err(|_| fail(ErrorCode::Unavailable, "prepared root handoff task lost"))?
    }
    async fn handoff_routes(
        &self,
        roots: &[RoutedRoot],
        recipient: &Activation,
        published: bool,
    ) -> Result<BTreeMap<String, WireGraph>> {
        let scopes = GraphScopes::in_scope(root(recipient));
        let mut offers = Vec::<StageOffer>::new();
        let mut graphs = BTreeMap::new();
        let mut issued = false;
        let result = async {
            for route in roots {
                {
                    let state = self.state.lock().unwrap();
                    if !state
                        .members
                        .get(&route.owner)
                        .is_some_and(|m| !m.closed && (!published || m.published))
                    {
                        return Err(fail(
                            ErrorCode::Unavailable,
                            "prepared provider has not been published",
                        ));
                    }
                }
                let offer: StageOffer = decode(
                    self.peer(&route.owner)?
                        .request(
                            "object/route",
                            wire(RouteStage {
                                activation: route.owner.clone(),
                                service: route.service.clone(),
                                source: route.source.clone(),
                            }),
                        )
                        .await?,
                )?;
                offers.push(offer);
                let offer = offers.last().unwrap();
                let mut expected = route.original.clone();
                for reference in &mut expected.references {
                    if let DraftSource::Own { view, .. } = &mut reference.source {
                        view.source = route.source.clone();
                    }
                }
                if offer.activation != route.owner
                    || offer.source != route.source
                    || crate::json::canonical(&wire(&offer.draft))
                        != crate::json::canonical(&wire(expected))
                {
                    return Err(fail(
                        ErrorCode::CapabilityDenied,
                        "owner changed the frozen route manifest",
                    ));
                }
            }
            // One transaction across every required root and every provider.
            // A rejected final graph leaves no earlier views or sequence gaps.
            {
                let mut state = self.state.lock().unwrap();
                let mut transaction = state.broker.clone();
                for (route, offer) in roots.iter().zip(&offers) {
                    let graph = transaction.offer_graph(
                        &route.owner,
                        self.bundles.service(&route.contract)?.as_ref(),
                        &crate::services::service_type(&route.contract),
                        &offer.draft,
                        &scopes,
                        &route.source,
                    )?;
                    graphs.insert(route.name.clone(), graph);
                }
                state.broker = transaction;
                for graph in graphs.values() {
                    Self::remember(&mut state, graph);
                }
                issued = true;
            }
            for (route, offer) in roots.iter().zip(&offers) {
                let graph = &graphs[&route.name];
                self.peer(&route.owner)?
                    .request(
                        "object/commit",
                        wire(Commit {
                            activation: route.owner.clone(),
                            stage: offer.stage,
                            deliveries: graph
                                .references
                                .iter()
                                .map(|r| r.delivery.clone())
                                .collect(),
                            scopes: scopes.clone(),
                        }),
                    )
                    .await?;
                self.pin_foreign(&offer.draft, graph).await?;
            }
            Ok(())
        }
        .await;
        // Only committed metadata can require recipient envelope rejection.
        // If the atomic transaction failed, graphs contains local candidates
        // but no grants; do not fabricate receipt evidence for those candidates.
        let mut errors = Vec::new();
        if result.is_err() && issued {
            for graph in graphs.values() {
                if let Err(error) = self.reject(graph).await {
                    errors.push(error);
                }
            }
        }
        for (route, offer) in roots.iter().zip(&offers) {
            match self.peer(&route.owner) {
                Ok(peer) => {
                    if let Err(error) = peer
                        .request(
                            "object/abort",
                            wire(SelectStage {
                                activation: route.owner.clone(),
                                stage: offer.stage,
                            }),
                        )
                        .await
                    {
                        errors.push(error);
                    }
                }
                Err(error) => errors.push(error),
            }
        }
        if !errors.is_empty() {
            // Even successful commits have not been exposed when staging
            // cleanup cannot be confirmed; reject the whole issued table.
            if result.is_ok() && issued {
                for graph in graphs.values() {
                    let _ = self.reject(graph).await;
                }
            }
            return Err(fail(
                ErrorCode::Unavailable,
                format!(
                    "prepared root cleanup failed: {}; handoff: {:?}",
                    errors[0],
                    result.err()
                ),
            ));
        }
        result.map(|_| graphs)
    }
    async fn handoff_services(
        &self,
        owner: &Activation,
        table: &ServiceTable,
        contracts: &BTreeMap<String, CatalogService>,
        recipient: &Activation,
    ) -> Result<BTreeMap<String, WireGraph>> {
        let scopes = GraphScopes::in_scope(root(recipient));
        let graphs = {
            let mut state = self.state.lock().unwrap();
            let graphs = offer_table(
                &mut state.broker,
                owner,
                table,
                contracts,
                &self.bundles,
                &scopes,
            )?;
            for graph in graphs.values() {
                Self::remember(&mut state, graph);
            }
            graphs
        };
        let result = async {
            self.peer(owner)?
                .request(
                    "object/services-commit",
                    wire(ServiceCommit {
                        activation: owner.clone(),
                        graphs: graphs.clone(),
                        scopes,
                    }),
                )
                .await?;
            for (name, graph) in &graphs {
                self.pin_foreign(&table.services[name].graph, graph).await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            for graph in graphs.values() {
                self.reject(graph).await?;
            }
            return Err(error);
        }
        Ok(graphs)
    }
    fn admit_call(
        self: &Arc<Self>,
        identity: &RuntimeIdentity,
        request: Call,
    ) -> Result<HandlerFuture> {
        let sender = request.target.recipient.activation.clone();
        identity.require(&sender)?;
        if request.input.activation != sender {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "call input belongs to another native member",
            ));
        }
        let owner = request.target.object.owner.clone();
        let contract = self.bundles.method(&request.target.view, &request.method)?;
        let (call, scopes, key, open, pin) = {
            let mut state = self.state.lock().unwrap();
            for activation in [&sender, &owner] {
                if !state.members.get(activation).is_some_and(|m| {
                    !m.closed
                        && (activation == &sender
                            || m.published
                            || request.target.view.interface.starts_with("$callback:"))
                }) {
                    return Err(fail(
                        ErrorCode::Unavailable,
                        "call member has not been published",
                    ));
                }
            }
            let peer = state
                .peers
                .get(&RuntimeIdentity {
                    runtime: owner.runtime.clone(),
                    epoch: owner.epoch,
                })
                .and_then(|p| p.peer.upgrade())
                .filter(|p| !p.is_closed())
                .ok_or_else(|| fail(ErrorCode::Unavailable, "owner session unavailable"))?;
            let call = Sequence(
                state
                    .calls
                    .get(&sender)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| fail(ErrorCode::Unavailable, "call sequence exhausted"))?,
            );
            let scope_id = Sequence(
                state
                    .scopes
                    .get(&owner)
                    .copied()
                    .unwrap_or(1)
                    .checked_add(1)
                    .ok_or_else(|| fail(ErrorCode::Unavailable, "scope sequence exhausted"))?,
            );
            let borrow = Scope {
                activation: owner.clone(),
                scope: scope_id,
            };
            state.broker.require_scope(&root(&owner))?;
            state.broker.begin_call(
                &request.target.recipient,
                call,
                &request.target,
                &request.method,
                &owner,
            )?;
            if let Err(error) = state.broker.open_scope(borrow.clone(), Some(root(&owner))) {
                state.broker.finish_call(&owner, &sender, call)?;
                return Err(error);
            }
            state.calls.insert(sender.clone(), call.0);
            state.scopes.insert(owner.clone(), scope_id.0);
            let key = PinKey::Execution {
                caller: sender.clone(),
                call,
            };
            // Queue metadata admission in stream order, ahead of any concurrent
            // later scope. The lock never spans business work or a peer await.
            let open = match peer.start_request("object/open", wire(&borrow)) {
                Ok(open) => open,
                Err(error) => {
                    state.broker.close_scope(&borrow);
                    state.broker.finish_call(&owner, &sender, call)?;
                    return Err(error);
                }
            };
            let pin = match peer.start_request(
                "object/pin",
                wire(Pin {
                    object: request.target.object.clone(),
                    key: key.clone(),
                }),
            ) {
                Ok(pin) => pin,
                Err(error) => {
                    state.broker.close_scope(&borrow);
                    state.broker.finish_call(&owner, &sender, call)?;
                    return Err(error);
                }
            };
            (
                call,
                GraphScopes {
                    scope: root(&owner),
                    borrow,
                },
                key,
                open,
                pin,
            )
        };
        let host = self.clone();
        Ok(Box::pin(async move {
            let mut output_stage = None;
            let result = async {
                open.await
                    .map_err(|_| fail(ErrorCode::Unavailable, "scope admission lost"))??;
                pin.await
                    .map_err(|_| fail(ErrorCode::Unavailable, "execution admission lost"))??;
                let hash = &request.target.view.bundle_sha256;
                let graph = host
                    .handoff(&sender, hash, &contract.params, &request.input, &scopes)
                    .await?;
                let output: StageOffer = decode(
                    host.peer(&owner)?
                        .request(
                            "object/execute",
                            wire(Execute {
                                object: request.target.object.clone(),
                                view: request.target.view.clone(),
                                method: request.method,
                                key: key.clone(),
                                graph,
                                scopes: scopes.clone(),
                            }),
                        )
                        .await?,
                )?;
                output_stage = Some(output.stage);
                host.handoff(
                    &owner,
                    hash,
                    &contract.result,
                    &output,
                    &GraphScopes::in_scope(request.target.recipient),
                )
                .await
            }
            .await;
            // ACK only after actual handler and descendants, including failure.
            // A lost owner session retains execution pins for the supervisor.
            let controls = decode(
                host.peer(&owner)?
                    .request(
                        "object/end",
                        wire(End {
                            activation: owner.clone(),
                            scope: scopes.borrow.clone(),
                            key,
                            stage: output_stage,
                        }),
                    )
                    .await?,
            )?;
            host.controls(
                &RuntimeIdentity {
                    runtime: owner.runtime.clone(),
                    epoch: owner.epoch,
                },
                controls,
            )
            .await?;
            {
                let mut state = host.state.lock().unwrap();
                state.broker.close_scope(&scopes.borrow);
                state.broker.finish_call(&owner, &sender, call)?;
            }
            result.map(wire)
        }))
    }
    pub fn close_member(self: &Arc<Self>, owner: &Activation) -> HandlerFuture {
        let (peers, hooks) = {
            let mut state = self.state.lock().unwrap();
            let mut hooks = Vec::new();
            if let Some(member) = state.members.get_mut(owner) {
                member.closed = true;
                member.published = false;
                hooks.extend(
                    member
                        .close_hooks
                        .iter()
                        .filter_map(std::sync::Weak::upgrade),
                );
                member.close_hooks.clear();
            }
            state.broker.close_activation(owner);
            let peers = state
                .peers
                .iter()
                .filter_map(|(id, p)| p.peer.upgrade().map(|p| (id.clone(), p)))
                .filter(|(_, peer)| !peer.is_closed())
                .collect::<Vec<_>>();
            (peers, hooks)
        };
        for hook in hooks {
            hook();
        }
        let host = self.clone();
        let requests = peers
            .into_iter()
            .map(|(identity, peer)| (identity, peer.start_request("object/revoke", wire(owner))))
            .collect::<Vec<_>>();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut errors = Vec::new();
            for (identity, request) in requests {
                let result = async {
                    let controls = decode(
                        request?
                            .await
                            .map_err(|_| fail(ErrorCode::Unavailable, "revocation ACK lost"))??,
                    )?;
                    host.controls(&identity, controls).await
                }
                .await;
                if let Err(error) = result {
                    errors.push(error);
                }
            }
            let result = if errors.is_empty() {
                Ok(Value::Null)
            } else {
                Err(fail(
                    ErrorCode::Unavailable,
                    format!("{} revocation failures: {}", errors.len(), errors[0]),
                ))
            };
            let _ = tx.send(result);
        });
        Box::pin(async move {
            rx.await
                .map_err(|_| fail(ErrorCode::Unavailable, "revocation task lost"))?
        })
    }

    pub fn disconnect(self: &Arc<Self>, identity: &RuntimeIdentity) {
        let (owners, peers, releases, hooks) = {
            let mut state = self.state.lock().unwrap();
            state.disconnected.insert(identity.clone());
            let mut hooks = state
                .runtime_hooks
                .remove(identity)
                .unwrap_or_default()
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .collect::<Vec<_>>();
            let releases = state
                .deliveries
                .iter()
                .filter(|((recipient, _), _)| recipient == identity)
                .map(|(_, d)| d.clone())
                .collect::<Vec<_>>();
            state.broker.close_epoch(&identity.runtime, identity.epoch);
            let owners = state
                .members
                .iter_mut()
                .filter_map(|(owner, member)| {
                    if identity.contains(owner) {
                        member.closed = true;
                        member.published = false;
                        hooks.extend(
                            member
                                .close_hooks
                                .iter()
                                .filter_map(std::sync::Weak::upgrade),
                        );
                        member.close_hooks.clear();
                        Some(owner.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            let peers = state
                .peers
                .iter()
                .filter(|(id, _)| *id != identity)
                .filter_map(|(id, p)| p.peer.upgrade().map(|p| (id.clone(), p)))
                .collect::<Vec<_>>();
            (owners, peers, releases, hooks)
        };
        for hook in hooks {
            hook();
        }
        let host = self.clone();
        let release_requests = releases
            .iter()
            .filter_map(|delivery| {
                host.peer(&delivery.object.owner)
                    .ok()
                    .filter(|p| !p.is_closed())
                    .map(|p| {
                        p.start_request(
                            "object/release",
                            wire(Release {
                                activation: delivery.object.owner.clone(),
                                key: PinKey::Delivery {
                                    recipient: delivery.recipient.activation.clone(),
                                    id: delivery.id,
                                },
                            }),
                        )
                    })
            })
            .collect::<Vec<_>>();
        tokio::spawn(async move {
            for request in release_requests.into_iter().flatten() {
                let _ = request.await;
            }
            for (identity, peer) in peers {
                for owner in &owners {
                    if let Ok(value) = peer.request("object/revoke", wire(owner)).await {
                        if let Ok(controls) = decode(value) {
                            let _ = host.controls(&identity, controls).await;
                        }
                    }
                }
            }
        });
    }
    pub fn pins(&self, object: &ObjectIdentity) -> (usize, usize) {
        self.state.lock().unwrap().broker.pins(object)
    }
}
fn wire_value_pin(delivery: &Delivery) -> Value {
    wire(Pin {
        object: delivery.object.clone(),
        key: PinKey::Delivery {
            recipient: delivery.recipient.activation.clone(),
            id: delivery.id,
        },
    })
}

/// Serve a service-capable lifecycle driver and its object actor together.
/// The embedding binary owns the native root and process supervision. Default
/// service-less Drivers continue rejecting object/event capabilities.
pub async fn serve<S>(
    stream: S,
    runner: Arc<crate::lifecycle::Runner>,
    objects: Arc<RuntimeObjects>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let peer = Peer::start(stream, objects.lifecycle_handler(runner.clone()));
    objects.attach(&peer)?;
    runner.attach(&peer)?;
    peer.closed().await;
    objects.close();
    let mut errors = Vec::new();
    for stopped in runner.close() {
        if let Err(error) = stopped.await {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(fail(
            ErrorCode::Business,
            format!("{} native cleanup failures: {}", errors.len(), errors[0]),
        ))
    }
}
