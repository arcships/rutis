//! How `run` and `dev` end: on a termination signal the host shuts its
//! root down, so every plugin's cleanup runs once, and waits for that up to
//! a deadline; a second signal, or the deadline, ends it at once.

use std::time::Duration;

use crate::host::Host;

/// How long cleanups may take after a signal, unless `--shutdown-timeout`
/// or [`DEADLINE_VARIABLE`] says otherwise.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(10);

/// Sets the deadline, in seconds, when the command line does not.
pub const DEADLINE_VARIABLE: &str = "RUTIS_SHUTDOWN_TIMEOUT";

const FLAG: &str = "--shutdown-timeout";

/// The exit code when cleanups did not finish: the deadline passed, or a
/// second signal ended the host first. Distinct from 1 (the host could not
/// start or run), and below 126, which shells use for a program they could
/// not run (126, 127) or one a signal ended (128 + n).
pub const UNFINISHED: i32 = 2;

/// How the host ended after a signal.
pub enum Stopped {
    /// Every cleanup finished.
    Done,
    /// Cleanups did not finish; the host exits with [`UNFINISHED`].
    Unfinished,
}

/// Take `--shutdown-timeout <seconds>` out of `args`; the deadline it, or
/// [`DEADLINE_VARIABLE`], gives.
pub fn deadline(args: &[String]) -> Result<(Duration, Vec<String>), String> {
    deadline_in(args, std::env::var(DEADLINE_VARIABLE).ok())
}

/// [`deadline`], with the variable's value `variable`.
fn deadline_in(
    args: &[String],
    variable: Option<String>,
) -> Result<(Duration, Vec<String>), String> {
    let mut given = None;
    let mut rest = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == FLAG {
            given = Some(
                args.next()
                    .ok_or_else(|| format!("{FLAG} needs a number of seconds"))?
                    .clone(),
            );
        } else if let Some(value) = arg.strip_prefix(&format!("{FLAG}=")) {
            given = Some(value.to_owned());
        } else {
            rest.push(arg.clone());
        }
    }
    let (source, value) = match given {
        Some(value) => (FLAG, value),
        None => match variable {
            Some(value) => (DEADLINE_VARIABLE, value),
            None => return Ok((DEFAULT_DEADLINE, rest)),
        },
    };
    let deadline = value
        .trim()
        .parse::<f64>()
        .ok()
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .ok_or_else(|| format!("{source}: {value:?} is not a number of seconds"))?;
    Ok((deadline, rest))
}

/// The termination signals: SIGINT, SIGTERM and SIGHUP; on Windows Ctrl-C,
/// Ctrl-Break and closing the console. Listened to from the start, so a
/// signal that comes while the host starts ends it once it has started.
pub struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hangup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
}

impl Signals {
    pub fn listen() -> Result<Self, String> {
        let cannot = |error: std::io::Error| format!("cannot listen for signals: {error}");
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt()).map_err(cannot)?,
                terminate: signal(SignalKind::terminate()).map_err(cannot)?,
                hangup: signal(SignalKind::hangup()).map_err(cannot)?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows;
            Ok(Self {
                ctrl_c: windows::ctrl_c().map_err(cannot)?,
                ctrl_break: windows::ctrl_break().map_err(cannot)?,
                ctrl_close: windows::ctrl_close().map_err(cannot)?,
            })
        }
    }

    /// The next signal's name.
    pub async fn next(&mut self) -> &'static str {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.interrupt.recv() => "SIGINT",
                _ = self.terminate.recv() => "SIGTERM",
                _ = self.hangup.recv() => "SIGHUP",
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => "Ctrl-C",
                _ = self.ctrl_break.recv() => "Ctrl-Break",
                _ = self.ctrl_close.recv() => "console close",
            }
        }
    }
}

/// After `signal`: shut the host down, waiting up to `deadline` for the
/// cleanups, or until another signal.
pub async fn stop(host: &Host, signal: &str, deadline: Duration, signals: &mut Signals) -> Stopped {
    eprintln!(
        "rutis-host: {signal}: stopping; cleanups have {} (again to exit at once)",
        seconds(deadline)
    );
    let ended = tokio::select! {
        result = host.shutdown(deadline) => result,
        signal = signals.next() => Err(format!("{signal} again")),
    };
    let Err(why) = ended else {
        return Stopped::Done;
    };
    let (stopping, waiting) = host.still_running();
    eprintln!("rutis-host: {why}: cleanups did not finish");
    for plugin in stopping {
        eprintln!("  still stopping: {plugin}");
    }
    if waiting > 0 {
        eprintln!("  and {waiting} plugin(s) waiting for them");
    }
    let processes = rutis_bridge::transport::local::running_processes();
    if !processes.is_empty() {
        eprintln!("  killing runtime processes {processes:?}");
    }
    host.kill_processes().await;
    Stopped::Unfinished
}

fn seconds(duration: Duration) -> String {
    format!("{}s", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    /// The flag, in either form, wins over the variable and leaves the
    /// other arguments; without either, the default.
    #[test]
    fn the_deadline_comes_from_the_flag_then_the_variable() {
        let some = |value: &str| Some(value.to_owned());
        assert_eq!(
            deadline_in(&args(&["--shutdown-timeout", "2.5", "a.json"]), some("7")),
            Ok((Duration::from_millis(2500), args(&["a.json"])))
        );
        assert_eq!(
            deadline_in(&args(&["a.json", "--shutdown-timeout=0"]), None),
            Ok((Duration::ZERO, args(&["a.json"])))
        );
        assert_eq!(
            deadline_in(&args(&["a.json"]), some("7")),
            Ok((Duration::from_secs(7), args(&["a.json"])))
        );
        assert_eq!(deadline_in(&[], None), Ok((DEFAULT_DEADLINE, Vec::new())));
    }

    #[test]
    fn a_deadline_that_is_not_a_number_of_seconds_is_an_error() {
        for bad in ["abc", "-1", "inf", ""] {
            let error = deadline_in(&args(&["--shutdown-timeout", bad]), None).unwrap_err();
            assert!(
                error.contains("is not a number of seconds"),
                "{bad}: {error}"
            );
        }
        let error = deadline_in(&[], Some("soon".into())).unwrap_err();
        assert!(error.starts_with(DEADLINE_VARIABLE), "{error}");
        let error = deadline_in(&args(&["--shutdown-timeout"]), None).unwrap_err();
        assert!(error.contains("needs a number"), "{error}");
    }
}
