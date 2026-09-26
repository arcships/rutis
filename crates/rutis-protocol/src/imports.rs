//! A single runtime import ledger serializes receive/release/scope-close. Proxy
//! identity is cached independently of delivery ids; released wrappers stay dead.
use crate::contract::{AdmittedBundle, TypeExpr};
use crate::error::{ErrorCode, ProtocolError, Result};
use crate::graph::{self, DecodedValue, GraphScopes, SnapshotValue, WireGraph};
use crate::identity::{Delivery, InterfaceView, ObjectIdentity, Scope, Sequence};
use crate::managed::ActivationGate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportControl {
    Accept { id: Sequence, token: String },
    Release { id: Sequence, token: String },
}
/// Runtime-wide evidence, including rejected envelopes. Zero prefixes are
/// omitted because wire sequence numbers are canonical positive decimals.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retirement {
    pub received_through: Sequence,
    pub terminal_through: Sequence,
}
impl std::fmt::Debug for ImportControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Accept { .. } => "Accept(<redacted>)",
            Self::Release { .. } => "Release(<redacted>)",
        })
    }
}

struct Seen {
    delivery: Delivery,
    terminal: bool,
}
struct Wrapper {
    object: ObjectIdentity,
    view: InterfaceView,
    scope: Scope,
    tokens: BTreeSet<Sequence>,
    active: bool,
    snapshot: Option<BTreeMap<String, SnapshotValue>>,
}
type CacheKey = (Scope, ObjectIdentity, InterfaceView);

#[derive(Default)]
struct State {
    scopes: BTreeMap<Scope, Option<Scope>>,
    gates: BTreeMap<Scope, ActivationGate>,
    closed_owners: BTreeSet<crate::identity::Activation>,
    cache: BTreeMap<CacheKey, Arc<Mutex<Wrapper>>>,
    seen: BTreeMap<Sequence, Seen>,
    retired: u64,
    controls: Vec<ImportControl>,
    latest_scopes: BTreeMap<crate::identity::Activation, Sequence>,
}

#[derive(Clone, Default)]
pub struct Imports(Arc<Mutex<State>>);

