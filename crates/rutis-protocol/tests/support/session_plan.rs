//! Frozen routing metadata for the real session fixture, not a frozen loader.
use rutis_protocol::prepare::{digest, PreparedDeployment, MANIFEST};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt};

pub fn prepare() -> PreparedDeployment {
    let root = std::env::temp_dir().join(format!("rutis-session-routes-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(root.clone());
    let mut deployment = json!({"id":"session-routes","packages":{},"groups":{
        "rust":{"kind":"rust-rutis","trust":"fixture"},
        "node":{"kind":"node-cordis","trust":"fixture"}},"instances":{},"native_services":{"data":{
            "interface":"Database","version":"1.0.0","bundle_sha256":digest(include_bytes!("../../../../protocol/fixtures/database.bundle.json"))}}});
    let services = json!({
        "rpc":{"interface":"Database","version":"1.0.0","bundle":"rpc.json"},
        "data":{"interface":"Database","version":"1.0.0","bundle":"data.json"},
    });
    for language in ["rust", "node"] {
        for role in ["provider", "consumer"] {
            let name = format!("{language}-{role}");
            let directory = root.join(&name).join("1.0.0");
            fs::create_dir_all(&directory).unwrap();
            let mut artifacts = vec![
                (
                    "runtime",
                    "executable",
                    fs::read(std::env::current_exe().unwrap()).unwrap(),
                ),
                (
                    "rpc.json",
                    "bundle",
                    include_bytes!("../../../../protocol/fixtures/rpc.bundle.json").to_vec(),
                ),
                (
                    "data.json",
                    "bundle",
                    include_bytes!("../../../../protocol/fixtures/database.bundle.json").to_vec(),
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
                // This plan proves frozen selection only; actual members use
                // the explicitly marked private-session management fixture.
                artifacts.push((
                    "entry.mjs",
                    "code",
                    b"throw new Error('not the frozen loader');\n".to_vec(),
                ));
                artifacts.push((
                    "runner.mjs",
                    "code",
                    b"throw new Error('not the frozen loader');\n".to_vec(),
                ));
            }
            let mut files = Vec::<Value>::new();
            for (path, kind, bytes) in artifacts {
                fs::write(directory.join(path), &bytes).unwrap();
                if kind == "executable" {
                    fs::set_permissions(directory.join(path), fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                files.push(json!({"path":path,"kind":kind,"sha256":digest(&bytes)}));
            }
            let manifest = json!({"id":name,"version":"1.0.0","protocol_family":"rutis-cordis-objects","protocol_version":"0.experimental",
                "runtime":{"kind":if language=="rust"{"rust-rutis"}else{"node-cordis"},"framework_version":if language=="rust"{"0.3.0"}else{"4.0.1"},"executable":"runtime","runner":if language=="node"{json!("runner.mjs")}else{Value::Null},"environment":["dependency.lock"],"capabilities":["object.scope","callback.borrow","event.parallel","event.serial"]},
                "plugin":if language=="rust"{json!({"kind":"rust","factory":role})}else{json!({"kind":"node","entry":"entry.mjs"})},
                "files":files,"config_schema":"config.json","provides":if role=="provider"{services.clone()}else{json!({})},"requires":if role=="consumer"{services.clone()}else{json!({})},"events":{}});
            fs::write(
                directory.join(MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            deployment["packages"][&name] = json!(format!("{name}/1.0.0"));
        }
        for suffix in ["provider", "consumer", "extra", "failed", "commit-failed"] {
            let name = format!("{language}-{suffix}");
            let provider = suffix == "provider";
            let other = if language == "rust" { "node" } else { "rust" };
            let mut routes = json!({});
            if !provider {
                for service in ["rpc", "data"] {
                    if language == "rust" && suffix == "extra" && service == "data" {
                        routes[service] = json!({"kind":"native","service":"data"});
                        continue;
                    }
                    let selected = if suffix == "extra" && service == "data" {
                        language
                    } else {
                        other
                    };
                    routes[service] = json!({"kind":"instance","instance":format!("{selected}-provider"),"service":service});
                }
            }
            deployment["instances"][&name] = json!({"package":format!("{language}-{}",if provider{"provider"}else{"consumer"}),"group":language,"config":{"label":name},"routes":routes,"exports":if provider{json!(["rpc","data"])}else{json!([])},"events":{}});
        }
    }
    PreparedDeployment::prepare(&root, &serde_json::to_vec(&deployment).unwrap()).unwrap()
}
