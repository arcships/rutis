//! How the `Process` compatibility facade starts its runtime process: on an
//! inherited socket (fd 3) or, for runtimes that cannot take one, on a Unix
//! socket in a private directory that the process dials back; on Windows
//! (or with `RUTIS_LOCAL_HANDOVER=loopback`), on a loopback address with a
//! one-time token, as the local transport does. How it ends is watched and
//! reported as the session's end.
//!
//! Runtimes that get their sessions through a link are started by the local
//! transport ([`crate::transport::local`]), which has its own spawner; this
//! copy goes with the facade.
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::channel::{Channel, ChannelError, Closer, Receiver, Sender};
use tokio::sync::{oneshot, watch};

use crate::runtime::Error;

/// The fd a runtime process finds its channel on (`fd:3`).
#[cfg(unix)]
const CHANNEL_FD: i32 = 3;

/// How the process gets its channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Connect {
    /// One end of a socket pair, as fd 3: `fd:3 <first>`.
    Inherit,
    /// A socket path the process dials: `<path> <first>`.
    DialBack,
    /// A loopback address the process dials, presenting a token first:
    /// `tcp:<host>:<port> <first>`.
    Loopback,
}

/// A started runtime process and the channel it connected.
pub(crate) struct Spawned {
    pub channel: Channel,
    pub child: Child,
    /// Holds the socket when the process dials back; removed when the
    /// process is dropped.
    pub directory: Option<tempfile::TempDir>,
}

#[cfg(unix)]
fn transport(error: std::io::Error) -> Error {
    Error::Transport(error.to_string())
}

/// Start `command` with the channel and `first` (the first plugin or the
/// anchor) as its last two arguments, and connect it. The channel ends with
/// how the process ended (`Cordis process exited …`).
pub(crate) async fn spawn(
    command: tokio::process::Command,
    first: &Path,
    connect: Connect,
) -> Result<Spawned, Error> {
    match connect {
        #[cfg(unix)]
        Connect::Inherit => inherit(command, first),
        #[cfg(unix)]
        Connect::DialBack => dial_back(command, first).await,
        _ => loopback(command, first).await,
    }
}

async fn loopback(mut command: tokio::process::Command, first: &Path) -> Result<Spawned, Error> {
    use crate::transport::local::spawn::{adopt, Loopback, CHANNEL_TOKEN};
    let transport = |error: std::io::Error| Error::Transport(error.to_string());
    let listener = Loopback::bind().await.map_err(transport)?;
    #[cfg(unix)]
    crate::transport::local::spawn::hand_over(&mut command, None);
    let mut child = command
        .env(CHANNEL_TOKEN, listener.token())
        .arg(listener.address())
        .arg(first)
        .kill_on_drop(true)
        .spawn()
        .map_err(transport)?;
    // A process that would outlive its host is not started.
    if let Err(reason) = adopt(&child) {
        let _ = child.start_kill();
        return Err(Error::Transport(format!(
            "cannot tie the runtime process to this process's lifetime: {reason}"
        )));
    }
    let channel = tokio::select! {
        channel = listener.accept() => channel.map_err(transport)?,
        status = child.wait() => return Err(Error::Transport(match status {
            Ok(status) => format!("Cordis process exited before connecting: {status}"),
            Err(error) => format!("Cordis process exited before connecting: {error}"),
        })),
    };
    let child = Child::watch(child);
    let channel = on_disconnect(channel, Box::new(child.disconnected()));
    Ok(Spawned {
        channel: traced(channel),
        child,
        directory: None,
    })
}

/// What the three ends of a watched channel share: whether this side closed
/// it, whether its receiver saw the far end go, and the reason the channel
/// ended with, once the receiver knows it.
#[derive(Default)]
struct Ending {
    state: Mutex<EndState>,
    changed: Condvar,
}

#[derive(Default)]
struct EndState {
    closed_here: bool,
    far_end_gone: bool,
    reason: Option<String>,
}

impl Ending {
    fn update(&self, change: impl FnOnce(&mut EndState)) {
        change(&mut self.state.lock().unwrap());
        self.changed.notify_all();
    }
}

/// Ends the channel, however it ends, with the error `disconnected` builds.
struct Disconnected {
    receiver: Box<dyn Receiver>,
    disconnected: Option<Box<dyn FnOnce() -> Error + Send>>,
    ending: Arc<Ending>,
}

