//! OS process completion, independent of native fiber cleanup. The dedicated
//! subreaper owns only one frozen runtime and its descendants. No process-wide
//! subreaper or waitpid(-1) is installed in the multithreaded Host.
use crate::{
    error::{ErrorCode, ProtocolError, Result},
    frame::{Peer, WeakPeer},
    snapshot::SnapshotGroup,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, ChildStdout},
    sync::{oneshot, watch},
};
use tokio_util::sync::CancellationToken;

fn fail(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Unavailable, "process", message)
}

/// Trusted Host launch options, never plugin configuration. The executable and
/// default Node catalog always come from the frozen snapshot.
#[derive(Clone)]
pub struct LaunchOptions {
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    /// A disconnected peer gets this independent OS grace period. It never
    /// waits for native effects; explicit terminate bypasses the grace period.
    pub disconnect_grace: Duration,
}
impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            disconnect_grace: Duration::from_millis(200),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}
impl From<std::process::ExitStatus> for ProcessExit {
    fn from(status: std::process::ExitStatus) -> Self {
        Self {
            code: status.code(),
            signal: status.signal(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessPhase {
    Starting,
    Running,
    Reaping,
    Reaped,
    FailedToLaunch,
    Quarantined,
}
#[derive(Clone, Debug)]
pub struct ProcessStatus {
    pub phase: ProcessPhase,
    pub guardian_pid: u32,
    pub runtime_pid: Option<u32>,
    pub remaining: Vec<u32>,
    pub exit: Option<ProcessExit>,
    pub error: Option<ProtocolError>,
}
/// Constructed only after the guardian reported ECHILD and the Host waited for
/// the guardian's successful exit. This proves OS reaping, not native cleanup.
#[derive(Clone, Debug)]
pub struct Reaped {
    guardian_pid: u32,
    runtime_pid: u32,
    runtime_exit: ProcessExit,
    descendants: u64,
}
impl Reaped {
    pub fn guardian_pid(&self) -> u32 {
        self.guardian_pid
    }
    pub fn runtime_pid(&self) -> u32 {
        self.runtime_pid
    }
    pub fn runtime_exit(&self) -> &ProcessExit {
        &self.runtime_exit
    }
    pub fn descendants(&self) -> u64 {
        self.descendants
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    program: PathBuf,
    arguments: Vec<PathBuf>,
    environment: BTreeMap<String, String>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Launched {
        pid: u32,
    },
    SpawnFailed {
        message: String,
    },
    Exited {
        status: ProcessExit,
    },
    Reaping {
        remaining: Vec<u32>,
    },
    Reaped {
        status: ProcessExit,
        descendants: u64,
    },
}
#[derive(Default)]
struct ExitSignal {
    closed: Mutex<(bool, Vec<WeakPeer>)>,
}
impl ExitSignal {
    fn attach(&self, peer: &Peer) {
        let closed = {
            let mut state = self.closed.lock().unwrap();
            if !state.0 {
                state.1.push(peer.downgrade());
            }
            state.0
        };
        if closed {
            peer.close(fail("runtime process exited"));
        }
    }
    fn close(&self) {
        let peers = {
            let mut state = self.closed.lock().unwrap();
            if state.0 {
                return;
            }
            state.0 = true;
            std::mem::take(&mut state.1)
        };
        for peer in peers {
            if let Some(peer) = peer.upgrade() {
                peer.close(fail("runtime process revoked"));
            }
        }
    }
}
struct Lease {
    stop: CancellationToken,
    disconnected: CancellationToken,
    exit: Arc<ExitSignal>,
    status: watch::Receiver<ProcessStatus>,
    receipt: watch::Receiver<Option<Result<Reaped>>>,
    hooks: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.exit.close();
        self.stop.cancel();
    }
}
#[derive(Clone)]
pub struct ProcessHandle(Arc<Lease>);
#[derive(Clone)]
pub struct Reaping(watch::Receiver<Option<Result<Reaped>>>);
impl Reaping {
    pub async fn wait(&self) -> Result<Reaped> {
        let mut receipt = self.0.clone();
        loop {
            if let Some(result) = receipt.borrow().clone() {
                return result;
            }
            receipt
                .changed()
                .await
                .map_err(|_| fail("reaping confirmation lost"))?;
        }
    }
}
impl ProcessHandle {
    pub fn status(&self) -> ProcessStatus {
        self.0.status.borrow().clone()
    }
    /// Closing this private session revokes authority synchronously, then OS
    /// reaping runs independently. The lease must outlive this hook registration.
    pub fn attach(&self, peer: &Peer) {
        self.0.exit.attach(peer);
        let token = self.0.disconnected.clone();
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || token.cancel());
        peer.on_close(&hook);
        self.0.hooks.lock().unwrap().push(hook);
    }
    pub fn terminate(&self) {
        self.0.exit.close();
        self.0.stop.cancel();
    }
    pub async fn reaped(&self) -> Result<Reaped> {
        self.reaping().wait().await
    }
    /// A waiter owns the cached receipt, not a process lease. Dropping it never
    /// interrupts the independent monitor; retaining it does not prevent stop.
    pub fn reaping(&self) -> Reaping {
        Reaping(self.0.receipt.clone())
    }
}
pub struct FrozenProcess {
    pub handle: ProcessHandle,
    pub stream: tokio::net::UnixStream,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}
/// A failed guardian cannot prove descendant reaping. Retain those snapshots
/// for this Host lifetime; do not silently delete paths or permit recovery.
/// A future epoch supervisor must also own the native consumer cleanup barrier.
static QUARANTINE: OnceLock<Mutex<Vec<SnapshotGroup>>> = OnceLock::new();
struct ReapingLease(Option<SnapshotGroup>);
impl ReapingLease {
    fn confirmed(&mut self) {
        self.0.take();
    }
}
struct Completion {
    status: watch::Sender<ProcessStatus>,
    receipt: watch::Sender<Option<Result<Reaped>>>,
    exit: Arc<ExitSignal>,
    complete: bool,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        let error = fail("process monitor terminated without reaping proof");
        self.exit.close();
        self.status.send_modify(|state| {
            state.phase = ProcessPhase::Quarantined;
            state.error = Some(error.clone());
        });
        self.receipt.send_replace(Some(Err(error)));
    }
}
impl Drop for ReapingLease {
    fn drop(&mut self) {
        if let Some(group) = self.0.take() {
            QUARANTINE
                .get_or_init(Mutex::default)
                .lock()
                .unwrap()
                .push(group);
        }
    }
}
pub fn quarantined_snapshots() -> Vec<PathBuf> {
    QUARANTINE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap()
        .iter()
        .map(|group| group.snapshot_root().to_owned())
        .collect()
}

impl FrozenProcess {
    pub async fn launch(
        reaper: &Path,
        group: SnapshotGroup,
        options: LaunchOptions,
    ) -> Result<Self> {
        let mut argv = group.argv();
        if let Some(catalog) = group.node_catalog() {
            argv.push(catalog.to_owned());
        }
        argv.extend(options.arguments.iter().map(PathBuf::from));
        let launch = Launch {
            program: argv.remove(0),
            arguments: argv,
            environment: options.environment,
        };
        let descriptor = serde_json::to_vec(&launch).map_err(|e| fail(e.to_string()))?;
        if descriptor.len() > crate::json::MAX_JSON_BYTES {
            return Err(fail("launch descriptor exceeds frame limit"));
        }
        let (stream, child_stream) = UnixStream::pair().map_err(|e| fail(e.to_string()))?;
        let (monitor, child_monitor) = UnixStream::pair().map_err(|e| fail(e.to_string()))?;
        // Keep both sources above the reserved targets; dup2(3/4) must not
        // overwrite the other source. OwnedFd retains them through fork/exec.
        let ipc = duplicate(&child_stream)?;
        let control = duplicate(&child_monitor)?;
        let ipc_fd = ipc.as_raw_fd();
        let control_fd = control.as_raw_fd();
        let mut command = tokio::process::Command::new(reaper);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.as_std_mut().process_group(0);
        unsafe {
            command.pre_exec(move || {
                for (source, target) in [(ipc_fd, 3), (control_fd, 4)] {
                    if libc::dup2(source, target) < 0 || libc::fcntl(target, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|e| fail(format!("reaper exec failed: {e}")))?;
        let guardian_pid = child.id().ok_or_else(|| fail("reaper has no PID"))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        drop((ipc, control, child_stream, child_monitor));
        monitor
            .set_nonblocking(true)
            .map_err(|e| fail(e.to_string()))?;
        stream
            .set_nonblocking(true)
            .map_err(|e| fail(e.to_string()))?;
        let monitor = tokio::net::UnixStream::from_std(monitor).map_err(|e| fail(e.to_string()))?;
        let stream = tokio::net::UnixStream::from_std(stream).map_err(|e| fail(e.to_string()))?;
        let initial = ProcessStatus {
            phase: ProcessPhase::Starting,
            guardian_pid,
            runtime_pid: None,
            remaining: Vec::new(),
            exit: None,
            error: None,
        };
        let (status, status_rx) = watch::channel(initial);
        let (receipt, receipt_rx) = watch::channel(None);
        let (started, startup) = oneshot::channel();
        let stop = CancellationToken::new();
        let disconnected = CancellationToken::new();
        let exit = Arc::new(ExitSignal::default());
        let handle = ProcessHandle(Arc::new(Lease {
            stop: stop.clone(),
            disconnected: disconnected.clone(),
            exit: exit.clone(),
            status: status_rx,
            receipt: receipt_rx,
            hooks: Mutex::new(Vec::new()),
        }));
        tokio::spawn(async move {
            // Task cancellation/panic is also unconfirmed. The snapshot pin
            // moves to quarantine even when this async body cannot report it.
            let mut snapshot = ReapingLease(Some(group));
            let mut completion = Completion {
                status: status.clone(),
                receipt: receipt.clone(),
                exit: exit.clone(),
                complete: false,
            };
            let mut worker = Monitor {
                child,
                monitor,
                status,
                started: Some(started),
                stop,
                disconnected,
                exit,
                grace: options.disconnect_grace,
                spawn_failed: false,
            };
            let result = worker.run(descriptor).await;
            if let Some(started) = worker.started.take() {
                let _ = started.send(result.as_ref().map(|_| ()).map_err(Clone::clone));
            }
            worker.exit.close();
            if let Err(error) = &result {
                worker.status.send_modify(|state| {
                    state.phase = if worker.spawn_failed {
                        ProcessPhase::FailedToLaunch
                    } else {
                        ProcessPhase::Quarantined
                    };
                    state.error = Some(error.clone());
                });
                let _ = worker.monitor.try_write(b"K");
                // The helper must also stop writing evidence if this monitor
                // cannot consume it. A write-only half-close could leave it
                // blocked on a full event socket while we wait for its exit.
                unsafe {
                    libc::shutdown(worker.monitor.as_raw_fd(), libc::SHUT_RDWR);
                }
            }
            if result.is_ok() || worker.spawn_failed {
                snapshot.confirmed();
            }
            receipt.send_replace(Some(result));
            completion.complete = true;
            if !worker.spawn_failed {
                let _ = worker.child.wait().await;
            }
        });
        let process = Self {
            handle,
            stream,
            stdout,
            stderr,
        };
        startup.await.map_err(|_| fail("reaper startup lost"))??;
        Ok(process)
    }
}
struct Monitor {
    child: tokio::process::Child,
    monitor: tokio::net::UnixStream,
    status: watch::Sender<ProcessStatus>,
    started: Option<oneshot::Sender<Result<()>>>,
    stop: CancellationToken,
    disconnected: CancellationToken,
    exit: Arc<ExitSignal>,
    grace: Duration,
    spawn_failed: bool,
}
impl Monitor {
    async fn run(&mut self, bytes: Vec<u8>) -> Result<Reaped> {
        let cancelled = tokio::select! {
            result = async {
                self.monitor.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
                self.monitor.write_all(&bytes).await
            } => { result.map_err(|e| fail(e.to_string()))?; false }
            _ = self.stop.cancelled() => true,
        };
        // Preserve framing: EOF means either an incomplete descriptor with no
        // spawn, or an already launched runtime that the helper must reap.
        if cancelled {
            self.monitor
                .shutdown()
                .await
                .map_err(|e| fail(e.to_string()))?;
        }
        let (mut read, mut write) = self.monitor.split();
        let mut forced = cancelled;
        let mut disconnected = false;
        let mut deadline = None;
        loop {
            // Keep this read future alive when a stop arrives mid-frame.
            let mut next = Box::pin(read_event(&mut read));
            let event = loop {
                tokio::select! {
                    event = &mut next => break event?,
                    _ = self.stop.cancelled(), if !forced => {
                        write.write_all(b"K").await.map_err(|e| fail(e.to_string()))?;
                        forced = true;
                        self.status.send_modify(|state| state.phase = ProcessPhase::Reaping);
                    }
                    _ = self.disconnected.cancelled(), if !disconnected => {
                        disconnected = true;
                        deadline = Some(tokio::time::Instant::now() + self.grace);
                    }
                    _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await } else { std::future::pending().await } }, if !forced => {
                        write.write_all(b"K").await.map_err(|e| fail(e.to_string()))?;
                        forced = true;
                        self.status.send_modify(|state| state.phase = ProcessPhase::Reaping);
                    }
                }
            };
            match event {
                Event::Launched { pid } => {
                    if pid == 0 || self.status.borrow().runtime_pid.is_some() {
                        return Err(fail("invalid reaper launch confirmation"));
                    }
                    self.status.send_modify(|state| {
                        state.runtime_pid = Some(pid);
                        state.phase = if forced {
                            ProcessPhase::Reaping
                        } else {
                            ProcessPhase::Running
                        };
                    });
                    if let Some(started) = self.started.take() {
                        let _ = started.send(Ok(()));
                    }
                }
                Event::SpawnFailed { message } => {
                    let status = self.child.wait().await.map_err(|e| fail(e.to_string()))?;
                    self.spawn_failed =
                        status.success() && self.status.borrow().runtime_pid.is_none();
                    return Err(fail(format!("frozen runtime exec failed: {message}")));
                }
                Event::Exited { status } => {
                    if self.status.borrow().runtime_pid.is_none() {
                        return Err(fail("exit before runtime launch"));
                    }
                    self.exit.close();
                    self.status.send_modify(|state| {
                        state.exit = Some(status);
                        state.phase = ProcessPhase::Reaping;
                    });
                }
                Event::Reaping { remaining } => self.status.send_modify(|state| {
                    state.remaining = remaining;
                    state.phase = ProcessPhase::Reaping;
                }),
                Event::Reaped {
                    status,
                    descendants,
                } => {
                    let state = self.status.borrow().clone();
                    let runtime_pid = state
                        .runtime_pid
                        .ok_or_else(|| fail("reaping before runtime launch"))?;
                    if state.exit.as_ref() != Some(&status) {
                        return Err(fail("reaping status changed runtime exit"));
                    }
                    let guardian = self.child.wait().await.map_err(|e| fail(e.to_string()))?;
                    if !guardian.success() {
                        return Err(fail("reaper failed after its completion frame"));
                    }
                    self.status.send_modify(|state| {
                        state.phase = ProcessPhase::Reaped;
                        state.remaining.clear();
                    });
                    return Ok(Reaped {
                        guardian_pid: state.guardian_pid,
                        runtime_pid,
                        runtime_exit: status,
                        descendants,
                    });
                }
            }
        }
    }
}
async fn read_event(read: &mut (impl tokio::io::AsyncRead + Unpin)) -> Result<Event> {
    let size = read
        .read_u32()
        .await
        .map_err(|e| fail(format!("reaper channel lost: {e}")))? as usize;
    if size > crate::json::MAX_JSON_BYTES {
        return Err(fail("reaper event exceeds frame limit"));
    }
    let mut bytes = vec![0; size];
    read.read_exact(&mut bytes)
        .await
        .map_err(|e| fail(e.to_string()))?;
    serde_json::from_value(crate::json::decode(&bytes)?).map_err(|e| fail(e.to_string()))
}
fn duplicate(stream: &UnixStream) -> Result<OwnedFd> {
    let fd = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if fd < 0 {
        return Err(fail(std::io::Error::last_os_error().to_string()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Entry for the SDK-owned helper binary. It must execute in a dedicated,
/// single-threaded process; never call this inside the Host.
pub fn run_reaper() -> Result<()> {
    for fd in [3, 4] {
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(fail("reaper requires private Unix fds 3 and 4"));
        }
    }
    let ipc = unsafe { OwnedFd::from_raw_fd(3) };
    let mut monitor = unsafe { UnixStream::from_raw_fd(4) };
    if unsafe { libc::fcntl(4, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(fail(std::io::Error::last_os_error().to_string()));
    }
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } < 0 {
        return Err(fail(std::io::Error::last_os_error().to_string()));
    }
    let launch = match read_launch(&mut monitor) {
        Ok(launch) => launch,
        Err(error) => {
            send_event(
                &mut monitor,
                &Event::SpawnFailed {
                    message: error.to_string(),
                },
            )?;
            return Ok(());
        }
    };
    let parent = unsafe { libc::getpid() };
    let mut command = Command::new(&launch.program);
    command
        .args(launch.arguments)
        .envs(launch.environment)
        .stdin(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    let mut attempts = 0;
    let child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && attempts < 8 => {
                std::thread::sleep(Duration::from_millis(1 << attempts));
                attempts += 1;
            }
            Err(error) => {
                send_event(
                    &mut monitor,
                    &Event::SpawnFailed {
                        message: error.to_string(),
                    },
                )?;
                return Ok(());
            }
        }
    };
    let runtime = child.id() as libc::pid_t;
    // waitpid below is the sole reaper in this dedicated single-threaded process.
    drop(child);
    drop(ipc);
    let mut connected = send_event(
        &mut monitor,
        &Event::Launched {
            pid: runtime as u32,
        },
    )
    .is_ok();
    let mut terminating = !connected;
    let mut exit = None;
    let mut descendants = 0;
    let children_path = format!("/proc/self/task/{parent}/children");
    let mut last_remaining = Vec::new();
    loop {
        let empty = loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid > 0 {
                if pid == runtime {
                    let status = ProcessExit::from(std::process::ExitStatus::from_raw(status));
                    if connected {
                        connected = send_event(
                            &mut monitor,
                            &Event::Exited {
                                status: status.clone(),
                            },
                        )
                        .is_ok();
                    }
                    exit = Some(status);
                    terminating = true;
                } else {
                    descendants += 1;
                }
                continue;
            }
            if pid == 0 {
                break false;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::ECHILD) => break true,
                _ => return Err(fail(std::io::Error::last_os_error().to_string())),
            }
        };
        if empty {
            let status = exit.ok_or_else(|| fail("no runtime exit status"))?;
            if connected {
                send_event(
                    &mut monitor,
                    &Event::Reaped {
                        status,
                        descendants,
                    },
                )?;
            }
            return Ok(());
        }
        let children = std::fs::read_to_string(&children_path)
            .map_err(|e| fail(e.to_string()))?
            .split_whitespace()
            .map(|pid| pid.parse::<u32>().map_err(|e| fail(e.to_string())))
            .collect::<Result<Vec<_>>>()?;
        if terminating {
            // No competing waiter exists. These are our unreaped direct
            // children, so their PIDs cannot be reused while we signal them.
            for pid in &children {
                if unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL) } < 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(fail(std::io::Error::last_os_error().to_string()));
                }
            }
            if connected && children != last_remaining {
                connected = send_event(
                    &mut monitor,
                    &Event::Reaping {
                        remaining: children.clone(),
                    },
                )
                .is_ok();
            }
            last_remaining = children;
        }
        if connected {
            let mut poll = libc::pollfd {
                fd: monitor.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 10) };
            if ready < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return Err(fail(std::io::Error::last_os_error().to_string()));
            }
            if ready > 0 {
                let mut command = [0];
                match monitor.read(&mut command) {
                    Ok(1) if command[0] == b'K' => terminating = true,
                    Ok(0) | Err(_) => {
                        terminating = true;
                        connected = false;
                    }
                    _ => terminating = true,
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn send_event(stream: &mut UnixStream, event: &Event) -> Result<()> {
    let bytes = serde_json::to_vec(event).map_err(|e| fail(e.to_string()))?;
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|_| stream.write_all(&bytes))
        .map_err(|e| fail(e.to_string()))
}
fn read_launch(monitor: &mut UnixStream) -> Result<Launch> {
    let mut header = [0; 4];
    monitor
        .read_exact(&mut header)
        .map_err(|e| fail(e.to_string()))?;
    let size = u32::from_be_bytes(header) as usize;
    if size > crate::json::MAX_JSON_BYTES {
        return Err(fail("launch descriptor exceeds frame limit"));
    }
    let mut bytes = vec![0; size];
    monitor
        .read_exact(&mut bytes)
        .map_err(|e| fail(e.to_string()))?;
    serde_json::from_value(crate::json::decode(&bytes)?).map_err(|e| fail(e.to_string()))
}