/// Cloning is an alias, not a new scope or ownership token. release() closes
/// this wrapper for every alias. Independent users must use independent scopes.
#[derive(Clone)]
pub struct ObjectProxy {
    wrapper: Arc<Mutex<Wrapper>>,
    imports: Weak<Mutex<State>>,
    availability: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

fn error(code: ErrorCode, message: &str) -> ProtocolError {
    ProtocolError::new(code, "import", message)
}

impl Imports {
    pub(crate) fn require_scope(&self, scope: &Scope) -> Result<()> {
        if scope_open(&self.0.lock().unwrap(), scope) {
            Ok(())
        } else {
            Err(error(ErrorCode::ScopeClosed, "native scope closed"))
        }
    }
    /// Runner-owned native admission. A child scope inherits every ancestor's
    /// gate, so cached properties cannot escape synchronous invalidation.
    pub fn bind_scope(&self, scope: &Scope, gate: ActivationGate) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if !scope_open(&state, scope) {
            return Err(error(ErrorCode::ScopeClosed, "scope closed"));
        }
        if state.gates.contains_key(scope) {
            return Err(error(ErrorCode::InvalidParams, "scope already bound"));
        }
        state.gates.insert(scope.clone(), gate);
        Ok(())
    }
    pub fn open_scope(&self, scope: Scope, parent: Option<Scope>) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.scopes.contains_key(&scope) {
            return Err(error(ErrorCode::InvalidParams, "scope already open"));
        }
        if parent
            .as_ref()
            .is_some_and(|p| p.activation != scope.activation || !scope_open(&state, p))
        {
            return Err(error(ErrorCode::ScopeClosed, "parent scope closed"));
        }
        if state
            .latest_scopes
            .get(&scope.activation)
            .is_some_and(|last| scope.scope <= *last)
        {
            return Err(error(ErrorCode::StaleObject, "scope id cannot be reused"));
        }
        state
            .latest_scopes
            .insert(scope.activation.clone(), scope.scope);
        state.scopes.insert(scope, parent);
        Ok(())
    }

    /// Only the authenticated broker connection supplies Delivery records.
    pub fn receive(&self, delivery: Delivery) -> Result<ObjectProxy> {
        Ok(self.receive_batch(vec![delivery])?.remove(0))
    }

    /// Validate the entire envelope before attaching any token. A failed graph
    /// only releases its new deliveries; previously returned aliases stay live.
    pub fn receive_batch(&self, deliveries: Vec<Delivery>) -> Result<Vec<ObjectProxy>> {
        let mut state = self.0.lock().unwrap();
        if let Err(error) = check_batch(&state, &deliveries) {
            reject_deliveries(&mut state, &deliveries);
            return Err(error);
        }
        Ok(attach_batch(
            &mut state,
            deliveries,
            Arc::downgrade(&self.0),
        ))
    }

    /// First create every wrapper, then connect immutable identity links. No
    /// wrapper owns another wrapper, so graph cycles do not create Arc cycles.
    pub fn receive_graph(
        &self,
        bundle: &AdmittedBundle,
        expr: &TypeExpr,
        graph: WireGraph,
        scopes: &GraphScopes,
    ) -> Result<DecodedValue> {
        let deliveries: Vec<_> = graph
            .references
            .iter()
            .map(|r| r.delivery.clone())
            .collect();
        let validated = match graph::validate(bundle, expr, &graph, scopes) {
            Ok(graph) => graph,
            Err(error) => {
                self.reject(&deliveries);
                return Err(error);
            }
        };
        let mut state = self.0.lock().unwrap();
        let admitted = (|| {
            if !scope_open(&state, &scopes.scope) || !scope_open(&state, &scopes.borrow) {
                return Err(error(
                    ErrorCode::ScopeClosed,
                    "graph receiving scope closed",
                ));
            }
            check_batch(&state, &deliveries)?;
            for (delivery, fields) in deliveries.iter().zip(&validated.properties) {
                let key = (
                    delivery.recipient.clone(),
                    delivery.object.clone(),
                    delivery.view.clone(),
                );
                if let Some(wrapper) = state.cache.get(&key) {
                    let wrapper = wrapper.lock().unwrap();
                    if wrapper.snapshot.as_ref().is_some_and(|old| old != fields) {
                        return Err(error(
                            ErrorCode::InvalidParams,
                            "immutable snapshot changed for a live object view",
                        ));
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            reject_deliveries(&mut state, &deliveries);
            return Err(error);
        }
        let proxies = attach_batch(&mut state, deliveries, Arc::downgrade(&self.0));
        for (proxy, fields) in proxies.iter().zip(validated.properties) {
            proxy.wrapper.lock().unwrap().snapshot.get_or_insert(fields);
        }
        materialize(&state, &validated.root, Arc::downgrade(&self.0))
    }

    /// Typed graph validation, cancelled results and unconsumed event payloads
    /// use this before handing anything to author code. Replays never release
    /// an already accepted token owned by a previous successful delivery.
    pub fn reject(&self, deliveries: &[Delivery]) {
        reject_deliveries(&mut self.0.lock().unwrap(), deliveries);
    }

    pub fn revoke_owner(&self, owner: &crate::identity::Activation) {
        let mut state = self.0.lock().unwrap();
        state.closed_owners.insert(owner.clone());
        let wrappers: Vec<_> = state
            .cache
            .iter()
            .filter(|((_, object, _), _)| &object.owner == owner)
            .map(|(_, w)| w.clone())
            .collect();
        for wrapper in wrappers {
            release_wrapper(&mut state, &wrapper);
        }
    }

    pub fn close_scope(&self, scope: &Scope) {
        let mut state = self.0.lock().unwrap();
        let mut closed = BTreeSet::from([scope.clone()]);
        loop {
            let count = closed.len();
            for (child, parent) in &state.scopes {
                if parent.as_ref().is_some_and(|p| closed.contains(p)) {
                    closed.insert(child.clone());
                }
            }
            if count == closed.len() {
                break;
            }
        }
        state.scopes.retain(|s, _| !closed.contains(s));
        state.gates.retain(|s, _| !closed.contains(s));
        let wrappers: Vec<_> = state
            .cache
            .iter()
            .filter(|((scope, _, _), _)| closed.contains(scope))
            .map(|(_, w)| w.clone())
            .collect();
        for wrapper in wrappers {
            release_wrapper(&mut state, &wrapper);
        }
        let pending: Vec<_> = state
            .seen
            .iter()
            .filter(|(_, seen)| !seen.terminal && closed.contains(&seen.delivery.recipient))
            .map(|(id, _)| *id)
            .collect();
        for id in pending {
            release_token(&mut state, id);
        }
        state
            .cache
            .retain(|(scope, _, _), _| !closed.contains(scope));
    }

    /// Receiver-side evidence for a terminal contiguous prefix. Out-of-order
    /// receipt is allowed, but a gap or live delivery prevents retirement.
    pub fn acknowledge_retirement(&self, through: Sequence) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if through.0 == state.retired {
            return Ok(());
        }
        if through.0 < state.retired {
            return Err(error(
                ErrorCode::InvalidParams,
                "watermark cannot move backwards",
            ));
        }
        let entries: Vec<_> = state
            .seen
            .range(Sequence(state.retired + 1)..=through)
            .collect();
        if entries.len() as u64 != through.0 - state.retired
            || entries.iter().any(|(_, seen)| !seen.terminal)
        {
            return Err(error(
                ErrorCode::InvalidParams,
                "delivery prefix has a gap or live token",
            ));
        }
        state.seen.retain(|id, _| id.0 > through.0);
        state
            .cache
            .retain(|_, wrapper| wrapper.lock().unwrap().active);
        state.retired = through.0;
        Ok(())
    }

    pub fn retirement(&self) -> Option<Retirement> {
        let state = self.0.lock().unwrap();
        let mut received = state.retired;
        let mut terminal = state.retired;
        let mut all_terminal = true;
        for (id, seen) in state
            .seen
            .range(Sequence(state.retired.saturating_add(1))..)
        {
            if received.checked_add(1) != Some(id.0) {
                break;
            }
            received = id.0;
            all_terminal &= seen.terminal;
            if all_terminal {
                terminal = id.0;
            }
        }
        (terminal > 0).then_some(Retirement {
            received_through: Sequence(received),
            terminal_through: Sequence(terminal),
        })
    }

    pub fn retained_objects(&self) -> usize {
        self.0.lock().unwrap().cache.len()
    }

    /// A structurally invalid payload can contain a valid broker manifest.
    /// Reject those handoffs before returning its decoding error. Invalid
    /// JSON/UTF-8 or an unreadable manifest requires transport epoch closure.
    pub fn receive_graph_value(
        &self,
        bundle: &AdmittedBundle,
        expr: &TypeExpr,
        value: serde_json::Value,
        scopes: &GraphScopes,
    ) -> Result<DecodedValue> {
        let deliveries: Vec<_> = value
            .get("references")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|reference| reference.get("delivery"))
            .filter_map(|delivery| serde_json::from_value::<Delivery>(delivery.clone()).ok())
            .collect();
        match serde_json::from_value(value) {
            Ok(graph) => self.receive_graph(bundle, expr, graph, scopes),
            Err(error) => {
                self.reject(&deliveries);
                Err(ProtocolError::new(
                    ErrorCode::InvalidParams,
                    "graph",
                    error.to_string(),
                ))
            }
        }
    }

    pub fn take_controls(&self) -> Vec<ImportControl> {
        std::mem::take(&mut self.0.lock().unwrap().controls)
    }
}

fn scope_open(state: &State, scope: &Scope) -> bool {
    let mut current = Some(scope);
    while let Some(scope) = current {
        let Some(parent) = state.scopes.get(scope) else {
            return false;
        };
        if state.gates.get(scope).is_some_and(|gate| !gate.is_open()) {
            return false;
        }
        current = parent.as_ref();
    }
    true
}

fn check_batch(state: &State, deliveries: &[Delivery]) -> Result<()> {
    let mut batch = BTreeMap::new();
    for delivery in deliveries {
        check_delivery(state, delivery)?;
        if batch
            .insert(delivery.id, delivery)
            .is_some_and(|old| old != delivery)
        {
            return Err(error(
                ErrorCode::CapabilityDenied,
                "conflicting delivery records in one graph",
            ));
        }
    }
    Ok(())
}

fn attach_batch(
    state: &mut State,
    deliveries: Vec<Delivery>,
    imports: Weak<Mutex<State>>,
) -> Vec<ObjectProxy> {
    deliveries
        .into_iter()
        .map(|delivery| {
            let key = (
                delivery.recipient.clone(),
                delivery.object.clone(),
                delivery.view.clone(),
            );
            let wrapper = state
                .cache
                .get(&key)
                .filter(|w| w.lock().unwrap().active)
                .cloned()
                .unwrap_or_else(|| {
                    Arc::new(Mutex::new(Wrapper {
                        object: delivery.object.clone(),
                        view: delivery.view.clone(),
                        scope: delivery.recipient.clone(),
                        tokens: BTreeSet::new(),
                        active: true,
                        snapshot: None,
                    }))
                });
            wrapper.lock().unwrap().tokens.insert(delivery.id);
            state.cache.insert(key, wrapper.clone());
            state.controls.push(ImportControl::Accept {
                id: delivery.id,
                token: delivery.token.clone(),
            });
            state.seen.entry(delivery.id).or_insert(Seen {
                delivery,
                terminal: false,
            });
            ObjectProxy {
                wrapper,
                imports: imports.clone(),
                availability: None,
            }
        })
        .collect()
}

fn materialize(
    state: &State,
    value: &SnapshotValue,
    imports: Weak<Mutex<State>>,
) -> Result<DecodedValue> {
    Ok(match value {
        SnapshotValue::Value(value) => DecodedValue::Value(value.clone()),
        SnapshotValue::Link(link) => {
            let wrapper = state
                .cache
                .get(&link.key())
                .filter(|w| w.lock().unwrap().active)
                .ok_or_else(|| {
                    error(
                        ErrorCode::ScopeClosed,
                        "snapshot relationship has no live grant",
                    )
                })?;
            DecodedValue::Object(ObjectProxy {
                wrapper: wrapper.clone(),
                imports,
                availability: None,
            })
        }
        SnapshotValue::Record(fields) => DecodedValue::Record(
            fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), materialize(state, v, imports.clone())?)))
                .collect::<Result<_>>()?,
        ),
        SnapshotValue::List(items) => DecodedValue::List(
            items
                .iter()
                .map(|v| materialize(state, v, imports.clone()))
                .collect::<Result<_>>()?,
        ),
        SnapshotValue::Optional(value) => DecodedValue::Optional(
            value
                .as_ref()
                .map(|v| materialize(state, v, imports).map(Box::new))
                .transpose()?,
        ),
    })
}

