//! Owner-side strong pins for actual objects. Identity is independent of field
//! equality and survives a shared object's unpinned intervals while it is alive.
use crate::error::{ErrorCode, ProtocolError, Result};
use crate::identity::{Activation, ObjectIdentity, Sequence};
use crate::managed::ActivationGate;
use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, Weak,
};
use tokio::sync::watch;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

type Object = Arc<dyn Any + Send + Sync>;
type CleanupFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
type Disposer = Box<dyn FnOnce(Object) -> CleanupFuture + Send>;
pub(crate) type RevokeObjects = Arc<dyn Fn(Vec<ObjectIdentity>) -> CleanupFuture + Send + Sync>;
type Revocation = watch::Receiver<Option<Result<()>>>;
type Cleanup = (ObjectIdentity, Object, Option<Disposer>);

trait WeakSource: Send + Sync {
    fn upgrade(&self) -> Option<Object>;
    fn strong_count(&self) -> usize;
}
struct AnySource(Weak<dyn Any + Send + Sync>);
impl WeakSource for AnySource {
    fn upgrade(&self) -> Option<Object> {
        self.0.upgrade()
    }
    fn strong_count(&self) -> usize {
        self.0.strong_count()
    }
}
struct TraitObject<T: ?Sized>(Arc<T>);
struct TraitSource<T: ?Sized>(Weak<T>);
impl<T: ?Sized + Send + Sync + 'static> WeakSource for TraitSource<T> {
    fn upgrade(&self) -> Option<Object> {
        self.0
            .upgrade()
            .map(|object| Arc::new(TraitObject(object)) as Object)
    }
    fn strong_count(&self) -> usize {
        self.0.strong_count()
    }
}

