//! An authenticated reference table carries immutable snapshots without
//! recursively expanding objects. Links contain identities, not owning Arcs.
use crate::contract::{validate_wire, AdmittedBundle, Ownership, TypeExpr, WireValue};
use crate::error::{ErrorCode, ProtocolError, Result};
use crate::identity::{Delivery, InterfaceView, ObjectIdentity, Scope};
use crate::imports::ObjectProxy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireReference {
    pub delivery: Delivery,
    pub properties: BTreeMap<String, WireValue>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireGraph {
    pub root: WireValue,
    pub references: Vec<WireReference>,
}

/// Root scope references may persist; borrow references belong to the actual
/// executing call. Snapshot relationships inherit their containing grant's
/// scope, so navigating a borrowed object cannot extend a child's lifetime.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphScopes {
    pub scope: Scope,
    pub borrow: Scope,
}
impl GraphScopes {
    pub fn in_scope(scope: Scope) -> Self {
        Self {
            borrow: scope.clone(),
            scope,
        }
    }
}

#[derive(Clone)]
pub enum DecodedValue {
    Value(Value),
    Object(ObjectProxy),
    Record(BTreeMap<String, DecodedValue>),
    List(Vec<DecodedValue>),
    Optional(Option<Box<DecodedValue>>),
}
impl DecodedValue {
    pub fn into_object(self) -> Result<ObjectProxy> {
        match self {
            Self::Object(object) => Ok(object),
            _ => Err(invalid("expected a decoded object")),
        }
    }
    pub fn into_json(self) -> Result<Value> {
        match self {
            Self::Value(value) => Ok(value),
            _ => Err(invalid("expected a decoded JSON value")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ObjectLink {
    pub scope: Scope,
    pub object: ObjectIdentity,
    pub view: InterfaceView,
}
impl ObjectLink {
    pub fn from_delivery(delivery: &Delivery) -> Self {
        Self {
            scope: delivery.recipient.clone(),
            object: delivery.object.clone(),
            view: delivery.view.clone(),
        }
    }
    pub fn key(&self) -> (Scope, ObjectIdentity, InterfaceView) {
        (self.scope.clone(), self.object.clone(), self.view.clone())
    }
}

#[derive(Clone)]
pub(crate) enum SnapshotValue {
    Value(Value),
    Link(ObjectLink),
    Record(BTreeMap<String, SnapshotValue>),
    List(Vec<SnapshotValue>),
    Optional(Option<Box<SnapshotValue>>),
}
impl PartialEq for SnapshotValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(a), Self::Value(b)) => {
                crate::json::canonical(a) == crate::json::canonical(b)
            }
            (Self::Link(a), Self::Link(b)) => a == b,
            (Self::Record(a), Self::Record(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            (Self::Optional(a), Self::Optional(b)) => a == b,
            _ => false,
        }
    }
}
pub(crate) struct ValidatedGraph {
    pub root: SnapshotValue,
    pub properties: Vec<BTreeMap<String, SnapshotValue>>,
}

fn invalid(message: &str) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "graph", message)
}
fn denied(message: &str) -> ProtocolError {
    ProtocolError::new(ErrorCode::CapabilityDenied, "graph", message)
}
fn mismatch(message: &str) -> ProtocolError {
    ProtocolError::new(ErrorCode::InterfaceMismatch, "graph", message)
}

