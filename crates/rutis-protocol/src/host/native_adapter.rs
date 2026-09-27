//! Host-owned native services share the graph's actual SDK and broker. The
//! original plugin, original generation Ctx, and native subtree own execution.
use super::*;
use crate::services::{NativeBindings, NativePorts};

impl HostGraph {
    /// Mount the actual native plugin with pure, export-only adapter metadata.
    /// Every declared port must match a frozen native_services entry exactly.
    pub fn mount_native(
        self: &Arc<Self>,
        parent: &Ctx,
        ports: Arc<NativePorts>,
        plugin: impl Plugin,
    ) -> Result<HostNative> {
        let names = ports.check_native(self.objects.plan().native_services())?;
        let gate = ActivationGate::default();
        let control = {
            let mut state = self.state.lock().unwrap();
            if names
                .iter()
                .any(|name| state.native_slots.contains_key(name))
            {
                return Err(fail(
                    ErrorCode::ScopeClosed,
                    "native adapter cannot rebind before recovery proof",
                ));
            }
            let owner = self.next_local(&mut state)?;
            self.objects.host().reserve(owner.clone())?;
            let receiver = match self.next_local(&mut state).and_then(|receiver| {
                self.objects.host().reserve(receiver.clone())?;
                Ok(receiver)
            }) {
                Ok(receiver) => receiver,
                Err(error) => {
                    drop(self.objects.host().close_member(&owner));
                    return Err(error);
                }
            };
            let (published, publication) = watch::channel(None);
            let bindings = ports.scope(parent, owner.clone(), gate.clone());
            let control = Arc::new(NativeControl {
                graph: self.clone(),
                owner,
                receiver,
                names,
                ports,
                gate: gate.clone(),
                available: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                stop_ready: tokio::sync::Notify::new(),
                stop: Mutex::new(None),
                ctx: Mutex::new(None),
                state: Mutex::new(NativeState {
                    bindings: Some(bindings),
                    ..Default::default()
                }),
                native: Mutex::new(Weak::new()),
                published,
                publication,
            });
            for name in &control.names {
                state
                    .native_slots
                    .insert(name.clone(), Arc::downgrade(&control));
            }
            control
        };
        let mounted = (|| {
            let weak = Arc::downgrade(&control);
            let hook: ClosedHook = Arc::new(move || {
                if let Some(control) = weak.upgrade() {
                    control.close_intent();
                }
            });
            self.objects
                .host()
                .on_runtime_close(&self.local_identity, &hook);
            self.objects.host().on_close(&control.owner, &hook)?;
            gate.on_close(&hook);
            control.state.lock().unwrap().hooks.push(hook);
            if !control.open() {
                return Err(fail(
                    ErrorCode::ScopeClosed,
                    "native adapter epoch is closed",
                ));
            }
            self.local.reserve(control.owner.clone(), gate.clone())?;
            self.local.reserve(control.receiver.clone(), gate.clone())?;
            let entering = control.clone();
            let enter: crate::managed::NativeEnter =
                Arc::new(move |ctx| entering.enter(ctx).map_err(cordis));
            let scoped = control
                .state
                .lock()
                .unwrap()
                .bindings
                .as_ref()
                .unwrap()
                .ctx()
                .clone();
            ManagedActivation::mount_enter(&scoped, plugin, gate, Some(enter))
                .map(Arc::new)
                .map_err(native)
        })();
        let member = match mounted {
            Ok(member) => member,
            Err(error) => {
                control.close_intent();
                return Err(error);
            }
        };
        *control.native.lock().unwrap() = Arc::downgrade(&member);
        let watching = control.clone();
        tokio::spawn(async move {
            watching.gate.revoked().await;
            watching.close_intent();
        });
        let publishing = control.clone();
        let native = member.clone();
        tokio::spawn(async move {
            let result = publishing.publish(&native).await;
            let result = if result.is_err() {
                match publishing.begin_stop().await {
                    Ok(()) => result,
                    Err(error) => Err(error),
                }
            } else {
                result
            };
            publishing.published.send_replace(Some(result));
        });
        Ok(HostNative {
            native: member,
            control,
        })
    }
}

