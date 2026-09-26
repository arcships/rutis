//! Named native service ports and complete root tables. These adapters reuse
//! generated clients/exports and the authoritative broker; they mint no grants.
use crate::{
    broker::{Broker, GraphOffer},
    contract::{identifier, AdmittedBundle, Ownership, TypeExpr, WireValue},
    draft::{DraftGraph, DraftSource, GraphExporter, StagedGraph},
    error::{ErrorCode, ProtocolError, Result},
    exports::{Exports, PinKey},
    graph::{DecodedValue, GraphScopes, WireGraph},
    identity::{Activation, Sequence},
    managed::{ActivationGate, ManagedActivation},
    runner_image::{CatalogService, FactoryCatalog},
    sdk::{bind, Caller, ClientHandle, Outbound},
};
use rutis::{Ctx, Disposer, TypeKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

fn fail(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, "services", message)
}
#[derive(Clone, Default)]
pub struct Bundles(BTreeMap<String, Arc<AdmittedBundle>>);
impl Bundles {
    pub fn admit(bytes: impl IntoIterator<Item = Vec<u8>>) -> Result<Self> {
        let mut bundles = BTreeMap::new();
        let mut identities = BTreeMap::new();
        for bytes in bytes {
            let bundle = Arc::new(AdmittedBundle::parse(&bytes)?);
            let identity = (bundle.bundle().id.clone(), bundle.bundle().version.clone());
            if identities
                .insert(identity, bundle.sha256().to_owned())
                .is_some_and(|old| old != bundle.sha256())
            {
                return Err(fail(
                    ErrorCode::InterfaceMismatch,
                    "same bundle identity has different raw bytes",
                ));
            }
            bundles.insert(bundle.sha256().to_owned(), bundle);
        }
        Ok(Self(bundles))
    }
    pub fn service(&self, contract: &CatalogService) -> Result<Arc<AdmittedBundle>> {
        let bundle = self.0.get(&contract.bundle_sha256).ok_or_else(|| {
            fail(
                ErrorCode::InterfaceMismatch,
                "service bundle was not admitted",
            )
        })?;
        if bundle.bundle().version != contract.version
            || !bundle.bundle().interfaces.contains_key(&contract.interface)
        {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "named service contract differs from exact bundle",
            ));
        }
        Ok(bundle.clone())
    }
    pub fn client<H: ClientHandle>(&self) -> Result<CatalogService> {
        let bundle = self.0.get(H::BUNDLE_SHA256).ok_or_else(|| {
            fail(
                ErrorCode::InterfaceMismatch,
                "generated port bundle was not admitted",
            )
        })?;
        let contract = CatalogService {
            interface: H::INTERFACE.into(),
            version: bundle.bundle().version.clone(),
            bundle_sha256: H::BUNDLE_SHA256.into(),
        };
        self.service(&contract)?;
        Ok(contract)
    }
    pub fn exact(&self, sha256: &str) -> Result<Arc<AdmittedBundle>> {
        self.0.get(sha256).cloned().ok_or_else(|| {
            fail(
                ErrorCode::InterfaceMismatch,
                "object bundle was not admitted",
            )
        })
    }
    /// Resolve the full callback signature, also inside properties and events.
    pub fn method(
        &self,
        view: &crate::identity::InterfaceView,
        name: &str,
    ) -> Result<crate::contract::Method> {
        let bundle = self.exact(&view.bundle_sha256)?;
        if let Some(method) = bundle
            .bundle()
            .interfaces
            .get(&view.interface)
            .and_then(|i| i.methods.get(name))
        {
            return Ok(method.clone());
        }
        fn scan(expr: &TypeExpr, interface: &str) -> Option<crate::contract::Method> {
            match expr {
                TypeExpr::Callback { params, result, .. } => {
                    if crate::contract::callback_key(expr) == interface {
                        Some(crate::contract::Method {
                            params: *params.clone(),
                            result: *result.clone(),
                        })
                    } else {
                        scan(params, interface).or_else(|| scan(result, interface))
                    }
                }
                TypeExpr::Record { fields } => fields.values().find_map(|e| scan(e, interface)),
                TypeExpr::List { item } | TypeExpr::Optional { item } => scan(item, interface),
                _ => None,
            }
        }
        if name == "call" {
            for interface in bundle.bundle().interfaces.values() {
                for method in interface.methods.values() {
                    if let Some(method) = scan(&method.params, &view.interface)
                        .or_else(|| scan(&method.result, &view.interface))
                    {
                        return Ok(method);
                    }
                }
                for expr in interface.properties.values() {
                    if let Some(method) = scan(expr, &view.interface) {
                        return Ok(method);
                    }
                }
            }
            for event in bundle.bundle().events.values() {
                if let Some(method) = scan(&event.params, &view.interface)
                    .or_else(|| scan(&event.result, &view.interface))
                {
                    return Ok(method);
                }
            }
        }
        Err(fail(
            ErrorCode::CapabilityDenied,
            "unknown exact interface selector",
        ))
    }
}

