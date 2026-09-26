//! Four independent packages launched from frozen bytes with the default drivers.
use rutis_protocol::prepare::{digest, PreparedDeployment, MANIFEST};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn dependencies(
    source: &Path,
    target: &Path,
    package: &Path,
    files: &mut Vec<Value>,
    environment: &mut Vec<String>,
) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let to = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            dependencies(&entry.path(), &to, package, files, environment);
        } else {
            let bytes = fs::read(entry.path()).unwrap();
            fs::write(&to, &bytes).unwrap();
            let path = to
                .strip_prefix(package)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            files.push(json!({"path":path,"kind":"dependency","sha256":digest(&bytes)}));
            environment.push(path);
        }
    }
}
pub fn prepare() -> PreparedDeployment {
    let root = std::env::temp_dir().join(format!("rutis-native-runners-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(root.clone());
    let ts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../protocol/ts");
    let compiled = root.join("compiled");
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
    let node = String::from_utf8(node.stdout).unwrap();
    let mut deployment = json!({"id":"native-runners","packages":{},"groups":{"rust":{"kind":"rust-rutis","trust":"fixture"},"node":{"kind":"node-cordis","trust":"fixture"}},"instances":{},"native_services":{}});
    let service = json!({"rpc":{"interface":"Database","version":"1.0.0","bundle":"rpc.json"}});
    for language in ["rust", "node"] {
        for role in ["provider", "consumer"] {
            let name = format!("{language}-{role}");
            let package = root.join(&name).join("1.0.0");
            fs::create_dir_all(&package).unwrap();
            let executable = if language == "rust" {
                std::env::current_exe().unwrap()
            } else {
                node.trim().into()
            };
            let mut artifacts = vec![
                ("runtime", "executable", fs::read(executable).unwrap()),
                (
                    "rpc.json",
                    "bundle",
                    include_bytes!("../../../../protocol/fixtures/rpc.bundle.json").to_vec(),
                ),
                (
                    "config.json",
                    "config_schema",
                    include_bytes!("../../../../protocol/fixtures/plugin.config.json").to_vec(),
                ),
                (
                    "dependency.lock",
                    "dependency",
                    include_bytes!("../../../../protocol/fixtures/runtime.environment.lock")
                        .to_vec(),
                ),
            ];
            if language == "node" {
                artifacts.push((
                    "runner.mjs",
                    "code",
                    b"import './node_modules/@rutis/protocol/src/node-runner.js'\n".to_vec(),
                ));
                artifacts.push((
                    "entry.mjs",
                    "code",
                    if role == "provider" {
                        include_bytes!("../../../../protocol/fixtures/native-provider.mjs").to_vec()
                    } else {
                        include_bytes!("../../../../protocol/fixtures/native-consumer.mjs").to_vec()
                    },
                ));
            }
            let mut files = Vec::new();
            let mut environment = vec!["dependency.lock".to_owned()];
            for (path, kind, bytes) in artifacts {
                fs::write(package.join(path), &bytes).unwrap();
                if kind == "executable" {
                    fs::set_permissions(package.join(path), fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                files.push(json!({"path":path,"kind":kind,"sha256":digest(&bytes)}));
            }
            if language == "node" {
                for dependency in [
                    "@deepseek-ai/cordis",
                    "@deepseek-ai/cosmokit",
                    "@standard-schema/spec",
                ] {
                    dependencies(
                        &ts.join("node_modules").join(dependency),
                        &package.join("node_modules").join(dependency),
                        &package,
                        &mut files,
                        &mut environment,
                    );
                }
                let sdk = package.join("node_modules/@rutis/protocol");
                dependencies(&compiled, &sdk, &package, &mut files, &mut environment);
                let bytes = br#"{"name":"@rutis/protocol","version":"0.1.0","type":"module"}"#;
                fs::write(sdk.join("package.json"), bytes).unwrap();
                let path = "node_modules/@rutis/protocol/package.json";
                files.push(json!({"path":path,"kind":"dependency","sha256":digest(bytes)}));
                environment.push(path.into());
            }
            let manifest = json!({"id":name,"version":"1.0.0","protocol_family":"rutis-cordis-objects","protocol_version":"0.experimental",
                "runtime":{"kind":if language=="rust"{"rust-rutis"}else{"node-cordis"},"framework_version":if language=="rust"{"0.3.0"}else{"4.0.1"},"executable":"runtime","runner":if language=="node"{json!("runner.mjs")}else{Value::Null},"environment":environment,"capabilities":["object.scope","callback.borrow"]},
                "plugin":if language=="rust"{json!({"kind":"rust","factory":role})}else{json!({"kind":"node","entry":"entry.mjs"})},
                "files":files,"config_schema":"config.json","provides":if role=="provider"{service.clone()}else{json!({})},"requires":if role=="consumer"{service.clone()}else{json!({})},"events":{}});
            fs::write(
                package.join(MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            deployment["packages"][&name] = json!(format!("{name}/1.0.0"));
            let other = if language == "rust" { "node" } else { "rust" };
            deployment["instances"][&name] = json!({"package":name,"group":language,"config":{"label":name},"routes":if role=="provider"{json!({})}else{json!({"rpc":{"kind":"instance","instance":format!("{other}-provider"),"service":"rpc"}})},"exports":if role=="provider"{json!(["rpc"])}else{json!([])},"events":{}});
        }
    }
    for language in ["rust", "node"] {
        let other = if language == "rust" { "node" } else { "rust" };
        let name = format!("{language}-cancel");
        deployment["instances"][&name] = json!({"package":format!("{language}-consumer"),"group":language,"config":{"label":name},"routes":{"rpc":{"kind":"instance","instance":format!("{other}-provider"),"service":"rpc"}},"exports":[],"events":{}});
    }
    PreparedDeployment::prepare(&root, &serde_json::to_vec(&deployment).unwrap()).unwrap()
}