#[derive(Default)]
struct NativeState {
    bindings: Option<NativeBindings>,
    hooks: Vec<ClosedHook>,
}
pub(super) struct NativeControl {
    graph: Arc<HostGraph>,
    owner: Activation,
    receiver: Activation,
    names: Vec<String>,
    ports: Arc<NativePorts>,
    gate: ActivationGate,
    available: AtomicBool,
    closed: AtomicBool,
    stop_ready: tokio::sync::Notify,
    stop: Mutex<Option<watch::Receiver<Option<Confirmation>>>>,
    ctx: Mutex<Option<Ctx>>,
    state: Mutex<NativeState>,
    native: Mutex<Weak<ManagedActivation>>,
    published: watch::Sender<Option<Publication>>,
    publication: watch::Receiver<Option<Publication>>,
}
impl NativeControl {
    pub(super) fn owner(&self) -> &Activation {
        &self.owner
    }
    fn open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && self.gate.is_open()
    }
    fn enter(self: &Arc<Self>, ctx: &Ctx) -> Result<()> {
        let cleanup = ctx.track_dependency_cleanup();
        self.graph
            .state
            .lock()
            .unwrap()
            .dependency_cleanup
            .push(cleanup);
        let cleanup = self.clone();
        ctx.effect_named("protocol native adapter cleanup", move || {
            Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move { cleanup.protocol_stopped().await.map_err(cordis) })
            }))
        })
        .map_err(native)?;
        let exports = Exports::managed(
            ctx,
            self.owner.clone(),
            self.graph.local.ids(),
            self.gate.clone(),
        )
        .map_err(native)?;
        self.graph.local.bind(&self.owner, ctx.clone(), exports)?;
        *self.ctx.lock().unwrap() = Some(ctx.clone());
        Ok(())
    }
    async fn publish(&self, member: &ManagedActivation) -> Publication {
        let mut states = member.view().watch();
        loop {
            let state = states.borrow_and_update().state;
            if !self.open() {
                return Err(fail(
                    ErrorCode::Unavailable,
                    "native adapter revoked before Active",
                ));
            }
            match state {
                FiberState::Active => break,
                FiberState::Failed | FiberState::Disposed => {
                    member.view().clone().await.map_err(native)?;
                    return Err(fail(
                        ErrorCode::Unavailable,
                        "native adapter did not become Active",
                    ));
                }
                _ => {}
            }
            tokio::select! {
                result = states.changed() => { result.map_err(|_| fail(ErrorCode::Unavailable, "native adapter state observer ended"))?; }
                _ = self.gate.revoked() => return Err(fail(ErrorCode::Unavailable, "native adapter revoked before Active")),
            }
        }
        // NativeBindings contains provider disposers and is only Send. Take it
        // out of metadata before encoding snapshots or mounting native guards.
        let bindings = self.state.lock().unwrap().bindings.take().unwrap();
        let ctx = member.native_context().unwrap();
        let staged = self.ports.stage(
            &bindings,
            self.graph.objects.bundles(),
            self.graph.local.exports(&self.owner)?,
        )?;
        for guard in self.ports.guard_context(&bindings, &ctx)? {
            (&guard).await.map_err(native)?;
            if guard.state().state != FiberState::Active || !self.open() {
                return Err(fail(
                    ErrorCode::Unavailable,
                    "native adapter export guard did not become Active",
                ));
            }
        }
        let table = self.graph.local.stage_services(staged)?;
        self.graph.objects.stage_native_mirrored(table.clone())?;
        let values = self
            .graph
            .objects
            .mirror_native(&self.owner, &self.names, self.receiver.clone())
            .await?
            .receive(&self.graph.local)
            .await?;
        let exports = Exports::managed(
            &ctx,
            self.receiver.clone(),
            self.graph.local.ids(),
            self.gate.clone(),
        )
        .map_err(native)?;
        self.graph
            .local
            .bind(&self.receiver, ctx.clone(), exports)?;
        for (name, value) in values {
            let DecodedValue::Object(proxy) = value else {
                return Err(fail(
                    ErrorCode::InterfaceMismatch,
                    "native mirror is not an object root",
                ));
            };
            let checked = self.available_check();
            let guarded = proxy.with_availability(checked.clone());
            ctx.provide_as_with_check(
                self.graph.objects.plan().native_key(&name)?,
                Arc::new(guarded),
                move || checked(),
            )
            .map_err(native)?;
        }
        if !self.open() || member.view().state().state != FiberState::Active {
            return Err(fail(
                ErrorCode::Unavailable,
                "native adapter revoked before publication",
            ));
        }
        self.graph.local.publish(&self.owner)?;
        self.graph.objects.host().publish(&self.owner)?;
        self.graph.local.publish(&self.receiver)?;
        self.graph.objects.host().publish(&self.receiver)?;
        self.available.store(true, Ordering::SeqCst);
        if !self.open() {
            return Err(fail(
                ErrorCode::Unavailable,
                "native adapter closed during publication",
            ));
        }
        ctx.refresh();
        Ok(Arc::new(table))
    }
    fn available_check(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        // The graph holds weak slots; this callback must not create a native
        // registration -> control -> scoped Ctx ownership cycle.
        let weak = self.graph.state.lock().unwrap().native_slots[&self.names[0]].clone();
        Arc::new(move || {
            weak.upgrade()
                .is_some_and(|control| control.available.load(Ordering::SeqCst) && control.open())
        })
    }
    fn close_intent(&self) {
        self.available.store(false, Ordering::SeqCst);
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.gate.close();
        let (tx, rx) = watch::channel(None);
        *self.stop.lock().unwrap() = Some(rx);
        self.stop_ready.notify_waiters();
        let revocations = [
            self.graph.objects.host().close_member(&self.owner),
            self.graph.objects.host().close_member(&self.receiver),
        ];
        if let Some(ctx) = self.ctx.lock().unwrap().clone() {
            ctx.refresh();
        }
        let actor = self.graph.local.clone();
        let owner = self.owner.clone();
        let receiver = self.receiver.clone();
        tokio::spawn(async move {
            // A gate hook may run during an SDK actor metadata check. Native
            // admission and Host authority are already sealed; SDK scope
            // draining must acquire that actor lock only after the hook returns.
            actor.close_member(&owner);
            actor.close_member(&receiver);
            let mut errors = Vec::new();
            for revocation in revocations {
                if let Err(error) = revocation.await {
                    errors.push(error);
                }
            }
            if let Err(error) = actor.flush().await {
                errors.push(error);
            }
            let result = match errors.len() {
                0 => Ok(()),
                1 => Err(errors.pop().unwrap()),
                _ => Err(fail(
                    ErrorCode::Unavailable,
                    format!(
                        "{} native adapter protocol cleanup failures: {}",
                        errors.len(),
                        errors[0]
                    ),
                )),
            };
            tx.send_replace(Some(result));
        });
    }
    async fn protocol_stopped(&self) -> Confirmation {
        self.close_intent();
        let mut rx = loop {
            let notified = self.stop_ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(rx) = self.stop.lock().unwrap().clone() {
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
                .map_err(|_| fail(ErrorCode::Unavailable, "native adapter cleanup task lost"))?;
        }
    }
    pub(super) fn begin_stop(self: &Arc<Self>) -> BoxFuture<'static, Result<()>> {
        self.close_intent();
        let native = self
            .native
            .lock()
            .unwrap()
            .upgrade()
            .map(|native| native.stop());
        let control = self.clone();
        Box::pin(async move {
            let protocol = control.protocol_stopped().await;
            let cleanup = match native {
                Some(cleanup) => cleanup.await.map_err(native_error),
                None => Ok(()),
            };
            protocol?;
            cleanup
        })
    }
}