impl Receiver for Disconnected {
    fn recv(&mut self) -> Result<Option<Vec<u8>>, ChannelError> {
        match self.receiver.recv() {
            Ok(Some(message)) => Ok(Some(message)),
            _ => Err(ChannelError::Closed {
                reason: match self.disconnected.take() {
                    Some(disconnected) => {
                        self.ending
                            .update(|state| state.far_end_gone = !state.closed_here);
                        let reason = disconnected().to_string();
                        self.ending
                            .update(|state| state.reason = Some(reason.clone()));
                        reason
                    }
                    None => "peer disconnected".into(),
                },
            }),
        }
    }
}

/// A send that fails because the far end went away fails with the reason
/// the receiver gives, not its own (#250). The receiver may be waiting for
/// the exit status when a send finds the channel ended; had the send's
/// "the channel ended" closed the session first, the exit status was lost.
struct Sending {
    sender: Box<dyn Sender>,
    ending: Arc<Ending>,
}

impl Sender for Sending {
    fn send(&mut self, message: &[u8]) -> Result<(), ChannelError> {
        let error = match self.sender.send(message) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        // Refused by the framing itself: nothing to do with the far end.
        if crate::transport::local::lines::refuses(message) {
            return Err(error);
        }
        // Otherwise the stream failed because this side closed it or the
        // far end went away; the receiver sees the latter as well, if it
        // has not yet: the framing fails sends only once its receiver saw
        // the end, and a stream the far end closed ends its reads too.
        let state = self.ending.state.lock().unwrap();
        let state = self
            .ending
            .changed
            .wait_while(state, |state| !state.closed_here && !state.far_end_gone)
            .unwrap();
        if !state.far_end_gone {
            return Err(error);
        }
        let state = self
            .ending
            .changed
            .wait_while(state, |state| state.reason.is_none())
            .unwrap();
        Err(ChannelError::Closed {
            reason: state.reason.clone().unwrap(),
        })
    }
}

/// Records that this side closed the channel, before closing it.
struct Closing {
    closer: Arc<dyn Closer>,
    ending: Arc<Ending>,
}

impl Closer for Closing {
    fn close(&self, reason: &str) {
        self.ending.update(|state| state.closed_here = true);
        self.closer.close(reason);
    }

    fn replaced(&self) {
        self.ending.update(|state| state.closed_here = true);
        self.closer.replaced();
    }
}

/// Replace how the channel reports its end: `disconnected` runs on the
/// reader thread once the far end is gone, and may block. A send that fails
/// because the far end went away fails with the same error.
pub(crate) fn on_disconnect(
    channel: Channel,
    disconnected: Box<dyn FnOnce() -> Error + Send>,
) -> Channel {
    let ending = Arc::new(Ending::default());
    Channel {
        sender: Box::new(Sending {
            sender: channel.sender,
            ending: ending.clone(),
        }),
        receiver: Box::new(Disconnected {
            receiver: channel.receiver,
            disconnected: Some(disconnected),
            ending: ending.clone(),
        }),
        closer: Arc::new(Closing {
            closer: channel.closer,
            ending,
        }),
        info: channel.info,
    }
}

#[cfg(unix)]
fn inherit(mut command: tokio::process::Command, first: &Path) -> Result<Spawned, Error> {
    let (ours, theirs) = UnixStream::pair().map_err(transport)?;
    // Only fd 3 (and stdio) crosses exec, whatever other threads opened.
    crate::transport::local::spawn::hand_over(&mut command, Some(theirs.as_raw_fd()));
    let child = command
        .arg(format!("fd:{CHANNEL_FD}"))
        .arg(first)
        .kill_on_drop(true)
        .spawn()
        .map_err(transport)?;
    // The child has its copy; ours would keep the channel open after it exits.
    drop(theirs);
    let child = Child::watch(child);
    let mut channel = crate::runtime::unix::channel(ours, "")?;
    channel.info.transport = "fd";
    let channel = on_disconnect(channel, Box::new(child.disconnected()));
    Ok(Spawned {
        channel: traced(channel),
        child,
        directory: None,
    })
}

#[cfg(unix)]
async fn dial_back(mut command: tokio::process::Command, first: &Path) -> Result<Spawned, Error> {
    let directory = tempfile::Builder::new()
        .prefix("rutis-mount-")
        .tempdir()
        .map_err(transport)?;
    let socket = directory.path().join("peer.sock");
    let listener = tokio::net::UnixListener::bind(&socket).map_err(transport)?;
    crate::transport::local::spawn::hand_over(&mut command, None);
    let mut child = command
        .arg(&socket)
        .arg(first)
        .kill_on_drop(true)
        .spawn()
        .map_err(transport)?;
    let stream = tokio::select! {
        accepted = listener.accept() => accepted.map_err(transport)?.0,
        status = child.wait() => return Err(Error::Transport(match status {
            Ok(status) => format!("Cordis process exited before connecting: {status}"),
            Err(error) => format!("Cordis process exited before connecting: {error}"),
        })),
    };
    let stream = stream.into_std().map_err(transport)?;
    let child = Child::watch(child);
    let channel = on_disconnect(
        crate::runtime::unix::channel(stream, "")?,
        Box::new(child.disconnected()),
    );
    Ok(Spawned {
        channel: traced(channel),
        child,
        directory: Some(directory),
    })
}

