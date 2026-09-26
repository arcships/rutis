//! An owner stages real objects before requesting grants. A draft carries
//! identity and snapshot data, never recipient authority minted by the owner.
use crate::contract::{
    callback_key, validate_wire, AdmittedBundle, Ownership, TypeExpr, WireValue,
};
use crate::error::{ErrorCode, ProtocolError, Result};
use crate::exports::{Exports, PinKey};
use crate::graph::{self, DecodedValue, GraphScopes, WireGraph, WireReference};
use crate::identity::{Delivery, InterfaceView, ObjectIdentity, Scope, Sequence};
use crate::sdk::{Dispatcher, Outbound};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftSource {
    Own {
        object: ObjectIdentity,
        view: InterfaceView,
    },
    Foreign {
        delivery: Delivery,
    },
}
impl DraftSource {
    pub fn object(&self) -> &ObjectIdentity {
        match self {
            Self::Own { object, .. } => object,
            Self::Foreign { delivery } => &delivery.object,
        }
    }
    pub fn view(&self) -> &InterfaceView {
        match self {
            Self::Own { view, .. } => view,
            Self::Foreign { delivery } => &delivery.view,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftReference {
    pub source: DraftSource,
    pub ownership: Ownership,
    pub properties: BTreeMap<String, WireValue>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftGraph {
    pub root: WireValue,
    pub references: Vec<DraftReference>,
}
fn fail(code: ErrorCode, message: &str) -> ProtocolError {
    ProtocolError::new(code, "encode", message)
}
impl DraftGraph {
    /// Placeholder deliveries validate only graph shape and ownership. They
    /// never enter either ledger and are not sent as grants to an author.
    pub(crate) fn validate(
        &self,
        bundle: &AdmittedBundle,
        expr: &TypeExpr,
        scopes: &GraphScopes,
    ) -> Result<WireGraph> {
        let graph = WireGraph {
            root: self.root.clone(),
            references: self
                .references
                .iter()
                .enumerate()
                .map(|(i, reference)| WireReference {
                    delivery: Delivery {
                        id: Sequence(i as u64 + 1),
                        token: "validation-only".into(),
                        object: reference.source.object().clone(),
                        view: reference.source.view().clone(),
                        recipient: match reference.ownership {
                            Ownership::Scope => scopes.scope.clone(),
                            Ownership::Borrow => scopes.borrow.clone(),
                        },
                    },
                    properties: reference.properties.clone(),
                })
                .collect(),
        };
        graph::validate(bundle, expr, &graph, scopes)?;
        Ok(graph)
    }
}
pub struct GraphExporter {
    bundle: Arc<AdmittedBundle>,
    exports: Exports,
    source: String,
    dispatchers: Mutex<BTreeMap<(ObjectIdentity, InterfaceView), Arc<dyn Dispatcher>>>,
}
impl GraphExporter {
    pub fn new(bundle: Arc<AdmittedBundle>, exports: Exports, source: String) -> Self {
        Self {
            bundle,
            exports,
            source,
            dispatchers: Mutex::default(),
        }
    }
    pub fn dispatcher(
        &self,
        object: &ObjectIdentity,
        view: &InterfaceView,
    ) -> Result<Arc<dyn Dispatcher>> {
        self.dispatchers
            .lock()
            .unwrap()
            .get(&(object.clone(), view.clone()))
            .cloned()
            .ok_or_else(|| {
                fail(
                    ErrorCode::InterfaceMismatch,
                    "native dispatch adapter unavailable",
                )
            })
    }
    /// Combine named roots encoded by this same native object table and exact
    /// bundle. This does not add an authorization view or widen any selector;
    /// the broker must independently admit every delivery before execution.
    pub fn merge_registered(&self, other: &Self) -> Result<()> {
        if !self.exports.same_table(&other.exports) || self.bundle.sha256() != other.bundle.sha256()
        {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "cannot merge another native export table or bundle",
            ));
        }
        if std::ptr::eq(self, other) {
            return Ok(());
        }
        let registered = other.dispatchers.lock().unwrap().clone();
        self.dispatchers.lock().unwrap().extend(registered);
        Ok(())
    }
    /// Host-selected route aliases retain the exact native identity and
    /// dispatcher. Ordinary commit still compares every full source view.
    pub(crate) fn stage_route(
        &self,
        expr: &TypeExpr,
        original: &DraftGraph,
        source: &str,
    ) -> Result<StagedGraph> {
        self.exports.require_open()?;
        let mut draft = original.clone();
        let mut aliases = Vec::new();
        for reference in &mut draft.references {
            if let DraftSource::Own { object, view } = &mut reference.source {
                if object.owner != *self.exports.owner()
                    || view.bundle_sha256 != self.bundle.sha256()
                    || view.source != self.source
                {
                    return Err(fail(
                        ErrorCode::CapabilityDenied,
                        "route changed native manifest",
                    ));
                }
                let dispatcher = self.dispatcher(object, view)?;
                view.source = source.into();
                aliases.push(((object.clone(), view.clone()), dispatcher));
            }
        }
        let scope = Scope {
            activation: self.exports.owner().clone(),
            scope: Sequence(1),
        };
        draft.validate(&self.bundle, expr, &GraphScopes::in_scope(scope))?;
        let mut staged = StagedGraph {
            bundle: self.bundle.clone(),
            exports: self.exports.clone(),
            expr: expr.clone(),
            draft,
            pins: Vec::new(),
            closed: false,
            committed: None,
        };
        // Drop rolls back earlier pins if a later native object is unavailable.
        for reference in &staged.draft.references {
            if let DraftSource::Own { object, .. } = &reference.source {
                staged.pins.push(self.exports.stage(object)?);
            }
        }
        self.dispatchers.lock().unwrap().extend(aliases);
        Ok(staged)
    }
    pub fn encode(&self, expr: &TypeExpr, value: Outbound) -> Result<StagedGraph> {
        self.exports.require_open()?;
        let mut encoder = Encoder {
            exporter: self,
            references: Vec::new(),
            indices: BTreeMap::new(),
            pins: Vec::new(),
        };
        let result = (|| {
            let root = encoder.value(expr, value, None)?;
            let graph = DraftGraph {
                root,
                references: std::mem::take(&mut encoder.references),
            };
            let scope = Scope {
                activation: self.exports.owner().clone(),
                scope: Sequence(1),
            };
            let borrow = Scope {
                scope: Sequence(2),
                ..scope.clone()
            };
            graph.validate(&self.bundle, expr, &GraphScopes { scope, borrow })?;
            Ok(StagedGraph {
                bundle: self.bundle.clone(),
                exports: self.exports.clone(),
                expr: expr.clone(),
                draft: graph,
                pins: std::mem::take(&mut encoder.pins),
                closed: false,
                committed: None,
            })
        })();
        result
    }
}
struct Encoder<'a> {
    exporter: &'a GraphExporter,
    references: Vec<DraftReference>,
    indices: BTreeMap<(ObjectIdentity, InterfaceView, bool), usize>,
    pins: Vec<PinKey>,
}
impl Drop for Encoder<'_> {
    fn drop(&mut self) {
        for pin in self.pins.drain(..) {
            self.exporter.exports.release(&pin);
        }
    }
}
impl Encoder<'_> {
    fn value(
        &mut self,
        expr: &TypeExpr,
        value: Outbound,
        inherited: Option<Ownership>,
    ) -> Result<WireValue> {
        Ok(match (expr, value) {
            (TypeExpr::Value { .. }, Outbound::Value(value)) => {
                let value = WireValue::Value { value };
                validate_wire(expr, &value, &[])?;
                value
            }
            (TypeExpr::Record { fields: types }, Outbound::Record(mut fields))
                if types.len() == fields.len() =>
            {
                WireValue::Record {
                    fields: types
                        .iter()
                        .map(|(name, expr)| {
                            Ok((
                                name.clone(),
                                self.value(
                                    expr,
                                    fields.remove(name).ok_or_else(|| {
                                        fail(ErrorCode::InvalidParams, "missing field")
                                    })?,
                                    inherited,
                                )?,
                            ))
                        })
                        .collect::<Result<_>>()?,
                }
            }
            (TypeExpr::List { item }, Outbound::List(items)) => WireValue::List {
                items: items
                    .into_iter()
                    .map(|v| self.value(item, v, inherited))
                    .collect::<Result<_>>()?,
            },
            (TypeExpr::Optional { item }, Outbound::Optional(value)) => WireValue::Optional {
                value: value
                    .map(|v| self.value(item, *v, inherited).map(Box::new))
                    .transpose()?,
            },
            (
                TypeExpr::Object {
                    interface,
                    ownership,
                },
                value @ (Outbound::Own(_) | Outbound::Foreign(_)),
            ) => return self.object(interface, value, inherited.unwrap_or(*ownership)),
            (
                TypeExpr::Callback { ownership, .. },
                value @ (Outbound::Own(_) | Outbound::Foreign(_)),
            ) => return self.object(&callback_key(expr), value, inherited.unwrap_or(*ownership)),
            _ => {
                return Err(fail(
                    ErrorCode::InvalidParams,
                    "outbound value does not match contract",
                ))
            }
        })
    }
    fn object(
        &mut self,
        interface: &str,
        value: Outbound,
        ownership: Ownership,
    ) -> Result<WireValue> {
        let (source, own, foreign) = match value {
            Outbound::Own(native) => {
                let registered = native.register(&self.exporter.exports)?;
                if registered.identity.owner != *self.exporter.exports.owner()
                    || registered.bundle_sha256 != self.exporter.bundle.sha256()
                    || registered.interface != interface
                {
                    return Err(fail(
                        ErrorCode::InterfaceMismatch,
                        "native adapter contract mismatch",
                    ));
                }
                let key = self.exporter.exports.stage(&registered.identity)?;
                self.pins.push(key);
                let view = InterfaceView {
                    interface: interface.into(),
                    bundle_sha256: self.exporter.bundle.sha256().into(),
                    source: self.exporter.source.clone(),
                };
                self.exporter.dispatchers.lock().unwrap().insert(
                    (registered.identity.clone(), view.clone()),
                    registered.dispatcher,
                );
                (
                    DraftSource::Own {
                        object: registered.identity,
                        view,
                    },
                    Some(native),
                    None,
                )
            }
            Outbound::Foreign(proxy) => {
                let delivery = proxy.delivery()?;
                if delivery.recipient.activation != *self.exporter.exports.owner() {
                    return Err(fail(
                        ErrorCode::CapabilityDenied,
                        "foreign reference belongs to another activation",
                    ));
                }
                (DraftSource::Foreign { delivery }, None, Some(proxy))
            }
            _ => unreachable!(),
        };
        if source.view().interface != interface
            || source.view().bundle_sha256 != self.exporter.bundle.sha256()
        {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "reference contract mismatch",
            ));
        }
        let key = (
            source.object().clone(),
            source.view().clone(),
            ownership == Ownership::Borrow,
        );
        if let Some(index) = self.indices.get(&key) {
            return Ok(WireValue::Ref { index: *index });
        }
        let index = self.references.len();
        self.indices.insert(key, index);
        self.references.push(DraftReference {
            source,
            ownership,
            properties: BTreeMap::new(),
        });
        let types = self
            .exporter
            .bundle
            .bundle()
            .interfaces
            .get(interface)
            .map(|i| i.properties.clone())
            .unwrap_or_default();
        let mut fields = if let Some(native) = own {
            let fields = native.snapshot()?;
            match self
                .exporter
                .exports
                .native_context(self.references[index].source.object())
            {
                Some(ctx) => fields
                    .into_iter()
                    .map(|(name, value)| (name, crate::sdk::with_native(&ctx, value)))
                    .collect(),
                None => fields,
            }
        } else {
            types
                .keys()
                .map(|name| {
                    Ok((
                        name.clone(),
                        outbound(foreign.as_ref().unwrap().property(name)?),
                    ))
                })
                .collect::<Result<_>>()?
        };
        if fields.keys().ne(types.keys()) {
            return Err(fail(
                ErrorCode::InvalidParams,
                "snapshot properties mismatch",
            ));
        }
        let properties = types
            .iter()
            .map(|(name, expr)| {
                Ok((
                    name.clone(),
                    self.value(expr, fields.remove(name).unwrap(), Some(ownership))?,
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
pub struct StagedGraph {
    bundle: Arc<AdmittedBundle>,
    exports: Exports,
    expr: TypeExpr,
    pub draft: DraftGraph,
    pins: Vec<PinKey>,
    closed: bool,
    committed: Option<WireGraph>,
}
impl StagedGraph {
    pub fn commit(&mut self, deliveries: Vec<Delivery>, scopes: &GraphScopes) -> Result<WireGraph> {
        if let Some(graph) = &self.committed {
            if deliveries
                .iter()
                .ne(graph.references.iter().map(|r| &r.delivery))
            {
                return Err(fail(
                    ErrorCode::CapabilityDenied,
                    "committed graph deliveries changed",
                ));
            }
            return Ok(graph.clone());
        }
        if self.closed {
            return Err(fail(ErrorCode::ScopeClosed, "staged graph aborted"));
        }
        let mut pins = Vec::new();
        let result = (|| {
            if deliveries.len() != self.draft.references.len() {
                return Err(fail(ErrorCode::InvalidParams, "incomplete graph handoff"));
            }
            let references = self
                .draft
                .references
                .iter()
                .zip(&deliveries)
                .map(|(reference, delivery)| {
                    if reference.source.object() != &delivery.object
                        || reference.source.view() != &delivery.view
                    {
                        return Err(fail(
                            ErrorCode::CapabilityDenied,
                            "broker changed source object or view",
                        ));
                    }
                    Ok(WireReference {
                        delivery: delivery.clone(),
                        properties: reference.properties.clone(),
                    })
                })
                .collect::<Result<_>>()?;
            let graph = WireGraph {
                root: self.draft.root.clone(),
                references,
            };
            graph::validate(&self.bundle, &self.expr, &graph, scopes)?;
            for (reference, delivery) in self.draft.references.iter().zip(deliveries) {
                if matches!(reference.source, DraftSource::Own { .. }) {
                    let pin = PinKey::Delivery {
                        recipient: delivery.recipient.activation,
                        id: delivery.id,
                    };
                    self.exports.pin(&delivery.object, pin.clone())?;
                    pins.push(pin);
                }
            }
            self.committed = Some(graph.clone());
            Ok(graph)
        })();
        if result.is_err() {
            for pin in pins {
                self.exports.release(&pin);
            }
        }
        self.abort();
        result
    }
    pub fn abort(&mut self) {
        if !self.closed {
            self.closed = true;
            for pin in self.pins.drain(..) {
                self.exports.release(&pin);
            }
        }
    }
}
impl Drop for StagedGraph {
    fn drop(&mut self) {
        self.abort();
    }
}
