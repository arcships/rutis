//! Package/graph admission evidence. Real process dispatch remains covered by
//! objects_ipc; synthetic module text here must never be imported by prepare.
#![cfg(target_os = "linux")]
use rutis_protocol::{error::ErrorCode, prepare::*, runner_image::RunnerCatalog};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
rutis_protocol::embed_runner_catalog!(include_bytes!(
    "../../../protocol/fixtures/runner.catalog.json"
));
const BUNDLE: &[u8] = include_bytes!("../../../protocol/fixtures/rpc.bundle.json");
const SCHEMA: &[u8] = include_bytes!("../../../protocol/fixtures/plugin.config.json");
const ENVIRONMENT: &[u8] = include_bytes!("../../../protocol/fixtures/runtime.environment.lock");
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    deployment: Value,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn service() -> Value {
    json!({"interface":"Database", "version":"1.0.0", "bundle":"bundle.json"})
}

fn copy_dependency_tree(
    source: &std::path::Path,
    target: &std::path::Path,
    package: &std::path::Path,
    manifest: &mut Value,
) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dependency_tree(&entry.path(), &target, package, manifest);
        } else {
            let bytes = fs::read(entry.path()).unwrap();
            fs::write(&target, &bytes).unwrap();
            fs::set_permissions(&target, fs::metadata(entry.path()).unwrap().permissions())
                .unwrap();
            let path = target.strip_prefix(package).unwrap().to_str().unwrap();
            manifest["files"]
                .as_array_mut()
                .unwrap()
                .push(json!({"path":path,"kind":"dependency","sha256":digest(&bytes)}));
            manifest["runtime"]["environment"]
                .as_array_mut()
                .unwrap()
                .push(json!(path));
        }
    }
}
impl Fixture {
    fn new() -> Self {
        assert!(RunnerCatalog::parse(runner_catalog_bytes()).is_ok());
        let root = std::env::temp_dir().join(format!(
            "rutis-protocol-prepare-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            deployment: json!({"id":"test", "packages":{}, "groups":{}, "native_services":{}, "instances":{}}),
        }
    }
    fn package(&mut self, name: &str, language: &str, factory: &str) {
        let directory = self.root.join(name).join("1.0.0");
        fs::create_dir_all(&directory).unwrap();
        fs::copy(std::env::current_exe().unwrap(), directory.join("runtime")).unwrap();
        fs::set_permissions(directory.join("runtime"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(directory.join("bundle.json"), BUNDLE).unwrap();
        fs::write(directory.join("config.json"), SCHEMA).unwrap();
        fs::write(directory.join("dependency.lock"), ENVIRONMENT).unwrap();
        let mut files = vec![
            ("runtime", "executable"),
            ("bundle.json", "bundle"),
            ("config.json", "config_schema"),
            ("dependency.lock", "dependency"),
        ];
        if language == "node-cordis" {
            // Preparing this entry must not execute its top-level effect.
            fs::write(directory.join("plugin.mjs"), "import { writeFileSync } from 'node:fs'; writeFileSync(new URL('executed', import.meta.url), 'executed'); throw new Error('plugin code ran during prepare');\n").unwrap();
            fs::write(
                directory.join("runner.mjs"),
                "throw new Error('runtime code ran during prepare');\n",
            )
            .unwrap();
            files.extend([("plugin.mjs", "code"), ("runner.mjs", "code")]);
        }
        let files: Vec<_> = files.into_iter().map(|(path,kind)| json!({"path":path,"kind":kind,"sha256":digest(&fs::read(directory.join(path)).unwrap())})).collect();
        let provides = if factory == "consumer" {
            json!({})
        } else {
            json!({"database":service()})
        };
        let requires = if factory == "provider" {
            json!({})
        } else {
            json!({"database":service()})
        };
        let manifest = json!({"id":name, "version":"1.0.0", "protocol_family":"rutis-cordis-objects", "protocol_version":"0.experimental", "runtime":{"kind":language,"framework_version":if language=="rust-rutis"{"0.3.0"}else{"4.0.1"},"executable":"runtime","runner":if language=="node-cordis"{json!("runner.mjs")}else{Value::Null},"environment":["dependency.lock"],"capabilities":["object.scope","callback.borrow"]}, "plugin":if language=="rust-rutis"{json!({"kind":"rust","factory":factory})}else{json!({"kind":"node","entry":"plugin.mjs"})}, "files":files,"config_schema":"config.json","provides":provides,"requires":requires,"events":{}});
        fs::write(
            directory.join(MANIFEST),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        self.deployment["packages"][name] = json!(format!("{name}/1.0.0"));
    }
    fn instance(
        &mut self,
        name: &str,
        package: &str,
        group: &str,
        language: &str,
        exports: &[&str],
    ) {
        self.deployment["groups"][group] = json!({"kind":language,"trust":"test-owned"});
        self.deployment["instances"][name] = json!({"package":package,"group":group,"config":{"label":name},"routes":{},"exports":exports,"events":{}});
    }
    fn route(&mut self, consumer: &str, provider: &str) {
        self.deployment["instances"][consumer]["routes"]["database"] =
            json!({"kind":"instance","instance":provider,"service":"database"});
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name).join("1.0.0")
    }
    fn manifest(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(name).join(MANIFEST)).unwrap()).unwrap()
    }
    fn write_manifest(&self, name: &str, manifest: &Value) {
        fs::write(
            self.path(name).join(MANIFEST),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
    }
    fn prepare(&self) -> rutis_protocol::error::Result<PreparedDeployment> {
        PreparedDeployment::prepare(&self.root, &serde_json::to_vec(&self.deployment).unwrap())
    }
}
fn two_languages() -> Fixture {
    let mut f = Fixture::new();
    f.package("rust-provider", "rust-rutis", "provider");
    f.package("node-consumer", "node-cordis", "consumer");
    f.instance(
        "rust",
        "rust-provider",
        "native",
        "rust-rutis",
        &["database"],
    );
    f.instance("node", "node-consumer", "js", "node-cordis", &[]);
    f.route("node", "rust");
    f
}
#[test]
fn native_elf_catalog_and_node_files_prepare_without_executing_plugins() {
    let f = two_languages();
    let plan = f.prepare().unwrap();
    assert_eq!(plan.dependency_order(), ["rust", "node"]);
    let route = &plan.instances()["node"].routes()["database"];
    assert_eq!(route.key(), &plan.export_key("rust", "database").unwrap());
    assert_eq!(route.contract().bundle_sha256, digest(BUNDLE));
    assert!(!f.path("node-consumer").join("executed").exists());
    plan.verify_unchanged().unwrap();
    let catalog = plan.instances()["rust"].package().catalog().unwrap();
    assert!(catalog.factories.contains_key("provider"));
    assert_eq!(plan.groups()["js"].argv().len(), 2);
    assert_eq!(plan.groups()["native"].argv().len(), 1);
}

#[tokio::test]
async fn prepared_member_selects_linked_factory_and_keeps_its_frozen_config() {
    use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, PluginFactory};
    use rutis_protocol::factories::{StaticFactories, StaticFactory};
    use std::sync::{Arc, Mutex};
    // This checks the prepare-to-native lifecycle boundary, not publication of
    // the fixture's Database service (the production export bridge is separate).
    struct Factory(Arc<Mutex<Vec<String>>>);
    struct Instance(String, Arc<Mutex<Vec<String>>>);
    impl PluginFactory<Value> for Factory {
        fn build(&self, config: &Value) -> Result<Box<dyn Plugin>, CordisError> {
            Ok(Box::new(Instance(
                config["label"].as_str().unwrap().into(),
                self.0.clone(),
            )))
        }
    }
    impl Plugin for Instance {
        fn name(&self) -> &str {
            "prepared-config"
        }
        fn apply<'a>(&'a self, _: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
            Box::pin(async move {
                self.1.lock().unwrap().push(self.0.clone());
                Ok(Effect::Done)
            })
        }
    }
    let mut f = two_languages();
    let plan = f.prepare().unwrap();
    f.deployment["instances"]["rust"]["config"]["label"] = json!("changed-after-prepare");
    let catalog = RunnerCatalog::parse(runner_catalog_bytes()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let constructed = Arc::new(AtomicU64::new(0));
    let entries = catalog
        .factories
        .iter()
        .map(|(name, metadata)| {
            let seen = seen.clone();
            let constructed = constructed.clone();
            StaticFactory::native(name.clone(), metadata.clone(), SCHEMA, move || {
                constructed.fetch_add(1, Ordering::Relaxed);
                Factory(seen.clone())
            })
            .unwrap()
        })
        .collect::<Vec<_>>();
    let registry = StaticFactories::admit(runner_catalog_bytes(), entries).unwrap();
    assert_eq!(constructed.load(Ordering::Relaxed), 0);
    let root = Ctx::root().unwrap();
    assert_eq!(
        registry
            .mount_prepared(&root, &plan.instances()["node"])
            .err()
            .unwrap()
            .code,
        ErrorCode::InterfaceMismatch
    );
    assert_eq!(constructed.load(Ordering::Relaxed), 0);
    let activation = registry
        .mount_prepared(&root, &plan.instances()["rust"])
        .unwrap();
    activation.view().await.unwrap();
    assert_eq!(constructed.load(Ordering::Relaxed), 1);
    assert_eq!(*seen.lock().unwrap(), vec!["rust"]);
    activation.stop().await.unwrap();
    root.shutdown().await.unwrap();
}
#[test]
fn shared_members_have_independent_configs_and_dependency_edges_without_group_barrier() {
    let mut f = Fixture::new();
    f.package("provider", "rust-rutis", "provider");
    f.package("consumer", "rust-rutis", "consumer");
    f.instance("a", "provider", "shared", "rust-rutis", &["database"]);
    f.instance("b", "consumer", "shared", "rust-rutis", &[]);
    f.route("b", "a");
    let plan = f.prepare().unwrap();
    assert_eq!(plan.groups()["shared"].members().len(), 2);
    assert_eq!(plan.dependency_order(), ["a", "b"]);
    assert_ne!(
        plan.instances()["a"].config(),
        plan.instances()["b"].config()
    );
    assert_eq!(
        plan.packages()["provider"]
            .file("runtime")
            .unwrap()
            .bytes()
            .as_ptr(),
        plan.packages()["consumer"]
            .file("runtime")
            .unwrap()
            .bytes()
            .as_ptr(),
        "shared images are retained once"
    );
    let code = plan.groups()["shared"].code_sha256().to_owned();
    let deployment = plan.sha256().to_owned();
    f.deployment["instances"]["a"]["config"]["label"] = json!("new config");
    let changed = f.prepare().unwrap();
    assert_eq!(changed.groups()["shared"].code_sha256(), code);
    assert_ne!(changed.sha256(), deployment);
    assert_eq!(
        plan.instances()["a"].config()["label"],
        "a",
        "prepared configuration is immutable"
    );
}
#[test]
fn missing_route_remains_explicitly_pending_and_native_adapter_contracts_are_exact() {
    let mut f = Fixture::new();
    f.package("consumer", "rust-rutis", "consumer");
    f.instance("member", "consumer", "group", "rust-rutis", &[]);
    let plan = f.prepare().unwrap();
    assert_eq!(
        plan.instances()["member"]
            .missing_routes()
            .collect::<Vec<_>>(),
        ["database"]
    );
    f.deployment["native_services"]["db"] =
        json!({"interface":"Database", "version":"1.0.0", "bundle_sha256":digest(BUNDLE)});
    f.deployment["instances"]["member"]["routes"]["database"] =
        json!({"kind":"native","service":"db"});
    assert_eq!(
        f.prepare().unwrap().instances()["member"]
            .missing_routes()
            .count(),
        0
    );
    f.deployment["native_services"]["db"]["bundle_sha256"] = json!("a".repeat(64));
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
}
#[test]
fn complete_required_graph_rejects_same_group_and_cross_group_cycles() {
    let mut f = Fixture::new();
    f.package("cycle", "rust-rutis", "cycle");
    for shared in [true, false] {
        f.deployment["groups"] = json!({});
        f.deployment["instances"] = json!({});
        f.instance("a", "cycle", "one", "rust-rutis", &["database"]);
        f.instance(
            "b",
            "cycle",
            if shared { "one" } else { "two" },
            "rust-rutis",
            &["database"],
        );
        f.route("a", "b");
        f.route("b", "a");
        assert!(f.prepare().err().unwrap().message.contains("cycle"));
    }
}
#[test]
fn unavailable_or_unexported_providers_and_undeclared_routes_are_rejected() {
    let mut f = two_languages();
    let good = f.deployment.clone();
    f.deployment["instances"]["node"]["routes"]["database"]["instance"] = json!("absent");
    assert!(f.prepare().is_err());
    f.deployment = good.clone();
    f.deployment["instances"]["rust"]["exports"] = json!([]);
    assert_eq!(f.prepare().err().unwrap().code, ErrorCode::CapabilityDenied);
    f.deployment = good;
    f.deployment["instances"]["node"]["routes"]["extra"] =
        json!({"kind":"native","service":"absent"});
    assert_eq!(f.prepare().err().unwrap().code, ErrorCode::InvalidParams);
}
#[test]
fn raw_bundle_digest_mismatch_is_not_semver_compatibility() {
    let mut f = two_languages();
    let path = f.path("node-consumer").join("bundle.json");
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(b'\n');
    fs::write(&path, &bytes).unwrap();
    let mut manifest = f.manifest("node-consumer");
    for file in manifest["files"].as_array_mut().unwrap() {
        if file["path"] == "bundle.json" {
            file["sha256"] = json!(digest(&bytes));
        }
    }
    f.write_manifest("node-consumer", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    f.deployment["instances"]["node"]["routes"] = json!({});
    assert!(
        f.prepare().is_ok(),
        "different interface bundles can coexist until a route claims equality"
    );
}
#[test]
fn frozen_package_detects_changed_files_manifest_and_inventory() {
    let f = two_languages();
    let plan = f.prepare().unwrap();
    let path = f.path("node-consumer").join("plugin.mjs");
    let old = fs::read(&path).unwrap();
    fs::write(&path, b"changed").unwrap();
    assert!(plan.verify_unchanged().is_err());
    fs::write(&path, &old).unwrap();
    let extra = f.path("node-consumer").join("undeclared.js");
    fs::write(&extra, b"extra").unwrap();
    assert!(plan.verify_unchanged().is_err());
    fs::remove_file(extra).unwrap();
    let mut manifest = f.manifest("node-consumer");
    manifest["version"] = json!("1.0.1");
    f.write_manifest("node-consumer", &manifest);
    assert!(plan.verify_unchanged().is_err());
    assert_eq!(
        plan.instances()["node"]
            .package()
            .file("plugin.mjs")
            .unwrap()
            .bytes(),
        old
    );
}
#[test]
fn paths_and_escaping_symlinks_are_rejected_but_internal_file_links_are_validated() {
    let f = two_languages();
    let mut manifest = f.manifest("node-consumer");
    let original = manifest.clone();
    for path in [
        "../outside",
        "/absolute",
        "a/../b",
        "a\\b",
        "./plugin.mjs",
        "a//b",
    ] {
        manifest["files"][0]["path"] = json!(path);
        f.write_manifest("node-consumer", &manifest);
        assert!(f.prepare().is_err(), "{path}");
    }
    f.write_manifest("node-consumer", &original);
    let file = f.path("node-consumer").join("plugin.mjs");
    let bytes = fs::read(&file).unwrap();
    fs::remove_file(&file).unwrap();
    let outside = f.root.join("outside.mjs");
    fs::write(&outside, &bytes).unwrap();
    symlink(&outside, &file).unwrap();
    assert!(f.prepare().is_err());
    fs::remove_file(&file).unwrap();
    let inside = f.path("node-consumer").join("entry.mjs");
    fs::write(&inside, &bytes).unwrap();
    symlink("entry.mjs", &file).unwrap();
    manifest = original;
    manifest["files"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"entry.mjs","kind":"code","sha256":digest(&bytes)}));
    f.write_manifest("node-consumer", &manifest);
    assert!(f.prepare().is_ok());
}
#[test]
fn catalog_inspection_rejects_missing_factory_changed_config_and_truncated_images() {
    let f = two_languages();
    let mut manifest = f.manifest("rust-provider");
    let original = manifest.clone();
    manifest["plugin"]["factory"] = json!("not-linked");
    f.write_manifest("rust-provider", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    f.write_manifest("rust-provider", &original);
    let schema = f.path("rust-provider").join("config.json");
    let mut bytes = fs::read(&schema).unwrap();
    bytes.push(b'\n');
    fs::write(schema, &bytes).unwrap();
    manifest = original.clone();
    for file in manifest["files"].as_array_mut().unwrap() {
        if file["path"] == "config.json" {
            file["sha256"] = json!(digest(&bytes));
        }
    }
    f.write_manifest("rust-provider", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    let executable = fs::read(std::env::current_exe().unwrap()).unwrap();
    for end in [0, 4, 7, 40, 63, 128] {
        assert!(RunnerCatalog::from_elf(&executable[..end]).is_err());
    }
}
#[test]
fn malformed_configs_duplicate_manifest_fields_and_extensions_fail_without_code() {
    let mut f = two_languages();
    f.deployment["instances"]["node"]["config"] = json!({"label":17});
    assert_eq!(f.prepare().err().unwrap().code, ErrorCode::InvalidParams);
    f.deployment["instances"]["node"]["config"] = json!({"label":"valid"});
    let mut manifest = f.manifest("node-consumer");
    manifest["runtime"]["capabilities"] = json!(["object.scope", "callback.borrow", "stream"]);
    f.write_manifest("node-consumer", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::UnsupportedCapability
    );
    manifest["runtime"]["capabilities"] = json!(["object.scope", "object.scope"]);
    f.write_manifest("node-consumer", &manifest);
    assert_eq!(f.prepare().err().unwrap().code, ErrorCode::InvalidParams);
    fs::write(
        f.path("node-consumer").join(MANIFEST),
        b"{\"id\":\"a\",\"id\":\"b\"}",
    )
    .unwrap();
    assert!(f.prepare().err().unwrap().message.contains("duplicate"));
    assert!(!f.path("node-consumer").join("executed").exists());
}
#[test]
fn shared_runtime_requires_equal_framework_capabilities_and_complete_dependency_inventory() {
    let mut f = Fixture::new();
    f.package("one", "node-cordis", "provider");
    f.package("two", "node-cordis", "provider");
    f.instance("a", "one", "shared", "node-cordis", &["database"]);
    f.instance("b", "two", "shared", "node-cordis", &["database"]);
    assert!(f.prepare().is_ok());
    let mut manifest = f.manifest("two");
    let original = manifest.clone();
    manifest["runtime"]["framework_version"] = json!("4.0.2");
    f.write_manifest("two", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    manifest = original.clone();
    manifest["runtime"]["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!("event.parallel"));
    f.write_manifest("two", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    manifest = original;
    manifest["runtime"]["environment"] = json!([]);
    f.write_manifest("two", &manifest);
    assert!(f.prepare().is_err());
}
#[test]
fn host_key_encoding_keeps_slashes_in_names_unambiguous() {
    let mut f = Fixture::new();
    f.package("node", "node-cordis", "consumer");
    f.instance("a/b", "node", "g", "node-cordis", &[]);
    f.instance("a", "node", "g", "node-cordis", &[]);
    let mut manifest = f.manifest("node");
    manifest["requires"]["b/database"] = service();
    f.write_manifest("node", &manifest);
    let plan = f.prepare().unwrap();
    assert_ne!(
        plan.instances()["a/b"].routes()["database"].key(),
        plan.instances()["a"].routes()["b/database"].key()
    );
    assert_ne!(
        plan.instances()["a/b"].routes()["database"].source(),
        plan.instances()["a"].routes()["b/database"].source()
    );
}
#[test]
fn prepare_cli_returns_reviewable_plan_without_config_secrets_or_execution() {
    let mut f = two_languages();
    f.deployment["instances"]["node"]["config"]["label"] = json!("secret-config-value");
    let path = f.root.join("deployment.json");
    fs::write(&path, serde_json::to_vec_pretty(&f.deployment).unwrap()).unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_rutis-protocol-prepare"))
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(output["dependency_order"], json!(["rust", "node"]));
    assert_eq!(output["groups"]["js"]["trust"], "test-owned");
    assert_eq!(
        output["instances"]["node"]["routes"]["database"]["provider"],
        json!({"kind":"instance","instance":"rust","service":"database"})
    );
    assert_eq!(
        output["instances"]["node"]["routes"]["database"]["bundle_sha256"],
        digest(BUNDLE)
    );
    assert!(!String::from_utf8_lossy(&result.stdout).contains("secret-config-value"));
    assert!(!f.path("node-consumer").join("executed").exists());
}

#[test]
fn whole_package_contract_admission_checks_inferred_features_and_unused_bundles() {
    let f = two_languages();
    let mut bundle: Value = serde_json::from_slice(BUNDLE).unwrap();
    bundle["required_capabilities"] = json!([]);
    let bytes = serde_json::to_vec(&bundle).unwrap();
    fs::write(f.path("node-consumer").join("bundle.json"), &bytes).unwrap();
    let mut manifest = f.manifest("node-consumer");
    for file in manifest["files"].as_array_mut().unwrap() {
        if file["path"] == "bundle.json" {
            file["sha256"] = json!(digest(&bytes));
        }
    }
    manifest["runtime"]["capabilities"] = json!(["object.scope"]);
    f.write_manifest("node-consumer", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::UnsupportedCapability,
        "an omitted required_capabilities field cannot hide actual callbacks"
    );
    manifest["runtime"]["capabilities"] = json!(["object.scope", "callback.borrow"]);
    let unsupported = br#"{"id":"unused","version":"1.0.0","interfaces":{"Stream":{"methods":{"read":{"params":{"kind":"value","schema":{"type":"null"}},"result":{"kind":"stream","item":{"kind":"value","schema":{"type":"string"}}}}}}}}"#;
    fs::write(
        f.path("node-consumer").join("unused.bundle.json"),
        unsupported,
    )
    .unwrap();
    manifest["files"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"unused.bundle.json","kind":"bundle","sha256":digest(unsupported)}));
    f.write_manifest("node-consumer", &manifest);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::UnsupportedCapability
    );

    let mut events = Fixture::new();
    events.package("events", "node-cordis", "consumer");
    events.instance("member", "events", "js", "node-cordis", &[]);
    let bytes = br#"{"id":"events","version":"1.0.0","interfaces":{"Session":{"methods":{},"properties":{}}},"events":{"session":{"params":{"kind":"object","interface":"Session","ownership":"scope"},"result":{"kind":"value","schema":{"type":"null"}},"modes":["parallel"]}}}"#;
    fs::write(events.path("events").join("bundle.json"), bytes).unwrap();
    let mut manifest = events.manifest("events");
    manifest["requires"] = json!({});
    manifest["runtime"]["capabilities"] = json!(["event.parallel"]);
    for file in manifest["files"].as_array_mut().unwrap() {
        if file["path"] == "bundle.json" {
            file["sha256"] = json!(digest(bytes));
        }
    }
    events.write_manifest("events", &manifest);
    assert_eq!(
        events.prepare().err().unwrap().code,
        ErrorCode::UnsupportedCapability,
        "an unused event payload still requires object.scope"
    );
    manifest["runtime"]["capabilities"] = json!(["object.scope"]);
    events.write_manifest("events", &manifest);
    assert_eq!(
        events.prepare().err().unwrap().code,
        ErrorCode::UnsupportedCapability,
        "an unused event still requires its actual mode capability"
    );
    manifest["runtime"]["capabilities"] = json!(["object.scope", "event.parallel"]);
    events.write_manifest("events", &manifest);
    assert!(events.prepare().is_ok());
}
#[test]
fn event_permissions_freeze_scope_and_reject_widening_or_synchronous_modes() {
    let mut f = Fixture::new();
    f.package("events", "node-cordis", "provider");
    f.instance("member", "events", "group", "node-cordis", &["database"]);
    let bytes = include_bytes!("../../../protocol/fixtures/database.bundle.json");
    fs::write(f.path("events").join("bundle.json"), bytes).unwrap();
    let mut manifest = f.manifest("events");
    for file in manifest["files"].as_array_mut().unwrap() {
        if file["path"] == "bundle.json" {
            file["sha256"] = json!(digest(bytes));
        }
    }
    manifest["runtime"]["capabilities"] = json!([
        "object.scope",
        "callback.borrow",
        "event.parallel",
        "event.serial"
    ]);
    manifest["events"]["session"] = json!({"bundle":"bundle.json","event":"session/event","publish":["parallel"],"subscribe":["parallel","serial"]});
    f.write_manifest("events", &manifest);
    f.deployment["instances"]["member"]["events"]["session"] =
        json!({"scope":"session/member","publish":["parallel"],"subscribe":["serial"]});
    let prepared = f.prepare().unwrap();
    assert_eq!(
        prepared.instances()["member"].events()["session"].scope,
        "session/member"
    );
    f.deployment["instances"]["member"]["events"]["session"]["publish"] = json!(["serial"]);
    assert_eq!(f.prepare().err().unwrap().code, ErrorCode::CapabilityDenied);
    f.deployment["instances"]["member"]["events"]["session"]["publish"] = json!(["bail"]);
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::UnsupportedCapability
    );
    assert_eq!(
        prepared.instances()["member"].events()["session"]
            .publish
            .len(),
        1
    );
}
#[test]
fn elf_overflows_and_duplicate_catalog_sections_fail_before_execution() {
    let image = fs::read(std::env::current_exe().unwrap()).unwrap();
    let mut overflow = image.clone();
    overflow[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(RunnerCatalog::from_elf(&overflow).is_err());
    let table = u64::from_le_bytes(image[40..48].try_into().unwrap()) as usize;
    let size = u16::from_le_bytes(image[58..60].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes(image[60..62].try_into().unwrap()) as usize;
    let names_index = u16::from_le_bytes(image[62..64].try_into().unwrap()) as usize;
    let names_header = table + names_index * size;
    let names_at = u64::from_le_bytes(
        image[names_header + 24..names_header + 32]
            .try_into()
            .unwrap(),
    ) as usize;
    let mut catalog = None;
    for i in 0..count {
        let at = table + i * size;
        let offset = u32::from_le_bytes(image[at..at + 4].try_into().unwrap()) as usize;
        let name = &image[names_at + offset..];
        let end = name.iter().position(|b| *b == 0).unwrap();
        if &name[..end] == rutis_protocol::runner_image::CATALOG_SECTION {
            catalog = Some(at);
        }
    }
    let mut duplicate = image.clone();
    let new_table = duplicate.len();
    duplicate.extend_from_slice(&image[table..table + size * count]);
    let at = catalog.unwrap();
    duplicate.extend_from_slice(&image[at..at + size]);
    duplicate[40..48].copy_from_slice(&(new_table as u64).to_le_bytes());
    duplicate[60..62].copy_from_slice(&((count + 1) as u16).to_le_bytes());
    assert!(RunnerCatalog::from_elf(&duplicate)
        .err()
        .unwrap()
        .message
        .contains("ambiguous"));
}

#[test]
fn snapshot_uses_frozen_bytes_and_keeps_paths_alive_until_all_leases_are_released() {
    use rutis_protocol::snapshot::Snapshot;
    let f = two_languages();
    let plan = f.prepare().unwrap();
    let expected_manifest = plan.instances()["rust"].package().manifest_bytes().to_vec();
    fs::remove_dir_all(f.path("rust-provider")).unwrap();
    fs::remove_dir_all(f.path("node-consumer")).unwrap();
    assert!(plan.verify_unchanged().is_err());
    let snapshot = Snapshot::materialize(&plan).unwrap();
    let root = snapshot.root().to_owned();
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for (name, group) in snapshot.groups() {
        let args = group.argv();
        assert!(args[0].starts_with(&root));
        assert_eq!(
            digest(&fs::read(&args[0]).unwrap()),
            plan.groups()[name].executable().sha256()
        );
        assert_eq!(
            fs::metadata(&args[0]).unwrap().permissions().mode() & 0o777,
            0o555
        );
        for member in group.members().values() {
            let dependency = fs::canonicalize(member.root().join("dependency.lock")).unwrap();
            assert_eq!(dependency, group.environment().join("dependency.lock"));
            assert!(dependency.starts_with(&root));
        }
    }
    assert_eq!(
        fs::read(
            snapshot.groups()["native"].members()["rust"]
                .root()
                .join(MANIFEST)
        )
        .unwrap(),
        expected_manifest
    );
    let group = snapshot.groups()["native"].clone();
    let member = group.members()["rust"].clone();
    assert_eq!(
        snapshot.cleanup().err().unwrap().code,
        ErrorCode::Unavailable
    );
    assert!(root.exists());
    drop(group);
    assert!(
        root.exists(),
        "a member lease also retains its dependency tree"
    );
    drop(member);
    assert!(!root.exists());
    let snapshot = Snapshot::materialize(&plan).unwrap();
    let root = snapshot.root().to_owned();
    snapshot.cleanup().unwrap();
    assert!(!root.exists());
}

#[test]
fn frozen_node_launch_uses_one_cordis_and_compiled_sdk_across_distinct_packages() {
    use rutis_protocol::snapshot::Snapshot;
    use std::process::Command;
    let mut f = Fixture::new();
    let ts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/ts");
    let compiled = f.root.join("compiled-sdk");
    let build = Command::new("npm")
        .arg("--prefix")
        .arg(&ts)
        .args(["run", "build", "--", "--outDir"])
        .arg(&compiled)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
    for name in ["first", "second"] {
        f.package(name, "node-cordis", "consumer");
        let package = f.path(name);
        fs::copy(&node, package.join("runtime")).unwrap();
        fs::write(
            package.join("runner.mjs"),
            include_bytes!("../../../protocol/fixtures/snapshot-runner.mjs"),
        )
        .unwrap();
        fs::write(
            package.join("plugin.mjs"),
            include_bytes!("../../../protocol/fixtures/snapshot-plugin.mjs"),
        )
        .unwrap();
        let mut manifest = f.manifest(name);
        manifest["requires"] = json!({});
        for file in manifest["files"].as_array_mut().unwrap() {
            let bytes = fs::read(package.join(file["path"].as_str().unwrap())).unwrap();
            file["sha256"] = json!(digest(&bytes));
        }
        for dependency in [
            "@deepseek-ai/cordis",
            "@deepseek-ai/cosmokit",
            "@standard-schema/spec",
        ] {
            copy_dependency_tree(
                &ts.join("node_modules").join(dependency),
                &package.join("node_modules").join(dependency),
                &package,
                &mut manifest,
            );
        }
        let sdk = package.join("node_modules/@rutis/protocol");
        copy_dependency_tree(&compiled, &sdk, &package, &mut manifest);
        let alias = "node_modules/@rutis/protocol/src/alias.js";
        symlink("managed.js", package.join(alias)).unwrap();
        manifest["files"].as_array_mut().unwrap().push(json!({"path":alias,"kind":"dependency","sha256":digest(&fs::read(sdk.join("src/managed.js")).unwrap())}));
        manifest["runtime"]["environment"]
            .as_array_mut()
            .unwrap()
            .push(json!(alias));
        let package_json = br#"{"name":"@rutis/protocol","version":"0.1.0","type":"module","exports":{"./managed":"./src/managed.js","./managedAlias":"./src/alias.js"}}"#;
        fs::write(sdk.join("package.json"), package_json).unwrap();
        let path = "node_modules/@rutis/protocol/package.json";
        manifest["files"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path":path,"kind":"dependency","sha256":digest(package_json)}));
        manifest["runtime"]["environment"]
            .as_array_mut()
            .unwrap()
            .push(json!(path));
        f.write_manifest(name, &manifest);
        f.instance(name, name, "shared", "node-cordis", &[]);
    }
    f.instance("alias", "first", "shared", "node-cordis", &[]);
    let plan = f.prepare().unwrap();
    fs::remove_dir_all(f.path("first")).unwrap();
    fs::remove_dir_all(f.path("second")).unwrap();
    assert!(plan.verify_unchanged().is_err());
    let snapshot = Snapshot::materialize(&plan).unwrap();
    let group = &snapshot.groups()["shared"];
    assert_eq!(
        group.members()["first"].root(),
        group.members()["alias"].root(),
        "one code package is not re-imported for each instance"
    );
    assert_ne!(
        group.members()["first"].root(),
        group.members()["second"].root()
    );
    let a = fs::canonicalize(
        group.members()["first"]
            .root()
            .join("node_modules/@rutis/protocol/src/managed.js"),
    )
    .unwrap();
    let b = fs::canonicalize(
        group.members()["second"]
            .root()
            .join("node_modules/@rutis/protocol/src/managed.js"),
    )
    .unwrap();
    assert_eq!(a, b);
    let argv = group.argv();
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .arg(group.members()["first"].entry().unwrap())
        .arg(group.members()["second"].entry().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"canonical_dependencies":true,"isolated_instances":2,"cleaned":["a","b"]})
    );
    snapshot.cleanup().unwrap();
}

#[test]
fn environment_identity_binds_symlink_targets_and_executable_flags() {
    use rutis_protocol::snapshot::Snapshot;
    let mut f = Fixture::new();
    let helper = b"export const token = {}";
    for name in ["first", "second"] {
        f.package(name, "node-cordis", "consumer");
        let root = f.path(name);
        let mut manifest = f.manifest(name);
        for path in ["helper.mjs", "other.mjs"] {
            fs::write(root.join(path), helper).unwrap();
            fs::set_permissions(root.join(path), fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink(
            if name == "first" {
                "helper.mjs"
            } else {
                "other.mjs"
            },
            root.join("alias.mjs"),
        )
        .unwrap();
        for path in ["helper.mjs", "other.mjs", "alias.mjs"] {
            manifest["files"]
                .as_array_mut()
                .unwrap()
                .push(json!({"path":path,"kind":"dependency","sha256":digest(helper)}));
            manifest["runtime"]["environment"]
                .as_array_mut()
                .unwrap()
                .push(json!(path));
        }
        // An executable alias to a code-role artifact must retain its target's
        // executable flag in the copied tree.
        fs::rename(root.join("runtime"), root.join("runtime.real")).unwrap();
        symlink("runtime.real", root.join("runtime")).unwrap();
        manifest["files"].as_array_mut().unwrap().push(json!({"path":"runtime.real","kind":"code","sha256":digest(&fs::read(root.join("runtime.real")).unwrap())}));
        f.write_manifest(name, &manifest);
        f.instance(name, name, "shared", "node-cordis", &[]);
    }
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch,
        "equal bytes do not erase dependency alias topology"
    );
    fs::remove_file(f.path("second").join("alias.mjs")).unwrap();
    symlink("helper.mjs", f.path("second").join("alias.mjs")).unwrap();
    let first = f.prepare().unwrap();
    fs::set_permissions(
        f.path("second").join("helper.mjs"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(first.verify_unchanged().is_err());
    assert_eq!(
        f.prepare().err().unwrap().code,
        ErrorCode::InterfaceMismatch,
        "dependency execution permissions are part of the environment"
    );
    fs::set_permissions(
        f.path("second").join("helper.mjs"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let plan = f.prepare().unwrap();
    let snapshot = Snapshot::materialize(&plan).unwrap();
    let group = &snapshot.groups()["shared"];
    assert_eq!(
        fs::canonicalize(group.environment().join("alias.mjs")).unwrap(),
        group.environment().join("helper.mjs")
    );
    assert_eq!(
        fs::metadata(group.environment().join("helper.mjs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    assert_eq!(
        fs::metadata(&group.argv()[0]).unwrap().permissions().mode() & 0o777,
        0o555
    );
    snapshot.cleanup().unwrap();
}
