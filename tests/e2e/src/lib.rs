//! Black-box end-to-end scenarios for rutis (issue #175).
//!
//! A scenario is a Rust test: it writes a project directory, starts the
//! `rutis-host` program on it, waits for what the host prints, calls
//! services through probe plugins, stops the host, and ends with the residue
//! checks of quality standard Q5.4.2 ([`Scenario::finish`]). It uses only
//! what a user has (Q7.6): the program, the packages, the files.
//!
//! ```no_run
//! use rutis_e2e::{Lang, Scenario};
//! use serde_json::json;
//!
//! let s = Scenario::new("greeter");
//! s.link_node_sdk();
//! s.write("greeter.ts", "…");
//! let probe = s.probe("probe", Lang::Node, &["greeter"]);
//! s.write_json("rutis.json", &json!({
//!     "runtimes": { "node": {} },
//!     "rows": [{ "id": "greeter", "name": "./greeter.ts" }, probe.row()],
//! }));
//! let mut host = s.host(["run".as_ref(), s.path("rutis.json").as_os_str()]);
//! probe.started(&mut host);
//! assert_eq!(probe.call(&mut host, "greeter", "hello", json!(["Ada"])), Ok(json!("Hello, Ada")));
//! host.kill();
//! host.wait_exit();
//! s.finish();
//! ```
//!
//! Every wait has a hang guard ([`hang_guard`]), never a sleep standing in
//! for an event. A failed scenario keeps its directory, with every host's
//! output in `logs/`, under [`root`].
//!
//! Running: `npm --prefix node/rutis-runtime ci`, a `python3` (3.12 or
//! newer; the checkout's `python/rutis` is used), then
//! `cargo test -p rutis-e2e`, which builds `rutis-host` first. Settings:
//! `RUTIS_E2E_HOST` (the program to test instead), `RUTIS_E2E_DIR` (where
//! scenario directories go), `RUTIS_E2E_TIMEOUT` (the hang guard, seconds).

mod host;
mod residue;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};

pub use host::{Host, Line, Stream};
pub use residue::Residue;

/// How long any wait lasts before the scenario fails as hung: 30 s, or
/// `RUTIS_E2E_TIMEOUT` seconds. It only guards against hangs (Q7.1).
pub fn hang_guard() -> Duration {
    let seconds = std::env::var("RUTIS_E2E_TIMEOUT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30);
    Duration::from_secs(seconds)
}

/// The repository this crate is in.
pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository")
}

/// Where scenario directories are made, and where a failed one stays:
/// `RUTIS_E2E_DIR`, else `/tmp/rutis-e2e` (short, for Unix socket paths
/// under it), else on Windows `%TEMP%\rutis-e2e`.
pub fn root() -> PathBuf {
    match std::env::var_os("RUTIS_E2E_DIR") {
        Some(dir) => PathBuf::from(dir),
        None if cfg!(unix) => PathBuf::from("/tmp/rutis-e2e"),
        None => std::env::temp_dir().join("rutis-e2e"),
    }
}

/// The `rutis-host` program scenarios run: `RUTIS_E2E_HOST`, else the one
/// `cargo build -p rutis-host` builds from this checkout (once per run).
pub fn host_program() -> PathBuf {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM
        .get_or_init(|| match std::env::var_os("RUTIS_E2E_HOST") {
            Some(program) => PathBuf::from(program),
            None => build_host(),
        })
        .clone()
}

fn build_host() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let cargo = std::process::Command::new(cargo)
        .args(["build", "-p", "rutis-host", "--message-format=json"])
        .current_dir(repo())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("cargo build -p rutis-host");
    // A child of this process, but no scenario's residue.
    residue::started(cargo.id());
    let output = cargo.wait_with_output().expect("cargo build -p rutis-host");
    assert!(output.status.success(), "cargo build -p rutis-host failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["target"]["name"] == "rutis-host")
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
        .expect("cargo build reported no rutis-host executable")
}

/// The language of a probe plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// `probes/rutis-probe.ts`, run by the Node runtime.
    Node,
    /// `probes/rutis_probe.py`, run by the Python runtime.
    Python,
}

/// One scenario's directory and everything it started.
///
/// ```text
/// <root>/<name>-<pid>-<n>/
///   project/   the project: rutis.json, plugins; hosts run here
///   tmp/       TMPDIR (TMP, TEMP) of every host
///   probes/    one request directory per probe
///   logs/      on failure: each host's output, the residue found
/// ```
pub struct Scenario {
    name: String,
    dir: PathBuf,
    hosts: RefCell<Vec<host::Started>>,
    ports: RefCell<Vec<u16>>,
    secrets: RefCell<Vec<String>>,
    finished: Cell<bool>,
}