/// One allocator belongs to the entire runtime epoch, across all activations.
#[derive(Clone, Default)]
pub struct ObjectIds(Arc<AtomicU64>);
impl ObjectIds {
    fn next(&self) -> Result<Sequence> {
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map(|n| Sequence(n + 1))
            .map_err(|_| error(ErrorCode::Unavailable, "object sequence exhausted"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PinKey {
    /// Staging pins cover the gap before a broker grants a result or export.
    Staging {
        id: Sequence,
    },
    Delivery {
        recipient: Activation,
        id: Sequence,
    },
    Execution {
        caller: Activation,
        call: Sequence,
    },
}

struct Entry {
    address: usize,
    native: Option<rutis::Ctx>,
    sources: BTreeMap<TypeId, Box<dyn WeakSource>>,
    held: Option<Object>,
    pins: usize,
    disposed: bool,
    closed: bool,
    disposer: Option<Disposer>,
}
struct State {
    open: bool,
    entries: BTreeMap<ObjectIdentity, Entry>,
    addresses: BTreeMap<usize, ObjectIdentity>,
    pins: BTreeMap<PinKey, ObjectIdentity>,
    released: BTreeSet<PinKey>,
    retired: BTreeMap<(String, Sequence), Sequence>,
    closed_epochs: BTreeMap<String, Sequence>,
    cleaning: usize,
    errors: Vec<ProtocolError>,
    creators: HashMap<rutis::InstanceId, CancellationToken>,
    revoke: Option<RevokeObjects>,
    pending_revocations: BTreeSet<ObjectIdentity>,
    revocations: BTreeMap<ObjectIdentity, Revocation>,
    cleanup_results: BTreeMap<ObjectIdentity, Option<Result<()>>>,
}
struct Inner {
    state: Mutex<State>,
    staging: ObjectIds,
    changed: Notify,
    runtime: tokio::runtime::Handle,
    creator_setup: Mutex<()>,
}
#[derive(Clone)]
pub struct Exports {
    owner: Activation,
    ids: ObjectIds,
    inner: Arc<Inner>,
    native: Option<Arc<NativeContext>>,
}
struct NativeContext {
    gate: ActivationGate,
    root: rutis::Ctx,
    creator: rutis::Ctx,
}

fn error(code: ErrorCode, message: &str) -> ProtocolError {
    ProtocolError::new(code, "export", message)
}

impl Exports {
    /// All bundle encoders for this activation share the staging namespace.
    pub fn stage(&self, identity: &ObjectIdentity) -> Result<PinKey> {
        let key = PinKey::Staging {
            id: self.inner.staging.next()?,
        };
        self.pin(identity, key.clone())?;
        Ok(key)
    }
    pub fn require_open(&self) -> Result<()> {
        if self.admission_open(&self.inner.state.lock().unwrap()) {
            Ok(())
        } else {
            Err(error(ErrorCode::ScopeClosed, "owner activation closed"))
        }
    }
    pub fn owner(&self) -> &Activation {
        &self.owner
    }
    pub(crate) fn same_table(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn gate(&self) -> Option<ActivationGate> {
        self.native.as_ref().map(|native| native.gate.clone())
    }
    pub fn new(owner: Activation, ids: ObjectIds) -> Self {
        Self {
            owner,
            ids,
            native: None,
            inner: Arc::new(Inner {
                staging: ObjectIds::default(),
                state: Mutex::new(State {
                    open: true,
                    entries: BTreeMap::new(),
                    addresses: BTreeMap::new(),
                    pins: BTreeMap::new(),
                    released: BTreeSet::new(),
                    retired: BTreeMap::new(),
                    closed_epochs: BTreeMap::new(),
                    cleaning: 0,
                    errors: Vec::new(),
                    creators: HashMap::new(),
                    revoke: None,
                    pending_revocations: BTreeSet::new(),
                    revocations: BTreeMap::new(),
                    cleanup_results: BTreeMap::new(),
                }),
                changed: Notify::new(),
                runtime: tokio::runtime::Handle::current(),
                creator_setup: Mutex::new(()),
            }),
        }
    }

    /// The native context owns cleanup. Admission also checks its managed gate
    /// synchronously, so native pre-cancellation beats an async cleanup observer.
    pub fn managed(
        ctx: &rutis::Ctx,
        owner: Activation,
        ids: ObjectIds,
        gate: ActivationGate,
    ) -> std::result::Result<Self, rutis::CordisError> {
        let mut exports = Self::new(owner, ids);
        exports.native = Some(Arc::new(NativeContext {
            gate: gate.clone(),
            root: ctx.clone(),
            creator: ctx.clone(),
        }));
        let cleanup = exports.clone();
        let token = ctx.cancellation_token();
        let handle = ctx.handle().clone();
        let stop_observer = tokio_util::sync::CancellationToken::new();
        ctx.effect_named("protocol exports", move || {
            let observer = cleanup.clone();
            let stopped = stop_observer.clone();
            let task = handle.spawn(async move {
                tokio::select! { _ = token.cancelled() => {}, _ = gate.revoked() => {}, _ = stopped.cancelled() => {} }
                observer.close();
            });
            rutis::Effect::AsyncDisposer(Box::new(move || {
                cleanup.close();
                stop_observer.cancel();
                Box::pin(async move {
                    task.await
                        .map_err(|e| rutis::CordisError::PluginFailed(Box::new(e)))?;
                    cleanup
                        .join()
                        .await
                        .map_err(|e| rutis::CordisError::PluginFailed(Box::new(e)))
                })
            }))
        })?;
        Ok(exports)
    }

    fn admission_open(&self, state: &State) -> bool {
        state.open
            && self.native.as_ref().is_none_or(|native| {
                native.gate.is_open() && !native.creator.cancellation_token().is_cancelled()
            })
    }

    /// Keep the same object table and activation, while assigning first-time
    /// registrations to this original native child context. Existing object
    /// identities retain their first creator, including its cancelled token.
    pub(crate) fn in_context(&self, ctx: &rutis::Ctx) -> Result<Self> {
        let native = self
            .native
            .as_ref()
            .filter(|native| ctx.is_within(&native.root))
            .ok_or_else(|| {
                error(
                    ErrorCode::CapabilityDenied,
                    "export creator is outside its managed native subtree",
                )
            })?;
        if ctx.cancellation_token().is_cancelled() {
            return Err(error(ErrorCode::ScopeClosed, "export creator is closed"));
        }
        if ctx.instance() != native.root.instance() {
            self.track_creator(ctx).map_err(|error| {
                ProtocolError::new(ErrorCode::ScopeClosed, "export", error.to_string())
            })?;
        }
        let mut table = self.clone();
        table.native = Some(Arc::new(NativeContext {
            gate: native.gate.clone(),
            root: native.root.clone(),
            creator: ctx.clone(),
        }));
        Ok(table)
    }

    fn track_creator(&self, ctx: &rutis::Ctx) -> std::result::Result<(), rutis::CordisError> {
        // Effect registration performs only our observer setup. Object-table
        // locks are released before entering native registration or shutdown.
        let _setup = self.inner.creator_setup.lock().unwrap();
        let instance = ctx.instance();
        let token = ctx.cancellation_token();
        {
            let mut state = self.inner.state.lock().unwrap();
            if state
                .creators
                .get(&instance)
                .is_some_and(|old| !old.is_cancelled())
            {
                return Ok(());
            }
            state.creators.insert(instance, token.clone());
        }
        let weak = Arc::downgrade(&self.inner);
        let owner = self.owner.clone();
        let runtime = self.inner.runtime.clone();
        let registered = ctx.effect_named("protocol child objects", move || {
            let stopped = CancellationToken::new();
            let stopping = stopped.clone();
            let observed = weak.clone();
            let observer_owner = owner.clone();
            let observer = runtime.spawn(async move {
                tokio::select! { _ = token.cancelled() => {}, _ = stopping.cancelled() => {} }
                if let Some(inner) = observed.upgrade() {
                    Self::from_inner(observer_owner, inner).close_creator(instance);
                }
            });
            rutis::Effect::AsyncDisposer(Box::new(move || {
                stopped.cancel();
                let closed = weak.upgrade().map(|inner| {
                    let table = Self::from_inner(owner, inner);
                    let objects = table.close_creator(instance);
                    (table, objects)
                });
                Box::pin(async move {
                    observer
                        .await
                        .map_err(|error| rutis::CordisError::PluginFailed(Box::new(error)))?;
                    if let Some((table, objects)) = closed {
                        table
                            .join_objects(&objects)
                            .await
                            .map_err(|error| rutis::CordisError::PluginFailed(Box::new(error)))?;
                    }
                    Ok(())
                })
            }))
        });
        if let Err(error) = registered {
            self.inner.state.lock().unwrap().creators.remove(&instance);
            return Err(error);
        }
        Ok(())
    }
    fn from_inner(owner: Activation, inner: Arc<Inner>) -> Self {
        Self {
            owner,
            inner,
            ids: ObjectIds::default(),
            native: None,
        }
    }
    pub(crate) fn on_revoke(&self, revoke: RevokeObjects) -> Result<()> {
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.revoke.is_some() {
                return Err(error(
                    ErrorCode::CapabilityDenied,
                    "object table already has a revocation transport",
                ));
            }
            state.revoke = Some(revoke);
        }
        self.send_revocations();
        Ok(())
    }
    fn close_creator(&self, instance: rutis::InstanceId) -> Vec<ObjectIdentity> {
        let (objects, cleanups) = {
            let mut state = self.inner.state.lock().unwrap();
            let objects = state
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry.native.as_ref().is_some_and(|ctx| {
                        ctx.instance() == instance && ctx.cancellation_token().is_cancelled()
                    })
                })
                .map(|(identity, _)| identity.clone())
                .collect::<Vec<_>>();
            let mut newly_closed = Vec::new();
            for identity in &objects {
                let entry = state.entries.get_mut(identity).unwrap();
                if !entry.closed {
                    entry.closed = true;
                    newly_closed.push(identity.clone());
                }
            }
            if state.open {
                state.pending_revocations.extend(newly_closed);
            }
            if state
                .creators
                .get(&instance)
                .is_some_and(CancellationToken::is_cancelled)
            {
                state.creators.remove(&instance);
            }
            let closed = objects.iter().cloned().collect::<BTreeSet<_>>();
            let keys = state
                .pins
                .iter()
                .filter(|(key, identity)| {
                    !matches!(key, PinKey::Execution { .. }) && closed.contains(*identity)
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            let mut cleanups = Vec::new();
            for key in keys {
                state.released.insert(key.clone());
                let identity = state.pins.remove(&key).unwrap();
                cleanups.extend(unpin(&mut state, &identity));
            }
            for identity in &objects {
                let entry = &state.entries[identity];
                if entry.pins == 0 && entry.held.is_some() && entry.disposer.is_some() {
                    cleanups.push(take_cleanup(&mut state, identity));
                }
            }
            (objects, cleanups)
        };
        self.schedule(cleanups);
        self.send_revocations();
        objects
    }
    fn send_revocations(&self) {
        let (objects, revoke, tx) = {
            let mut state = self.inner.state.lock().unwrap();
            let Some(revoke) = state.revoke.clone() else {
                return;
            };
            if state.pending_revocations.is_empty() {
                return;
            }
            let objects = std::mem::take(&mut state.pending_revocations)
                .into_iter()
                .collect::<Vec<_>>();
            let (tx, rx) = watch::channel(None);
            for object in &objects {
                state.revocations.insert(object.clone(), rx.clone());
            }
            (objects, revoke, tx)
        };
        let inner = self.inner.clone();
        let task = self
            .inner
            .runtime
            .spawn(async move { revoke(objects).await });
        self.inner.runtime.spawn(async move {
            let result = task.await.unwrap_or_else(|error| {
                Err(ProtocolError::new(
                    ErrorCode::Unavailable,
                    "export",
                    format!("object revocation task failed: {error}"),
                ))
            });
            if let Err(error) = &result {
                inner.state.lock().unwrap().errors.push(error.clone());
            }
            tx.send_replace(Some(result));
            inner.changed.notify_waiters();
        });
    }
    async fn join_objects(&self, objects: &[ObjectIdentity]) -> Result<()> {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let done = {
                let state = self.inner.state.lock().unwrap();
                let mut ready = true;
                for object in objects {
                    if state
                        .entries
                        .get(object)
                        .is_some_and(|entry| entry.pins != 0)
                    {
                        ready = false;
                    }
                    if let Some(result) = state.cleanup_results.get(object) {
                        match result {
                            Some(result) => result.clone()?,
                            None => ready = false,
                        }
                    }
                    if state.open {
                        if state.revoke.is_some() && state.pending_revocations.contains(object) {
                            ready = false;
                        }
                        if let Some(receipt) = state.revocations.get(object) {
                            match receipt.borrow().clone() {
                                Some(result) => result?,
                                None => ready = false,
                            }
                        }
                    }
                }
                ready
            };
            if done {
                return Ok(());
            }
            changed.await;
        }
    }

    pub fn register<T: Any + Send + Sync>(&self, object: &Arc<T>) -> Result<ObjectIdentity> {
        self.register_inner(object.clone(), None)
    }

    /// Only for objects whose external resource lifetime is exclusively owned
    /// by the protocol. Objects with legitimate local users use register().
    pub fn register_exclusive<T, F, Fut>(
        &self,
        object: &Arc<T>,
        disposer: F,
    ) -> Result<ObjectIdentity>
    where
        T: Any + Send + Sync,
        F: FnOnce(Arc<T>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let disposer: Disposer = Box::new(move |object| {
            Box::pin(disposer(
                object.downcast::<T>().expect("registered object type"),
            ))
        });
        self.register_inner(object.clone(), Some(disposer))
    }

    pub fn register_trait<T: ?Sized + Send + Sync + 'static>(
        &self,
        object: &Arc<T>,
    ) -> Result<ObjectIdentity> {
        self.register_trait_inner(object, None)
    }

    pub fn register_trait_exclusive<T, F, Fut>(
        &self,
        object: &Arc<T>,
        disposer: F,
    ) -> Result<ObjectIdentity>
    where
        T: ?Sized + Send + Sync + 'static,
        F: FnOnce(Arc<T>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let disposer: Disposer = Box::new(move |object| {
            let holder = object
                .downcast::<TraitObject<T>>()
                .expect("registered trait object type");
            Box::pin(disposer(holder.0.clone()))
        });
        self.register_trait_inner(object, Some(disposer))
    }

    fn register_trait_inner<T: ?Sized + Send + Sync + 'static>(
        &self,
        object: &Arc<T>,
        disposer: Option<Disposer>,
    ) -> Result<ObjectIdentity> {
        self.register_source(
            Arc::as_ptr(object) as *const () as usize,
            TypeId::of::<TraitObject<T>>(),
            Box::new(TraitSource(Arc::downgrade(object))),
            Arc::new(TraitObject(object.clone())),
            disposer,
        )
    }

    fn register_inner(&self, object: Object, disposer: Option<Disposer>) -> Result<ObjectIdentity> {
        self.register_source(
            Arc::as_ptr(&object) as *const () as usize,
            object.as_ref().type_id(),
            Box::new(AnySource(Arc::downgrade(&object))),
            object,
            disposer,
        )
    }

    fn register_source(
        &self,
        address: usize,
        source_id: TypeId,
        source: Box<dyn WeakSource>,
        object: Object,
        disposer: Option<Disposer>,
    ) -> Result<ObjectIdentity> {
        // Closure captures may have user Drop implementations. Declare this
        // before the lock so every early return also drops it after unlocking.
        let mut discarded = None;
        let mut state = self.inner.state.lock().unwrap();
        if !self.admission_open(&state) {
            return Err(error(ErrorCode::ScopeClosed, "owner activation closed"));
        }
        if let Some(identity) = state.addresses.get(&address) {
            let identity = identity.clone();
            let entry = state.entries.get_mut(&identity).unwrap();
            if entry
                .sources
                .values()
                .any(|source| source.strong_count() != 0)
            {
                if entry.closed
                    || entry
                        .native
                        .as_ref()
                        .is_some_and(|ctx| ctx.cancellation_token().is_cancelled())
                {
                    return Err(error(ErrorCode::ScopeClosed, "export creator is closed"));
                }
                if entry.disposed {
                    return Err(error(ErrorCode::StaleObject, "exclusive object disposed"));
                }
                if disposer.is_some() {
                    return Err(error(
                        ErrorCode::InvalidParams,
                        "exclusive registration must use its existing identity",
                    ));
                }
                entry.sources.entry(source_id).or_insert(source);
                return Ok(identity);
            }
            state.addresses.remove(&address);
            let pending = state.pending_revocations.contains(&identity)
                || state
                    .revocations
                    .get(&identity)
                    .is_some_and(|receipt| !matches!(*receipt.borrow(), Some(Ok(()))))
                || state
                    .cleanup_results
                    .get(&identity)
                    .is_some_and(|result| !matches!(result, Some(Ok(()))));
            if !pending {
                discarded = state.entries.remove(&identity);
            }
        }
        let identity = ObjectIdentity {
            owner: self.owner.clone(),
            object: self.ids.next()?,
        };
        state.addresses.insert(address, identity.clone());
        state.entries.insert(
            identity.clone(),
            Entry {
                address,
                native: self.native.as_ref().map(|native| native.creator.clone()),
                sources: BTreeMap::from([(source_id, source)]),
                // Exclusive registration already transfers resource ownership.
                // Hold it provisionally until the first pin, or owner rollback.
                held: disposer.as_ref().map(|_| object.clone()),
                pins: 0,
                disposed: false,
                closed: false,
                disposer,
            },
        );
        drop(state);
        drop(discarded);
        Ok(identity)
    }

    pub fn pin(&self, identity: &ObjectIdentity, key: PinKey) -> Result<()> {
        let mut state = self.inner.state.lock().unwrap();
        if !self.admission_open(&state) {
            return Err(error(ErrorCode::ScopeClosed, "owner activation closed"));
        }
        if state.released.contains(&key) || retired(&state, &key) {
            return Err(error(
                ErrorCode::StaleObject,
                "released pin cannot be reacquired",
            ));
        }
        if state
            .entries
            .get(identity)
            .is_some_and(|entry| entry.closed)
            || state
                .entries
                .get(identity)
                .and_then(|entry| entry.native.as_ref())
                .is_some_and(|ctx| ctx.cancellation_token().is_cancelled())
        {
            return Err(error(ErrorCode::ScopeClosed, "export creator is closed"));
        }
        if let Some(old) = state.pins.get(&key) {
            return if old == identity {
                Ok(())
            } else {
                Err(error(ErrorCode::CapabilityDenied, "pin key changed object"))
            };
        }
        let entry = state
            .entries
            .get_mut(identity)
            .ok_or_else(|| error(ErrorCode::StaleObject, "unknown object"))?;
        if entry.disposed {
            return Err(error(ErrorCode::StaleObject, "exclusive object disposed"));
        }
        let object = entry
            .held
            .clone()
            .or_else(|| entry.sources.values().find_map(|source| source.upgrade()))
            .ok_or_else(|| error(ErrorCode::StaleObject, "local object already dropped"))?;
        entry.held = Some(object);
        entry.pins += 1;
        state.pins.insert(key, identity.clone());
        Ok(())
    }

    /// Called only by an authenticated owner dispatch after obtaining an
    /// execution pin. Passback author APIs expose a gated facade, never this.
    pub fn execution_object<T: Any + Send + Sync>(&self, key: &PinKey) -> Result<Arc<T>> {
        self.execution_source(key, TypeId::of::<T>())?
            .downcast::<T>()
            .map_err(|_| {
                error(
                    ErrorCode::InterfaceMismatch,
                    "export adapter object type mismatch",
                )
            })
    }

    pub fn execution_trait<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: &PinKey,
    ) -> Result<Arc<T>> {
        let holder = self
            .execution_source(key, TypeId::of::<TraitObject<T>>())?
            .downcast::<TraitObject<T>>()
            .map_err(|_| {
                error(
                    ErrorCode::InterfaceMismatch,
                    "export adapter trait type mismatch",
                )
            })?;
        Ok(holder.0.clone())
    }