/// Set to report every message crossing a runtime channel (direction and
/// length, never content) on stderr.
pub(crate) const TRACE_VARIABLE: &str = "RUTIS_TRACE";

fn traced(channel: Channel) -> Channel {
    match std::env::var_os(TRACE_VARIABLE) {
        Some(_) => crate::channel::trace::trace(
            channel,
            Arc::new(|line: &str| eprintln!("rutis trace: {line}")),
        ),
        None => channel,
    }
}

/// The command a runtime process starts with: `launcher`, or the Node
/// runtime of the npm package `node_package` (feature `node`), and how it
/// takes its channel.
pub(crate) fn command(
    launcher: Option<&crate::runtime::Launcher>,
    node_package: &Path,
) -> Result<(tokio::process::Command, Connect), Error> {
    #[cfg(feature = "node")]
    let node = crate::runtime::Launcher::node(node_package);
    let launcher = match launcher {
        Some(launcher) => launcher,
        #[cfg(feature = "node")]
        None => &node,
        #[cfg(not(feature = "node"))]
        None => {
            let _ = node_package;
            return Err(Error::Value(
                "no launcher given, and the Node runtime needs the `node` feature".into(),
            ));
        }
    };
    let mut command = tokio::process::Command::new(&launcher.program);
    command
        .args(&launcher.args)
        .envs(launcher.env.iter().map(|(name, value)| (name, value)))
        .stdin(launcher.stdin.process())
        .stdout(launcher.stdout.process())
        .stderr(launcher.stderr.process());
    // Without a directory of its own, it runs where the application does.
    if let Some(cwd) = &launcher.cwd {
        command.current_dir(cwd);
    }
    let loopback = !cfg!(unix)
        || std::env::var_os(crate::transport::local::HANDOVER_VARIABLE)
            .is_some_and(|value| value == "loopback");
    let connect = match (loopback, launcher.inherit_fd) {
        (true, _) => Connect::Loopback,
        (false, true) => Connect::Inherit,
        (false, false) => Connect::DialBack,
    };
    Ok((command, connect))
}

/// Whether the Node runtime package at `package` takes an inherited channel:
/// its `package.json` lists `"fd"` in `rutisChannels`. Older packages do
/// not, and dial back.
#[cfg(feature = "node")]
pub(crate) fn node_inherits(package: &Path) -> bool {
    let channels = std::fs::read(package.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|manifest| manifest.get("rutisChannels").cloned());
    matches!(channels, Some(serde_json::Value::Array(channels)) if channels.iter().any(|c| c == "fd"))
}

/// The Node process, owned by a task that records how it ended. Dropping
/// `_kill` ends the process.
pub(crate) struct Child {
    exit: watch::Receiver<Option<String>>,
    /// The same status for the reader thread, which must not depend on the
    /// runtime: a current-thread runtime may be blocked in a synchronous call.
    ended: Arc<(Mutex<Option<String>>, Condvar)>,
    _kill: oneshot::Sender<()>,
}

impl Child {
    pub(crate) fn watch(mut child: tokio::process::Child) -> Self {
        let (kill, killed) = oneshot::channel::<()>();
        let (report, exit) = watch::channel(None);
        let ended = Arc::new((Mutex::new(None), Condvar::new()));
        if let Some(pid) = child.id() {
            let record = ended.clone();
            let _ = std::thread::Builder::new()
                .name("rutis-exit".into())
                .spawn(move || {
                    if let Some(status) = peek_exit(pid) {
                        record_exit(&record, describe(Ok(status)));
                    }
                });
        }
        let record = ended.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = killed => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let status = describe(status);
            record_exit(&record, status.clone());
            report.send_replace(Some(status));
        });
        Self {
            exit,
            ended,
            _kill: kill,
        }
    }

    /// How the process ended, once it has: as soon as either the exit
    /// watcher or the runtime records it, so it agrees with the error that
    /// ended the session.
    pub(crate) fn status(&self) -> Option<String> {
        self.ended.0.lock().unwrap().clone()
    }

    pub(crate) async fn exited(&self) -> String {
        let mut exit = self.exit.clone();
        let status = exit.wait_for(Option::is_some).await;
        status.map_or_else(|_| "is gone".to_owned(), |status| status.clone().unwrap())
    }

    /// The error that ends a session whose peer went away: waits for the
    /// process to end so the error can say how.
    pub(crate) fn disconnected(&self) -> impl FnOnce() -> Error + Send {
        let ended = self.ended.clone();
        move || ended_with(&ended, HANG_GUARD)
    }
}

