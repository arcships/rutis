//! One host activation owns one native fiber. The permit uses native generation
//! cancellation synchronously; dependency restoration cannot reopen that permit.
use rutis::{
    BoxFuture, CordisError, Ctx, Disposer, Effect, FiberView, Plugin, PluginFactory, TypeKey,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::{
    runtime::Handle,
    sync::{watch, Notify},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct GateState {
    entered: bool,
    closed: bool,
    tokens: Vec<CancellationToken>,
    native: Vec<Weak<FiberView>>,
}

/// Admission for an activation and its necessary exports. Never reopened.
#[derive(Clone, Default)]
pub struct ActivationGate(Arc<Mutex<GateState>>, CancellationToken, Arc<Notify>);

impl ActivationGate {
    pub(crate) fn same_activation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn is_open(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.tokens.iter().any(CancellationToken::is_cancelled) {
            state.closed = true;
        }
        let closed = state.closed;
        drop(state);
        if closed {
            self.close();
        }
        !closed
    }

    pub fn close(&self) {
        let (tokens, native) = {
            let mut state = self.0.lock().unwrap();
            state.closed = true;
            (state.tokens.clone(), state.native.clone())
        };
        // Signal existing execution before closing native registration below.
        for token in tokens {
            token.cancel();
        }
        self.1.cancel();
        // Cancellation alone is not native registration closure. The public
        // shutdown operation synchronously closes effects and child mounts;
        // its independent native task remains owned by the kernel.
        for view in native.into_iter().filter_map(|view| view.upgrade()) {
            view.seal_effects();
            drop(view.shutdown());
        }
    }

    fn bind_native(&self, view: &Arc<FiberView>) {
        let closed = {
            let mut state = self.0.lock().unwrap();
            state.native.push(Arc::downgrade(view));
            state.closed
        };
        self.2.notify_waiters();
        if closed {
            view.seal_effects();
            drop(view.shutdown());
        }
    }

    /// Necessary services exported by an internal child share the root's
    /// activation, but retain the child's native cancellation boundary.
    pub fn track_export(&self, ctx: &Ctx) -> Result<(), CordisError> {
        let token = ctx.cancellation_token();
        let closed = {
            let mut state = self.0.lock().unwrap();
            state.tokens.push(token.clone());
            state.closed || token.is_cancelled()
        };
        if closed {
            self.close();
            return Err(CordisError::InactiveEffect);
        }
        self.observe(ctx)
    }

    /// A necessary named service owns a native dependency guard. The runner
    /// must await this guard becoming Active before publishing the export.
    /// Native service removal pre-cancels the guard even if the provider fiber
    /// itself stays Active, so direct disposal cannot leave a callable export.
    pub fn track_service(&self, ctx: &Ctx, key: TypeKey) -> FiberView {
        ctx.plugin(ExportGuard {
            gate: self.clone(),
            injects: vec![key],
        })
    }

    pub async fn revoked(&self) {
        self.1.cancelled().await;
    }

    fn observe(&self, ctx: &Ctx) -> Result<(), CordisError> {
        let token = ctx.cancellation_token();
        let gate = self.clone();
        let refreshed = ctx.clone();
        let handle = ctx.handle().clone();
        ctx.effect(move || {
            let task = handle.spawn(async move {
                tokio::select! {
                    _ = token.cancelled() => { gate.close(); refreshed.refresh(); }
                    _ = gate.revoked() => { refreshed.refresh(); }
                }
            });
            Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move {
                    task.await
                        .map_err(|e| CordisError::PluginFailed(Box::new(e)))
                })
            }))
        })?;
        Ok(())
    }

    async fn enter(&self, ctx: &Ctx) -> Result<(), CordisError> {
        // Native tasks may run on another worker before ctx.plugin() returns
        // its view. Do not expose a business context until close() can also
        // close its native registration synchronously.
        loop {
            let bound = self.2.notified();
            tokio::pin!(bound);
            bound.as_mut().enable();
            let ready = {
                let state = self.0.lock().unwrap();
                if state.closed {
                    return Err(CordisError::InactiveEffect);
                }
                state.native.iter().any(|view| view.upgrade().is_some())
            };
            if ready {
                break;
            }
            tokio::select! {
                _ = bound => {}
                _ = self.revoked() => return Err(CordisError::InactiveEffect),
            }
        }
        let mut state = self.0.lock().unwrap();
        if state.closed || state.entered {
            return Err(CordisError::InactiveEffect);
        }
        state.entered = true;
        state.tokens.push(ctx.cancellation_token());
        drop(state);
        self.observe(ctx)
    }
}

