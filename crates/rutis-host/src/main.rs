//! rutis-host: a rutis host that needs no Rust.
//!
//! ```text
//! rutis-host run [rutis.json]      run what the file names
//! rutis-host dev                   run the plugin project here, reloading it as it changes
//! rutis-host check [rutis.json]    describe every row, and fail on what cannot run
//! rutis-host new <name> --lang node|python|go
//! rutis-host go add <module>@<version> [rutis.json]
//! ```

mod config;
mod host;
mod new;
mod project;
mod status;

/// The plugin API this host supports; plugins needing more cannot run here.
const PLUGIN_API: u32 = 1;

const USAGE: &str = "\
rutis-host: run rutis plugins written in TypeScript, JavaScript, Python and Go

usage:
  rutis-host run [rutis.json]                 run what the configuration names
  rutis-host dev [dir]                        run the plugin project in dir (default .),
                                              reloading (Go: rebuilding) it as its files change
  rutis-host check [rutis.json]               describe every row and Go binary; fail on what
                                              cannot run (in a plugin project without
                                              rutis.json: the project)
  rutis-host new <name> --lang node|python|go create a plugin project
  rutis-host go add <module>@<version> [rutis.json]
                                              install a Go plugin binary into runtimes.go.dir
                                              (go install; needs the Go toolchain)
  rutis-host --version

Credentials for links come from RUTIS_TOKEN (or RUTIS_TOKEN_<PEER>), RUTIS_CA,
RUTIS_CERT and RUTIS_KEY.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("new") => new_project(&args[1..]),
        Some("go") => go_command(&args[1..]),
        Some("--version" | "-V") => {
            println!("rutis-host {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(command @ ("run" | "dev" | "check")) => serve(command, &args[1..]),
        Some(other) => Err(format!("unknown command {other}\n\n{USAGE}")),
    };
    if let Err(error) = result {
        eprintln!("rutis-host: {error}");
        std::process::exit(1);
    }
}

/// On macOS, a downloaded binary carries the quarantine attribute, and the
/// system refuses to run it unsigned: say so, and what to do.
fn quarantine_hint(path: &std::path::Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let quarantined = std::process::Command::new("xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(path)
        .output()
        .ok()?
        .status
        .success();
    quarantined.then(|| {
        format!(
            "it was downloaded and is quarantined: once you trust it, run `xattr -d com.apple.quarantine {}`",
            path.display()
        )
    })
}

fn new_project(args: &[String]) -> Result<(), String> {
    let mut name = None;
    let mut lang = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lang" => lang = args.next().cloned(),
            other if other.starts_with("--lang=") => {
                lang = Some(other["--lang=".len()..].to_owned())
            }
            other if name.is_none() => name = Some(other.to_owned()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let name = name.ok_or("usage: rutis-host new <name> --lang node|python|go")?;
    let lang = lang.ok_or("which language? --lang node, --lang python or --lang go")?;
    new::create(std::path::Path::new("."), &name, &lang)?;
    let next = match lang.as_str() {
        "python" | "py" => {
            "uv sync && uv run python -m unittest discover -s tests && uv run rutis-host dev"
        }
        "go" | "golang" => "go mod tidy && go test ./... && rutis-host dev",
        _ => "npm install && npm test && npx rutis-host dev",
    };
    println!("created {name}/\nnext: cd {name} && {next}");
    Ok(())
}

fn serve(command: &str, args: &[String]) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        match command {
            "run" => run(args).await,
            "dev" => dev(args).await,
            _ => check(args).await,
        }
    })
}

fn config_file(args: &[String]) -> std::path::PathBuf {
    std::path::PathBuf::from(args.first().map(String::as_str).unwrap_or("rutis.json"))
}

async fn run(args: &[String]) -> Result<(), String> {
    let config = config::HostConfig::read(&config_file(args))?;
    let host = host::Host::start(&config).await?;
    host.runtimes_ready().await?;
    host.load(config.rows()).await?;
    status::follow(host.loader.clone());
    // Until Ctrl-C (or a signal) ends the process; the runtimes it started
    // end with their channels.
    std::future::pending::<()>().await;
    Ok(())
}

/// `rutis-host go add <module>@<version> [rutis.json]`: `go install` the
/// binary into the configuration's Go plugin directory.
fn go_command(args: &[String]) -> Result<(), String> {
    let usage = "usage: rutis-host go add <module>@<version> [rutis.json]";
    let (Some("add"), Some(module)) = (args.first().map(String::as_str), args.get(1)) else {
        return Err(usage.into());
    };
    if !module.contains('@') {
        return Err(format!("{module}: name a version, as {module}@latest"));
    }
    let config = config::HostConfig::read(&config_file(&args[2..]))?;
    let dir = config
        .runtimes
        .go
        .and_then(|go| go.dir)
        .ok_or("the configuration has no runtimes.go.dir to install into")?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let status = std::process::Command::new("go")
        .args(["install", module.as_str()])
        .env("GOBIN", &dir)
        .status()
        .map_err(|error| {
            format!(
                "cannot run go ({error}): install the Go toolchain, or download the plugin's binary \
                 for this platform into {} yourself",
                dir.display()
            )
        })?;
    if !status.success() {
        return Err(format!("go install {module} failed"));
    }
    println!("installed {module} into {}", dir.display());
    Ok(())
}

