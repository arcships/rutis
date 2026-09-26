//! Runtime-neutral surface used by generated bindings. Calls and owner-returned
//! objects always go through Caller; native exports only execute with a pin.
use crate::error::{ErrorCode, ProtocolError, Result};
use crate::exports::{Exports, PinKey};
use crate::graph::DecodedValue;
use crate::identity::ObjectIdentity;
use crate::imports::ObjectProxy;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub use serde;
pub use serde_json;
pub type RpcFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'static>>;

/// JSON property absence differs from a present null. Option<T>'s serde
/// decoding collapses those states, so optional DTO fields use this wrapper.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OptionalField<T> {
    #[default]
    Missing,
    Present(T),
}
impl<T> OptionalField<T> {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}
impl<T: serde::Serialize> serde::Serialize for OptionalField<T> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Present(value) => value.serialize(serializer),
            Self::Missing => Err(serde::ser::Error::custom(
                "missing DTO field must be omitted",
            )),
        }
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for OptionalField<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Clone)]
pub enum Outbound {
    Value(serde_json::Value),
    Own(Arc<dyn NativeExport>),
    Foreign(ObjectProxy),
    Record(BTreeMap<String, Outbound>),
    List(Vec<Outbound>),
    Optional(Option<Box<Outbound>>),
}
impl Outbound {
    pub fn json<T: serde::Serialize>(value: T) -> Result<Self> {
        serde_json::to_value(value)
            .map(Self::Value)
            .map_err(|e| invalid(e.to_string()))
    }
}

/// Attach the actual native creator to exported own objects and their snapshot
/// graph. Foreign grants keep their original authority. Repeated registration
/// of the same native object never changes its first creator context.
pub fn with_native(ctx: &rutis::Ctx, value: Outbound) -> Outbound {
    match value {
        Outbound::Own(inner) => Outbound::Own(Arc::new(ContextExport {
            ctx: ctx.clone(),
            inner,
        })),
        Outbound::Record(fields) => Outbound::Record(
            fields
                .into_iter()
                .map(|(name, value)| (name, with_native(ctx, value)))
                .collect(),
        ),
        Outbound::List(values) => Outbound::List(
            values
                .into_iter()
                .map(|value| with_native(ctx, value))
                .collect(),
        ),
        Outbound::Optional(value) => {
            Outbound::Optional(value.map(|value| Box::new(with_native(ctx, *value))))
        }
        value => value,
    }
}
struct ContextExport {
    ctx: rutis::Ctx,
    inner: Arc<dyn NativeExport>,
}
impl NativeExport for ContextExport {
    fn register(&self, exports: &Exports) -> Result<RegisteredExport> {
        self.inner.register(&exports.in_context(&self.ctx)?)
    }
    fn snapshot(&self) -> Result<BTreeMap<String, Outbound>> {
        // GraphExporter inherits the object's immutable first creator. A later
        // wrapper must not change the creator of newly encountered properties.
        self.inner.snapshot()
    }
}

pub trait Caller: Send + Sync {
    /// Associate local own objects (such as a generated borrow callback) with
    /// their actual native creator before the generated client encodes them.
    /// This creates no grants, pins or alternate dispatch path.
    fn bind_native(&self, _ctx: &rutis::Ctx, _value: Outbound) -> Result<()> {
        Err(ProtocolError::new(
            ErrorCode::UnsupportedCapability,
            "dispatch",
            "caller has no managed native export table",
        ))
    }
    fn call(
        &self,
        target: ObjectProxy,
        method: String,
        params: Outbound,
    ) -> RpcFuture<DecodedValue>;
}

pub(crate) fn register_native(exports: &Exports, ctx: &rutis::Ctx, value: Outbound) -> Result<()> {
    let exports = exports.in_context(ctx)?;
    exports.require_open()?;
    fn register(exports: &Exports, value: Outbound) -> Result<()> {
        match value {
            Outbound::Own(value) => {
                value.register(exports)?;
            }
            Outbound::Record(fields) => {
                for value in fields.into_values() {
                    register(exports, value)?;
                }
            }
            Outbound::List(values) => {
                for value in values {
                    register(exports, value)?;
                }
            }
            Outbound::Optional(Some(value)) => register(exports, *value)?,
            _ => {}
        }
        Ok(())
    }
    register(&exports, value)
}