struct Permit;

struct ExportGuard {
    gate: ActivationGate,
    injects: Vec<TypeKey>,
}
impl Plugin for ExportGuard {
    fn name(&self) -> &str {
        "protocol-export-guard"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            self.gate.track_export(ctx)?;
            Ok(Effect::Done)
        })
    }
}

struct ValidationAdmission(ActivationGate, bool);
impl Drop for ValidationAdmission {
    fn drop(&mut self) {
        if !self.1 {
            self.0.close();
        }
    }
}

struct ManagedPlugin {
    plugin: Arc<dyn Plugin>,
    gate: ActivationGate,
    injects: Vec<TypeKey>,
    context: watch::Sender<Option<Ctx>>,
    enter: Option<NativeEnter>,
}

impl Plugin for ManagedPlugin {
    fn name(&self) -> &str {
        self.plugin.name()
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn validate(&self) -> Result<(), CordisError> {
        let mut guard = ValidationAdmission(self.gate.clone(), false);
        let result = self.plugin.validate();
        guard.1 = result.is_ok();
        result
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            self.gate.enter(ctx).await?;
            self.context.send_replace(Some(ctx.clone()));
            if let Some(enter) = &self.enter {
                enter(ctx)?;
            }
            self.plugin.apply(ctx).await
        })
    }
}

struct ManagedFactory<F> {
    factory: F,
    name: String,
    gate: ActivationGate,
    injects: Vec<TypeKey>,
    context: watch::Sender<Option<Ctx>>,
    enter: Option<NativeEnter>,
}
impl<F: PluginFactory<C>, C: Send + Sync + 'static> PluginFactory<C> for ManagedFactory<F> {
    fn name(&self) -> &str {
        &self.name
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn validate_config(&self, _: &C) -> Result<(), CordisError> {
        Err(CordisError::Validation {
            issues: vec!["managed configuration updates require a new host activation".into()],
        })
    }
    fn build(&self, config: &C) -> Result<Box<dyn Plugin>, CordisError> {
        let mut guard = ValidationAdmission(self.gate.clone(), false);
        let plugin = self.factory.build(config)?;
        guard.1 = true;
        Ok(Box::new(ManagedPlugin {
            plugin: Arc::from(plugin),
            gate: self.gate.clone(),
            injects: self.injects.clone(),
            context: self.context.clone(),
            enter: self.enter.clone(),
        }))
    }
}

/// An activation cannot be restarted. The host stops it and creates a new
/// activation with freshly installed, isolated dependencies and a new plugin.
type StopResult = Result<(), Arc<CordisError>>;
pub(crate) type NativeEnter = Arc<dyn Fn(&Ctx) -> Result<(), CordisError> + Send + Sync>;

pub struct ManagedActivation {
    view: Arc<FiberView>,
    gate: ActivationGate,
    resources: Mutex<Vec<Disposer>>,
    context: watch::Receiver<Option<Ctx>>,
    handle: Handle,
    stop: Mutex<Option<watch::Receiver<Option<StopResult>>>>,
}

impl ManagedActivation {
    pub fn mount(parent: &Ctx, plugin: impl Plugin) -> Result<Self, CordisError> {
        Self::mount_gated(parent, plugin, ActivationGate::default())
    }
    pub fn mount_gated(
        parent: &Ctx,
        plugin: impl Plugin,
        gate: ActivationGate,
    ) -> Result<Self, CordisError> {
        let mut injects = plugin.injects().to_vec();
        Self::mount_native(parent, gate, move |isolated, gate, key, context| {
            injects.push(key);
            isolated.plugin(ManagedPlugin {
                plugin: Arc::new(plugin),
                gate,
                injects,
                context,
                enter: None,
            })
        })
    }