/// How long a process whose channel ended may go on running before the
/// session ends without its exit status. Only a process that keeps running
/// with its channel closed reaches it: one that is exiting gets there
/// eventually, and a process may close its channel well before it is gone
/// (Node tears down its I/O worker, and with it the socket, before exiting;
/// under load that took more than the second this once was, #233).
const HANG_GUARD: Duration = Duration::from_secs(10);

/// Waits for the process to end, as `ended` records it, and says how; a
/// process still running after `guard` is reported as such.
fn ended_with(ended: &(Mutex<Option<String>>, Condvar), guard: Duration) -> Error {
    let (status, changed) = ended;
    let status = changed
        .wait_timeout_while(status.lock().unwrap(), guard, |status| status.is_none())
        .unwrap()
        .0
        .clone();
    Error::Transport(match status {
        Some(status) => format!("Cordis process {status}"),
        None => format!(
            "Cordis process closed its channel but has not exited after {}s",
            guard.as_secs_f32()
        ),
    })
}

fn describe(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) if status.success() => "exited normally".to_owned(),
        Ok(status) => format!("exited with {status}"),
        Err(error) => format!("cannot be waited for: {error}"),
    }
}

fn record_exit(ended: &(Mutex<Option<String>>, Condvar), status: String) {
    ended.0.lock().unwrap().get_or_insert(status);
    ended.1.notify_all();
}

/// Waits until the process `pid` ends and reports its status, independently
/// of any runtime.
#[cfg(windows)]
use crate::transport::local::spawn::peek_exit;

#[cfg(not(any(unix, windows)))]
fn peek_exit(_pid: u32) -> Option<std::process::ExitStatus> {
    None
}