#[derive(Clone)]
pub struct CallContext {
    caller: Arc<dyn Caller>,
    native: Option<rutis::Ctx>,
    tasks: Arc<TaskScope>,
}
#[derive(Default)]
struct TaskState {
    pending: usize,
    root_finished: bool,
    closed: bool,
    errors: Vec<ProtocolError>,
}
#[derive(Default)]
struct TaskScope {
    state: Mutex<TaskState>,
    changed: Notify,
}
impl CallContext {
    pub fn new(caller: Arc<dyn Caller>, native: Option<rutis::Ctx>) -> Self {
        Self {
            caller,
            native,
            tasks: Arc::new(TaskScope::default()),
        }
    }
    pub fn caller(&self) -> Arc<dyn Caller> {
        self.caller.clone()
    }
    pub fn native(&self) -> Result<&rutis::Ctx> {
        self.native.as_ref().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::Unavailable,
                "dispatch",
                "native context unavailable",
            )
        })
    }
    pub(crate) fn outbound(&self, value: Outbound) -> Result<Outbound> {
        match &self.native {
            Some(ctx) if ctx.cancellation_token().is_cancelled() => Err(ProtocolError::new(
                ErrorCode::ScopeClosed,
                "dispatch",
                "native creator closed before result encoding",
            )),
            Some(ctx) => Ok(with_native(ctx, value)),
            None => Ok(value),
        }
    }
    /// Registered work belongs to actual execution, even if its caller drops
    /// the waiter. Children may register descendants until execution converges.
    pub fn spawn<F>(&self, work: F) -> Result<()>
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        let mut state = self.tasks.state.lock().unwrap();
        if state.closed {
            return Err(ProtocolError::new(
                ErrorCode::ScopeClosed,
                "dispatch",
                "execution finished",
            ));
        }
        state.pending += 1;
        drop(state);
        let tasks = self.tasks.clone();
        let child = tokio::spawn(work);
        tokio::spawn(async move {
            let result = child.await.unwrap_or_else(|e| {
                Err(ProtocolError::new(
                    ErrorCode::Business,
                    "child",
                    e.to_string(),
                ))
            });
            let mut state = tasks.state.lock().unwrap();
            state.pending -= 1;
            if let Err(error) = result {
                state.errors.push(error);
            }
            if state.root_finished && state.pending == 0 {
                state.closed = true;
            }
            drop(state);
            tasks.changed.notify_waiters();
        });
        Ok(())
    }
    /// Runner completion barrier: seal the root handler and join every
    /// registered descendant before reporting execution finished.
    pub async fn finish(&self) -> Result<()> {
        {
            let mut state = self.tasks.state.lock().unwrap();
            state.root_finished = true;
            if state.pending == 0 {
                state.closed = true;
            }
        }
        loop {
            let changed = self.tasks.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.tasks.state.lock().unwrap();
                if state.closed {
                    return state.errors.first().cloned().map_or(Ok(()), Err);
                }
            }
            changed.await;
        }
    }
}

pub trait Dispatcher: Send + Sync {
    fn dispatch(
        &self,
        key: PinKey,
        context: CallContext,
        method: String,
        params: DecodedValue,
    ) -> RpcFuture<Outbound>;
}
pub struct RegisteredExport {
    pub identity: ObjectIdentity,
    pub interface: &'static str,
    pub bundle_sha256: &'static str,
    pub dispatcher: Arc<dyn Dispatcher>,
}
pub trait NativeExport: Send + Sync {
    fn register(&self, exports: &Exports) -> Result<RegisteredExport>;
    fn snapshot(&self) -> Result<BTreeMap<String, Outbound>>;
}

#[derive(Clone)]
pub struct Client {
    proxy: ObjectProxy,
    caller: Arc<dyn Caller>,
}
impl Client {
    pub fn new(
        proxy: ObjectProxy,
        caller: Arc<dyn Caller>,
        hash: &str,
        interface: &str,
    ) -> Result<Self> {
        let delivery = proxy.delivery()?;
        if delivery.view.bundle_sha256 != hash || delivery.view.interface != interface {
            return Err(ProtocolError::new(
                ErrorCode::InterfaceMismatch,
                "binding",
                "generated interface does not match grant",
            ));
        }
        Ok(Self { proxy, caller })
    }
    pub fn caller(&self) -> Arc<dyn Caller> {
        self.caller.clone()
    }
    pub fn proxy(&self) -> &ObjectProxy {
        &self.proxy
    }
    pub fn call(&self, method: &str, params: Outbound) -> RpcFuture<DecodedValue> {
        if let Err(error) = self.proxy.delivery() {
            return Box::pin(async { Err(error) });
        }
        self.caller.call(self.proxy.clone(), method.into(), params)
    }
    pub fn property(&self, name: &str) -> Result<DecodedValue> {
        self.proxy.property(name)
    }
}
pub trait ClientHandle: Clone + Send + Sync + 'static {
    const INTERFACE: &'static str;
    const BUNDLE_SHA256: &'static str;
    fn client(&self) -> &Client;
    fn from_client(client: Client) -> Self;
}
pub fn bind<H: ClientHandle>(value: DecodedValue, caller: Arc<dyn Caller>) -> Result<H> {
    Ok(H::from_client(Client::new(
        value.into_object()?,
        caller,
        H::BUNDLE_SHA256,
        H::INTERFACE,
    )?))
}
pub fn release<H: ClientHandle>(handle: &H) {
    handle.client().proxy.release();
}
pub fn same_object<A: ClientHandle, B: ClientHandle>(a: &A, b: &B) -> bool {
    a.client().proxy.same_object(&b.client().proxy)
}
pub fn same_wrapper<A: ClientHandle, B: ClientHandle>(a: &A, b: &B) -> bool {
    a.client().proxy.same_wrapper(&b.client().proxy)
}
pub fn json<T: serde::de::DeserializeOwned>(value: DecodedValue) -> Result<T> {
    serde_json::from_value(value.into_json()?).map_err(|e| invalid(e.to_string()))
}
pub fn record(value: DecodedValue) -> Result<BTreeMap<String, DecodedValue>> {
    match value {
        DecodedValue::Record(fields) => Ok(fields),
        _ => Err(invalid("expected record")),
    }
}
pub fn field(fields: &mut BTreeMap<String, DecodedValue>, name: &str) -> Result<DecodedValue> {
    fields
        .remove(name)
        .ok_or_else(|| invalid("missing record field"))
}
pub fn list(value: DecodedValue) -> Result<Vec<DecodedValue>> {
    match value {
        DecodedValue::List(items) => Ok(items),
        _ => Err(invalid("expected list")),
    }
}
pub fn optional(value: DecodedValue) -> Result<Option<DecodedValue>> {
    match value {
        DecodedValue::Optional(value) => Ok(value.map(|v| *v)),
        _ => Err(invalid("expected optional")),
    }
}
pub fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "binding", message)
}
pub fn denied_method() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::CapabilityDenied,
        "dispatch",
        "method is not declared",
    )
}