/// The real native plugin owns its SDK exports, mirror slots and guards. Native
/// service loss closes it and every consumer captured on those prepared keys.
pub struct HostNative {
    native: Arc<ManagedActivation>,
    control: Arc<NativeControl>,
}
impl HostNative {
    pub fn native(&self) -> &ManagedActivation {
        &self.native
    }
    pub fn activation(&self) -> &Activation {
        &self.control.owner
    }
    pub fn is_available(&self) -> bool {
        self.control.available.load(Ordering::SeqCst) && self.control.open()
    }
    pub fn service<H: crate::sdk::ClientHandle>(&self, name: &str) -> Result<H> {
        if !self.is_available() || !self.control.names.iter().any(|provided| provided == name) {
            return Err(fail(
                ErrorCode::Unavailable,
                "native service is not published by this adapter",
            ));
        }
        let ctx = self
            .native
            .native_context()
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native adapter context not bound"))?;
        let proxy = ctx
            .get_as::<ObjectProxy>(self.control.graph.objects.plan().native_key(name)?)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "native mirror slot disappeared"))?;
        crate::sdk::bind(
            DecodedValue::Object(proxy.as_ref().clone()),
            self.control.graph.local.caller(&self.control.receiver),
        )
    }
    pub async fn ready(&self) -> Publication {
        let mut rx = self.control.publication.clone();
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                if result.is_ok() && !self.control.open() {
                    return Err(fail(ErrorCode::Unavailable, "native adapter is closed"));
                }
                return result;
            }
            rx.changed()
                .await
                .map_err(|_| fail(ErrorCode::Unavailable, "native publication task lost"))?;
        }
    }
    pub fn stop(&self) -> BoxFuture<'static, Result<()>> {
        self.control.begin_stop()
    }
}
impl Drop for HostNative {
    fn drop(&mut self) {
        drop(self.control.begin_stop());
    }
}
