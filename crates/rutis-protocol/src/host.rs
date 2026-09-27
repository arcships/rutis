//! Native Host dependency fibers for a frozen deployment. Actual ObjectProxy
//! values occupy prepare's TypeKeys; staging never opens their availability.
use crate::{
    deployment::DeploymentObjects,
    error::{ErrorCode, ProtocolError, Result},
    exports::Exports,
    frame::{Handler, HandlerFuture, Peer},
    graph::DecodedValue,
    identity::{Activation, Sequence},
    imports::ObjectProxy,
    lifecycle::{ready_key, Phase, RuntimeIdentity, RuntimeReady, Status},
    managed::{ActivationGate, ManagedActivation},
    services::ServiceTable,
    session::{RootDelivery, RuntimeIdentity as ObjectIdentity, RuntimeObjects},
};
use rutis::{
    BoxFuture, ConsumerCleanup, CordisError, Ctx, DependencyCleanup, Effect, FiberState, Plugin,
    TypeKey,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::watch;
mod native_adapter;
pub use native_adapter::HostNative;

fn fail(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, "host_proxy", message)
}
fn native(error: impl std::fmt::Display) -> ProtocolError {
    fail(ErrorCode::Business, error.to_string())
}
fn cordis(error: ProtocolError) -> CordisError {
    CordisError::PluginFailed(Box::new(error))
}
type ClosedHook = Arc<dyn Fn() + Send + Sync>;
type Confirmation = std::result::Result<(), ProtocolError>;
type Publication = std::result::Result<Arc<ServiceTable>, ProtocolError>;

