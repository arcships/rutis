//! A local runtime gets no standard input unless its launcher asks for this
//! process's (`Launcher::inherit_stdin`), for a plugin that is a terminal UI.
//!
//! Its own test binary: the test points this process's standard input at a
//! pipe holding a line, then starts runtimes that copy what they read.
#![cfg(unix)]
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};

use rutis::Ctx;
use rutis_bridge::runtime::{Launcher, LocalRuntime};

/// Start a runtime that copies its standard input to `out` and exits (so the
/// start fails: it never greets), and return what it copied.
async fn copied(launcher: Launcher, dir: &std::path::Path, out: &str) -> String {
    let runtime = LocalRuntime::launcher("stdin-probe", launcher, dir);
    let root = Ctx::root().unwrap();
    let view = root.plugin(runtime);
    let _ = (&view).await;
    root.shutdown().await.unwrap();
    std::fs::read_to_string(dir.join(out)).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_reads_this_process_stdin_only_when_its_launcher_asks() {
    // This process's stdin: a pipe holding one line, its write end closed.
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    writer.write_all(b"hello\n").unwrap();
    drop(writer);
    assert!(unsafe { libc::dup2(fds[0], std::io::stdin().as_raw_fd()) } >= 0);
    unsafe { libc::close(fds[0]) };

    let dir = tempfile::tempdir().unwrap();
    let probe = |out: &str| {
        Launcher::new("sh")
            .arg("-c")
            .arg(format!("cat > {out}; exit 3"))
            .cwd(dir.path())
            .inherit_fd()
    };
    // By default: nothing to read (and the line stays for the next one).
    assert_eq!(
        copied(probe("default.txt"), dir.path(), "default.txt").await,
        ""
    );
    // Asked for: this process's input.
    assert_eq!(
        copied(
            probe("inherited.txt").inherit_stdin(),
            dir.path(),
            "inherited.txt"
        )
        .await,
        "hello\n"
    );
}