/// Waits until the process `pid` ends and reports its status without reaping
/// it (tokio still does), independently of any runtime.
#[cfg(unix)]
fn peek_exit(pid: u32) -> Option<std::process::ExitStatus> {
    use std::os::unix::process::ExitStatusExt;
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `info` is a valid, writable siginfo_t.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return None; // already reaped: the runtime reports it
        }
    }
    // SAFETY: waitid filled a SIGCHLD siginfo_t.
    let status = unsafe { info.si_status() };
    // Rebuild the raw wait status so it formats like `ExitStatus`.
    let raw = match info.si_code {
        libc::CLD_EXITED => (status & 0xff) << 8,
        libc::CLD_DUMPED => status | 0x80,
        _ => status,
    };
    Some(std::process::ExitStatus::from_raw(raw))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::mpsc;

    /// A shell that closes its channel (fd 3), then waits for a line on the
    /// fifo `gate` before exiting with 17: the channel ends while the
    /// process is still running, for as long as the test keeps it so.
    fn closes_then_waits(gate: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(r#"exec 3>&-; read line < "$GATE"; exit 17"#)
            .env("GATE", gate);
        command
    }

    fn fifo(directory: &Path) -> std::path::PathBuf {
        let path = directory.join("gate");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a valid C string.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        path
    }

    /// #233: a process may close its channel well before it exits (Node
    /// closes it while tearing down). The channel's end waits for the exit,
    /// however long that takes, and says how the process ended.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_end_of_the_channel_waits_for_the_exit_status() {
        let directory = tempfile::tempdir().unwrap();
        let gate = fifo(directory.path());
        let Spawned { channel, child, .. } =
            inherit(closes_then_waits(&gate), Path::new("first")).unwrap();
        let mut receiver = channel.receiver;
        let (ended, end) = mpsc::channel();
        std::thread::spawn(move || ended.send(receiver.recv()).unwrap());

        // Past the second the channel's end used to wait for: still
        // waiting, since the process is still running.
        assert!(
            end.recv_timeout(Duration::from_secs(2)).is_err(),
            "the channel ended before the process did"
        );
        assert_eq!(child.status(), None);

        std::fs::OpenOptions::new()
            .write(true)
            .open(&gate)
            .unwrap()
            .write_all(b"exit\n")
            .unwrap();
        match end.recv().unwrap() {
            Err(ChannelError::Closed { reason }) => {
                assert_eq!(reason, "Cordis process exited with exit status: 17")
            }
            other => panic!("the channel should end with the exit status, got {other:?}"),
        }
        assert_eq!(child.exited().await, "exited with exit status: 17");
    }

    /// A channel whose receiver ends when `end` says so, whose sender fails
    /// as the line framing does once its receiver saw the end, and whose
    /// end waits for `exit` to say how the process ended.
    fn watched(end: mpsc::Receiver<()>, exit: mpsc::Receiver<&'static str>) -> Channel {
        struct Ends(mpsc::Receiver<()>);
        impl Receiver for Ends {
            fn recv(&mut self) -> Result<Option<Vec<u8>>, ChannelError> {
                let _ = self.0.recv();
                Ok(None)
            }
        }
        struct Ended;
        impl Sender for Ended {
            fn send(&mut self, _: &[u8]) -> Result<(), ChannelError> {
                Err(ChannelError::Closed {
                    reason: "the channel ended".into(),
                })
            }
        }
        struct Nothing;
        impl Closer for Nothing {
            fn close(&self, _: &str) {}
        }
        on_disconnect(
            Channel {
                sender: Box::new(Ended),
                receiver: Box::new(Ends(end)),
                closer: Arc::new(Nothing),
                info: Default::default(),
            },
            Box::new(move || Error::Transport(format!("Cordis process {}", exit.recv().unwrap()))),
        )
    }

    /// #250: a send after the channel ended but before the exit status is
    /// known fails with the exit status too, not with "the channel ended":
    /// whichever fails first ends the session, and with its reason. Either
    /// order: the send before or after the receiver saw the end.
    #[test]
    fn a_send_after_the_end_fails_with_the_exit_status() {
        for send_first in [false, true] {
            let (end, ends) = mpsc::channel();
            let (exit, exits) = mpsc::channel();
            let Channel {
                mut sender,
                mut receiver,
                ..
            } = watched(ends, exits);
            let (received, receiving) = mpsc::channel();
            let (sent, sending) = mpsc::channel();
            let mut send = Some(move || {
                std::thread::spawn(move || sent.send(sender.send(b"{}")).unwrap());
            });
            if send_first {
                send.take().unwrap()();
            }
            std::thread::spawn(move || received.send(receiver.recv()).unwrap());
            end.send(()).unwrap();
            if let Some(send) = send.take() {
                send();
            }
            // The exit status is not known yet: the send waits with the
            // receiver.
            assert!(
                sending.recv_timeout(Duration::from_millis(200)).is_err(),
                "the send failed before the exit status was known"
            );
            exit.send("exited with exit status: 17").unwrap();
            let reason = "Cordis process exited with exit status: 17";
            for (end, ended) in [
                ("send", sending.recv().unwrap()),
                ("recv", receiving.recv().unwrap().map(|_| ())),
            ] {
                match ended {
                    Err(ChannelError::Closed { reason: got }) => {
                        assert_eq!(got, reason, "{end}, send first: {send_first}")
                    }
                    other => panic!("the {end} should fail with the exit status, got {other:?}"),
                }
            }
        }
    }

    /// Closed here, a send fails at once with its own error: it does not
    /// wait for the process.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_send_after_closing_here_does_not_wait() {
        let directory = tempfile::tempdir().unwrap();
        let gate = fifo(directory.path());
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(r#"read line < "$GATE"; exit 17"#)
            .env("GATE", &gate);
        let Spawned { channel, child, .. } = inherit(command, Path::new("first")).unwrap();
        channel.closer.close("closed here");
        let mut sender = channel.sender;
        assert!(sender.send(b"{}").is_err());
        assert_eq!(child.status(), None);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&gate)
            .unwrap()
            .write_all(b"exit\n")
            .unwrap();
    }

    /// A process that closed its channel and does not exit ends the session
    /// once the guard runs out, saying so rather than `peer disconnected`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_process_that_does_not_exit_is_reported_as_such() {
        let directory = tempfile::tempdir().unwrap();
        let gate = fifo(directory.path());
        let Spawned { child, .. } = inherit(closes_then_waits(&gate), Path::new("first")).unwrap();
        let ended = child.ended.clone();
        let error =
            tokio::task::spawn_blocking(move || ended_with(&ended, Duration::from_millis(100)))
                .await
                .unwrap();
        assert_eq!(
            error.to_string(),
            "Cordis process closed its channel but has not exited after 0.1s"
        );
    }
}