#[derive(Default)]
struct GraphState {
    next: BTreeMap<ObjectIdentity, u64>,
    next_local: u64,
    slots: BTreeMap<String, Weak<Control>>,
    native_slots: BTreeMap<String, Weak<native_adapter::NativeControl>>,
    dependency_cleanup: Vec<DependencyCleanup>,
}
/// Construct proxies before RuntimeReady. Native injection keeps missing routes
/// Pending; no member start waits for the rest of its runtime group.
pub struct HostGraph {
    objects: Arc<DeploymentObjects>,
    local: Arc<RuntimeObjects>,
    local_identity: ObjectIdentity,
    broker_peer: Peer,
    sdk_peer: Peer,
    state: Mutex<GraphState>,
}
impl HostGraph {
    pub fn new(objects: Arc<DeploymentObjects>, identity: ObjectIdentity) -> Result<Arc<Self>> {
        if !crate::contract::identifier(&identity.runtime) || identity.epoch.0 == 0 {
            return Err(fail(
                ErrorCode::InvalidParams,
                "invalid Host SDK epoch identity",
            ));
        }
        let local = RuntimeObjects::new(identity.clone(), objects.bundles());
        let (broker_stream, sdk_stream) = tokio::io::duplex(65536);
        let reject: Handler = Arc::new(|_, _| {
            Box::pin(async {
                Err(fail(
                    ErrorCode::UnsupportedCapability,
                    "unknown Host SDK operation",
                ))
            })
        });
        let broker_peer = Peer::start(
            broker_stream,
            objects.host().handler(identity.clone(), reject.clone()),
        );
        objects.host().attach(identity.clone(), &broker_peer)?;
        let sdk_peer = Peer::start(sdk_stream, local.handler(reject));
        local.attach(&sdk_peer)?;
        Ok(Arc::new(Self {
            objects,
            local,
            local_identity: identity,
            broker_peer,
            sdk_peer,
            state: Mutex::default(),
        }))
    }
    pub fn objects(&self) -> &Arc<DeploymentObjects> {
        &self.objects
    }
    /// Actual consumer generation drains, including failures whose native
    /// dependency edges have already disappeared. Join only after all affected
    /// providers' native cleanup; an early empty list is not a recovery proof.
    pub fn consumer_cleanup(&self) -> Vec<ConsumerCleanup> {
        let observations = self.state.lock().unwrap().dependency_cleanup.clone();
        let mut consumers = BTreeMap::new();
        for observation in observations {
            for consumer in observation.consumers() {
                consumers
                    .entry((consumer.id(), consumer.generation()))
                    .or_insert(consumer);
            }
        }
        consumers.into_values().collect()
    }
    /// Identity is the supervisor's successful hello identity, not configuration.
    pub fn mount(
        self: &Arc<Self>,
        parent: &Ctx,
        instance: &str,
        identity: RuntimeIdentity,
    ) -> Result<HostProxy> {
        let prepared = self
            .objects
            .plan()
            .instances()
            .get(instance)
            .ok_or_else(|| fail(ErrorCode::InvalidParams, "unknown prepared instance"))?;
        let group = &self.objects.plan().groups()[prepared.group()];
        if identity.runtime == self.local_identity.runtime
            || identity.epoch.0 == 0
            || !crate::contract::identifier(&identity.runtime)
            || identity.kind != *group.kind()
            || identity.framework_version != group.framework_version()
            || identity.environment_sha256 != group.environment_sha256()
            || identity.code_sha256 != group.code_sha256()
            || identity.capabilities != *group.capabilities()
        {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "Host runtime identity differs from frozen group",
            ));
        }
        let gate = ActivationGate::default();
        let (published, publication) = watch::channel(None);
        let control = Arc::new(Control {
            graph: self.clone(),
            instance: instance.into(),
            identity,
            gate: gate.clone(),
            available: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            stop_ready: tokio::sync::Notify::new(),
            state: Mutex::default(),
            native: Mutex::new(Weak::new()),
            published,
            publication,
        });
        {
            let mut state = self.state.lock().unwrap();
            if state.slots.contains_key(instance) {
                return Err(fail(
                    ErrorCode::ScopeClosed,
                    "Host proxy cannot rebind before recovery proof",
                ));
            }
            state
                .slots
                .insert(instance.into(), Arc::downgrade(&control));
        }
        let mut injects = vec![ready_key(&control.identity)];
        for identity in [
            ObjectIdentity {
                runtime: control.identity.runtime.clone(),
                epoch: control.identity.epoch,
            },
            self.local_identity.clone(),
        ] {
            let weak = Arc::downgrade(&control);
            let hook: ClosedHook = Arc::new(move || {
                if let Some(control) = weak.upgrade() {
                    control.close_intent();
                }
            });
            self.objects.host().on_runtime_close(&identity, &hook);
            control.state.lock().unwrap().hooks.push(hook);
        }
        if !control.open() {
            return Err(fail(ErrorCode::ScopeClosed, "Host runtime epoch is closed"));
        }
        injects.extend(prepared.routes().values().map(|route| route.key().clone()));
        let mut seen = HashSet::new();
        injects.retain(|key| seen.insert(key.clone()));
        let member = Arc::new(
            ManagedActivation::mount_gated(
                parent,
                ProxyPlugin {
                    control: control.clone(),
                    injects,
                },
                gate,
            )
            .map_err(native)?,
        );
        *control.native.lock().unwrap() = Arc::downgrade(&member);
        let watched = control.clone();
        tokio::spawn(async move {
            watched.gate.revoked().await;
            watched.close_intent();
        });
        let publishing = control.clone();
        let mounted = member.clone();
        tokio::spawn(async move {
            let result = publishing.publish(&mounted).await;
            if result.is_err() {
                publishing.close_intent();
                let stop = publishing.remote_stopped().await;
                let cleanup = mounted.stop().await;
                let result = match (stop, cleanup) {
                    (Err(error), _) => Err(error),
                    (_, Err(error)) => Err(native(error)),
                    _ => publishing
                        .state
                        .lock()
                        .unwrap()
                        .start_error
                        .take()
                        .map_or(result, Err),
                };
                publishing.published.send_replace(Some(result));
            } else {
                publishing.published.send_replace(Some(result));
            }
        });
        Ok(HostProxy {
            native: member,
            control,
        })
    }
    fn reserve(
        &self,
        instance: &str,
        identity: &RuntimeIdentity,
    ) -> Result<(Activation, Activation)> {
        let runtime = ObjectIdentity {
            runtime: identity.runtime.clone(),
            epoch: identity.epoch,
        };
        // Keep sequence allocation and broker admission in the same order.
        // Different native fibers can enter concurrently, including runtimes
        // sharing the Host SDK epoch's monotonically increasing local IDs.
        let mut state = self.state.lock().unwrap();
        let (remote, local) = {
            let next = state.next.entry(runtime.clone()).or_default();
            *next = next
                .checked_add(1)
                .ok_or_else(|| fail(ErrorCode::Unavailable, "activation sequence exhausted"))?;
            let remote = Activation {
                runtime: runtime.runtime,
                epoch: runtime.epoch,
                activation: Sequence(*next),
            };
            let local = self.next_local(&mut state)?;
            (remote, local)
        };
        self.objects.reserve(instance, remote.clone())?;
        if let Err(error) = self.objects.host().reserve(local.clone()) {
            // No close lease or start exists yet for this fresh remote ID.
            // A failed second reservation must not leave it callable/alive.
            drop(self.objects.host().close_member(&remote));
            return Err(error);
        }
        Ok((remote, local))
    }
    fn next_local(&self, state: &mut GraphState) -> Result<Activation> {
        state.next_local = state
            .next_local
            .checked_add(1)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "Host SDK sequence exhausted"))?;
        Ok(Activation {
            runtime: self.local_identity.runtime.clone(),
            epoch: self.local_identity.epoch,
            activation: Sequence(state.next_local),
        })
    }
    /// Both intents start before any native drain await. Process supervision is
    /// independent and must retain its snapshot through reaping and this join.
    pub async fn shutdown(&self) -> Result<()> {
        let controls = self
            .state
            .lock()
            .unwrap()
            .slots
            .values()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        let adapters = self
            .state
            .lock()
            .unwrap()
            .native_slots
            .values()
            .filter_map(Weak::upgrade)
            .map(|control| (control.owner().clone(), control))
            .collect::<BTreeMap<_, _>>();
        let adapter_stops = adapters
            .values()
            .map(|control| control.begin_stop())
            .collect::<Vec<_>>();
        let stops = controls
            .iter()
            .map(|control| {
                control.close_intent();
                control
                    .native
                    .lock()
                    .unwrap()
                    .upgrade()
                    .map(|member| member.stop())
            })
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for (control, stop) in controls.iter().zip(stops) {
            if let Err(error) = control.remote_stopped().await {
                errors.push(error);
            }
            if let Some(stop) = stop {
                if let Err(error) = stop.await {
                    errors.push(native(error));
                }
            }
        }
        for stop in adapter_stops {
            if let Err(error) = stop.await {
                errors.push(error);
            }
        }
        let observations = self.state.lock().unwrap().dependency_cleanup.clone();
        for observation in observations {
            if let Err(error) = observation.wait_provider().await {
                errors.push(native(error));
            }
        }
        // Native providers have finished draining all registrations, so no
        // further bound consumer cleanup can be missed by this snapshot.
        for consumer in self.consumer_cleanup() {
            if let Err(error) = consumer.wait().await {
                errors.push(fail(
                    ErrorCode::Business,
                    format!(
                        "native consumer {:?} generation {} ({}) cleanup failed: {error}",
                        consumer.id(),
                        consumer.generation(),
                        consumer.name(),
                    ),
                ));
            }
        }
        if !errors.is_empty() {
            return Err(fail(
                ErrorCode::Unavailable,
                format!("{} proxy cleanup failures: {}", errors.len(), errors[0]),
            ));
        }
        self.sdk_peer.close(fail(
            ErrorCode::Unavailable,
            "Host native cleanup confirmed",
        ));
        self.broker_peer.close(fail(
            ErrorCode::Unavailable,
            "Host native cleanup confirmed",
        ));
        Ok(())
    }
}