type Read = dyn Fn(&Ctx) -> Result<Outbound> + Send + Sync;
type Install = Box<dyn FnOnce(&Ctx, ActivationGate) -> Result<Disposer> + Send>;
type Import = dyn Fn(DecodedValue, Arc<dyn Caller>, &Activation) -> Result<Install> + Send + Sync;
struct ExportPort {
    key: TypeKey,
    contract: CatalogService,
    read: Arc<Read>,
}
struct ImportPort {
    key: TypeKey,
    contract: CatalogService,
    bind: Arc<Import>,
}
#[derive(Default)]
pub struct NativePorts {
    exports: BTreeMap<String, ExportPort>,
    imports: BTreeMap<String, ImportPort>,
    keys: Vec<TypeKey>,
}
impl NativePorts {
    fn reserve(&mut self, name: &str, key: &TypeKey, exists: bool) -> Result<()> {
        if !identifier(name) || exists || self.keys.contains(key) {
            return Err(fail(
                ErrorCode::InvalidParams,
                "duplicate/invalid native service port or key",
            ));
        }
        self.keys.push(key.clone());
        Ok(())
    }
    /// H is the generated contract witness, T the author's actual native trait
    /// or value. Rust TypeId stays local; only H's exact interface/SHA is wire.
    pub fn provide<T: ?Sized + Send + Sync + 'static, H: ClientHandle>(
        &mut self,
        name: &str,
        key: TypeKey,
        bundles: &Bundles,
        export: impl Fn(Arc<T>) -> Outbound + Send + Sync + 'static,
    ) -> Result<()> {
        let contract = bundles.client::<H>()?;
        self.reserve(name, &key, self.exports.contains_key(name))?;
        let read_key = key.clone();
        self.exports.insert(
            name.into(),
            ExportPort {
                key,
                contract,
                read: Arc::new(move |ctx| {
                    // This is the SDK's private scoped parent, not the author's
                    // business context. It collects exports provided by the
                    // member or a necessary child without fabricating injects.
                    let value = ctx.get_as::<T>(read_key.clone()).ok_or_else(|| {
                        fail(ErrorCode::Unavailable, "native export is unavailable")
                    })?;
                    Ok(export(value))
                }),
            },
        );
        Ok(())
    }
    pub fn require<H: ClientHandle>(
        &mut self,
        name: &str,
        key: TypeKey,
        bundles: &Bundles,
    ) -> Result<()> {
        let contract = bundles.client::<H>()?;
        self.reserve(name, &key, self.imports.contains_key(name))?;
        let install_key = key.clone();
        self.imports.insert(
            name.into(),
            ImportPort {
                key,
                contract,
                bind: Arc::new(move |value, caller, owner| {
                    let handle = bind::<H>(value, caller)?;
                    let proof = handle.client().proxy().delivery()?;
                    if proof.recipient.activation != *owner || proof.recipient.scope != Sequence(1)
                    {
                        return Err(fail(
                            ErrorCode::CapabilityDenied,
                            "native import belongs to another activation",
                        ));
                    }
                    let proxy = handle.client().proxy().clone();
                    let key = install_key.clone();
                    Ok(Box::new(move |ctx, gate| {
                        ctx.provide_as_with_check(key, Arc::new(handle), move || {
                            gate.is_open() && proxy.delivery().is_ok()
                        })
                        .map_err(|e| fail(ErrorCode::Unavailable, e.to_string()))
                    }))
                }),
            },
        );
        Ok(())
    }
    pub fn check(&self, catalog: &FactoryCatalog) -> Result<()> {
        let exports: BTreeMap<_, _> = self
            .exports
            .iter()
            .map(|(n, p)| (n.clone(), p.contract.clone()))
            .collect();
        let imports: BTreeMap<_, _> = self
            .imports
            .iter()
            .map(|(n, p)| (n.clone(), p.contract.clone()))
            .collect();
        if exports != catalog.provides || imports != catalog.requires {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "native ports differ from frozen factory service declarations",
            ));
        }
        Ok(())
    }
    pub fn required_keys(&self) -> Vec<TypeKey> {
        self.imports.values().map(|p| p.key.clone()).collect()
    }
    /// The selected parent contains every port's private native scope. Import
    /// providers belong to this binding until explicitly adopted by native.
    pub fn scope(&self, parent: &Ctx, owner: Activation, gate: ActivationGate) -> NativeBindings {
        let label = crate::json::canonical(&serde_json::json!(["protocol-member", owner]));
        let ctx = self
            .keys
            .iter()
            .fold(parent.clone(), |ctx, key| ctx.isolate(key.clone(), &label));
        NativeBindings {
            ctx,
            owner,
            gate,
            resources: Vec::new(),
            installed: false,
            staged: AtomicBool::new(false),
        }
    }
    pub fn install(
        &self,
        bindings: &mut NativeBindings,
        values: BTreeMap<String, DecodedValue>,
        caller: Arc<dyn Caller>,
    ) -> Result<()> {
        if bindings.installed || !bindings.gate.is_open() {
            return Err(fail(
                ErrorCode::Unavailable,
                "native bindings cannot be installed or reused",
            ));
        }
        if values.keys().ne(self.imports.keys()) {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "native required service table differs",
            ));
        }
        // Type/identity checks for all values precede any provider side effect.
        let installations = values
            .into_iter()
            .map(|(name, value)| {
                self.imports[&name].bind.as_ref()(value, caller.clone(), &bindings.owner)
            })
            .collect::<Result<Vec<_>>>()?;
        bindings.installed = true;
        for install in installations {
            if !bindings.gate.is_open() {
                return Err(fail(
                    ErrorCode::Unavailable,
                    "native bindings closed during installation",
                ));
            }
            bindings
                .resources
                .push(install(&bindings.ctx, bindings.gate.clone())?);
        }
        Ok(())
    }
    /// Run after native Ready. Each named service has a separate authorization
    /// source; the whole returned table keeps staging pins until Host commit.
    pub fn stage(
        &self,
        bindings: &NativeBindings,
        bundles: Bundles,
        exports: Exports,
    ) -> Result<StagedServices> {
        if !bindings.gate.is_open() || exports.owner() != &bindings.owner {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "native services are not admitted",
            ));
        }
        if bindings.staged.swap(true, Ordering::AcqRel) {
            return Err(fail(
                ErrorCode::Unavailable,
                "native service table cannot be restaged",
            ));
        }
        let mut staged = StagedServices {
            table: ServiceTable {
                activation: bindings.owner.clone(),
                services: BTreeMap::new(),
            },
            graphs: BTreeMap::new(),
            exporters: BTreeMap::new(),
            bundles,
            contracts: BTreeMap::new(),
            exports: exports.clone(),
            committed: None,
            aborted: false,
        };
        for (index, (name, port)) in self.exports.iter().enumerate() {
            let bundle = staged.bundles.service(&port.contract)?;
            let exporter = Arc::new(GraphExporter::new(
                bundle,
                exports.clone(),
                service_source(&bindings.owner, name),
            ));
            let graph =
                exporter.encode(&service_type(&port.contract), (port.read)(&bindings.ctx)?)?;
            staged.table.services.insert(
                name.clone(),
                ServiceDraft {
                    stage: Sequence(index as u64 + 1),
                    graph: graph.draft.clone(),
                },
            );
            staged.graphs.insert(name.clone(), graph);
            staged.exporters.insert(name.clone(), exporter);
            staged.contracts.insert(name.clone(), port.contract.clone());
        }
        validate_table(&staged.table, &staged.contracts, &staged.bundles)?;
        Ok(staged)
    }
    /// Necessary native services, including those provided by internal children,
    /// carry dependency guards on the same admission gate.
    pub fn guard_exports(
        &self,
        bindings: &NativeBindings,
        native: &ManagedActivation,
    ) -> Result<Vec<rutis::FiberView>> {
        if !bindings.gate.same_activation(&native.gate()) || !bindings.gate.is_open() {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "native export guards use another activation",
            ));
        }
        let ctx = native.native_context().ok_or_else(|| {
            fail(
                ErrorCode::Unavailable,
                "native context has not entered apply",
            )
        })?;
        // Guards are native children of the business member, so its shutdown
        // owns their cleanup. Mounting guards on the runtime root would leave
        // a Pending fiber behind for every stopped activation.
        Ok(self
            .exports
            .values()
            .map(|p| bindings.gate.track_service(&ctx, p.key.clone()))
            .collect())
    }
}