    fn execution_source(&self, key: &PinKey, source: TypeId) -> Result<Object> {
        if !matches!(key, PinKey::Execution { .. }) {
            return Err(error(ErrorCode::CapabilityDenied, "execution pin required"));
        }
        let state = self.inner.state.lock().unwrap();
        let identity = state
            .pins
            .get(key)
            .ok_or_else(|| error(ErrorCode::StaleObject, "execution already finished"))?;
        state.entries[identity]
            .sources
            .get(&source)
            .and_then(|source| source.upgrade())
            .ok_or_else(|| {
                error(
                    ErrorCode::InterfaceMismatch,
                    "object does not have this native export adapter",
                )
            })
    }

    pub(crate) fn native_context(&self, identity: &ObjectIdentity) -> Option<rutis::Ctx> {
        self.inner
            .state
            .lock()
            .unwrap()
            .entries
            .get(identity)
            .and_then(|entry| entry.native.clone())
    }

    pub(crate) fn execution_context(&self, key: &PinKey) -> Result<Option<rutis::Ctx>> {
        if !matches!(key, PinKey::Execution { .. }) {
            return Err(error(ErrorCode::CapabilityDenied, "execution pin required"));
        }
        let state = self.inner.state.lock().unwrap();
        let identity = state
            .pins
            .get(key)
            .ok_or_else(|| error(ErrorCode::StaleObject, "execution already finished"))?;
        let native = state.entries[identity].native.clone();
        if state.entries[identity].closed
            || native
                .as_ref()
                .is_some_and(|ctx| ctx.cancellation_token().is_cancelled())
        {
            return Err(error(ErrorCode::ScopeClosed, "export creator is closed"));
        }
        Ok(native)
    }