/// Complete validation happens before the import ledger attaches any token.
/// Every table item must be reachable and every immutable relationship must
/// have a separately authenticated delivery in the correct scope.
pub(crate) fn validate(
    bundle: &AdmittedBundle,
    expr: &TypeExpr,
    graph: &WireGraph,
    scopes: &GraphScopes,
) -> Result<ValidatedGraph> {
    if scopes.scope.activation != scopes.borrow.activation {
        return Err(denied(
            "persistent and borrow scopes belong to different activations",
        ));
    }
    let interfaces: Vec<_> = graph
        .references
        .iter()
        .map(|r| r.delivery.view.interface.clone())
        .collect();
    for reference in &graph.references {
        if reference.delivery.view.bundle_sha256 != bundle.sha256 {
            return Err(mismatch("snapshot references another interface bundle"));
        }
        if reference.delivery.view.interface.starts_with("$callback:") {
            if !reference.properties.is_empty() {
                return Err(invalid(
                    "callback references cannot have snapshot properties",
                ));
            }
            continue;
        }
        bundle
            .bundle
            .interfaces
            .get(&reference.delivery.view.interface)
            .ok_or_else(|| mismatch("snapshot interface is not declared"))?;
    }
    for reference in &graph.references {
        let Some(iface) = bundle
            .bundle
            .interfaces
            .get(&reference.delivery.view.interface)
        else {
            continue;
        };
        if reference.properties.keys().ne(iface.properties.keys()) {
            return Err(invalid(
                "snapshot must contain exactly the declared properties",
            ));
        }
        for (name, value) in &reference.properties {
            validate_wire(&iface.properties[name], value, &interfaces)?;
        }
    }
    validate_wire(expr, &graph.root, &interfaces)?;
    let mut expected = BTreeMap::new();
    let mut queue = VecDeque::new();
    assign(expr, &graph.root, scopes, None, &mut expected, &mut queue)?;
    let mut reached = BTreeSet::new();
    while let Some(index) = queue.pop_front() {
        if !reached.insert(index) {
            continue;
        }
        let reference = &graph.references[index]; // validate_wire checked all indexes
        let scope = expected[&index].clone();
        if reference.delivery.recipient != scope {
            return Err(denied(
                "reference would escape its declared ownership scope",
            ));
        }
        if let Some(iface) = bundle
            .bundle
            .interfaces
            .get(&reference.delivery.view.interface)
        {
            for (name, value) in &reference.properties {
                assign(
                    &iface.properties[name],
                    value,
                    scopes,
                    Some(&scope),
                    &mut expected,
                    &mut queue,
                )?;
            }
        }
    }
    if reached.len() != graph.references.len() {
        return Err(invalid("reference table contains unconsumed deliveries"));
    }
    let mut snapshots = BTreeMap::new();
    let properties: Vec<BTreeMap<_, _>> = graph
        .references
        .iter()
        .map(|reference| {
            reference
                .properties
                .iter()
                .map(|(name, value)| (name.clone(), normalize(value, &graph.references)))
                .collect()
        })
        .collect();
    for (reference, fields) in graph.references.iter().zip(&properties) {
        if snapshots
            .insert(ObjectLink::from_delivery(&reference.delivery), fields)
            .is_some_and(|old| old != fields)
        {
            return Err(invalid(
                "one object view has conflicting immutable snapshots",
            ));
        }
    }
    Ok(ValidatedGraph {
        root: normalize(&graph.root, &graph.references),
        properties,
    })
}

fn assign(
    expr: &TypeExpr,
    value: &WireValue,
    scopes: &GraphScopes,
    inherited: Option<&Scope>,
    expected: &mut BTreeMap<usize, Scope>,
    queue: &mut VecDeque<usize>,
) -> Result<()> {
    match (expr, value) {
        (
            TypeExpr::Object { ownership, .. } | TypeExpr::Callback { ownership, .. },
            WireValue::Ref { index },
        ) => {
            let scope = inherited.unwrap_or(match ownership {
                Ownership::Scope => &scopes.scope,
                Ownership::Borrow => &scopes.borrow,
            });
            if let Some(old) = expected.get(index) {
                if old != scope {
                    return Err(denied("one reference cannot cross ownership scopes"));
                }
            } else {
                expected.insert(*index, scope.clone());
                queue.push_back(*index);
            }
        }
        (TypeExpr::Record { fields: types }, WireValue::Record { fields }) => {
            for (name, expr) in types {
                assign(expr, &fields[name], scopes, inherited, expected, queue)?;
            }
        }
        (TypeExpr::List { item }, WireValue::List { items }) => {
            for value in items {
                assign(item, value, scopes, inherited, expected, queue)?;
            }
        }
        (TypeExpr::Optional { item }, WireValue::Optional { value: Some(value) }) => {
            assign(item, value, scopes, inherited, expected, queue)?;
        }
        _ => {}
    }
    Ok(())
}

fn normalize(value: &WireValue, references: &[WireReference]) -> SnapshotValue {
    match value {
        WireValue::Value { value } => SnapshotValue::Value(value.clone()),
        WireValue::Ref { index } => {
            SnapshotValue::Link(ObjectLink::from_delivery(&references[*index].delivery))
        }
        WireValue::Record { fields } => SnapshotValue::Record(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v, references)))
                .collect(),
        ),
        WireValue::List { items } => {
            SnapshotValue::List(items.iter().map(|v| normalize(v, references)).collect())
        }
        WireValue::Optional { value } => {
            SnapshotValue::Optional(value.as_ref().map(|v| Box::new(normalize(v, references))))
        }
    }
}