    pub fn mount_factory<C: Send + Sync + 'static>(
        parent: &Ctx,
        factory: impl PluginFactory<C>,
        config: C,
    ) -> Result<Self, CordisError> {
        Self::mount_factory_gated(parent, factory, config, ActivationGate::default())
    }
    pub fn mount_factory_gated<C: Send + Sync + 'static>(
        parent: &Ctx,
        factory: impl PluginFactory<C>,
        config: C,
        gate: ActivationGate,
    ) -> Result<Self, CordisError> {
        Self::mount_factory_enter(parent, factory, config, gate, None)
    }
    pub(crate) fn mount_factory_enter<C: Send + Sync + 'static>(
        parent: &Ctx,
        factory: impl PluginFactory<C>,
        config: C,
        gate: ActivationGate,
        enter: Option<NativeEnter>,
    ) -> Result<Self, CordisError> {
        // Capture author metadata before registering the permit. A panic here
        // cannot leave an orphaned permit or a partially mounted fiber.
        let name = factory.name().to_owned();
        let mut injects = factory.injects().to_vec();
        Self::mount_native(parent, gate, move |isolated, gate, key, context| {
            injects.push(key);
            isolated.plugin_with(
                ManagedFactory {
                    factory,
                    name,
                    gate,
                    injects,
                    context,
                    enter,
                },
                config,
            )
        })
    }

    fn mount_native(
        parent: &Ctx,
        gate: ActivationGate,
        mount: impl FnOnce(&Ctx, ActivationGate, TypeKey, watch::Sender<Option<Ctx>>) -> FiberView,
    ) -> Result<Self, CordisError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .expect("managed activation ids exhausted");
        let key = TypeKey::keyed_dynamic::<Permit>(format!("permit:{id}"));
        let isolated = parent.isolate(key.clone(), &format!("permit:{id}"));
        let checked = gate.clone();
        let permit = isolated
            .provide_as_with_check(key.clone(), Arc::new(Permit), move || checked.is_open())?;
        let (context_tx, context) = watch::channel(None);
        let view = Arc::new(mount(&isolated, gate.clone(), key, context_tx));
        gate.bind_native(&view);
        Ok(Self {
            view,
            gate,
            resources: Mutex::new(vec![permit]),
            context,
            handle: parent.handle().clone(),
            stop: Mutex::new(None),
        })
    }

    pub fn view(&self) -> &FiberView {
        &self.view
    }
    pub fn gate(&self) -> ActivationGate {
        self.gate.clone()
    }
    /// The original generation-bound business context, captured before apply.
    /// Existing execution may retain it after closing; admission must still
    /// check the gate. This does not manufacture an SDK replacement context.
    pub fn native_context(&self) -> Option<Ctx> {
        self.context.borrow().clone()
    }
    /// Import providers are isolated on the runtime parent before native mount.
    /// Their ownership joins this activation's confirmation, including when a
    /// caller abandons its stop waiter. Late adoption returns ownership so the
    /// mounting caller must join rollback before reporting its failure.
    pub fn own_disposers(&self, disposers: Vec<Disposer>) -> Result<(), Vec<Disposer>> {
        let stop = self.stop.lock().unwrap();
        if stop.is_none() {
            self.resources.lock().unwrap().extend(disposers);
            return Ok(());
        }
        Err(disposers)
    }

    /// Close admission at the call site. Native shutdown includes children and
    /// waits for cleanup even when the caller drops this particular waiter.
    pub fn stop(&self) -> BoxFuture<'static, Result<(), Arc<CordisError>>> {
        self.gate.close();
        let mut stop = self.stop.lock().unwrap();
        if stop.is_none() {
            let cleanup = self.view.shutdown();
            let released: Vec<_> = std::mem::take(&mut *self.resources.lock().unwrap())
                .into_iter()
                .map(Disposer::dispose)
                .collect();
            let (tx, rx) = watch::channel(None);
            *stop = Some(rx);
            self.handle.spawn(async move {
                let mut errors = Vec::new();
                if let Err(error) = cleanup.await {
                    errors.push(error);
                }
                for released in released {
                    if let Err(error) = released.await {
                        errors.push(error);
                    }
                }
                let result = match errors.len() {
                    0 => Ok(()),
                    1 => Err(errors.pop().unwrap()),
                    _ => Err(Arc::new(CordisError::Aggregate { errors })),
                };
                tx.send_replace(Some(result));
            });
        }
        let mut rx = stop.as_ref().unwrap().clone();
        Box::pin(async move {
            loop {
                if let Some(result) = rx.borrow_and_update().clone() {
                    return result;
                }
                rx.changed()
                    .await
                    .expect("managed stop task must deliver its result");
            }
        })
    }
}