    /// Identity comparison does not expose a raw business pointer or grant any
    /// authority. Owner-returned method arguments remain broker-routed facades.
    pub fn is_native<T: ?Sized>(&self, identity: &ObjectIdentity, object: &Arc<T>) -> bool {
        self.inner
            .state
            .lock()
            .unwrap()
            .addresses
            .get(&(Arc::as_ptr(object) as *const () as usize))
            == Some(identity)
    }

    pub fn release(&self, key: &PinKey) {
        let cleanup = {
            let mut state = self.inner.state.lock().unwrap();
            if !retired(&state, key) {
                state.released.insert(key.clone());
            }
            state
                .pins
                .remove(key)
                .and_then(|identity| unpin(&mut state, &identity))
        };
        self.schedule(cleanup.into_iter().collect());
    }

    /// Admission closes synchronously. Executing tasks keep their objects;
    /// caller cancellation and shutdown waiters do not remove execution pins.
    pub fn close(&self) {
        let cleanups = {
            let mut state = self.inner.state.lock().unwrap();
            state.open = false;
            let keys: Vec<_> = state
                .pins
                .keys()
                .filter(|k| !matches!(k, PinKey::Execution { .. }))
                .cloned()
                .collect();
            let mut cleanups: Vec<_> = keys
                .into_iter()
                .filter_map(|key| {
                    state.released.insert(key.clone());
                    let identity = state.pins.remove(&key).unwrap();
                    unpin(&mut state, &identity)
                })
                .collect();
            let provisional: Vec<_> = state
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry.pins == 0 && entry.held.is_some() && entry.disposer.is_some()
                })
                .map(|(id, _)| id.clone())
                .collect();
            for identity in provisional {
                cleanups.push(take_cleanup(&mut state, &identity));
            }
            cleanups
        };
        self.schedule(cleanups);
    }

    fn schedule(&self, cleanups: Vec<Cleanup>) {
        for (identity, object, disposer) in cleanups {
            if let Some(disposer) = disposer {
                let inner = self.inner.clone();
                // Supervise panic/abort as a cleanup error instead of leaving
                // a counter stuck or treating an unconfirmed disposer as done.
                let task = self
                    .inner
                    .runtime
                    .spawn(async move { disposer(object).await });
                self.inner.runtime.spawn(async move {
                    let result = task.await.unwrap_or_else(|e| {
                        Err(error(
                            ErrorCode::Business,
                            &format!("object disposer failed: {e}"),
                        ))
                    });
                    let mut state = inner.state.lock().unwrap();
                    state.cleaning -= 1;
                    state.cleanup_results.insert(identity, Some(result.clone()));
                    if let Err(error) = result {
                        state.errors.push(error);
                    }
                    drop(state);
                    inner.changed.notify_waiters();
                });
            } else {
                drop(object);
            }
        }
        self.inner.changed.notify_waiters();
    }

    pub async fn join(&self) -> Result<()> {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.inner.state.lock().unwrap();
                if state.pins.is_empty() && state.cleaning == 0 {
                    return state.errors.first().cloned().map_or(Ok(()), Err);
                }
            }
            changed.await;
        }
    }

    pub fn pins(&self, identity: &ObjectIdentity) -> usize {
        self.inner
            .state
            .lock()
            .unwrap()
            .entries
            .get(identity)
            .map_or(0, |e| e.pins)
    }

    /// Weak identity entries do not retain temporary objects. Prune dead
    /// identities without reusing ids or dropping live exclusive tombstones.
    pub fn sweep(&self) {
        let mut state = self.inner.state.lock().unwrap();
        let dead: Vec<_> = state
            .entries
            .iter()
            .filter(|(id, e)| {
                e.pins == 0
                    && !state
                        .cleanup_results
                        .get(id)
                        .is_some_and(|result| !matches!(result, Some(Ok(()))))
                    && !state.pending_revocations.contains(*id)
                    && !state
                        .revocations
                        .get(id)
                        .is_some_and(|receipt| !matches!(*receipt.borrow(), Some(Ok(()))))
                    && e.sources.values().all(|source| source.strong_count() == 0)
            })
            .map(|(id, e)| (id.clone(), e.address))
            .collect();
        let mut discarded = Vec::new();
        for (id, address) in dead {
            discarded.push(state.entries.remove(&id));
            if state.addresses.get(&address) == Some(&id) {
                state.addresses.remove(&address);
            }
        }
        drop(state);
        drop(discarded);
    }

    /// This watermark is supplied by the broker after receiver acknowledgement,
    /// not inferred by the owner from clocks or locally observed sparse ids.
    pub fn retire_deliveries(
        &self,
        runtime: &str,
        epoch: Sequence,
        through: Sequence,
    ) -> Result<()> {
        let mut state = self.inner.state.lock().unwrap();
        let target = (runtime.to_string(), epoch);
        if state
            .retired
            .get(&target)
            .is_some_and(|last| through < *last)
            || state.pins.keys().any(|key| {
                matches!(key, PinKey::Delivery { recipient, id }
                if recipient.runtime == runtime && recipient.epoch == epoch && *id <= through)
            })
        {
            return Err(error(
                ErrorCode::InvalidParams,
                "unconfirmed export delivery retirement",
            ));
        }
        state.released.retain(|key| {
            !matches!(key, PinKey::Delivery { recipient, id }
            if recipient.runtime == runtime && recipient.epoch == epoch && *id <= through)
        });
        state.retired.insert(target, through);
        Ok(())
    }

    /// Broker-confirmed disconnection removes delivery tombstones for the
    /// recipient epoch. Execution pins remain until actual owner completion.
    pub fn close_recipient_epoch(&self, runtime: &str, epoch: Sequence) {
        let cleanups = {
            let mut state = self.inner.state.lock().unwrap();
            state
                .closed_epochs
                .entry(runtime.into())
                .and_modify(|last| *last = (*last).max(epoch))
                .or_insert(epoch);
            let keys: Vec<_> = state
                .pins
                .keys()
                .filter(|key| {
                    matches!(key, PinKey::Delivery { recipient, .. }
                if recipient.runtime == runtime && recipient.epoch <= epoch)
                })
                .cloned()
                .collect();
            let cleanups = keys
                .into_iter()
                .filter_map(|key| {
                    let identity = state.pins.remove(&key).unwrap();
                    unpin(&mut state, &identity)
                })
                .collect();
            state.released.retain(|key| {
                !matches!(key, PinKey::Delivery { recipient, .. }
                if recipient.runtime == runtime && recipient.epoch <= epoch)
            });
            state
                .retired
                .retain(|(recipient, e), _| recipient != runtime || *e > epoch);
            cleanups
        };
        self.schedule(cleanups);
    }
}