async fn dev(args: &[String]) -> Result<(), String> {
    use std::time::Duration;

    let dir = std::path::PathBuf::from(args.first().map(String::as_str).unwrap_or("."));
    // Go runtimes warn of calls that carry no call chain.
    std::env::set_var("RUTIS_DEV", "1");
    let (config, id) = project::dev_config(&dir)?;
    let go_project = match (
        dir.join("package.json").exists(),
        dir.join("pyproject.toml").exists(),
    ) {
        (false, false) => project::GoDev::of(&std::path::absolute(&dir).unwrap_or(dir.clone())),
        _ => None,
    };
    let mut build = 1;
    let host = host::Host::start(&config).await?;
    host.runtimes_ready().await?;
    host.load(config.rows()).await?;
    println!("rutis-host dev: running {id}; changes reload it (Ctrl-C ends)");
    status::follow(host.loader.clone());
    let mut seen = project::sources(&dir);
    loop {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let now = project::sources(&dir);
        if now == seen {
            continue;
        }
        seen = now;
        if let (Some(go), Some(running)) = (&go_project, &host.go) {
            // A Go plugin is a binary: build it again, then restart its
            // runtime on the new build. A failed build keeps the old one.
            match go.build(build + 1) {
                Ok(binary) => {
                    build += 1;
                    running.resolver.replace(&go.runtime, &binary);
                    match running.runtimes.restart(&go.runtime).await {
                        Ok(()) => println!("{}: rebuilt and restarted", go.runtime),
                        Err(error) => println!("{}: cannot restart: {error}", go.runtime),
                    }
                    let previous = go.dir.join(".rutis/go").join(match cfg!(windows) {
                        true => format!("{}-{}.exe", go.name, build - 1),
                        false => format!("{}-{}", go.name, build - 1),
                    });
                    let _ = std::fs::remove_file(previous);
                }
                Err(errors) => println!(
                    "{}: the build failed; the last build keeps running\n{errors}",
                    go.name
                ),
            }
            continue;
        }
        // Plugin rows load their new code; peer rows keep their sessions.
        host.invalidate();
        let rows: Vec<String> = host
            .loader
            .entries()
            .into_iter()
            .filter(|entry| entry.options["name"] != "rutis-bridge/peer")
            .map(|entry| entry.id)
            .collect();
        for row in rows {
            match host.loader.reload(&row).await {
                Ok(_) => println!("{row}: reloaded"),
                Err(error) => println!("{row}: cannot reload: {error}"),
            }
        }
    }
}

async fn check(args: &[String]) -> Result<(), String> {
    let path = config_file(args);
    let config = match (args.first(), path.exists()) {
        (None, false) => project::dev_config(std::path::Path::new("."))?.0,
        _ => config::HostConfig::read(&path)?,
    };
    let host = host::Host::start(&config).await?;
    host.runtimes_ready().await?;
    println!("plugin API: {PLUGIN_API} (supported by this host)");
    let mut failed = 0;
    for row in config.rows() {
        let id = row["id"].as_str().unwrap_or("?");
        let name = row["name"].as_str().unwrap_or("");
        match host.loader.resolve(name).await {
            Ok(resolved) => {
                println!("{id} ({name}): ok");
                let meta = &resolved.meta;
                if let Some(api) = meta.get("api").and_then(|api| api.as_u64()) {
                    let compatible = if api <= PLUGIN_API as u64 {
                        "compatible"
                    } else {
                        "incompatible: the plugin needs a newer runtime"
                    };
                    println!("  api: {api} ({compatible})");
                }
                for field in ["version", "inject", "provides"] {
                    if let Some(value) = meta.get(field).filter(|value| !value.is_null()) {
                        println!("  {field}: {value}");
                    }
                }
                if let Some(schema) = &resolved.schema {
                    println!("  config: {schema}");
                }
            }
            Err(error) => {
                failed += 1;
                println!("{id} ({name}): {error}");
            }
        }
    }
    if let Some(go) = &host.go {
        println!("go binaries:");
        for binary in go.resolver.binaries() {
            match &binary.manifest {
                Ok(manifest) => {
                    let plugins: Vec<String> = manifest
                        .plugins
                        .iter()
                        .map(|(name, plugin)| {
                            match plugin.version.as_ref().and_then(|v| v.as_str()) {
                                Some(version) => format!("{name} {version}"),
                                None => name.clone(),
                            }
                        })
                        .collect();
                    let api = match manifest.plugin_api <= PLUGIN_API as u64 {
                        true => "compatible",
                        false => "incompatible: upgrade the host",
                    };
                    println!(
                        "  {} ({}): sdk {}, plugin API {} ({api}), plugins: {}",
                        binary.runtime,
                        binary.path.display(),
                        manifest.sdk,
                        manifest.plugin_api,
                        plugins.join(", ")
                    );
                    if manifest.plugin_api > PLUGIN_API as u64 {
                        failed += 1;
                    }
                }
                Err(error) => {
                    failed += 1;
                    println!("  {} ({}): {error}", binary.runtime, binary.path.display());
                    if let Some(hint) = quarantine_hint(&binary.path) {
                        println!("    {hint}");
                    }
                }
            }
        }
        for skipped in go.resolver.diagnostics() {
            failed += 1;
            println!("  skipped {skipped}");
        }
    }
    match failed {
        0 => Ok(()),
        n => Err(format!("{n} row(s) or binaries cannot run")),
    }
}