pub struct NativeBindings {
    ctx: Ctx,
    owner: Activation,
    gate: ActivationGate,
    resources: Vec<Disposer>,
    installed: bool,
    staged: AtomicBool,
}
impl NativeBindings {
    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }
    pub fn gate(&self) -> ActivationGate {
        self.gate.clone()
    }
    pub fn owner(&self) -> &Activation {
        &self.owner
    }
    pub fn adopt(&mut self, native: &ManagedActivation) -> Result<()> {
        if !self.gate.same_activation(&native.gate()) {
            return Err(fail(
                ErrorCode::Unavailable,
                "native mount uses another gate",
            ));
        }
        if let Err(resources) = native.own_disposers(std::mem::take(&mut self.resources)) {
            self.resources = resources;
            return Err(fail(
                ErrorCode::Unavailable,
                "native cleanup already started; rollback must be joined",
            ));
        }
        Ok(())
    }
    /// Mount failures join this rollback before reporting failure. All provider
    /// removals are started before awaits; SDK Drop is only a last-resort guard.
    pub async fn rollback(mut self) -> Result<()> {
        self.gate.close();
        dispose(std::mem::take(&mut self.resources)).await
    }
}
impl Drop for NativeBindings {
    fn drop(&mut self) {
        if self.resources.is_empty() {
            return;
        }
        self.gate.close();
        let resources = std::mem::take(&mut self.resources);
        let sink = self.ctx.error_sink();
        self.ctx.handle().spawn(async move {
            if let Err(error) = dispose(resources).await {
                sink(Arc::new(rutis::CordisError::PluginFailed(Box::new(error))));
            }
        });
    }
}
async fn dispose(resources: Vec<Disposer>) -> Result<()> {
    let tasks: Vec<_> = resources.into_iter().map(Disposer::dispose).collect();
    let mut errors = Vec::new();
    for task in tasks {
        if let Err(error) = task.await {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(fail(ErrorCode::Business, errors.join("; ")))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDraft {
    pub stage: Sequence,
    pub graph: DraftGraph,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTable {
    pub activation: Activation,
    pub services: BTreeMap<String, ServiceDraft>,
}
pub fn service_source(owner: &Activation, name: &str) -> String {
    crate::prepare::digest(
        crate::json::canonical(&serde_json::json!(["protocol-service", owner, name])).as_bytes(),
    )
}
pub fn service_type(contract: &CatalogService) -> TypeExpr {
    TypeExpr::Object {
        interface: contract.interface.clone(),
        ownership: Ownership::Scope,
    }
}
pub fn validate_table(
    table: &ServiceTable,
    contracts: &BTreeMap<String, CatalogService>,
    bundles: &Bundles,
) -> Result<()> {
    if !identifier(&table.activation.runtime)
        || table.activation.epoch.0 == 0
        || table.activation.activation.0 == 0
    {
        return Err(fail(
            ErrorCode::InvalidParams,
            "invalid service table activation",
        ));
    }
    if table.services.keys().ne(contracts.keys()) {
        return Err(fail(
            ErrorCode::InterfaceMismatch,
            "complete named service table differs",
        ));
    }
    let mut stages = BTreeSet::new();
    let scope = crate::identity::Scope {
        activation: table.activation.clone(),
        scope: Sequence(1),
    };
    for (name, service) in &table.services {
        if !identifier(name) || service.stage.0 == 0 || !stages.insert(service.stage) {
            return Err(fail(
                ErrorCode::InvalidParams,
                "invalid/duplicate service staging identity",
            ));
        }
        let bundle = bundles.service(&contracts[name])?;
        service.graph.validate(
            &bundle,
            &service_type(&contracts[name]),
            &GraphScopes::in_scope(scope.clone()),
        )?;
        let WireValue::Ref { index } = service.graph.root else {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "named service must be an object root",
            ));
        };
        if !matches!(&service.graph.references[index].source, DraftSource::Own{object,..} if object.owner==table.activation)
        {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "named root must be native-owned by this activation",
            ));
        }
        for reference in &service.graph.references {
            match &reference.source {
                DraftSource::Own { object, view }
                    if object.owner == table.activation
                        && view.source == service_source(&table.activation, name) => {}
                DraftSource::Foreign { delivery }
                    if delivery.recipient.activation == table.activation => {}
                _ => {
                    return Err(fail(
                        ErrorCode::CapabilityDenied,
                        "named graph changed its owner or service source",
                    ))
                }
            }
        }
    }
    Ok(())
}
/// Pure Host batch admission. A caller commits owner staging pins before
/// exposing any returned grants, and releases the whole batch on failure.
pub fn offer_table(
    broker: &mut Broker,
    sender: &Activation,
    table: &ServiceTable,
    contracts: &BTreeMap<String, CatalogService>,
    bundles: &Bundles,
    scopes: &GraphScopes,
) -> Result<BTreeMap<String, WireGraph>> {
    if table.activation != *sender {
        return Err(fail(
            ErrorCode::CapabilityDenied,
            "service table belongs to another activation",
        ));
    }
    validate_table(table, contracts, bundles)?;
    let prepared: Vec<_> = table
        .services
        .iter()
        .map(|(name, service)| {
            Ok((
                name,
                service,
                bundles.service(&contracts[name])?,
                service_type(&contracts[name]),
                service_source(sender, name),
            ))
        })
        .collect::<Result<_>>()?;
    let offers: Vec<_> = prepared
        .iter()
        .map(|(_, service, bundle, expr, source)| GraphOffer {
            bundle,
            expr,
            draft: &service.graph,
            source,
        })
        .collect();
    let graphs = broker.offer_graph_batch(sender, &offers, scopes)?;
    Ok(prepared
        .into_iter()
        .zip(graphs)
        .map(|((name, ..), graph)| (name.clone(), graph))
        .collect())
}
pub struct StagedServices {
    table: ServiceTable,
    graphs: BTreeMap<String, StagedGraph>,
    exporters: BTreeMap<String, Arc<GraphExporter>>,
    bundles: Bundles,
    contracts: BTreeMap<String, CatalogService>,
    exports: Exports,
    committed: Option<BTreeMap<String, WireGraph>>,
    aborted: bool,
}
impl StagedServices {
    pub fn table(&self) -> &ServiceTable {
        &self.table
    }
    pub(crate) fn exports(&self) -> &Exports {
        &self.exports
    }
    pub fn contracts(&self) -> &BTreeMap<String, CatalogService> {
        &self.contracts
    }
    pub(crate) fn bundles(&self) -> &Bundles {
        &self.bundles
    }
    pub fn merge_dispatchers(&self, target: &GraphExporter) -> Result<()> {
        for exporter in self.exporters.values() {
            target.merge_registered(exporter)?;
        }
        Ok(())
    }
    pub fn exporter(&self, name: &str) -> Result<Arc<GraphExporter>> {
        self.exporters
            .get(name)
            .cloned()
            .ok_or_else(|| fail(ErrorCode::InvalidParams, "unknown named exporter"))
    }
    pub fn commit(
        &mut self,
        graphs: &BTreeMap<String, WireGraph>,
        scopes: &GraphScopes,
    ) -> Result<()> {
        if self.aborted {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "named service handoff aborted",
            ));
        }
        self.exports.require_open()?;
        if let Some(committed) = &self.committed {
            if crate::json::canonical(&serde_json::to_value(committed).unwrap())
                != crate::json::canonical(&serde_json::to_value(graphs).unwrap())
            {
                return Err(fail(
                    ErrorCode::CapabilityDenied,
                    "committed named service table changed",
                ));
            }
            return Ok(());
        }
        if graphs.keys().ne(self.graphs.keys()) {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "service commit table differs",
            ));
        }
        // Validate all proposed handoffs before converting the first staging
        // pin. Per-graph commit independently checks its exact native manifest.
        for (name, graph) in graphs {
            let bundle = self.bundles.service(&self.contracts[name])?;
            crate::graph::validate(&bundle, &service_type(&self.contracts[name]), graph, scopes)?;
            let draft = &self.table.services[name].graph;
            if graph.references.len() != draft.references.len()
                || crate::json::canonical(&serde_json::to_value(&graph.root).unwrap())
                    != crate::json::canonical(&serde_json::to_value(&draft.root).unwrap())
                || graph
                    .references
                    .iter()
                    .zip(&draft.references)
                    .any(|(wire, decl)| {
                        wire.delivery.object != *decl.source.object()
                            || wire.delivery.view != *decl.source.view()
                            || crate::json::canonical(
                                &serde_json::to_value(&wire.properties).unwrap(),
                            ) != crate::json::canonical(
                                &serde_json::to_value(&decl.properties).unwrap(),
                            )
                    })
            {
                return Err(fail(
                    ErrorCode::CapabilityDenied,
                    "service commit changed native object manifest",
                ));
            }
        }
        for (name, graph) in graphs {
            let result = self.graphs.get_mut(name).unwrap().commit(
                graph
                    .references
                    .iter()
                    .map(|r| r.delivery.clone())
                    .collect(),
                scopes,
            );
            if let Err(error) = result {
                self.aborted = true;
                for (service, graph) in graphs {
                    for (wire, draft) in graph
                        .references
                        .iter()
                        .zip(&self.table.services[service].graph.references)
                    {
                        if matches!(draft.source, DraftSource::Own { .. }) {
                            self.exports.release(&PinKey::Delivery {
                                recipient: wire.delivery.recipient.activation.clone(),
                                id: wire.delivery.id,
                            });
                        }
                    }
                }
                for graph in self.graphs.values_mut() {
                    graph.abort();
                }
                return Err(error);
            }
        }
        self.committed = Some(graphs.clone());
        Ok(())
    }
}
