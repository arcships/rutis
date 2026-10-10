//! Helpers shared by the integration tests that check, on the paused
//! clock, that something is still waiting. Not every test file uses every
//! helper.
#![allow(dead_code)]

use std::future::Future;
use std::time::Duration;

use tokio::sync::oneshot;

/// Whether `f` is still unfinished after every other task has run as far
/// as it can. Only for tests on the paused clock (`start_paused`): the
/// runtime moves that clock forward only once every task is waiting, so
/// the timeout fires only after `f` had every chance to finish. Unlike a
/// short real-time wait, this does not get weaker on a faster machine.
pub async fn still_pending<F: Future + Unpin>(f: &mut F) -> bool {
    tokio::time::timeout(Duration::from_secs(3600), f)
        .await
        .is_err()
}

/// Runs blocking `f` on its own thread, inside this runtime. Not
/// `spawn_blocking`: while a blocking task runs, the paused clock does not
/// move on its own, and `still_pending` needs it to.
pub fn on_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> oneshot::Receiver<T> {
    let runtime = tokio::runtime::Handle::current();
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _runtime = runtime.enter();
        let _ = tx.send(f());
    });
    rx
}