#[derive(Default)]
struct ControlState {
    remote: Option<Activation>,
    local: Option<Activation>,
    peer: Option<Peer>,
    start_queued: bool,
    stop: Option<watch::Receiver<Option<Confirmation>>>,
    local_stop: Option<watch::Receiver<Option<Confirmation>>>,
    table: Option<Arc<ServiceTable>>,
    start_error: Option<ProtocolError>,
    ctx: Option<Ctx>,
    captures: Vec<Arc<ObjectProxy>>,
    hooks: Vec<ClosedHook>,
}
#[derive(Clone, Copy)]
enum StopPart {
    Local,
    Paired,
}
struct Control {
    graph: Arc<HostGraph>,
    instance: String,
    identity: RuntimeIdentity,
    gate: ActivationGate,
    available: AtomicBool,
    closed: AtomicBool,
    state: Mutex<ControlState>,
    stop_ready: tokio::sync::Notify,
    native: Mutex<Weak<ManagedActivation>>,
    published: watch::Sender<Option<Publication>>,
    publication: watch::Receiver<Option<Publication>>,
}
impl Control {
    fn open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && self.gate.is_open()
    }
    fn close_intent(&self) {
        self.available.store(false, Ordering::SeqCst);
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.gate.close();
        let (tx, local_tx, stopping, remote, local, ctx) = {
            let mut state = self.state.lock().unwrap();
            let (tx, rx) = watch::channel(None);
            state.stop = Some(rx);
            let (local_tx, local_rx) = watch::channel(None);
            state.local_stop = Some(local_rx);
            let stopping: BoxFuture<'static, Confirmation> = if state.start_queued {
                let request = state.peer.as_ref().unwrap().start_request(
                    "plugin/stop",
                    json!({"activation":state.remote.as_ref().unwrap()}),
                );
                let activation = state.remote.clone().unwrap();
                let instance = self.instance.clone();
                Box::pin(async move {
                    let value = request?
                        .await
                        .map_err(|_| fail(ErrorCode::Unavailable, "stop ACK lost"))??;
                    let status: Status = serde_json::from_value(value).map_err(native)?;
                    if status.activation != activation
                        || status.instance != instance
                        || status.phase != Phase::Stopped
                    {
                        return Err(fail(
                            ErrorCode::InterfaceMismatch,
                            "stop ACK differs from paired member",
                        ));
                    }
                    Ok(())
                })
            } else {
                Box::pin(async { Ok(()) })
            };
            (
                tx,
                local_tx,
                stopping,
                state.remote.clone(),
                state.local.clone(),
                state.ctx.clone(),
            )
        };
        self.stop_ready.notify_waiters();
        // Host native effects may still be Loading. Stop is already queued;
        // revocation and native drain cannot postpone its transmission.
        let mut revocations = Vec::new();
        if let Some(remote) = remote {
            revocations.push(self.graph.objects.host().close_member(&remote));
        }
        if let Some(local) = local {
            revocations.push(self.graph.objects.host().close_member(&local));
        }
        if let Some(ctx) = ctx {
            ctx.refresh();
        }
        let actor = self.graph.local.clone();
        let local = self.state.lock().unwrap().local.clone();
        tokio::spawn(async move {
            // A Host-native owner gate can revoke consumers from inside this
            // SDK actor's metadata check. Gates and broker authority are already
            // sealed; acquire the actor lock only after that callback returns.
            if let Some(local) = local {
                actor.close_member(&local);
            }
            let mut errors = Vec::new();
            for revocation in revocations {
                if let Err(error) = revocation.await {
                    errors.push(error);
                }
            }
            if let Err(error) = actor.flush().await {
                errors.push(error);
            }
            let local = match errors.len() {
                0 => Ok(()),
                1 => Err(errors.pop().unwrap()),
                _ => Err(fail(
                    ErrorCode::Unavailable,
                    format!(
                        "{} local proxy cleanup failures: {}",
                        errors.len(),
                        errors[0]
                    ),
                )),
            };
            // Native drain owns the local SDK cleanup, independently of the
            // remote physical confirmation. Stop was synchronously queued.
            local_tx.send_replace(Some(local.clone()));
            let result = match (local, stopping.await) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                (Err(local), Err(remote)) => Err(fail(
                    ErrorCode::Unavailable,
                    format!("paired cleanup failures: local: {local}; remote: {remote}"),
                )),
            };
            tx.send_replace(Some(result));
        });
    }
    async fn remote_stopped(&self) -> Confirmation {
        self.confirm_stop(StopPart::Paired).await
    }
    async fn local_stopped(&self) -> Confirmation {
        self.confirm_stop(StopPart::Local).await
    }
    async fn confirm_stop(&self, part: StopPart) -> Confirmation {
        self.close_intent();
        let mut rx = loop {
            let notified = self.stop_ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let rx = {
                let state = self.state.lock().unwrap();
                match part {
                    StopPart::Local => state.local_stop.clone(),
                    StopPart::Paired => state.stop.clone(),
                }
            };
            if let Some(rx) = rx {
                break rx;
            }
            notified.await;
        };
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                return result;
            }
            rx.changed()
                .await
                .map_err(|_| fail(ErrorCode::Unavailable, "stop confirmation task lost"))?;
        }
    }
    fn hook(self: &Arc<Self>, owner: &Activation) -> Result<()> {
        let control = Arc::downgrade(self);
        let hook: ClosedHook = Arc::new(move || {
            if let Some(control) = control.upgrade() {
                control.close_intent();
            }
        });
        self.graph.objects.host().on_close(owner, &hook)?;
        self.state.lock().unwrap().hooks.push(hook);
        Ok(())
    }
    fn begin_start(&self, delivery: RootDelivery) -> Result<HandlerFuture> {
        let response = {
            let mut state = self.state.lock().unwrap();
            if !self.open() {
                Err(fail(
                    ErrorCode::Cancelled,
                    "Host proxy stopped before start",
                ))
            } else {
                let result = delivery.queue_start(&self.instance);
                if result.is_ok() {
                    state.start_queued = true;
                }
                result
            }
        };
        // Receipt Drop may invoke the Host close hook. It must run after the
        // pairing lock is released, including a failed stream enqueue.
        response.map(|response| delivery.start_response(response))
    }
    async fn enter(self: &Arc<Self>, ctx: &Ctx) -> Result<()> {
        let cleanup = ctx.track_dependency_cleanup();
        self.graph
            .state
            .lock()
            .unwrap()
            .dependency_cleanup
            .push(cleanup);
        // Rollback is a real effect of this original native generation, and is
        // registered before issuance or a request can create remote ownership.
        let cleanup = self.clone();
        ctx.effect_named("protocol proxy local cleanup", move || {
            Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move { cleanup.local_stopped().await.map_err(cordis) })
            }))
        })
        .map_err(native)?;
        let ready = ctx
            .get_as::<RuntimeReady>(ready_key(&self.identity))
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native RuntimeReady disappeared"))?;
        if ready.identity() != &self.identity || ready.peer().is_closed() {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "native RuntimeReady differs",
            ));
        }
        let prepared = &self.graph.objects.plan().instances()[&self.instance];
        let mut captures = Vec::new();
        for (name, route) in prepared.routes() {
            let value = ctx
                .get_as::<ObjectProxy>(route.key().clone())
                .ok_or_else(|| {
                    fail(
                        ErrorCode::Unavailable,
                        "native required binding disappeared",
                    )
                })?;
            let proof = value.delivery()?;
            let contract = route.contract();
            if proof.view.interface != contract.interface
                || proof.view.bundle_sha256 != contract.bundle_sha256
            {
                return Err(fail(
                    ErrorCode::InterfaceMismatch,
                    "captured native root differs",
                ));
            }
            self.graph
                .objects
                .capture_required(&self.instance, name, &proof)?;
            captures.push(value);
        }
        if !self.open() {
            return Err(fail(
                ErrorCode::Cancelled,
                "Host proxy stopped before reservation",
            ));
        }
        let (remote, local) = {
            let mut state = self.state.lock().unwrap();
            if !self.open() {
                return Err(fail(
                    ErrorCode::Cancelled,
                    "Host proxy stopped before reservation",
                ));
            }
            let (remote, local) = self.graph.reserve(&self.instance, &self.identity)?;
            state.remote = Some(remote.clone());
            state.local = Some(local.clone());
            state.peer = Some(ready.peer().clone());
            state.ctx = Some(ctx.clone());
            state.captures = captures;
            (remote, local)
        };
        self.graph.local.reserve(local.clone(), self.gate.clone())?;
        self.hook(&remote)?;
        let captures = self.state.lock().unwrap().captures.clone();
        let objects = captures
            .iter()
            .map(|root| root.delivery().map(|proof| proof.object))
            .collect::<Result<Vec<_>>>()?;
        for object in objects {
            self.hook(&object.owner)?;
            let weak = Arc::downgrade(self);
            let hook: ClosedHook = Arc::new(move || {
                if let Some(control) = weak.upgrade() {
                    control.close_intent();
                }
            });
            self.graph.objects.host().on_object_close(&object, &hook)?;
            self.state.lock().unwrap().hooks.push(hook);
        }
        let weak = Arc::downgrade(self);
        let hook: ClosedHook = Arc::new(move || {
            if let Some(control) = weak.upgrade() {
                control.close_intent();
            }
        });
        ready.peer().on_close(&hook);
        self.state.lock().unwrap().hooks.push(hook);
        let table: ServiceTable = serde_json::from_value(
            self.begin_start(self.graph.objects.offer_required(&self.instance).await?)?
                .await?,
        )
        .map_err(native)?;
        if !self.open() {
            return Err(fail(
                ErrorCode::Cancelled,
                "Host proxy stopped before ready table",
            ));
        }
        self.graph.objects.stage(&self.instance, table.clone())?;
        let values = self
            .graph
            .objects
            .mirror(&self.instance, local.clone())
            .await?
            .receive(&self.graph.local)
            .await?;
        let exports = Exports::managed(
            ctx,
            local.clone(),
            self.graph.local.ids(),
            self.gate.clone(),
        )
        .map_err(native)?;
        self.graph.local.bind(&local, ctx.clone(), exports)?;
        for (name, value) in values {
            let DecodedValue::Object(value) = value else {
                return Err(fail(
                    ErrorCode::InterfaceMismatch,
                    "Host export is not an object root",
                ));
            };
            let key = self
                .graph
                .objects
                .plan()
                .export_key(&self.instance, &name)?;
            let control = Arc::downgrade(self);
            let check: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                control.upgrade().is_some_and(|control| {
                    control.available.load(Ordering::SeqCst) && control.open()
                })
            });
            let guarded = value.with_availability(check.clone());
            ctx.provide_as_with_check(key, Arc::new(guarded), move || check())
                .map_err(native)?;
        }
        self.state.lock().unwrap().table = Some(Arc::new(table));
        Ok(())
    }
    async fn publish(&self, member: &ManagedActivation) -> Publication {
        // FiberView await settles an initial missing dependency as Pending.
        // That is a valid native graph node, not a failed ready handshake.
        let mut states = member.view().watch();
        loop {
            let state = states.borrow_and_update().state;
            if !self.open() {
                return Err(fail(
                    ErrorCode::Unavailable,
                    "Host native proxy revoked before Active",
                ));
            }
            match state {
                FiberState::Active => break,
                FiberState::Failed | FiberState::Disposed => {
                    member.view().clone().await.map_err(native)?;
                    return Err(fail(
                        ErrorCode::Unavailable,
                        "Host native proxy did not become Active",
                    ));
                }
                _ => {}
            }
            tokio::select! {
                result = states.changed() => { result.map_err(|_| fail(ErrorCode::Unavailable, "Host native state observer ended"))?; }
                _ = self.gate.revoked() => return Err(fail(ErrorCode::Unavailable, "Host native proxy revoked before Active")),
            }
        }
        let (peer, remote, local, table, ctx) = {
            let state = self.state.lock().unwrap();
            (
                state.peer.clone().unwrap(),
                state.remote.clone().unwrap(),
                state.local.clone().unwrap(),
                state.table.clone().unwrap(),
                state.ctx.clone().unwrap(),
            )
        };
        let status: Status = serde_json::from_value(
            peer.request("plugin/activate", json!({"activation":remote}))
                .await?,
        )
        .map_err(native)?;
        if status.activation != remote
            || status.instance != self.instance
            || status.phase != Phase::Published
        {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "activate ACK differs from paired member",
            ));
        }
        // Pairing identities are immutable. Each broker/SDK publish rejects a
        // closed member, and the final native availability also checks the
        // one-shot gate. Do not hold the pairing lock across SDK gate callbacks.
        if !self.open() || member.view().state().state != FiberState::Active {
            return Err(fail(
                ErrorCode::Unavailable,
                "Host proxy revoked before activate ACK",
            ));
        }
        self.graph.objects.publish(&self.instance)?;
        self.graph.local.publish(&local)?;
        self.graph.objects.host().publish(&local)?;
        self.available.store(true, Ordering::SeqCst);
        if !self.open() {
            return Err(fail(
                ErrorCode::Unavailable,
                "Host proxy closed during publication",
            ));
        }
        ctx.refresh();
        Ok(table)
    }
}
struct ProxyPlugin {
    control: Arc<Control>,
    injects: Vec<TypeKey>,
}
impl Plugin for ProxyPlugin {
    fn name(&self) -> &str {
        "protocol-host-proxy"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            match self.control.enter(ctx).await {
                Ok(()) => Ok(Effect::Done),
                Err(error) if !self.control.open() => {
                    // A paired stop can cancel the start response while this
                    // native apply is Loading. Preserve its admission failure
                    // for ready(), while native shutdown joins the registered
                    // cleanup effect independently of that start failure.
                    self.control.state.lock().unwrap().start_error = Some(error);
                    Ok(Effect::Done)
                }
                Err(error) => Err(cordis(error)),
            }
        })
    }
}
pub struct HostProxy {
    native: Arc<ManagedActivation>,
    control: Arc<Control>,
}
impl HostProxy {
    pub fn native(&self) -> &ManagedActivation {
        &self.native
    }
    pub fn activation(&self) -> Option<Activation> {
        self.control.state.lock().unwrap().remote.clone()
    }
    pub fn captured(&self) -> Vec<Arc<ObjectProxy>> {
        self.control.state.lock().unwrap().captures.clone()
    }
    pub fn is_available(&self) -> bool {
        self.control.available.load(Ordering::SeqCst) && self.control.open()
    }
    /// Generated local client over the actual native ObjectProxy slot. Its
    /// immutable SDK scope and caller keep old references bound to this member.
    pub fn service<H: crate::sdk::ClientHandle>(&self, name: &str) -> Result<H> {
        if !self.is_available() {
            return Err(fail(
                ErrorCode::Unavailable,
                "Host service is not published",
            ));
        }
        let key = self
            .control
            .graph
            .objects
            .plan()
            .export_key(&self.control.instance, name)?;
        let ctx = self
            .native
            .native_context()
            .ok_or_else(|| fail(ErrorCode::Unavailable, "Host original context is not bound"))?;
        let value = ctx
            .get_as::<ObjectProxy>(key)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "Host native service disappeared"))?;
        let local = self.control.state.lock().unwrap().local.clone().unwrap();
        crate::sdk::bind(
            DecodedValue::Object(value.as_ref().clone()),
            self.control.graph.local.caller(&local),
        )
    }
    pub async fn ready(&self) -> Publication {
        let mut rx = self.control.publication.clone();
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                if result.is_ok() && !self.control.open() {
                    return Err(fail(ErrorCode::Unavailable, "Host proxy is closed"));
                }
                return result;
            }
            rx.changed()
                .await
                .map_err(|_| fail(ErrorCode::Unavailable, "publication task lost"))?;
        }
    }
    pub fn stop(&self) -> BoxFuture<'static, Result<()>> {
        self.control.close_intent();
        let native = self.native.stop();
        let control = self.control.clone();
        Box::pin(async move {
            let remote = control.remote_stopped().await;
            let native = native.await.map_err(native_error);
            remote?;
            native
        })
    }
}
fn native_error(error: Arc<CordisError>) -> ProtocolError {
    native(error)
}
impl Drop for HostProxy {
    fn drop(&mut self) {
        self.control.close_intent();
        drop(self.native.stop());
    }
}