fn check_delivery(state: &State, delivery: &Delivery) -> Result<()> {
    if state.closed_owners.contains(&delivery.object.owner) {
        return Err(error(ErrorCode::StaleObject, "owner activation closed"));
    }
    if delivery.id.0 <= state.retired {
        return Err(error(
            ErrorCode::StaleObject,
            "delivery is below the confirmed retirement watermark",
        ));
    }
    if !scope_open(state, &delivery.recipient) {
        return Err(error(ErrorCode::ScopeClosed, "delivery scope closed"));
    }
    if let Some(seen) = state.seen.get(&delivery.id) {
        if seen.delivery != *delivery {
            return Err(error(
                ErrorCode::CapabilityDenied,
                "delivery id changed identity",
            ));
        }
        if seen.terminal {
            return Err(error(
                ErrorCode::StaleObject,
                "released delivery cannot revive a wrapper",
            ));
        }
    }
    Ok(())
}

fn reject_deliveries(state: &mut State, deliveries: &[Delivery]) {
    for delivery in deliveries {
        if delivery.id.0 <= state.retired || state.seen.contains_key(&delivery.id) {
            continue;
        }
        state.seen.insert(
            delivery.id,
            Seen {
                delivery: delivery.clone(),
                terminal: false,
            },
        );
        release_token(state, delivery.id);
    }
}

