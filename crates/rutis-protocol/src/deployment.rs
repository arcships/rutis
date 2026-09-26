//! Frozen service selections for the native Host graph. Business declarations
//! cannot supply routes or replace an activation's captured owner bindings.
use crate::{
    error::{ErrorCode, ProtocolError, Result},
    identity::Activation,
    lifecycle::member_contracts,
    prepare::{FileKind, PreparedDeployment, Provider},
    runner_image::CatalogService,
    services::{validate_table, Bundles, ServiceTable},
    session::{HostObjects, RootDelivery, RoutedRoot, RuntimeIdentity},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

fn fail(code: ErrorCode, message: &str) -> ProtocolError {
    ProtocolError::new(code, "deployment_objects", message)
}
struct Member {
    activation: Activation,
    table: Option<ServiceTable>,
    issued: bool,
}
struct NativeMember {
    activation: Activation,
    table: ServiceTable,
    mirrored: bool,
}
#[derive(Default)]
struct State {
    members: BTreeMap<String, Member>,
    groups: BTreeMap<String, RuntimeIdentity>,
    native: BTreeMap<String, NativeMember>,
}
/// Retains a single immutable deployment and one initial activation per member.
/// Recovery replacement requires the later supervisor's cleanup/reaping proof;
/// this layer intentionally exposes no unchecked rebind operation.
pub struct DeploymentObjects {
    plan: PreparedDeployment,
    bundles: Bundles,
    host: Arc<HostObjects>,
    state: Mutex<State>,
}
impl DeploymentObjects {
    pub fn new(plan: PreparedDeployment) -> Result<Arc<Self>> {
        let mut bytes = BTreeMap::new();
        for package in plan.packages().values() {
            for file in &package.manifest().files {
                if file.kind == FileKind::Bundle {
                    let frozen = package.file(&file.path)?;
                    bytes
                        .entry(frozen.sha256().to_owned())
                        .or_insert_with(|| frozen.bytes().to_vec());
                }
            }
        }
        let bundles = Bundles::admit(bytes.into_values())?;
        Ok(Arc::new(Self {
            host: HostObjects::new(bundles.clone()),
            bundles,
            plan,
            state: Mutex::default(),
        }))
    }
    pub fn host(&self) -> Arc<HostObjects> {
        self.host.clone()
    }
    pub fn plan(&self) -> &PreparedDeployment {
        &self.plan
    }
    pub(crate) fn bundles(&self) -> Bundles {
        self.bundles.clone()
    }
    pub(crate) fn capture_required(
        &self,
        instance: &str,
        name: &str,
        proof: &crate::identity::Delivery,
    ) -> Result<()> {
        let state = self.state.lock().unwrap();
        let route = &self.plan.instances()[instance].routes()[name];
        let (owner, table, service, source) = match route.provider() {
            Some(Provider::Instance { instance, service }) => {
                let member = state.members.get(instance).ok_or_else(|| {
                    fail(ErrorCode::Unavailable, "captured provider is not reserved")
                })?;
                (
                    &member.activation,
                    member.table.as_ref().ok_or_else(|| {
                        fail(ErrorCode::Unavailable, "captured provider is not staged")
                    })?,
                    service,
                    Some(self.mirror_source(instance, service)),
                )
            }
            Some(Provider::Native { service }) => {
                let member = state.native.get(service).ok_or_else(|| {
                    fail(
                        ErrorCode::Unavailable,
                        "captured native provider is not staged",
                    )
                })?;
                (
                    &member.activation,
                    &member.table,
                    service,
                    member.mirrored.then(|| self.native_mirror_source(service)),
                )
            }
            None => return Err(fail(ErrorCode::Unavailable, "required route is missing")),
        };
        self.host.require_published(owner)?;
        let graph = &table.services[service].graph;
        let crate::contract::WireValue::Ref { index } = graph.root else {
            return Err(fail(
                ErrorCode::InterfaceMismatch,
                "captured provider has no object root",
            ));
        };
        let original = &graph.references[index].source;
        if &proof.object.owner != owner
            || &proof.object != original.object()
            || proof.view.interface != original.view().interface
            || proof.view.bundle_sha256 != original.view().bundle_sha256
            || proof.view.source != source.as_deref().unwrap_or(&original.view().source)
        {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "captured native binding differs from frozen provider",
            ));
        }
        Ok(())
    }
    fn mirror_source(&self, instance: &str, service: &str) -> String {
        format!(
            "route.{}",
            crate::prepare::digest(
                crate::json::canonical(&serde_json::json!([
                    "host-mirror",
                    self.plan.sha256(),
                    instance,
                    service
                ]))
                .as_bytes()
            )
        )
    }
    fn native_mirror_source(&self, service: &str) -> String {
        format!(
            "route.{}",
            crate::prepare::digest(
                crate::json::canonical(&serde_json::json!([
                    "host-native-mirror",
                    self.plan.sha256(),
                    service
                ]))
                .as_bytes()
            )
        )
    }
    pub(crate) async fn mirror_native(
        self: &Arc<Self>,
        owner: &Activation,
        names: &[String],
        recipient: Activation,
    ) -> Result<RootDelivery> {
        let roots = {
            let state = self.state.lock().unwrap();
            names
                .iter()
                .map(|name| {
                    let member = state
                        .native
                        .get(name)
                        .ok_or_else(|| fail(ErrorCode::Unavailable, "native mirror not staged"))?;
                    if &member.activation != owner || !member.mirrored {
                        return Err(fail(
                            ErrorCode::CapabilityDenied,
                            "native mirror owner differs",
                        ));
                    }
                    Ok(RoutedRoot {
                        name: name.clone(),
                        service: name.clone(),
                        owner: owner.clone(),
                        source: self.native_mirror_source(name),
                        contract: self.plan.native_services()[name].clone(),
                        original: member.table.services[name].graph.clone(),
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };
        self.host.mirror_routes(roots, recipient).await
    }
    pub(crate) async fn mirror(
        self: &Arc<Self>,
        instance: &str,
        recipient: Activation,
    ) -> Result<RootDelivery> {
        let roots = {
            let state = self.state.lock().unwrap();
            let prepared = &self.plan.instances()[instance];
            let member = state
                .members
                .get(instance)
                .ok_or_else(|| fail(ErrorCode::Unavailable, "mirror owner not reserved"))?;
            let table = member
                .table
                .as_ref()
                .ok_or_else(|| fail(ErrorCode::Unavailable, "mirror owner not staged"))?;
            let contracts = member_contracts(prepared.package())?.provides;
            prepared
                .exports()
                .iter()
                .map(|name| {
                    let source = self.mirror_source(instance, name);
                    RoutedRoot {
                        name: name.clone(),
                        service: name.clone(),
                        owner: member.activation.clone(),
                        source,
                        contract: contracts[name].clone(),
                        original: table.services[name].graph.clone(),
                    }
                })
                .collect()
        };
        self.host.mirror_routes(roots, recipient).await
    }
    /// Reserve in Host order before independent native starts. A runtime epoch
    /// belongs to exactly one prepared group, and shared members use that epoch.
    pub fn reserve(&self, instance: &str, activation: Activation) -> Result<()> {
        let prepared = self
            .plan
            .instances()
            .get(instance)
            .ok_or_else(|| fail(ErrorCode::InvalidParams, "unknown prepared member"))?;
        let group = prepared.group();
        let identity = RuntimeIdentity {
            runtime: activation.runtime.clone(),
            epoch: activation.epoch,
        };
        let mut state = self.state.lock().unwrap();
        if state.members.contains_key(instance) {
            return Err(fail(
                ErrorCode::ScopeClosed,
                "prepared member cannot rebind before recovery barriers",
            ));
        }
        if state.groups.get(group).is_some_and(|old| *old != identity)
            || state
                .groups
                .iter()
                .any(|(name, old)| name != group && old.runtime == identity.runtime)
        {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "runtime epoch differs from prepared group binding",
            ));
        }
        self.host.reserve(activation.clone())?;
        state.groups.insert(group.into(), identity);
        state.members.insert(
            instance.into(),
            Member {
                activation,
                table: None,
                issued: false,
            },
        );
        Ok(())
    }
    /// Stage the complete declared provides table, including disabled exports.
    /// Route selection below separately checks the frozen publication allowlist.
    pub fn stage(&self, instance: &str, table: ServiceTable) -> Result<()> {
        let prepared = self
            .plan
            .instances()
            .get(instance)
            .ok_or_else(|| fail(ErrorCode::InvalidParams, "unknown prepared member"))?;
        let contracts = member_contracts(prepared.package())?.provides;
        validate_table(&table, &contracts, &self.bundles)?;
        let mut state = self.state.lock().unwrap();
        let member = state
            .members
            .get_mut(instance)
            .ok_or_else(|| fail(ErrorCode::Unavailable, "prepared member not reserved"))?;
        if member.activation != table.activation || member.table.is_some() {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "staged table changed prepared owner",
            ));
        }
        member.table = Some(table);
        Ok(())
    }
    /// Explicit SDK registration of Host native adapters. Only services in the
    /// prepared native catalog are eligible; this is not a third-party proxy.
    pub fn stage_native(&self, table: ServiceTable) -> Result<()> {
        self.stage_native_mode(table, false)
    }
    pub(crate) fn stage_native_mirrored(&self, table: ServiceTable) -> Result<()> {
        self.stage_native_mode(table, true)
    }
    fn stage_native_mode(&self, table: ServiceTable, mirrored: bool) -> Result<()> {
        let mut contracts = BTreeMap::<String, CatalogService>::new();
        for name in table.services.keys() {
            contracts.insert(
                name.clone(),
                self.plan
                    .native_services()
                    .get(name)
                    .cloned()
                    .ok_or_else(|| {
                        fail(ErrorCode::CapabilityDenied, "undeclared native adapter")
                    })?,
            );
        }
        validate_table(&table, &contracts, &self.bundles)?;
        let mut state = self.state.lock().unwrap();
        if table
            .services
            .keys()
            .any(|name| state.native.contains_key(name))
        {
            return Err(fail(
                ErrorCode::CapabilityDenied,
                "native adapter cannot rebind",
            ));
        }
        for name in table.services.keys() {
            state.native.insert(
                name.clone(),
                NativeMember {
                    activation: table.activation.clone(),
                    table: table.clone(),
                    mirrored,
                },
            );
        }
        Ok(())
    }
    /// Provider publication is called only after its native Host proxy is Active.
    /// Runtime publication remains the separate activate ACK on its private peer.
    pub fn publish(&self, instance: &str) -> Result<()> {
        let state = self.state.lock().unwrap();
        let member = state
            .members
            .get(instance)
            .filter(|m| m.table.is_some())
            .ok_or_else(|| {
                fail(
                    ErrorCode::Unavailable,
                    "prepared services have not been staged",
                )
            })?;
        self.host.publish(&member.activation)
    }
    /// Capture the entire required table from this frozen plan. Different
    /// provider names and bundle SHAs may coexist; no root is exposed until all
    /// owner commits and the actual receiver's Accept acknowledgements finish.
    pub async fn offer_required(self: &Arc<Self>, instance: &str) -> Result<RootDelivery> {
        let (recipient, roots) = {
            let mut state = self.state.lock().unwrap();
            let prepared = self
                .plan
                .instances()
                .get(instance)
                .ok_or_else(|| fail(ErrorCode::InvalidParams, "unknown prepared consumer"))?;
            let recipient = state
                .members
                .get(instance)
                .ok_or_else(|| fail(ErrorCode::Unavailable, "prepared consumer not reserved"))?
                .activation
                .clone();
            if state.members[instance].issued {
                return Err(fail(
                    ErrorCode::ScopeClosed,
                    "prepared required roots were already issued",
                ));
            }
            let mut roots = Vec::new();
            for (name, route) in prepared.routes() {
                let (owner, table, service) = match route.provider() {
                    Some(Provider::Instance { instance, service }) => {
                        if !self.plan.instances()[instance].exports().contains(service) {
                            return Err(fail(
                                ErrorCode::CapabilityDenied,
                                "prepared provider export is disabled",
                            ));
                        }
                        let member = state.members.get(instance).ok_or_else(|| {
                            fail(ErrorCode::Unavailable, "prepared provider not reserved")
                        })?;
                        let table = member.table.as_ref().ok_or_else(|| {
                            fail(ErrorCode::Unavailable, "prepared provider not staged")
                        })?;
                        (&member.activation, table, service)
                    }
                    Some(Provider::Native { service }) => {
                        let member = state.native.get(service).ok_or_else(|| {
                            fail(ErrorCode::Unavailable, "native adapter not staged")
                        })?;
                        (&member.activation, &member.table, service)
                    }
                    None => {
                        return Err(fail(
                            ErrorCode::Unavailable,
                            "prepared route is missing; native consumer remains Pending",
                        ))
                    }
                };
                self.host.require_published(owner)?;
                let contract = route.contract();
                roots.push(RoutedRoot {
                    name: name.clone(),
                    service: service.clone(),
                    owner: owner.clone(),
                    source: route.source().into(),
                    contract: CatalogService {
                        interface: contract.interface.clone(),
                        version: contract.version.clone(),
                        bundle_sha256: contract.bundle_sha256.clone(),
                    },
                    original: table.services[service].graph.clone(),
                });
            }
            state.members.get_mut(instance).unwrap().issued = true;
            (recipient, roots)
        };
        self.host.offer_routes(roots, recipient).await
    }
}