impl Scenario {
    pub fn new(name: &str) -> Self {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        residue::watch_orphans();
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let dir = root().join(format!("{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["project", "tmp", "probes"] {
            std::fs::create_dir_all(dir.join(sub)).expect("the scenario directory");
        }
        Self {
            name: name.to_owned(),
            dir,
            hosts: RefCell::new(Vec::new()),
            ports: RefCell::new(Vec::new()),
            secrets: RefCell::new(Vec::new()),
            finished: Cell::new(false),
        }
    }

    /// The scenario's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `relative` in the project.
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.dir.join("project").join(relative)
    }

    /// Write (or replace) a project file.
    pub fn write(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the file's directory");
        }
        std::fs::write(&path, contents)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }

    pub fn write_json(&self, relative: impl AsRef<Path>, value: &Value) {
        self.write(relative, serde_json::to_string_pretty(value).unwrap());
    }

    /// Change a project file: `edit` gets its text and returns the new one.
    pub fn edit(&self, relative: impl AsRef<Path>, edit: impl FnOnce(String) -> String) {
        let path = self.path(&relative);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        self.write(relative, edit(text));
    }

    /// Make the project a Node project whose `@arcships/rutis` is this
    /// checkout's `node/rutis` (linked, not installed from a registry).
    pub fn link_node_sdk(&self) {
        if !self.path("package.json").exists() {
            self.write_json(
                "package.json",
                &json!({ "name": "e2e-scenario", "private": true, "type": "module" }),
            );
        }
        let link = self.path("node_modules/@arcships/rutis");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let target = repo().join("node/rutis");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("link @arcships/rutis");
        // A junction needs no privilege, unlike a symbolic link.
        #[cfg(windows)]
        assert!(std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&target)
            .status()
            .expect("mklink")
            .success());
    }

    /// Add a probe row's plugin to the project: `<id>.ts`, or the Python
    /// module `<id>` (`-` as `_`). `inject` are the services it calls; it
    /// starts once they run. Put [`Probe::row`] in `rutis.json`.
    pub fn probe(&self, id: &str, lang: Lang, inject: &[&str]) -> Probe {
        let inject = serde_json::to_string(inject).unwrap();
        let (file, name, source) = match lang {
            Lang::Node => (
                format!("{id}.ts"),
                format!("./{id}.ts"),
                include_str!("../probes/rutis-probe.ts")
                    .replace("inject: INJECT,", &format!("inject: {inject},")),
            ),
            Lang::Python => {
                let module: String = id
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                (
                    format!("{module}.py"),
                    format!("py:{module}"),
                    include_str!("../probes/rutis_probe.py")
                        .replace("inject = INJECT", &format!("inject = {inject}")),
                )
            }
        };
        self.write(file, source);
        let dir = self.dir.join("probes").join(id);
        std::fs::create_dir_all(&dir).expect("the probe's directory");
        Probe {
            id: id.to_owned(),
            name,
            dir,
            seq: Cell::new(0),
        }
    }

    /// A free loopback port for the project to listen on; after the
    /// scenario it must be free again.
    ///
    /// The port is free when returned, not reserved: another process may
    /// take it before the host binds it (the usual bind-0-and-release race).
    /// A scenario that cannot afford that lets the host pick the port (`:0`)
    /// and reads it from the host's output instead.
    pub fn port(&self) -> u16 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("a free port")
            .port();
        self.ports.borrow_mut().push(port);
        port
    }

    /// A fresh credential (a token for `RUTIS_TOKEN`, say); after the
    /// scenario no captured output may contain it.
    pub fn secret(&self) -> String {
        use std::hash::{BuildHasher, RandomState};
        let token = format!(
            "e2e-{:016x}{:016x}",
            RandomState::new().hash_one(1),
            RandomState::new().hash_one(2)
        );
        self.secrets.borrow_mut().push(token.clone());
        token
    }

    /// Start `rutis-host <args>` in the project.
    pub fn host<I, S>(&self, args: I) -> Host
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.host_with(args, &[])
    }

    /// Start `rutis-host <args>` in the project with more environment.
    ///
    /// Every host gets the scenario's `tmp/` as its temporary directory,
    /// no inherited credentials (`RUTIS_TOKEN*`, `RUTIS_CA`, `RUTIS_CERT`,
    /// `RUTIS_KEY`), and this checkout's runtimes: `RUTIS_NODE_RUNTIME` is
    /// `node/rutis-runtime`, `RUTIS_PYTHON_PATH` is `python/rutis`.
    pub fn host_with<I, S>(&self, args: I, env: &[(&str, &str)]) -> Host
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = std::process::Command::new(host_program());
        command.args(args).current_dir(self.path(""));
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("RUTIS_TOKEN")
                || ["RUTIS_CA", "RUTIS_CERT", "RUTIS_KEY"].contains(&name.as_ref())
            {
                command.env_remove(name.as_ref());
            }
        }
        let tmp = self.dir.join("tmp");
        for name in ["TMPDIR", "TMP", "TEMP"] {
            command.env(name, &tmp);
        }
        command
            .env("RUTIS_NODE_RUNTIME", repo().join("node/rutis-runtime"))
            .env("RUTIS_PYTHON_PATH", repo().join("python/rutis"));
        command.envs(env.iter().copied());
        let name = format!("host-{}", self.hosts.borrow().len() + 1);
        let (host, started) = Host::spawn(&name, command);
        self.hosts.borrow_mut().push(started);
        host
    }

    /// Run the residue checks and return what they found, without failing.
    pub fn residue(&self) -> Residue {
        residue::check(
            &self.dir,
            &self.hosts.borrow(),
            &self.ports.borrow(),
            &self.secrets.borrow(),
        )
    }

    /// End the scenario: the residue checks (Q5.4.2) must find nothing.
    /// Stop the hosts first: a host still running is residue, and is killed
    /// with its process group (Unix) before the other checks. On success the
    /// directory is removed; otherwise it stays, with the hosts' output and
    /// the residue in `logs/`.
    pub fn finish(self) {
        self.finished.set(true);
        let residue = self.residue();
        for skipped in &residue.skipped {
            eprintln!("e2e {}: residue check skipped: {skipped}", self.name);
        }
        if !residue.found.is_empty() {
            let report = residue.report();
            let _ = std::fs::create_dir_all(self.dir.join("logs"));
            let _ = std::fs::write(self.dir.join("logs/residue.txt"), &report);
            panic!("residue after scenario {}:\n{report}", self.name);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    fn keep_logs(&self) {
        let logs = self.dir.join("logs");
        let _ = std::fs::create_dir_all(&logs);
        for started in self.hosts.borrow().iter() {
            let _ = std::fs::write(
                logs.join(format!("{}.log", started.name)),
                started.output.transcript(),
            );
        }
        eprintln!(
            "e2e {}: the scenario failed; its directory and output are in {}",
            self.name,
            self.dir.display()
        );
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.keep_logs();
        } else if !self.finished.get() {
            self.keep_logs();
            panic!(
                "scenario {} ended without finish(): every scenario ends with the residue checks",
                self.name
            );
        }
    }
}