fn release_token(state: &mut State, id: Sequence) {
    if let Some(seen) = state.seen.get_mut(&id) {
        if !seen.terminal {
            seen.terminal = true;
            state.controls.push(ImportControl::Release {
                id,
                token: seen.delivery.token.clone(),
            });
        }
    }
}

fn release_wrapper(state: &mut State, wrapper_arc: &Arc<Mutex<Wrapper>>) {
    let mut wrapper = wrapper_arc.lock().unwrap();
    if !wrapper.active {
        return;
    }
    wrapper.active = false;
    for id in std::mem::take(&mut wrapper.tokens) {
        release_token(state, id);
    }
    let key = (
        wrapper.scope.clone(),
        wrapper.object.clone(),
        wrapper.view.clone(),
    );
    if state
        .cache
        .get(&key)
        .is_some_and(|current| Arc::ptr_eq(current, wrapper_arc))
    {
        state.cache.remove(&key);
    }
}

impl ObjectProxy {
    pub(crate) fn with_availability(mut self, check: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.availability = Some(match self.availability.take() {
            Some(previous) => Arc::new(move || previous() && check()),
            None => check,
        });
        self
    }
    fn require_available(&self) -> Result<()> {
        if self.availability.as_ref().is_some_and(|check| !check()) {
            return Err(error(
                ErrorCode::Unavailable,
                "native service publication is closed",
            ));
        }
        Ok(())
    }
    pub fn same_wrapper(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.wrapper, &other.wrapper)
    }
    pub fn same_object(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
    pub fn identity(&self) -> ObjectIdentity {
        self.wrapper.lock().unwrap().object.clone()
    }
    pub fn view(&self) -> InterfaceView {
        self.wrapper.lock().unwrap().view.clone()
    }
    pub fn scope(&self) -> Scope {
        self.wrapper.lock().unwrap().scope.clone()
    }

    pub fn delivery(&self) -> Result<Delivery> {
        self.require_available()?;
        let imports = self
            .imports
            .upgrade()
            .ok_or_else(|| error(ErrorCode::ScopeClosed, "runtime import ledger dropped"))?;
        let state = imports.lock().unwrap();
        let wrapper = self.wrapper.lock().unwrap();
        if !wrapper.active || !scope_open(&state, &wrapper.scope) {
            return Err(error(ErrorCode::ScopeClosed, "proxy wrapper released"));
        }
        let id = wrapper
            .tokens
            .first()
            .ok_or_else(|| error(ErrorCode::StaleObject, "proxy has no delivery token"))?;
        Ok(state.seen.get(id).unwrap().delivery.clone())
    }

    /// Only declared, fully materialized immutable properties are readable.
    /// An identity link resolves the current live wrapper in this same scope;
    /// explicit release never resurrects a previously returned child alias.
    pub fn property(&self, name: &str) -> Result<DecodedValue> {
        self.require_available()?;
        let imports = self
            .imports
            .upgrade()
            .ok_or_else(|| error(ErrorCode::ScopeClosed, "runtime import ledger dropped"))?;
        let state = imports.lock().unwrap();
        let snapshot = {
            let wrapper = self.wrapper.lock().unwrap();
            if !wrapper.active || !scope_open(&state, &wrapper.scope) {
                return Err(error(ErrorCode::ScopeClosed, "proxy wrapper released"));
            }
            wrapper
                .snapshot
                .as_ref()
                .ok_or_else(|| {
                    error(
                        ErrorCode::Unavailable,
                        "object snapshot has not been materialized",
                    )
                })?
                .get(name)
                .cloned()
                .ok_or_else(|| error(ErrorCode::CapabilityDenied, "property is not declared"))?
        };
        let value = materialize(&state, &snapshot, Arc::downgrade(&imports))?;
        fn guarded(
            value: DecodedValue,
            check: &Arc<dyn Fn() -> bool + Send + Sync>,
        ) -> DecodedValue {
            match value {
                DecodedValue::Object(proxy) => {
                    DecodedValue::Object(proxy.with_availability(check.clone()))
                }
                DecodedValue::Record(fields) => DecodedValue::Record(
                    fields
                        .into_iter()
                        .map(|(key, value)| (key, guarded(value, check)))
                        .collect(),
                ),
                DecodedValue::List(values) => DecodedValue::List(
                    values
                        .into_iter()
                        .map(|value| guarded(value, check))
                        .collect(),
                ),
                DecodedValue::Optional(value) => {
                    DecodedValue::Optional(value.map(|value| Box::new(guarded(*value, check))))
                }
                value => value,
            }
        }
        Ok(match &self.availability {
            Some(check) => guarded(value, check),
            None => value,
        })
    }

    pub fn release(&self) {
        if let Some(imports) = self.imports.upgrade() {
            release_wrapper(&mut imports.lock().unwrap(), &self.wrapper);
        } else {
            self.wrapper.lock().unwrap().active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn staged_publication_preserves_wrappers_but_closes_captured_property_cycles() {
        let bundle = AdmittedBundle::parse(include_bytes!(
            "../../../protocol/fixtures/database.bundle.json"
        ))
        .unwrap();
        let mut graph: WireGraph = serde_json::from_slice(include_bytes!(
            "../../../protocol/fixtures/session.graph.json"
        ))
        .unwrap();
        for reference in &mut graph.references {
            reference.delivery.view.bundle_sha256 = bundle.sha256().into();
        }
        let scope = graph.references[0].delivery.recipient.clone();
        let imports = Imports::default();
        imports.open_scope(scope.clone(), None).unwrap();
        let proxy = imports
            .receive_graph(
                &bundle,
                &bundle.bundle().interfaces["Agent"].properties["session"],
                graph,
                &GraphScopes::in_scope(scope),
            )
            .unwrap()
            .into_object()
            .unwrap();
        let published = Arc::new(AtomicBool::new(false));
        let check = published.clone();
        let guarded = proxy
            .clone()
            .with_availability(Arc::new(move || check.load(Ordering::SeqCst)));
        assert!(
            proxy.delivery().is_ok(),
            "staging must retain its SDK roots"
        );
        assert_eq!(guarded.delivery().unwrap_err().code, ErrorCode::Unavailable);
        assert_eq!(
            guarded.property("agent").err().unwrap().code,
            ErrorCode::Unavailable
        );
        published.store(true, Ordering::SeqCst);
        let child = guarded.property("agent").unwrap().into_object().unwrap();
        let back = child.property("session").unwrap().into_object().unwrap();
        assert!(guarded.same_wrapper(&proxy));
        assert!(guarded.same_wrapper(&back));
        assert!(child.delivery().is_ok());
        published.store(false, Ordering::SeqCst);
        assert_eq!(child.delivery().unwrap_err().code, ErrorCode::Unavailable);
        assert_eq!(
            back.property("agent").err().unwrap().code,
            ErrorCode::Unavailable
        );
        child.release();
        guarded.release();
        assert_eq!(imports.retained_objects(), 0);
    }
}
