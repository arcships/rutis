//! In-memory broker transport for binding/lifecycle conformance. It uses the
//! same grants, object pins and graph decoder as the future framed transport.
//! No handler runs while the broker lock is held; nested calls are reentrant.
use crate::broker::Broker;
use crate::contract::{
    callback_key, AdmittedBundle, Interface, Method, Ownership, TypeExpr, WireValue,
};
use crate::error::{ErrorCode, Execution, ProtocolError, Result};
use crate::exports::{Exports, ObjectIds, PinKey};
use crate::graph::{self, DecodedValue, GraphScopes, WireGraph, WireReference};
use crate::identity::{Activation, Delivery, InterfaceView, ObjectIdentity, Scope, Sequence};
use crate::imports::{ImportControl, Imports, ObjectProxy};
use crate::sdk::{bind, CallContext, Caller, ClientHandle, Dispatcher, Outbound, RpcFuture};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, Weak,
};

fn fail(code: ErrorCode, message: &str) -> ProtocolError {
    ProtocolError::new(code, "memory", message)
}
fn next(counter: &AtomicU64) -> Result<Sequence> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map(|n| Sequence(n + 1))
        .map_err(|_| fail(ErrorCode::Unavailable, "sequence exhausted"))
}
struct State {
    broker: Broker,
    endpoints: BTreeMap<Activation, Arc<Endpoint>>,
}
pub struct Network {
    bundle: Arc<AdmittedBundle>,
    interfaces: BTreeMap<String, Interface>,
    state: Mutex<State>,
}
pub struct Endpoint {
    network: Weak<Network>,
    activation: Activation,
    root: Scope,
    exports: Exports,
    imports: Imports,
    native: Option<rutis::Ctx>,
    dispatchers: Mutex<BTreeMap<(ObjectIdentity, InterfaceView), Arc<dyn Dispatcher>>>,
    scopes: AtomicU64,
    calls: AtomicU64,
    staging: AtomicU64,
}
struct EndpointCaller(Arc<Endpoint>);
fn callbacks(expr: &TypeExpr, interfaces: &mut BTreeMap<String, Interface>) {
    match expr {
        TypeExpr::Callback { params, result, .. } => {
            interfaces.insert(
                callback_key(expr),
                Interface {
                    methods: BTreeMap::from([(
                        "call".into(),
                        Method {
                            params: *params.clone(),
                            result: *result.clone(),
                        },
                    )]),
                    properties: BTreeMap::new(),
                },
            );
            callbacks(params, interfaces);
            callbacks(result, interfaces);
        }
        TypeExpr::Record { fields } => {
            for expr in fields.values() {
                callbacks(expr, interfaces);
            }
        }
        TypeExpr::List { item } | TypeExpr::Optional { item } => callbacks(item, interfaces),
        _ => {}
    }
}
impl Network {
    pub fn new(bundle: AdmittedBundle) -> Arc<Self> {
        let mut interfaces = bundle.bundle().interfaces.clone();
        for iface in bundle.bundle().interfaces.values() {
            for method in iface.methods.values() {
                callbacks(&method.params, &mut interfaces);
            }
        }
        Arc::new(Self {
            bundle: Arc::new(bundle),
            interfaces,
            state: Mutex::new(State {
                broker: Broker::default(),
                endpoints: BTreeMap::new(),
            }),
        })
    }
    pub fn endpoint(
        self: &Arc<Self>,
        activation: Activation,
        ids: ObjectIds,
        native: Option<rutis::Ctx>,
    ) -> Result<Arc<Endpoint>> {
        let exports = if let Some(ctx) = &native {
            let gate = crate::managed::ActivationGate::default();
            gate.track_export(ctx)
                .map_err(|e| fail(ErrorCode::ScopeClosed, &e.to_string()))?;
            Exports::managed(ctx, activation.clone(), ids, gate)
                .map_err(|e| fail(ErrorCode::ScopeClosed, &e.to_string()))?
        } else {
            Exports::new(activation.clone(), ids)
        };
        self.endpoint_with_exports(activation, exports, native)
    }
    pub fn endpoint_with_exports(
        self: &Arc<Self>,
        activation: Activation,
        exports: Exports,
        native: Option<rutis::Ctx>,
    ) -> Result<Arc<Endpoint>> {
        if exports.owner() != &activation {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "export table belongs to another activation",
            ));
        }
        if native.is_some() && exports.gate().is_none() {
            return Err(fail(
                ErrorCode::InvalidParams,
                "native endpoint requires managed exports",
            ));
        }
        let root = Scope {
            activation: activation.clone(),
            scope: Sequence(1),
        };
        let imports = Imports::default();
        imports.open_scope(root.clone(), None)?;
        if let Some(gate) = exports.gate() {
            imports.bind_scope(&root, gate)?;
        }
        let mut state = self.state.lock().unwrap();
        state.broker.start_activation(activation.clone())?;
        state.broker.open_scope(root.clone(), None)?;
        let endpoint = Arc::new(Endpoint {
            network: Arc::downgrade(self),
            activation: activation.clone(),
            root,
            exports,
            imports,
            native,
            dispatchers: Mutex::default(),
            scopes: AtomicU64::new(1),
            calls: AtomicU64::new(0),
            staging: AtomicU64::new(0),
        });
        state.endpoints.insert(activation, endpoint.clone());
        drop(state);
        if let Some(ctx) = &endpoint.native {
            let cleanup = endpoint.clone();
            if let Err(error) = ctx.effect_named("protocol endpoint", move || {
                rutis::Effect::Disposer(Box::new(move || {
                    cleanup.close();
                    Ok(())
                }))
            }) {
                endpoint.close();
                return Err(fail(ErrorCode::ScopeClosed, &error.to_string()));
            }
        }
        Ok(endpoint)
    }
    fn endpoint_for(&self, owner: &Activation) -> Result<Arc<Endpoint>> {
        self.state
            .lock()
            .unwrap()
            .endpoints
            .get(owner)
            .cloned()
            .ok_or_else(|| fail(ErrorCode::StaleObject, "owner unavailable"))
    }
    fn flush(&self, recipient: &Endpoint) {
        for control in recipient.imports.take_controls() {
            match control {
                ImportControl::Accept { id, token } => {
                    let _ =
                        self.state
                            .lock()
                            .unwrap()
                            .broker
                            .accept(&recipient.activation, id, &token);
                }
                ImportControl::Release { id, token } => {
                    // The owner is obtained from our delivery record, never from
                    // an author's id/token pair. Pin release stays idempotent.
                    let owner = {
                        let mut state = self.state.lock().unwrap();
                        let owner = state
                            .broker
                            .delivery_object(&recipient.activation, id, &token)
                            .ok()
                            .flatten()
                            .and_then(|o| state.endpoints.get(&o.owner).cloned());
                        let _ = state.broker.release(&recipient.activation, id, &token);
                        owner
                    };
                    if let Some(owner) = owner {
                        owner.exports.release(&PinKey::Delivery {
                            recipient: recipient.activation.clone(),
                            id,
                        });
                    }
                }
            }
        }
    }
    fn close_scope(&self, endpoint: &Endpoint, scope: &Scope) {
        endpoint.imports.close_scope(scope);
        self.flush(endpoint);
        self.state.lock().unwrap().broker.close_scope(scope);
    }
    pub fn pins(&self, object: &ObjectIdentity) -> (usize, usize) {
        self.state.lock().unwrap().broker.pins(object)
    }
}
impl Drop for Network {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap();
        for endpoint in state.endpoints.values() {
            endpoint.imports.close_scope(&endpoint.root);
            endpoint.exports.close();
        }
    }
}
impl Endpoint {
    pub fn caller(self: &Arc<Self>) -> Arc<dyn Caller> {
        Arc::new(EndpointCaller(self.clone()))
    }
    pub fn exports(&self) -> &Exports {
        &self.exports
    }
    pub fn imports(&self) -> &Imports {
        &self.imports
    }
    pub fn import<H: ClientHandle>(
        self: &Arc<Self>,
        owner: &Arc<Endpoint>,
        value: Outbound,
    ) -> Result<H> {
        let network = self
            .network
            .upgrade()
            .ok_or_else(|| fail(ErrorCode::Unavailable, "network closed"))?;
        let expr = TypeExpr::Object {
            interface: H::INTERFACE.into(),
            ownership: Ownership::Scope,
        };
        let graph = Encoder::encode(
            network.clone(),
            owner.clone(),
            self.clone(),
            &expr,
            value,
            &GraphScopes::in_scope(self.root.clone()),
        )?;
        bind(
            graph.consume(&expr, &GraphScopes::in_scope(self.root.clone()))?,
            self.caller(),
        )
    }
    pub fn flush(&self) {
        if let Some(network) = self.network.upgrade() {
            network.flush(self);
        }
    }
    pub fn close(&self) {
        if let Some(network) = self.network.upgrade() {
            let recipients: Vec<_> = network
                .state
                .lock()
                .unwrap()
                .endpoints
                .values()
                .cloned()
                .collect();
            for recipient in recipients {
                recipient.imports.revoke_owner(&self.activation);
                network.flush(&recipient);
            }
            network.close_scope(self, &self.root);
            network
                .state
                .lock()
                .unwrap()
                .broker
                .close_activation(&self.activation);
        }
        self.exports.close();
    }
}
struct Envelope {
    network: Arc<Network>,
    recipient: Arc<Endpoint>,
    graph: Option<WireGraph>,
}
impl Envelope {
    fn consume(mut self, expr: &TypeExpr, scopes: &GraphScopes) -> Result<DecodedValue> {
        let result = self.recipient.imports.receive_graph(
            &self.network.bundle,
            expr,
            self.graph.take().unwrap(),
            scopes,
        );
        self.network.flush(&self.recipient);
        result
    }
}
impl Drop for Envelope {
    fn drop(&mut self) {
        if let Some(graph) = self.graph.take() {
            self.recipient.imports.reject(
                &graph
                    .references
                    .into_iter()
                    .map(|r| r.delivery)
                    .collect::<Vec<_>>(),
            );
            self.network.flush(&self.recipient);
        }
    }
}
struct Encoder {
    network: Arc<Network>,
    sender: Arc<Endpoint>,
    recipient: Arc<Endpoint>,
    references: Vec<WireReference>,
    indices: BTreeMap<(Scope, ObjectIdentity, InterfaceView), usize>,
    staging: Vec<(Exports, PinKey)>,
    offered: Vec<(Exports, Delivery)>,
    committed: bool,
}
impl Encoder {
    fn encode(
        network: Arc<Network>,
        sender: Arc<Endpoint>,
        recipient: Arc<Endpoint>,
        expr: &TypeExpr,
        value: Outbound,
        scopes: &GraphScopes,
    ) -> Result<Envelope> {
        sender.imports.require_scope(&sender.root)?;
        recipient.imports.require_scope(&scopes.scope)?;
        recipient.imports.require_scope(&scopes.borrow)?;
        {
            let state = network.state.lock().unwrap();
            state.broker.require_scope(&sender.root)?;
            state.broker.require_scope(&scopes.scope)?;
            state.broker.require_scope(&scopes.borrow)?;
        }
        let mut encoder = Self {
            network,
            sender,
            recipient,
            references: Vec::new(),
            indices: BTreeMap::new(),
            staging: Vec::new(),
            offered: Vec::new(),
            committed: false,
        };
        let root = encoder.value(expr, value, scopes, None)?;
        let graph = WireGraph {
            root,
            references: std::mem::take(&mut encoder.references),
        };
        graph::validate(&encoder.network.bundle, expr, &graph, scopes)?;
        encoder.committed = true;
        Ok(Envelope {
            network: encoder.network.clone(),
            recipient: encoder.recipient.clone(),
            graph: Some(graph),
        })
    }
    fn value(
        &mut self,
        expr: &TypeExpr,
        value: Outbound,
        scopes: &GraphScopes,
        inherited: Option<&Scope>,
    ) -> Result<WireValue> {
        Ok(match (expr, value) {
            (TypeExpr::Value { .. }, Outbound::Value(value)) => WireValue::Value { value },
            (TypeExpr::Record { fields: types }, Outbound::Record(mut fields))
                if fields.len() == types.len() =>
            {
                let fields = types
                    .iter()
                    .map(|(name, expr)| {
                        Ok((
                            name.clone(),
                            self.value(
                                expr,
                                fields.remove(name).ok_or_else(|| {
                                    fail(ErrorCode::InvalidParams, "missing field")
                                })?,
                                scopes,
                                inherited,
                            )?,
                        ))
                    })
                    .collect::<Result<_>>()?;
                WireValue::Record { fields }
            }
            (TypeExpr::List { item }, Outbound::List(items)) => WireValue::List {
                items: items
                    .into_iter()
                    .map(|v| self.value(item, v, scopes, inherited))
                    .collect::<Result<_>>()?,
            },
            (TypeExpr::Optional { item }, Outbound::Optional(value)) => WireValue::Optional {
                value: value
                    .map(|v| self.value(item, *v, scopes, inherited).map(Box::new))
                    .transpose()?,
            },
            (
                TypeExpr::Object {
                    interface,
                    ownership,
                },
                value @ (Outbound::Own(_) | Outbound::Foreign(_)),
            ) => {
                let scope = inherited.unwrap_or(if *ownership == Ownership::Scope {
                    &scopes.scope
                } else {
                    &scopes.borrow
                });
                return self.object(interface, value, scopes, scope);
            }
            (TypeExpr::Callback { .. }, value @ (Outbound::Own(_) | Outbound::Foreign(_))) => {
                return self.object(&callback_key(expr), value, scopes, &scopes.borrow)
            }
            _ => {
                return Err(fail(
                    ErrorCode::InvalidParams,
                    "outbound value does not match generated contract",
                ))
            }
        })
    }
    fn object(
        &mut self,
        interface: &str,
        value: Outbound,
        scopes: &GraphScopes,
        scope: &Scope,
    ) -> Result<WireValue> {
        let iface = self
            .network
            .interfaces
            .get(interface)
            .cloned()
            .ok_or_else(|| fail(ErrorCode::InterfaceMismatch, "interface unavailable"))?;
        let (identity, view, owned, foreign) = match value {
            Outbound::Own(native) => {
                let registered = native.register(&self.sender.exports)?;
                if registered.bundle_sha256 != self.network.bundle.sha256()
                    || registered.interface != interface
                    || registered.identity.owner != self.sender.activation
                {
                    return Err(fail(
                        ErrorCode::InterfaceMismatch,
                        "native adapter contract mismatch",
                    ));
                }
                let key = PinKey::Staging(next(&self.sender.staging)?);
                self.sender.exports.pin(&registered.identity, key.clone())?;
                self.staging.push((self.sender.exports.clone(), key));
                let view = InterfaceView {
                    interface: interface.into(),
                    bundle_sha256: self.network.bundle.sha256().into(),
                    source: self.network.bundle.bundle().id.clone(),
                };
                self.network
                    .state
                    .lock()
                    .unwrap()
                    .broker
                    .register_export_view(
                        &self.sender.activation,
                        registered.identity.clone(),
                        view.clone(),
                        iface.methods.keys().cloned().collect(),
                    )?;
                self.sender.dispatchers.lock().unwrap().insert(
                    (registered.identity.clone(), view.clone()),
                    registered.dispatcher,
                );
                (registered.identity, view, Some(native), None)
            }
            Outbound::Foreign(proxy) => {
                let delivery = proxy.delivery()?;
                if delivery.recipient.activation != self.sender.activation
                    || delivery.view.interface != interface
                    || delivery.view.bundle_sha256 != self.network.bundle.sha256()
                {
                    return Err(fail(
                        ErrorCode::CapabilityDenied,
                        "foreign object does not belong to sender",
                    ));
                }
                (delivery.object, delivery.view, None, Some(proxy))
            }
            _ => unreachable!(),
        };
        let cache = (scope.clone(), identity.clone(), view.clone());
        if let Some(index) = self.indices.get(&cache) {
            return Ok(WireValue::Ref { index: *index });
        }
        let owner = self.network.endpoint_for(&identity.owner)?;
        let delivery = if let Some(proxy) = &foreign {
            let old = proxy.delivery()?;
            self.network
                .state
                .lock()
                .unwrap()
                .broker
                .pass_back(&old.recipient, &old, scope)?
        } else {
            self.network.state.lock().unwrap().broker.offer(
                &identity.owner,
                &identity,
                scope,
                &view,
            )?
        };
        if let Err(error) = owner.exports.pin(
            &identity,
            PinKey::Delivery {
                recipient: scope.activation.clone(),
                id: delivery.id,
            },
        ) {
            let _ = self.network.state.lock().unwrap().broker.release(
                &scope.activation,
                delivery.id,
                &delivery.token,
            );
            return Err(error);
        }
        self.offered.push((owner.exports.clone(), delivery.clone()));
        let index = self.references.len();
        self.indices.insert(cache, index);
        self.references.push(WireReference {
            delivery,
            properties: BTreeMap::new(),
        });
        let mut snapshot = if let Some(native) = owned {
            native.snapshot()?
        } else {
            iface
                .properties
                .keys()
                .map(|name| {
                    Ok((
                        name.clone(),
                        outbound(foreign.as_ref().unwrap().property(name)?),
                    ))
                })
                .collect::<Result<_>>()?
        };
        if snapshot.len() != iface.properties.len() {
            return Err(fail(ErrorCode::InvalidParams, "snapshot fields mismatch"));
        }
        let properties = iface
            .properties
            .iter()
            .map(|(name, expr)| {
                Ok((
                    name.clone(),
                    self.value(
                        expr,
                        snapshot.remove(name).ok_or_else(|| {
                            fail(ErrorCode::InvalidParams, "snapshot property missing")
                        })?,
                        scopes,
                        Some(scope),
                    )?,
                ))
            })
            .collect::<Result<_>>()?;
        self.references[index].properties = properties;
        Ok(WireValue::Ref { index })
    }
}
fn outbound(value: DecodedValue) -> Outbound {
    match value {
        DecodedValue::Value(value) => Outbound::Value(value),
        DecodedValue::Object(value) => Outbound::Foreign(value),
        DecodedValue::Record(fields) => {
            Outbound::Record(fields.into_iter().map(|(k, v)| (k, outbound(v))).collect())
        }
        DecodedValue::List(items) => Outbound::List(items.into_iter().map(outbound).collect()),
        DecodedValue::Optional(value) => Outbound::Optional(value.map(|v| Box::new(outbound(*v)))),
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.committed {
            for (exports, delivery) in &self.offered {
                let _ = self.network.state.lock().unwrap().broker.release(
                    &delivery.recipient.activation,
                    delivery.id,
                    &delivery.token,
                );
                exports.release(&PinKey::Delivery {
                    recipient: delivery.recipient.activation.clone(),
                    id: delivery.id,
                });
            }
        }
        for (exports, key) in &self.staging {
            exports.release(key);
        }
    }
}
struct ExecutionLease {
    network: Arc<Network>,
    owner: Arc<Endpoint>,
    caller: Activation,
    call: Sequence,
    scope: Option<Scope>,
}
impl Drop for ExecutionLease {
    fn drop(&mut self) {
        if let Some(scope) = &self.scope {
            self.network.close_scope(&self.owner, scope);
        }
        self.owner.exports.release(&PinKey::Execution {
            caller: self.caller.clone(),
            call: self.call,
        });
        let _ = self.network.state.lock().unwrap().broker.finish_call(
            &self.owner.activation,
            &self.caller,
            self.call,
        );
    }
}
impl Caller for EndpointCaller {
    fn call(
        &self,
        target: ObjectProxy,
        method: String,
        params: Outbound,
    ) -> RpcFuture<DecodedValue> {
        let sender = self.0.clone();
        Box::pin(async move {
            let network = sender
                .network
                .upgrade()
                .ok_or_else(|| fail(ErrorCode::Unavailable, "network closed"))?;
            let delivery = target.delivery()?;
            if delivery.recipient.activation != sender.activation {
                return Err(fail(
                    ErrorCode::CapabilityDenied,
                    "caller cannot use another activation's grant",
                ));
            }
            let contract = network
                .interfaces
                .get(&delivery.view.interface)
                .and_then(|iface| iface.methods.get(&method))
                .cloned()
                .ok_or_else(|| fail(ErrorCode::CapabilityDenied, "method outside view"))?;
            let owner = network.endpoint_for(&delivery.object.owner)?;
            let dispatcher = owner
                .dispatchers
                .lock()
                .unwrap()
                .get(&(delivery.object.clone(), delivery.view.clone()))
                .cloned()
                .ok_or_else(|| fail(ErrorCode::Unavailable, "dispatch adapter unavailable"))?;
            let call = {
                let mut state = network.state.lock().unwrap();
                let call = next(&sender.calls)?;
                state.broker.begin_call(
                    &delivery.recipient,
                    call,
                    &delivery,
                    &method,
                    &owner.activation,
                )?;
                call
            };
            let key = PinKey::Execution {
                caller: sender.activation.clone(),
                call,
            };
            if let Err(error) = owner.exports.pin(&delivery.object, key.clone()) {
                network.state.lock().unwrap().broker.finish_call(
                    &owner.activation,
                    &sender.activation,
                    call,
                )?;
                return Err(error);
            }
            let mut lease = ExecutionLease {
                network: network.clone(),
                owner: owner.clone(),
                caller: sender.activation.clone(),
                call,
                scope: None,
            };
            let admitted = {
                let mut state = network.state.lock().unwrap();
                (|| {
                    let scope = Scope {
                        activation: owner.activation.clone(),
                        scope: next(&owner.scopes)?,
                    };
                    lease.scope = Some(scope.clone());
                    state
                        .broker
                        .open_scope(scope.clone(), Some(owner.root.clone()))?;
                    owner
                        .imports
                        .open_scope(scope.clone(), Some(owner.root.clone()))?;
                    Ok::<_, ProtocolError>(scope)
                })()
            };
            let scope = admitted?;
            let param_scopes = GraphScopes {
                scope: owner.root.clone(),
                borrow: scope,
            };
            let args = Encoder::encode(
                network.clone(),
                sender.clone(),
                owner.clone(),
                &contract.params,
                params,
                &param_scopes,
            )?
            .consume(&contract.params, &param_scopes)?;
            let context = CallContext::new(owner.caller(), owner.native.clone());
            let (send, receive) = tokio::sync::oneshot::channel();
            let result_scopes = GraphScopes::in_scope(delivery.recipient.clone());
            let receive_scopes = result_scopes.clone();
            let result_type = contract.result.clone();
            tokio::spawn(async move {
                let _lease = lease;
                let task = tokio::spawn(dispatcher.dispatch(key, context.clone(), method, args));
                let result = task
                    .await
                    .unwrap_or_else(|e| Err(fail(ErrorCode::Business, &e.to_string())));
                let children = context.finish().await;
                let result = result
                    .and_then(|value| children.map(|_| value))
                    .and_then(|value| {
                        if send.is_closed() {
                            return Err(fail(ErrorCode::Cancelled, "result waiter dropped"));
                        }
                        Encoder::encode(
                            network,
                            owner,
                            sender,
                            &contract.result,
                            value,
                            &result_scopes,
                        )
                    })
                    .map_err(|mut error| {
                        error.execution = Execution::Unknown;
                        error
                    });
                let _ = send.send(result);
            });
            receive
                .await
                .map_err(|_| fail(ErrorCode::Unavailable, "owner execution task unavailable"))??
                .consume(&result_type, &receive_scopes)
        })
    }
}
