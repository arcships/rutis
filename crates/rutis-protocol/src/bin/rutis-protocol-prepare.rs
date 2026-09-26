//! Read-only deployment preparation. No runtime or plugin code is launched.
use rutis_protocol::prepare::PreparedDeployment;
use serde_json::json;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or("usage: rutis-protocol-prepare DEPLOYMENT.json")?;
    if args.next().is_some() {
        return Err("usage: rutis-protocol-prepare DEPLOYMENT.json".into());
    }
    let path = std::fs::canonicalize(path)?;
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() || metadata.len() > rutis_protocol::json::MAX_JSON_BYTES as u64 {
        return Err("deployment must be a bounded regular JSON file".into());
    }
    let prepared = PreparedDeployment::prepare(
        path.parent().ok_or("deployment directory missing")?,
        &std::fs::read(&path)?,
    )?;
    let groups: serde_json::Map<String, serde_json::Value> = prepared
        .groups()
        .iter()
        .map(|(name, group)| {
            (
                name.clone(),
                json!({
                    "kind": group.kind(),
                    "trust": group.trust(),
                    "framework_version": group.framework_version(),
                    "capabilities": group.capabilities(),
                    "image_sha256": group.executable().sha256(),
                    "environment_sha256": group.environment_sha256(),
                    "code_sha256": group.code_sha256(),
                    "members": group.members()
                }),
            )
        })
        .collect();
    let instances: serde_json::Map<String, serde_json::Value> = prepared
        .instances()
        .iter()
        .map(|(name, member)| {
            let routes: serde_json::Map<String, serde_json::Value> = member
                .routes()
                .iter()
                .map(|(name, route)| {
                    (
                        name.clone(),
                        json!({
                            "provider": route.provider(),
                            "interface": route.contract().interface,
                            "version": route.contract().version,
                            "bundle_sha256": route.contract().bundle_sha256,
                            "source": route.source()
                        }),
                    )
                })
                .collect();
            (
                name.clone(),
                json!({
                    "package": member.package().manifest().id,
                    "version": member.package().manifest().version,
                    "group": member.group(),
                    "missing_routes": member.missing_routes().collect::<Vec<_>>(),
                    "routes": routes,
                    "exports": member.exports(),
                    "events": member.events()
                }),
            )
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"id":prepared.id(), "sha256":prepared.sha256(), "groups":groups, "instances":instances, "native_services":prepared.native_services(), "dependency_order":prepared.dependency_order()})
        )?
    );
    Ok(())
}