fn retired(state: &State, key: &PinKey) -> bool {
    match key {
        PinKey::Delivery { recipient, id } => {
            state
                .retired
                .get(&(recipient.runtime.clone(), recipient.epoch))
                .is_some_and(|through| id <= through)
                || state
                    .closed_epochs
                    .get(&recipient.runtime)
                    .is_some_and(|closed| recipient.epoch <= *closed)
        }
        _ => false,
    }
}

fn unpin(state: &mut State, identity: &ObjectIdentity) -> Option<Cleanup> {
    let entry = state.entries.get_mut(identity).unwrap();
    entry.pins -= 1;
    if entry.pins != 0 {
        return None;
    }
    Some(take_cleanup(state, identity))
}

fn take_cleanup(state: &mut State, identity: &ObjectIdentity) -> Cleanup {
    let entry = state.entries.get_mut(identity).unwrap();
    let object = entry.held.take().unwrap();
    let disposer = entry.disposer.take();
    if disposer.is_some() {
        entry.disposed = true;
        state.cleaning += 1;
        state.cleanup_results.insert(identity.clone(), None);
    }
    (identity.clone(), object, disposer)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Child(Arc<Mutex<Option<rutis::Ctx>>>);
    impl rutis::Plugin for Child {
        fn name(&self) -> &str {
            "object-child"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a rutis::Ctx,
        ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
            *self.0.lock().unwrap() = Some(ctx.clone());
            Box::pin(async { Ok(rutis::Effect::Done) })
        }
    }
    fn owner() -> Activation {
        Activation {
            runtime: "native-owner".into(),
            epoch: Sequence(1),
            activation: Sequence(1),
        }
    }
    fn pin(execution: bool) -> PinKey {
        if execution {
            PinKey::Execution {
                caller: owner(),
                call: Sequence(1),
            }
        } else {
            PinKey::Delivery {
                recipient: owner(),
                id: Sequence(1),
            }
        }
    }

    #[tokio::test]
    async fn child_stop_keeps_execution_and_joins_cleanup_and_remote_ack_after_waiter_drop() {
        let root = rutis::Ctx::root().unwrap();
        let captured = Arc::new(Mutex::new(None));
        let view = root.plugin(Child(captured.clone()));
        (&view).await.unwrap();
        let child = captured.lock().unwrap().clone().unwrap();
        let exports = Exports::managed(
            &root,
            owner(),
            ObjectIds::default(),
            ActivationGate::default(),
        )
        .unwrap();
        let table = exports.in_context(&child).unwrap();
        let (notified, notification) = tokio::sync::oneshot::channel();
        let (ack, receipt) = tokio::sync::oneshot::channel();
        let transport = Mutex::new(Some((notified, receipt)));
        exports
            .on_revoke(Arc::new(move |objects| {
                let (notified, receipt) = transport.lock().unwrap().take().unwrap();
                Box::pin(async move {
                    notified.send(objects).unwrap();
                    receipt.await.unwrap();
                    Ok(())
                })
            }))
            .unwrap();
        let (entered, cleaning) = tokio::sync::oneshot::channel();
        let (finish, cleanup) = tokio::sync::oneshot::channel();
        let object = Arc::new("child connection");
        let identity = table
            .register_exclusive(&object, move |_| async move {
                entered.send(()).unwrap();
                cleanup.await.unwrap();
                Ok(())
            })
            .unwrap();
        let root_object = Arc::new("root service");
        let root_id = exports.register(&root_object).unwrap();
        let root_pin = PinKey::Delivery {
            recipient: owner(),
            id: Sequence(2),
        };
        table.pin(&identity, pin(false)).unwrap();
        table.pin(&identity, pin(true)).unwrap();
        exports.pin(&root_id, root_pin.clone()).unwrap();
        drop(view.shutdown());
        assert_eq!(notification.await.unwrap(), vec![identity.clone()]);
        assert_eq!(table.pins(&identity), 1);
        assert_eq!(exports.pins(&root_id), 1);
        assert!(Arc::ptr_eq(
            &table.execution_object(&pin(true)).unwrap(),
            &object
        ));
        let mut stopped = Box::pin(view.shutdown());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), stopped.as_mut())
                .await
                .is_err()
        );
        table.release(&pin(true));
        cleaning.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), stopped.as_mut())
                .await
                .is_err()
        );
        finish.send(()).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), stopped.as_mut())
                .await
                .is_err()
        );
        ack.send(()).unwrap();
        stopped.await.unwrap();
        assert_eq!(
            table.register(&object).unwrap_err().code,
            ErrorCode::ScopeClosed
        );
        assert_eq!(
            table
                .register(&Arc::new("late child object"))
                .unwrap_err()
                .code,
            ErrorCode::ScopeClosed
        );
        assert_eq!(exports.register(&root_object).unwrap(), root_id);
        exports.release(&root_pin);
        root.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_revocation_task_is_observable_instead_of_leaving_child_stop_pending() {
        let root = rutis::Ctx::root().unwrap();
        let captured = Arc::new(Mutex::new(None));
        let view = root.plugin(Child(captured.clone()));
        (&view).await.unwrap();
        let child = captured.lock().unwrap().clone().unwrap();
        let exports = Exports::managed(
            &root,
            owner(),
            ObjectIds::default(),
            ActivationGate::default(),
        )
        .unwrap();
        exports
            .on_revoke(Arc::new(|_| {
                Box::pin(async { panic!("lost revocation task") })
            }))
            .unwrap();
        let table = exports.in_context(&child).unwrap();
        let object = Arc::new("child");
        let identity = table.register(&object).unwrap();
        table.pin(&identity, pin(false)).unwrap();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(1), view.shutdown())
            .await
            .unwrap();
        assert!(stopped.is_err());
        assert_eq!(
            exports.join().await.unwrap_err().code,
            ErrorCode::Unavailable
        );
        assert_eq!(table.pins(&identity), 0);
        let _ = root.shutdown().await;
    }
}