/// A probe row: a plugin that calls the services the scenario asks for and
/// prints each result as one JSON line (`probes/`).
pub struct Probe {
    id: String,
    name: String,
    dir: PathBuf,
    seq: Cell<u64>,
}

impl Probe {
    /// The row for `rutis.json`.
    pub fn row(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "config": { "id": self.id, "dir": self.dir },
        })
    }

    /// Wait until the probe has started (its services run).
    pub fn started(&self, host: &mut Host) {
        self.event(host, "started");
    }

    /// Wait for the probe's `event` (`started`, `stopped`).
    pub fn event(&self, host: &mut Host, event: &str) {
        let what = format!("probe {} {event}", self.id);
        host.wait_for(&what, |line| {
            let record = parse(line);
            record["probe"] == self.id.as_str() && record["event"] == event
        });
    }

    /// Call `service.method(...args)` from the probe: the result, or the
    /// error the call ended with. `args` is a JSON array.
    pub fn call(
        &self,
        host: &mut Host,
        service: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, String> {
        assert!(args.is_array(), "the arguments are a JSON array");
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        let request = json!({ "service": service, "method": method, "args": args });
        // Written whole, then renamed: the probe reads only `<seq>.json`.
        let part = self.dir.join(format!("{seq}.json.part"));
        std::fs::write(&part, request.to_string()).expect("the probe request");
        std::fs::rename(&part, self.dir.join(format!("{seq}.json"))).expect("the probe request");
        let what = format!("probe {} answering {service}.{method} (#{seq})", self.id);
        let line = host.wait_for(&what, |line| {
            let record = parse(line);
            record["probe"] == self.id.as_str()
                && (record["seq"] == seq || record.get("failed").is_some())
        });
        let mut record = parse(&line);
        if let Some(failed) = record.get("failed") {
            panic!("probe {} failed while {what}: {failed}", self.id);
        }
        match record.get("error") {
            Some(error) => Err(error.as_str().unwrap_or_default().to_owned()),
            None => Ok(record["ok"].take()),
        }
    }
}

fn parse(line: &str) -> Value {
    match line.starts_with("{\"probe\"") {
        true => serde_json::from_str(line).unwrap_or(Value::Null),
        false => Value::Null,
    }
}

/// Lines of `output` that contain any of `secrets`, for scenarios that
/// check output other than the hosts' (B7).
pub fn leaked<'a>(output: impl IntoIterator<Item = &'a str>, secrets: &[String]) -> Vec<String> {
    output
        .into_iter()
        .filter(|line| secrets.iter().any(|secret| line.contains(secret.as_str())))
        .map(str::to_owned)
        .collect()
}
