//! Prepare reads immutable version packages and freezes the entire service
//! graph. It never imports a Node module or constructs/executes a Rust plugin.
use crate::{
    contract::{identifier, AdmittedBundle, EventMode, FAMILY, VERSION},
    error::{ErrorCode, ProtocolError, Result},
    json,
    runner_image::{CatalogService, FactoryCatalog, RunnerCatalog},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MANIFEST: &str = "protocol-plugin.json";
pub(crate) fn sha256_field(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && (p.len() == 1 || !p.starts_with('0'))
                && p.bytes().all(|b| b.is_ascii_digit())
        })
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "prepare", message)
}
fn mismatch(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InterfaceMismatch, "prepare", message)
}
fn unavailable(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Unavailable, "prepare", message)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_value(json::decode(bytes)?).map_err(|e| invalid(e.to_string()))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    RustRutis,
    NodeCordis,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Code,
    Dependency,
    Bundle,
    ConfigSchema,
    Executable,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    pub path: String,
    pub sha256: String,
    pub kind: FileKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSpec {
    pub kind: RuntimeKind,
    pub framework_version: String,
    pub executable: String,
    /// Node's protocol runner module, absent for a native Rust image.
    pub runner: Option<String>,
    /// Complete dependency/environment inventory, not just a lockfile claim.
    pub environment: Vec<String>,
    pub capabilities: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginEntry {
    Rust { factory: String },
    Node { entry: String },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub interface: String,
    pub version: String,
    pub bundle: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSpec {
    pub bundle: String,
    pub event: String,
    pub publish: Vec<EventMode>,
    pub subscribe: Vec<EventMode>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub id: String,
    pub version: String,
    pub protocol_family: String,
    pub protocol_version: String,
    pub runtime: RuntimeSpec,
    pub plugin: PluginEntry,
    pub files: Vec<FileSpec>,
    pub config_schema: String,
    pub provides: BTreeMap<String, ServiceSpec>,
    pub requires: BTreeMap<String, ServiceSpec>,
    pub events: BTreeMap<String, EventSpec>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provider {
    Instance { instance: String, service: String },
    Native { service: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSpec {
    pub kind: RuntimeKind,
    pub trust: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventAccess {
    /// Host-owned named event scope; no author-supplied context reflection.
    pub scope: String,
    pub publish: Vec<EventMode>,
    pub subscribe: Vec<EventMode>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSpec {
    pub package: String,
    pub group: String,
    pub config: Value,
    pub routes: BTreeMap<String, Provider>,
    pub exports: Vec<String>,
    pub events: BTreeMap<String, EventAccess>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub id: String,
    /// Paths are relative to the deployment directory, including version dir.
    pub packages: BTreeMap<String, String>,
    pub groups: BTreeMap<String, GroupSpec>,
    /// Native host adapters must register these exact contracts before mount.
    pub native_services: BTreeMap<String, CatalogService>,
    pub instances: BTreeMap<String, InstanceSpec>,
}

#[derive(Clone)]
pub struct PreparedFile {
    path: PathBuf,
    canonical_name: String,
    executable: bool,
    sha256: String,
    bytes: Arc<[u8]>,
}
impl PreparedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn canonical_name(&self) -> &str {
        &self.canonical_name
    }
    pub fn is_executable(&self) -> bool {
        self.executable
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedService {
    pub interface: String,
    pub version: String,
    pub bundle_sha256: String,
}
impl PreparedService {
    fn catalog(&self) -> CatalogService {
        CatalogService {
            interface: self.interface.clone(),
            version: self.version.clone(),
            bundle_sha256: self.bundle_sha256.clone(),
        }
    }
}
#[derive(Clone)]
pub struct PreparedPackage {
    root: PathBuf,
    manifest: PackageManifest,
    manifest_bytes: Arc<[u8]>,
    files: BTreeMap<String, PreparedFile>,
    bundles: BTreeMap<String, Arc<AdmittedBundle>>,
    config_schema: Value,
    provides: BTreeMap<String, PreparedService>,
    requires: BTreeMap<String, PreparedService>,
    environment_sha256: String,
    snapshot_sha256: String,
    catalog: Option<RunnerCatalog>,
}
impl PreparedPackage {
    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn file(&self, name: &str) -> Result<&PreparedFile> {
        self.files
            .get(name)
            .ok_or_else(|| invalid("file is not in the frozen package"))
    }
    pub fn bundle(&self, name: &str) -> Result<&Arc<AdmittedBundle>> {
        self.bundles
            .get(name)
            .ok_or_else(|| invalid("bundle is not in the frozen package"))
    }
    pub fn provides(&self) -> &BTreeMap<String, PreparedService> {
        &self.provides
    }
    pub fn requires(&self) -> &BTreeMap<String, PreparedService> {
        &self.requires
    }
    pub fn environment_sha256(&self) -> &str {
        &self.environment_sha256
    }
    pub fn snapshot_sha256(&self) -> &str {
        &self.snapshot_sha256
    }
    pub fn catalog(&self) -> Option<&RunnerCatalog> {
        self.catalog.as_ref()
    }
    /// Call before launch. Frozen input stays unchanged; a changed disk package
    /// is rejected. The supervisor must pin/copy verified artifacts for launch
    /// to close the check-to-exec interval; this is not an OS sandbox.
    pub fn verify_unchanged(&self) -> Result<()> {
        let current = read_file(&self.root, MANIFEST)?;
        if current.bytes.as_ref() != self.manifest_bytes.as_ref() {
            return Err(mismatch("immutable package manifest changed"));
        }
        let entries = inventory(&self.root)?;
        if entries.keys().ne(self.files.keys()) {
            return Err(mismatch("immutable package inventory changed"));
        }
        for (name, expected) in &self.files {
            let actual = read_file(&self.root, name)?;
            if actual.path != expected.path
                || actual.sha256 != expected.sha256
                || actual.executable != expected.executable
            {
                return Err(mismatch(format!("immutable package file changed: {name}")));
            }
        }
        Ok(())
    }
    pub fn read(path: &Path) -> Result<Arc<Self>> {
        let root = fs::canonicalize(path).map_err(|e| unavailable(e.to_string()))?;
        if !root.is_dir() {
            return Err(invalid("package must be a directory"));
        }
        let manifest_file = confined(&root, MANIFEST)?;
        let metadata = fs::metadata(&manifest_file).map_err(|e| unavailable(e.to_string()))?;
        if !metadata.is_file() || metadata.len() > json::MAX_JSON_BYTES as u64 {
            return Err(invalid("manifest must be a bounded regular JSON file"));
        }
        let bytes = fs::read(manifest_file).map_err(|e| unavailable(e.to_string()))?;
        let manifest: PackageManifest = decode(&bytes)?;
        if !identifier(&manifest.id)
            || !version(&manifest.version)
            || root.file_name().and_then(|s| s.to_str()) != Some(&manifest.version)
        {
            return Err(invalid(
                "package id/version or immutable version directory invalid",
            ));
        }
        if manifest.protocol_family != FAMILY || manifest.protocol_version != VERSION {
            return Err(mismatch("package protocol family/version differs"));
        }
        if !version(&manifest.runtime.framework_version) {
            return Err(invalid("framework version must be exact"));
        }
        unique_strings(
            &manifest.runtime.capabilities,
            "runtime capabilities",
            identifier,
        )?;
        for c in &manifest.runtime.capabilities {
            if !matches!(
                c.as_str(),
                "object.scope" | "callback.borrow" | "event.parallel" | "event.serial"
            ) {
                return Err(ProtocolError::new(
                    ErrorCode::UnsupportedCapability,
                    "prepare",
                    format!("unsupported package capability {c}"),
                ));
            }
        }
        let mut files = BTreeMap::new();
        for file in &manifest.files {
            relative(&file.path)?;
            if file.path == MANIFEST
                || !sha256_field(&file.sha256)
                || files.contains_key(&file.path)
            {
                return Err(invalid("invalid or duplicate package file"));
            }
            if matches!(file.kind, FileKind::Bundle | FileKind::ConfigSchema) {
                let path = confined(&root, &file.path)?;
                if fs::metadata(path)
                    .map_err(|e| unavailable(e.to_string()))?
                    .len()
                    > json::MAX_JSON_BYTES as u64
                {
                    return Err(invalid("JSON artifact exceeds decoder byte bound"));
                }
            }
            let actual = read_file(&root, &file.path)?;
            if actual.sha256 != file.sha256 {
                return Err(mismatch(format!("file digest differs: {}", file.path)));
            }
            files.insert(file.path.clone(), actual);
        }
        if inventory(&root)?.keys().ne(files.keys()) {
            return Err(invalid("package has missing or undeclared files"));
        }
        let required = |name: &str| -> Result<&PreparedFile> {
            files
                .get(name)
                .ok_or_else(|| invalid(format!("undeclared artifact: {name}")))
        };
        let executable = required(&manifest.runtime.executable)?;
        let role = |name: &str, kind: FileKind| -> Result<()> {
            if !manifest
                .files
                .iter()
                .any(|file| file.path == name && file.kind == kind)
            {
                return Err(invalid(format!("incorrect artifact role: {name}")));
            }
            Ok(())
        };
        role(&manifest.runtime.executable, FileKind::Executable)?;
        if executable.bytes.get(..4) != Some(b"\x7fELF") {
            return Err(invalid("runtime executable must be a Linux ELF image"));
        }
        if !executable.executable {
            return Err(invalid("runtime image is not executable"));
        }
        unique_strings(&manifest.runtime.environment, "runtime environment", |s| {
            relative(s).is_ok()
        })?;
        if manifest.runtime.environment.is_empty() {
            return Err(invalid("runtime dependency inventory missing"));
        }
        let dependencies: BTreeSet<_> = manifest
            .files
            .iter()
            .filter(|file| file.kind == FileKind::Dependency)
            .map(|file| file.path.as_str())
            .collect();
        if manifest
            .runtime
            .environment
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != dependencies
        {
            return Err(invalid(
                "runtime environment must cover every dependency artifact",
            ));
        }
        let environment: BTreeMap<_, _> = manifest
            .runtime
            .environment
            .iter()
            .map(|name| {
                let file = required(name)?;
                Ok((
                    name.clone(),
                    serde_json::json!({"sha256":file.sha256,"executable":file.executable}),
                ))
            })
            .collect::<Result<_>>()?;
        let mut aliases = BTreeMap::new();
        for name in &manifest.runtime.environment {
            let canonical = &required(name)?.canonical_name;
            if canonical != name {
                role(canonical, FileKind::Dependency)?;
                aliases.insert(name, canonical);
            }
        }
        let environment_sha256 = digest(
            json::canonical(&serde_json::json!({"files":environment,"aliases":aliases})).as_bytes(),
        );
        let config_file = required(&manifest.config_schema)?;
        role(&manifest.config_schema, FileKind::ConfigSchema)?;
        let config_schema = json::decode(&config_file.bytes)?;
        crate::contract::check_schema_for_prepare(&config_schema)?;
        for file in manifest
            .files
            .iter()
            .filter(|file| file.kind == FileKind::ConfigSchema)
        {
            crate::contract::check_schema_for_prepare(&json::decode(
                &required(&file.path)?.bytes,
            )?)?;
        }
        let mut bundles = BTreeMap::new();
        for name in manifest
            .provides
            .values()
            .chain(manifest.requires.values())
            .map(|s| &s.bundle)
            .chain(manifest.events.values().map(|e| &e.bundle))
            .chain(
                manifest
                    .files
                    .iter()
                    .filter(|file| file.kind == FileKind::Bundle)
                    .map(|file| &file.path),
            )
        {
            if !bundles.contains_key(name) {
                role(name, FileKind::Bundle)?;
                let bundle = Arc::new(AdmittedBundle::parse(&required(name)?.bytes)?);
                for capability in &bundle.bundle().required_capabilities {
                    if !manifest.runtime.capabilities.contains(capability) {
                        return Err(ProtocolError::new(
                            ErrorCode::UnsupportedCapability,
                            "prepare",
                            format!("runtime lacks required {capability}"),
                        ));
                    }
                }
                for interface in bundle.bundle().interfaces.values() {
                    for method in interface.methods.values() {
                        require_type_capabilities(&manifest.runtime.capabilities, &method.params)?;
                        require_type_capabilities(&manifest.runtime.capabilities, &method.result)?;
                    }
                    for ty in interface.properties.values() {
                        require_type_capabilities(&manifest.runtime.capabilities, ty)?;
                    }
                }
                for event in bundle.bundle().events.values() {
                    require_type_capabilities(&manifest.runtime.capabilities, &event.params)?;
                    require_type_capabilities(&manifest.runtime.capabilities, &event.result)?;
                    for mode in &event.modes {
                        require_event_capability(&manifest.runtime.capabilities, *mode)?;
                    }
                }
                bundles.insert(name.clone(), bundle);
            }
        }
        let services =
            |specs: &BTreeMap<String, ServiceSpec>| -> Result<BTreeMap<String, PreparedService>> {
                specs
                    .iter()
                    .map(|(name, service)| {
                        if !identifier(name) || !identifier(&service.interface) {
                            return Err(invalid("invalid service name"));
                        }
                        let bundle = bundles
                            .get(&service.bundle)
                            .ok_or_else(|| invalid("service bundle missing"))?;
                        if bundle.bundle().version != service.version
                            || !bundle.bundle().interfaces.contains_key(&service.interface)
                        {
                            return Err(mismatch(format!("service contract differs: {name}")));
                        }
                        Ok((
                            name.clone(),
                            PreparedService {
                                interface: service.interface.clone(),
                                version: service.version.clone(),
                                bundle_sha256: bundle.sha256().into(),
                            },
                        ))
                    })
                    .collect()
            };
        let provides = services(&manifest.provides)?;
        let requires = services(&manifest.requires)?;
        if !provides.is_empty() || !requires.is_empty() {
            require_capability(&manifest.runtime.capabilities, "object.scope")?;
        }
        for (name, event) in &manifest.events {
            if !identifier(name) || !identifier(&event.event) {
                return Err(invalid("invalid event identity"));
            }
            let declared = bundles[&event.bundle]
                .bundle()
                .events
                .get(&event.event)
                .ok_or_else(|| mismatch("event absent from bundle"))?;
            modes(&event.publish)?;
            modes(&event.subscribe)?;
            for mode in event.publish.iter().chain(&event.subscribe) {
                if !declared.modes.contains(mode) {
                    return Err(mismatch("event mode absent from bundle"));
                }
                require_event_capability(&manifest.runtime.capabilities, *mode)?;
            }
        }
        let catalog = match (&manifest.runtime.kind, &manifest.plugin) {
            (RuntimeKind::RustRutis, PluginEntry::Rust { factory })
                if manifest.runtime.runner.is_none() =>
            {
                let catalog = RunnerCatalog::from_elf(&executable.bytes)?;
                let expected = FactoryCatalog {
                    config_sha256: config_file.sha256.clone(),
                    provides: provides
                        .iter()
                        .map(|(k, v)| (k.clone(), v.catalog()))
                        .collect(),
                    requires: requires
                        .iter()
                        .map(|(k, v)| (k.clone(), v.catalog()))
                        .collect(),
                };
                if catalog.framework_version != manifest.runtime.framework_version
                    || catalog.environment_sha256 != environment_sha256
                    || catalog.capabilities
                        != manifest.runtime.capabilities.iter().cloned().collect()
                {
                    return Err(mismatch("runner metadata differs from package"));
                }
                if catalog.factories.get(factory) != Some(&expected) {
                    return Err(mismatch(
                        "static factory is missing or its contracts differ",
                    ));
                }
                Some(catalog)
            }
            (RuntimeKind::NodeCordis, PluginEntry::Node { entry }) => {
                module_path(entry)?;
                required(entry)?;
                role(entry, FileKind::Code)?;
                let runner = manifest
                    .runtime
                    .runner
                    .as_deref()
                    .ok_or_else(|| invalid("Node runner module missing"))?;
                module_path(runner)?;
                required(runner)?;
                role(runner, FileKind::Code)?;
                if manifest.runtime.environment.is_empty() {
                    return Err(invalid("Node dependency inventory missing"));
                }
                None
            }
            _ => return Err(invalid("plugin entry does not match runtime")),
        };
        let metadata: BTreeMap<_, _> = files.iter().map(|(name, file)| (name, serde_json::json!({
            "sha256":file.sha256,"canonical_name":file.canonical_name,"executable":file.executable
        }))).collect();
        let snapshot_sha256 = digest(
            json::canonical(&serde_json::json!({
                "manifest_sha256":digest(&bytes), "files":metadata
            }))
            .as_bytes(),
        );
        Ok(Arc::new(Self {
            root,
            manifest,
            manifest_bytes: bytes.into(),
            files,
            bundles,
            config_schema,
            provides,
            requires,
            environment_sha256,
            snapshot_sha256,
            catalog,
        }))
    }
}

#[derive(Clone)]
pub struct PreparedRoute {
    provider: Option<Provider>,
    contract: PreparedService,
    source: String,
    key: rutis::TypeKey,
}
impl PreparedRoute {
    pub fn provider(&self) -> Option<&Provider> {
        self.provider.as_ref()
    }
    pub fn contract(&self) -> &PreparedService {
        &self.contract
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn key(&self) -> &rutis::TypeKey {
        &self.key
    }
}
#[derive(Clone)]
pub struct PreparedInstance {
    package: Arc<PreparedPackage>,
    group: String,
    config: Value,
    routes: BTreeMap<String, PreparedRoute>,
    exports: BTreeSet<String>,
    events: BTreeMap<String, EventAccess>,
}
impl PreparedInstance {
    pub fn package(&self) -> &Arc<PreparedPackage> {
        &self.package
    }
    pub fn group(&self) -> &str {
        &self.group
    }
    pub fn config(&self) -> &Value {
        &self.config
    }
    pub fn routes(&self) -> &BTreeMap<String, PreparedRoute> {
        &self.routes
    }
    pub fn exports(&self) -> &BTreeSet<String> {
        &self.exports
    }
    pub fn events(&self) -> &BTreeMap<String, EventAccess> {
        &self.events
    }
    pub fn missing_routes(&self) -> impl Iterator<Item = &str> {
        self.routes
            .iter()
            .filter(|(_, r)| r.provider.is_none())
            .map(|(name, _)| name.as_str())
    }
}
#[derive(Clone)]
pub struct PreparedGroup {
    kind: RuntimeKind,
    trust: String,
    executable: PreparedFile,
    runner: Option<PreparedFile>,
    framework_version: String,
    environment_sha256: String,
    capabilities: BTreeSet<String>,
    members: BTreeSet<String>,
    code_sha256: String,
}
impl PreparedGroup {
    pub fn kind(&self) -> &RuntimeKind {
        &self.kind
    }
    pub fn trust(&self) -> &str {
        &self.trust
    }
    pub fn members(&self) -> &BTreeSet<String> {
        &self.members
    }
    pub fn executable(&self) -> &PreparedFile {
        &self.executable
    }
    pub fn runner(&self) -> Option<&PreparedFile> {
        self.runner.as_ref()
    }
    pub fn framework_version(&self) -> &str {
        &self.framework_version
    }
    pub fn environment_sha256(&self) -> &str {
        &self.environment_sha256
    }
    pub fn code_sha256(&self) -> &str {
        &self.code_sha256
    }
    pub fn capabilities(&self) -> &BTreeSet<String> {
        &self.capabilities
    }
    pub fn argv(&self) -> Vec<PathBuf> {
        let mut argv = vec![self.executable.path.clone()];
        if let Some(runner) = &self.runner {
            argv.push(runner.path.clone());
        }
        argv
    }
}
#[derive(Clone)]
pub struct PreparedDeployment {
    id: String,
    sha256: String,
    packages: BTreeMap<String, Arc<PreparedPackage>>,
    instances: BTreeMap<String, PreparedInstance>,
    groups: BTreeMap<String, PreparedGroup>,
    order: Vec<String>,
    native_services: BTreeMap<String, CatalogService>,
}
impl PreparedDeployment {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn packages(&self) -> &BTreeMap<String, Arc<PreparedPackage>> {
        &self.packages
    }
    pub fn instances(&self) -> &BTreeMap<String, PreparedInstance> {
        &self.instances
    }
    pub fn groups(&self) -> &BTreeMap<String, PreparedGroup> {
        &self.groups
    }
    /// Topological information only: never a requirement to wait for all
    /// members. RuntimeReady starts independently of these business edges.
    pub fn dependency_order(&self) -> &[String] {
        &self.order
    }
    pub fn native_services(&self) -> &BTreeMap<String, CatalogService> {
        &self.native_services
    }
    pub fn verify_unchanged(&self) -> Result<()> {
        for package in self.packages.values() {
            package.verify_unchanged()?;
        }
        Ok(())
    }
    pub fn prepare(directory: &Path, bytes: &[u8]) -> Result<Self> {
        let deployment: Deployment = decode(bytes)?;
        if !identifier(&deployment.id) || deployment.instances.is_empty() {
            return Err(invalid("deployment identity or instances invalid"));
        }
        let directory = fs::canonicalize(directory).map_err(|e| unavailable(e.to_string()))?;
        let mut packages = BTreeMap::new();
        let mut immutable_files: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
        for (name, location) in &deployment.packages {
            if !identifier(name) {
                return Err(invalid("invalid package alias"));
            }
            let location = confined(&directory, location)?;
            let mut package = PreparedPackage::read(&location)?;
            // Shared groups need one immutable image/dependency snapshot,
            // rather than a second retained binary for every package root.
            let prepared = Arc::get_mut(&mut package).expect("new package is privately owned");
            for file in prepared.files.values_mut() {
                if let Some(bytes) = immutable_files.get(&file.sha256) {
                    if bytes.as_ref() != file.bytes.as_ref() {
                        return Err(mismatch("artifact digest collision"));
                    }
                    file.bytes = bytes.clone();
                } else {
                    immutable_files.insert(file.sha256.clone(), file.bytes.clone());
                }
            }
            packages.insert(name.clone(), package);
        }
        for (name, service) in &deployment.native_services {
            if !identifier(name)
                || !identifier(&service.interface)
                || !version(&service.version)
                || !sha256_field(&service.bundle_sha256)
            {
                return Err(invalid("invalid native adapter contract"));
            }
        }
        for (name, group) in &deployment.groups {
            if !identifier(name) || !identifier(&group.trust) {
                return Err(invalid("invalid runtime group or trust boundary"));
            }
        }
        let mut instances = BTreeMap::new();
        let mut groups: BTreeMap<String, PreparedGroup> = BTreeMap::new();
        for (name, spec) in &deployment.instances {
            if !identifier(name) {
                return Err(invalid("invalid instance identity"));
            }
            let package = packages
                .get(&spec.package)
                .ok_or_else(|| invalid("unknown instance package"))?
                .clone();
            let group = deployment
                .groups
                .get(&spec.group)
                .ok_or_else(|| invalid("unknown runtime group"))?;
            if group.kind != package.manifest.runtime.kind {
                return Err(mismatch("group runtime kind differs"));
            }
            crate::contract::validate_json(&package.config_schema, &spec.config)?;
            unique_strings(&spec.exports, "exports", identifier)?;
            let exports: BTreeSet<_> = spec.exports.iter().cloned().collect();
            if exports.iter().any(|n| !package.provides.contains_key(n))
                || spec
                    .routes
                    .keys()
                    .any(|n| !package.requires.contains_key(n))
            {
                return Err(invalid("undeclared export or required route"));
            }
            let mut routes = BTreeMap::new();
            for (required, contract) in &package.requires {
                let provider = spec.routes.get(required).cloned();
                let source = format!(
                    "route.{}",
                    digest(
                        json::canonical(&serde_json::json!([deployment.id, name, required]))
                            .as_bytes()
                    )
                );
                let key = service_key(&deployment.id, provider.as_ref(), name, required);
                routes.insert(
                    required.clone(),
                    PreparedRoute {
                        provider,
                        contract: contract.clone(),
                        source,
                        key,
                    },
                );
            }
            for (event, access) in &spec.events {
                if !identifier(&access.scope) {
                    return Err(invalid("invalid event scope"));
                }
                let permission = package
                    .manifest
                    .events
                    .get(event)
                    .ok_or_else(|| invalid("undeclared event permission"))?;
                modes(&access.publish)?;
                modes(&access.subscribe)?;
                if access
                    .publish
                    .iter()
                    .any(|m| !permission.publish.contains(m))
                    || access
                        .subscribe
                        .iter()
                        .any(|m| !permission.subscribe.contains(m))
                {
                    return Err(ProtocolError::new(
                        ErrorCode::CapabilityDenied,
                        "prepare",
                        "deployment widened event permissions",
                    ));
                }
            }
            let executable = package.file(&package.manifest.runtime.executable)?.clone();
            let runner = package
                .manifest
                .runtime
                .runner
                .as_deref()
                .map(|r| package.file(r).cloned())
                .transpose()?;
            let prepared = groups
                .entry(spec.group.clone())
                .or_insert_with(|| PreparedGroup {
                    kind: group.kind.clone(),
                    trust: group.trust.clone(),
                    executable: executable.clone(),
                    runner: runner.clone(),
                    framework_version: package.manifest.runtime.framework_version.clone(),
                    environment_sha256: package.environment_sha256.clone(),
                    capabilities: package
                        .manifest
                        .runtime
                        .capabilities
                        .iter()
                        .cloned()
                        .collect(),
                    members: BTreeSet::new(),
                    code_sha256: String::new(),
                });
            if prepared.executable.sha256 != executable.sha256
                || prepared.runner.as_ref().map(|r| &r.sha256) != runner.as_ref().map(|r| &r.sha256)
                || prepared.framework_version != package.manifest.runtime.framework_version
                || prepared.environment_sha256 != package.environment_sha256
                || prepared.capabilities
                    != package
                        .manifest
                        .runtime
                        .capabilities
                        .iter()
                        .cloned()
                        .collect()
            {
                return Err(mismatch(
                    "shared runtime members have incompatible image/framework/environment",
                ));
            }
            prepared.members.insert(name.clone());
            instances.insert(
                name.clone(),
                PreparedInstance {
                    package,
                    group: spec.group.clone(),
                    config: spec.config.clone(),
                    routes,
                    exports,
                    events: spec.events.clone(),
                },
            );
        }
        if groups.len() != deployment.groups.len() {
            return Err(invalid("empty runtime group"));
        }
        for instance in instances.values() {
            for route in instance.routes.values() {
                let actual = match &route.provider {
                    None => continue,
                    Some(Provider::Native { service }) => deployment
                        .native_services
                        .get(service)
                        .cloned()
                        .ok_or_else(|| invalid("unknown native service"))?,
                    Some(Provider::Instance { instance, service }) => {
                        let provider = instances
                            .get(instance)
                            .ok_or_else(|| invalid("unknown provider instance"))?;
                        if !provider.exports.contains(service) {
                            return Err(ProtocolError::new(
                                ErrorCode::CapabilityDenied,
                                "prepare",
                                "provider service is not exported",
                            ));
                        }
                        provider
                            .package
                            .provides
                            .get(service)
                            .ok_or_else(|| invalid("unknown provider service"))?
                            .catalog()
                    }
                };
                if actual != route.contract.catalog() {
                    return Err(mismatch(
                        "route interface/version/raw bundle digest differs",
                    ));
                }
            }
        }
        let order = dependency_order(&instances)?;
        for group in groups.values_mut() {
            let images: BTreeMap<_, _> = group
                .members
                .iter()
                .map(|member| (member, instances[member].package.snapshot_sha256()))
                .collect();
            group.code_sha256 = digest(
                json::canonical(&serde_json::to_value(images).map_err(|e| invalid(e.to_string()))?)
                    .as_bytes(),
            );
        }
        Ok(Self {
            id: deployment.id,
            sha256: digest(bytes),
            packages,
            instances,
            groups,
            order,
            native_services: deployment.native_services,
        })
    }
    pub fn export_key(&self, instance: &str, service: &str) -> Result<rutis::TypeKey> {
        let owner = self
            .instances
            .get(instance)
            .ok_or_else(|| invalid("unknown instance"))?;
        if !owner.exports.contains(service) {
            return Err(invalid("service is not exported"));
        }
        Ok(service_key(
            &self.id,
            Some(&Provider::Instance {
                instance: instance.into(),
                service: service.into(),
            }),
            instance,
            service,
        ))
    }
}

fn service_key(
    deployment: &str,
    provider: Option<&Provider>,
    consumer: &str,
    required: &str,
) -> rutis::TypeKey {
    let parts = match provider {
        Some(Provider::Instance { instance, service }) => {
            serde_json::json!([deployment, "instance", instance, service])
        }
        Some(Provider::Native { service }) => serde_json::json!([deployment, "native", service]),
        None => serde_json::json!([deployment, "missing", consumer, required]),
    };
    rutis::TypeKey::keyed_dynamic::<crate::imports::ObjectProxy>(json::canonical(&parts))
}
fn dependency_order(instances: &BTreeMap<String, PreparedInstance>) -> Result<Vec<String>> {
    let mut pending: BTreeMap<String, BTreeSet<String>> = instances
        .iter()
        .map(|(name, member)| {
            (
                name.clone(),
                member
                    .routes
                    .values()
                    .filter_map(|r| match &r.provider {
                        Some(Provider::Instance { instance, .. }) => Some(instance.clone()),
                        _ => None,
                    })
                    .collect(),
            )
        })
        .collect();
    let mut order = Vec::new();
    while !pending.is_empty() {
        let next = pending
            .iter()
            .find(|(_, deps)| deps.is_empty())
            .map(|(name, _)| name.clone())
            .ok_or_else(|| invalid("required service dependency cycle"))?;
        pending.remove(&next);
        for deps in pending.values_mut() {
            deps.remove(&next);
        }
        order.push(next);
    }
    Ok(order)
}
fn unique_strings(values: &[String], label: &str, valid: impl Fn(&str) -> bool) -> Result<()> {
    if values.iter().collect::<BTreeSet<_>>().len() != values.len()
        || values.iter().any(|s| !valid(s))
    {
        return Err(invalid(format!("invalid or duplicate {label}")));
    }
    Ok(())
}
fn modes(values: &[EventMode]) -> Result<()> {
    if values
        .iter()
        .enumerate()
        .any(|(i, m)| values[..i].contains(m))
    {
        return Err(invalid("duplicate event mode"));
    }
    if values
        .iter()
        .any(|m| !matches!(m, EventMode::Parallel | EventMode::Serial))
    {
        return Err(ProtocolError::new(
            ErrorCode::UnsupportedCapability,
            "prepare",
            "event mode requires an extension or asynchronous migration",
        ));
    }
    Ok(())
}
fn require_event_capability(capabilities: &[String], mode: EventMode) -> Result<()> {
    let required = match mode {
        EventMode::Parallel => "event.parallel",
        EventMode::Serial => "event.serial",
        _ => return Err(invalid("unsupported event mode")),
    };
    require_capability(capabilities, required)
}
fn require_capability(capabilities: &[String], required: &str) -> Result<()> {
    if !capabilities.iter().any(|c| c == required) {
        return Err(ProtocolError::new(
            ErrorCode::UnsupportedCapability,
            "prepare",
            format!("runner lacks {required}"),
        ));
    }
    Ok(())
}
fn require_type_capabilities(
    capabilities: &[String],
    ty: &crate::contract::TypeExpr,
) -> Result<()> {
    use crate::contract::TypeExpr;
    match ty {
        TypeExpr::Object { .. } => require_capability(capabilities, "object.scope")?,
        TypeExpr::Callback { params, result, .. } => {
            require_capability(capabilities, "callback.borrow")?;
            require_type_capabilities(capabilities, params)?;
            require_type_capabilities(capabilities, result)?;
        }
        TypeExpr::Record { fields } => {
            for ty in fields.values() {
                require_type_capabilities(capabilities, ty)?;
            }
        }
        TypeExpr::List { item } | TypeExpr::Optional { item } => {
            require_type_capabilities(capabilities, item)?
        }
        TypeExpr::Value { .. } => (),
        TypeExpr::Stream { .. } => {
            return Err(ProtocolError::new(
                ErrorCode::UnsupportedCapability,
                "prepare",
                "stream is not in the base protocol",
            ))
        }
    }
    Ok(())
}
fn module_path(path: &str) -> Result<()> {
    relative(path)?;
    if !matches!(
        Path::new(path).extension().and_then(|s| s.to_str()),
        Some("mjs" | "js" | "cjs")
    ) {
        return Err(invalid(
            "Node entry/runner must be a packaged JavaScript module",
        ));
    }
    Ok(())
}
fn relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\\', '\0', ':'])
        || path.starts_with('/')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(invalid(
            "artifact path must be a package-relative normalized path",
        ));
    }
    Ok(())
}
fn confined(root: &Path, name: &str) -> Result<PathBuf> {
    relative(name)?;
    let path = fs::canonicalize(root.join(name)).map_err(|e| unavailable(e.to_string()))?;
    if !path.starts_with(root) {
        return Err(invalid("artifact symlink escapes package"));
    }
    Ok(path)
}
fn read_file(root: &Path, name: &str) -> Result<PreparedFile> {
    let path = confined(root, name)?;
    let metadata = fs::metadata(&path).map_err(|e| unavailable(e.to_string()))?;
    if !metadata.is_file() {
        return Err(invalid("artifact must be a regular file"));
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    let bytes = fs::read(&path).map_err(|e| unavailable(e.to_string()))?;
    Ok(PreparedFile {
        canonical_name: path
            .strip_prefix(root)
            .expect("artifact is confined")
            .to_str()
            .ok_or_else(|| invalid("canonical artifact path is not UTF-8"))?
            .replace(std::path::MAIN_SEPARATOR, "/"),
        path,
        executable,
        sha256: digest(&bytes),
        bytes: bytes.into(),
    })
}
fn inventory(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    fn visit(
        root: &Path,
        relative_dir: &Path,
        ancestors: &mut BTreeSet<PathBuf>,
        files: &mut BTreeMap<String, PathBuf>,
    ) -> Result<()> {
        let directory =
            fs::canonicalize(root.join(relative_dir)).map_err(|e| unavailable(e.to_string()))?;
        if !directory.starts_with(root) || !ancestors.insert(directory.clone()) {
            return Err(invalid("package has an escaping or cyclic directory link"));
        }
        for entry in fs::read_dir(&directory).map_err(|e| unavailable(e.to_string()))? {
            let entry = entry.map_err(|e| unavailable(e.to_string()))?;
            let name = relative_dir.join(entry.file_name());
            let spelling = name
                .to_str()
                .ok_or_else(|| invalid("package path is not UTF-8"))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let path = confined(root, &spelling)?;
            let metadata = fs::metadata(&path).map_err(|e| unavailable(e.to_string()))?;
            if metadata.is_dir() {
                visit(root, &name, ancestors, files)?;
            } else if metadata.is_file() {
                if spelling != MANIFEST {
                    files.insert(spelling, path);
                }
            } else {
                return Err(invalid("package contains a non-regular artifact"));
            }
        }
        ancestors.remove(&directory);
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, Path::new(""), &mut BTreeSet::new(), &mut files)?;
    Ok(files)
}
