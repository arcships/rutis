//! Host-owned event order. Remote listeners use the existing object Caller;
//! transport and object ownership are not duplicated here.
use crate::{
    contract::{EventMode, TypeExpr, WireValue},
    error::{ErrorCode, ProtocolError, Result},
    exports::Exports,
    identity::Activation,
    sdk::{Outbound, RpcFuture},
    services::Bundles,
};
use rutis::{Ctx, Effect};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventKey {
    pub scope: String,
    pub bundle: String,
    pub event: String,
}
pub enum ListenerResult {
    Continue,
    Return(Value),
}
#[derive(Default)]
pub struct DispatchResult {
    pub returned: Option<Value>,
    pub errors: Vec<ProtocolError>,
}
pub type Listener = Arc<dyn Fn(Outbound) -> RpcFuture<ListenerResult> + Send + Sync>;
fn fail(code: ErrorCode, text: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, "events", text)
}

struct FlightState {
    active: bool,
    closed: bool,
    count: usize,
}
struct Owner {
    ctx: Ctx,
    state: Mutex<FlightState>,
    idle: Notify,
}
impl Owner {
    fn new(ctx: &Ctx) -> Arc<Self> {
        Arc::new(Self {
            ctx: ctx.clone(),
            state: Mutex::new(FlightState {
                active: true,
                closed: false,
                count: 0,
            }),
            idle: Notify::new(),
        })
    }
    fn start(self: &Arc<Self>, once: bool) -> Option<Flight> {
        let mut state = self.state.lock().unwrap();
        if !state.active || self.ctx.cancellation_token().is_cancelled() {
            return None;
        }
        if once {
            state.active = false;
        }
        state.count += 1;
        Some(Flight(self.clone()))
    }
    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.active = false;
        state.closed = true;
    }
    fn closed(&self) -> bool {
        self.state.lock().unwrap().closed || self.ctx.cancellation_token().is_cancelled()
    }
    async fn wait(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.lock().unwrap().count == 0 {
                return;
            }
            notified.await;
        }
    }
    fn effect(self: &Arc<Self>, ctx: &Ctx, remove: impl FnOnce() + Send + 'static) -> Result<()> {
        let owner = self.clone();
        ctx.effect(move || {
            Effect::AsyncDisposer(Box::new(move || {
                owner.close();
                remove();
                Box::pin(async move {
                    owner.wait().await;
                    Ok(())
                })
            }))
        })
        .map_err(|e| fail(ErrorCode::ScopeClosed, e.to_string()))?;
        Ok(())
    }
}
struct Flight(Arc<Owner>);
impl Drop for Flight {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.count -= 1;
        drop(state);
        self.0.idle.notify_waiters();
    }
}
struct Entry {
    id: u64,
    key: EventKey,
    owner: Arc<Owner>,
    once: bool,
    call: Listener,
}
#[derive(Clone)]
struct Publisher {
    owner: Arc<Owner>,
    exports: Exports,
    keys: BTreeSet<EventKey>,
}
// Reuse the protocol's value/type validator. References are checked against
// the publisher's actual ledger; forwarding remains the broker's decision.
fn validate_payload(
    expr: &TypeExpr,
    payload: &Outbound,
    exports: &Exports,
    bundle: &str,
) -> Result<()> {
    fn wire(
        value: &Outbound,
        exports: &Exports,
        bundle: &str,
        references: &mut Vec<String>,
    ) -> Result<WireValue> {
        Ok(match value {
            Outbound::Value(value) => WireValue::Value {
                value: value.clone(),
            },
            Outbound::Record(fields) => WireValue::Record {
                fields: fields
                    .iter()
                    .map(|(key, value)| {
                        Ok((key.clone(), wire(value, exports, bundle, references)?))
                    })
                    .collect::<Result<_>>()?,
            },
            Outbound::List(items) => WireValue::List {
                items: items
                    .iter()
                    .map(|value| wire(value, exports, bundle, references))
                    .collect::<Result<_>>()?,
            },
            Outbound::Optional(value) => WireValue::Optional {
                value: value
                    .as_ref()
                    .map(|value| wire(value, exports, bundle, references).map(Box::new))
                    .transpose()?,
            },
            Outbound::Own(value) => {
                let registered = value.register(exports)?;
                if &registered.identity.owner != exports.owner()
                    || registered.bundle_sha256 != bundle
                {
                    return Err(fail(
                        ErrorCode::InterfaceMismatch,
                        "event object belongs to another owner or bundle",
                    ));
                }
                let index = references.len();
                references.push(registered.interface.to_owned());
                WireValue::Ref { index }
            }
            Outbound::Foreign(value) => {
                let delivery = value.delivery()?;
                if &delivery.recipient.activation != exports.owner()
                    || delivery.view.bundle_sha256 != bundle
                {
                    return Err(fail(
                        ErrorCode::CapabilityDenied,
                        "event object belongs to another publisher or bundle",
                    ));
                }
                let index = references.len();
                references.push(delivery.view.interface);
                WireValue::Ref { index }
            }
        })
    }
    let mut references = Vec::new();
    let wire = wire(payload, exports, bundle, &mut references)?;
    crate::contract::validate_wire(expr, &wire, &references)
}
#[derive(Default)]
struct State {
    next: u64,
    entries: BTreeMap<u64, Arc<Entry>>,
    publishers: BTreeMap<Activation, Publisher>,
}
pub struct HostEvents {
    bundles: Bundles,
    state: Mutex<State>,
}
/// A registration is also owned by its native context. Explicit unsubscribe
/// closes admission synchronously; its future only joins existing callbacks.
pub struct Subscription {
    hub: Weak<HostEvents>,
    id: u64,
    owner: Arc<Owner>,
}
impl Subscription {
    pub fn unsubscribe(&self) -> RpcFuture<()> {
        self.owner.close();
        if let Some(hub) = self.hub.upgrade() {
            hub.state.lock().unwrap().entries.remove(&self.id);
        }
        let owner = self.owner.clone();
        Box::pin(async move {
            owner.wait().await;
            Ok(())
        })
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        drop(self.unsubscribe());
    }
}
impl HostEvents {
    pub fn new(bundles: Bundles) -> Arc<Self> {
        Arc::new(Self {
            bundles,
            state: Mutex::default(),
        })
    }
    fn contract(
        &self,
        key: &EventKey,
        mode: Option<EventMode>,
    ) -> Result<crate::contract::EventContract> {
        if key.scope.is_empty() {
            return Err(fail(ErrorCode::CapabilityDenied, "empty Host event scope"));
        }
        let bundle = self.bundles.exact(&key.bundle)?;
        let event = bundle
            .bundle()
            .events
            .get(&key.event)
            .cloned()
            .ok_or_else(|| fail(ErrorCode::InterfaceMismatch, "event is not declared"))?;
        if !matches!(event.result, TypeExpr::Value { .. }) {
            return Err(fail(
                ErrorCode::UnsupportedCapability,
                "event results must be JSON values",
            ));
        }
        if let Some(mode) = mode {
            if !matches!(mode, EventMode::Parallel | EventMode::Serial) {
                return Err(fail(
                    ErrorCode::UnsupportedCapability,
                    "event mode is not supported",
                ));
            }
            if !event.modes.contains(&mode) {
                return Err(fail(
                    ErrorCode::CapabilityDenied,
                    "event mode is not declared",
                ));
            }
        }
        Ok(event)
    }
    /// Trusted application wiring. Neither scope nor publisher permissions are
    /// accepted from runtime frames or from the event payload.
    pub fn bind_publisher(
        &self,
        ctx: &Ctx,
        exports: Exports,
        keys: BTreeSet<EventKey>,
    ) -> Result<()> {
        for key in &keys {
            self.contract(key, None)?;
        }
        let exports = exports.in_context(ctx)?;
        let activation = exports.owner().clone();
        let owner = Owner::new(ctx);
        owner.effect(ctx, || {})?;
        let mut state = self.state.lock().unwrap();
        if state.publishers.contains_key(&activation) {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "publisher cannot be rebound",
            ));
        }
        state.publishers.insert(
            activation,
            Publisher {
                owner,
                exports,
                keys,
            },
        );
        Ok(())
    }
    /// Synchronous registration is ready when it returns. Host-selected scope
    /// and a real native ctx are supplied by the adapter, not by wire data.
    pub fn subscribe(
        self: &Arc<Self>,
        ctx: &Ctx,
        key: EventKey,
        once: bool,
        call: Listener,
    ) -> Result<Subscription> {
        self.contract(&key, None)?;
        let owner = Owner::new(ctx);
        let mut state = self.state.lock().unwrap();
        let id = state
            .next
            .checked_add(1)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "event order exhausted"))?;
        state.next = id;
        drop(state);
        let weak = Arc::downgrade(self);
        owner.effect(ctx, move || {
            if let Some(hub) = weak.upgrade() {
                hub.state.lock().unwrap().entries.remove(&id);
            }
        })?;
        let mut state = self.state.lock().unwrap();
        if owner.closed() {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "listener closed during registration",
            ));
        }
        state.entries.insert(
            id,
            Arc::new(Entry {
                id,
                key,
                owner: owner.clone(),
                once,
                call,
            }),
        );
        Ok(Subscription {
            hub: Arc::downgrade(self),
            id,
            owner,
        })
    }
    pub fn dispatch(
        self: &Arc<Self>,
        publisher: &Activation,
        key: &EventKey,
        mode: EventMode,
        payload: Outbound,
    ) -> RpcFuture<DispatchResult> {
        let admitted = (|| {
            let contract = self.contract(key, Some(mode))?;
            let (publisher, entries) = {
                let state = self.state.lock().unwrap();
                let publisher = state
                    .publishers
                    .get(publisher)
                    .filter(|p| p.keys.contains(key))
                    .cloned()
                    .ok_or_else(|| {
                        fail(
                            ErrorCode::CapabilityDenied,
                            "publisher is not authorized in this scope",
                        )
                    })?;
                let entries = state
                    .entries
                    .values()
                    .filter(|e| &e.key == key)
                    .cloned()
                    .collect::<Vec<_>>();
                (publisher, entries)
            };
            validate_payload(&contract.params, &payload, &publisher.exports, &key.bundle)?;
            let flight = publisher
                .owner
                .start(false)
                .ok_or_else(|| fail(ErrorCode::ScopeClosed, "publisher is closed"))?;
            Ok((contract, flight, entries))
        })();
        let (contract, publisher_flight, entries) = match admitted {
            Ok(value) => value,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let hub = self.clone();
        let publisher = publisher.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        // An abandoned dispatch waiter cannot cancel native or remote work.
        tokio::spawn(async move {
            let _publisher = publisher_flight;
            let registry = Arc::downgrade(&hub);
            let invoke = move |entry: Arc<Entry>, payload: Outbound| {
                let flight = entry.owner.start(entry.once);
                if flight.is_some() && entry.once {
                    if let Some(hub) = registry.upgrade() {
                        hub.state.lock().unwrap().entries.remove(&entry.id);
                    }
                }
                let result_type = contract.result.clone();
                async move {
                    let Some(_flight) = flight else {
                        return Ok(ListenerResult::Continue);
                    };
                    let task = tokio::spawn(async move {
                        if entry.owner.closed() {
                            return Ok(ListenerResult::Continue);
                        }
                        (entry.call)(payload).await
                    });
                    let value = task.await.map_err(|e| {
                        fail(
                            ErrorCode::Business,
                            format!("event listener task failed: {e}"),
                        )
                    })??;
                    if let ListenerResult::Return(ref value) = value {
                        crate::contract::validate_wire(
                            &result_type,
                            &crate::contract::WireValue::Value {
                                value: value.clone(),
                            },
                            &[],
                        )?;
                    }
                    Ok::<_, ProtocolError>(value)
                }
            };
            let mut report = DispatchResult::default();
            match mode {
                EventMode::Parallel => {
                    let tasks = entries
                        .into_iter()
                        .map(|entry| tokio::spawn(invoke(entry, payload.clone())))
                        .collect::<Vec<_>>();
                    for task in tasks {
                        match task.await {
                            Ok(Ok(_)) => {}
                            Ok(Err(error)) => report.errors.push(error),
                            Err(error) => report
                                .errors
                                .push(fail(ErrorCode::Business, error.to_string())),
                        }
                    }
                }
                EventMode::Serial => {
                    for entry in entries {
                        if hub.state.lock().unwrap().publishers[&publisher]
                            .owner
                            .ctx
                            .cancellation_token()
                            .is_cancelled()
                        {
                            report.errors.push(fail(
                                ErrorCode::ScopeClosed,
                                "publisher closed during dispatch",
                            ));
                            break;
                        }
                        match invoke(entry, payload.clone()).await {
                            Ok(ListenerResult::Continue) => {}
                            Ok(ListenerResult::Return(value)) => {
                                report.returned = Some(value);
                                break;
                            }
                            Err(error) => {
                                report.errors.push(error);
                                break;
                            }
                        }
                    }
                }
                _ => unreachable!(),
            }
            let _ = tx.send(Ok(report));
        });
        Box::pin(async move {
            rx.await
                .map_err(|_| fail(ErrorCode::Unavailable, "event dispatch task lost"))?
        })
    }
}
