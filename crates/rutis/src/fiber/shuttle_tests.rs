//! Shuttle models of the three places in the kernel where an interleaving
//! has gone wrong before (#179; standard Q6.1.3, risk K2). Shuttle runs the
//! threads below one at a time and picks who runs next at every sync
//! operation it controls, so each iteration tries one interleaving, and a
//! failing one is printed as a schedule that `shuttle::replay` runs again.
//!
//! The models run the kernel's own code, not a copy of it: `TransitionTask`
//! and `join_task`, [`post_join_to`] (what `FiberInner::post_join` runs) and
//! `EffectRecord::{claim, finish, join}` (what `EffectRecord::drain` runs).
//! Built with `--cfg rutis_shuttle`, `crate::sync` makes the driver's
//! `alive` flag and an effect record's state shuttle primitives; the tokio
//! channels in between (`watch`, `mpsc`, `Notify`) need no runtime and are
//! single steps to shuttle. Shuttle switches just before an operation on
//! its primitives, so the two windows the code guards that no primitive
//! opens are marked with `crate::sync::switch_point()` (nothing in a plain
//! build): between `post_join`'s check and its send, and between an effect
//! join's check and its wait. What stands in for the rest of the fiber (the
//! driver's exit, `drain()`'s spawned runner) is written out below, each
//! next to the code it follows.
//!
//! Run (CONTRIBUTING.md):
//!
//! ```text
//! RUSTFLAGS="--cfg rutis_shuttle" cargo test -p rutis --lib \
//!     --target-dir target/shuttle shuttle
//! ```
//!
//! `RUTIS_SHUTTLE_ITERATIONS` sets the iterations of each scheduler: 1000
//! by default, as on pull requests; the nightly run (`stress.yml`) uses a
//! million. A failure prints its schedule (`failing schedule: "…"`);
//! `RUTIS_SHUTTLE_REPLAY=<schedule>` replays it, with the failing test's
//! name as the filter.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use shuttle::future::block_on;
use shuttle::sync::atomic::AtomicBool;
use shuttle::thread;
use tokio::sync::mpsc;

use super::{
    complete_intent, join_task, mark_subtree_lineage, post_join_to, publish_subtree_closing,
    CloseTree, Intent, TransitionTask,
};
use crate::effect::{run_cleanups, Effect, EffectRecord};
use crate::error::{Error, ErrorKind};
use crate::sync::{switch_point, AtomicBool as SyncBool, Mutex as SyncMutex};

/// Iterations of each scheduler: `RUTIS_SHUTTLE_ITERATIONS`, or 1000.
fn iterations() -> usize {
    std::env::var("RUTIS_SHUTTLE_ITERATIONS")
        .ok()
        .map(|value| value.parse().expect("RUTIS_SHUTTLE_ITERATIONS is a number"))
        .unwrap_or(1000)
}

/// Explore `model` with shuttle's random scheduler and with PCT (which
/// finds bugs that need a few preemptions at the right places with better
/// odds than uniform choices), or replay `RUTIS_SHUTTLE_REPLAY`.
fn explore<F>(model: F)
where
    F: Fn() + Send + Sync + 'static,
{
    if let Ok(schedule) = std::env::var("RUTIS_SHUTTLE_REPLAY") {
        shuttle::replay(model, schedule.trim().trim_matches('"'));
        return;
    }
    let model = Arc::new(model);
    let iterations = iterations();
    let random = model.clone();
    shuttle::check_random(move || random(), iterations);
    shuttle::check_pct(move || model(), iterations, 3);
}

fn failed(what: &str) -> Arc<ErrorKind> {
    Arc::new(ErrorKind::PluginFailed(what.into()))
}

/// Model 1, TransitionTask lost wakeup (cordis-spec-parity §5.2, item 3): the
/// task's watch channel kept no receiver, so a `complete` before anyone
/// joined was dropped and a later join waited forever. The keep-alive
/// receiver holds the result for joins that come later; every join, early
/// or late, gets the one result (the same `Arc`, D25).
#[test]
fn shuttle_transition_task_reaches_every_join() {
    explore(|| {
        let task = TransitionTask::new();
        let error = failed("transition");
        let completer = {
            let (task, error) = (task.clone(), error.clone());
            thread::spawn(move || task.complete(Some(error)))
        };
        let joins: Vec<_> = (0..2)
            .map(|_| {
                let task = task.clone();
                thread::spawn(move || block_on(join_task(&task)))
            })
            .collect();
        completer.join().unwrap();
        for join in joins {
            let got = join.join().unwrap().expect_err("the task failed");
            assert!(Arc::ptr_eq(&got, &error), "a join got another error");
        }
    });
}

/// Model 2, the `post_join` exit race (design-rust-port, review item 24;
/// design-config-hot-update §10 item 4): a poster finds the driver alive, the driver then
/// stops and drains its mailbox, and only then does the post land, with
/// nobody left to read it. `post_join` checks `alive` again after sending,
/// so the task completes either way; its join must always return.
///
/// The driver stands in for `drive()`'s exit (terminal dispose and
/// shutdown): it may handle one intent while running, then sets `alive`
/// false, drains once, and keeps its receiver open while it releases what
/// the fiber held (`release_transient(..).await`), as `drive()` does.
#[test]
fn shuttle_post_join_completes_across_driver_exit() {
    explore(|| {
        let alive = Arc::new(AtomicBool::new(true));
        let (intents, mut mailbox) = mpsc::unbounded_channel::<Intent>();
        let driver = {
            let alive = alive.clone();
            thread::spawn(move || {
                if let Ok(intent) = mailbox.try_recv() {
                    complete_intent(&intent, None);
                }
                alive.store(false, Ordering::SeqCst);
                while let Ok(intent) = mailbox.try_recv() {
                    complete_intent(&intent, Some(Arc::new(ErrorKind::Closed)));
                }
                // release_transient(..).await: the mailbox is still open.
                thread::yield_now();
                drop(mailbox);
            })
        };
        let posters: Vec<_> = (0..2)
            .map(|_| {
                let (alive, intents) = (alive.clone(), intents.clone());
                thread::spawn(move || {
                    let task = TransitionTask::new();
                    post_join_to(&alive, &intents, task.clone(), Intent::Settle, || {
                        Some(Arc::new(ErrorKind::Closed))
                    });
                    // Ok when the driver handled it, Err(Closed) after it
                    // stopped; what matters is that the join returns.
                    let _ = block_on(join_task(&task));
                })
            })
            .collect();
        drop(intents);
        driver.join().unwrap();
        for poster in posters {
            poster.join().unwrap();
        }
    });
}

/// Model 3, concurrent dispose runs a cleanup exactly once (D6/D20, Q3.1): three
/// callers drain one effect record at once, as `Disposer::dispose`,
/// `drain_effects` and a mount record's drain can. Exactly one of them
/// claims the cleanups; every one of them gets the one result (the same
/// `Arc`); none waits forever for a result posted between its check and
/// its wait.
///
/// Each caller does what `EffectRecord::drain` does: claim, and if it got
/// the cleanups, run them on a task of their own (`handle.spawn` there, a
/// shuttle thread here) that finishes the record; then join.
#[test]
fn shuttle_concurrent_dispose_cleans_up_exactly_once() {
    // `run_cleanups` takes a runtime handle for async cleanups; the model's
    // cleanup is synchronous, so the runtime never runs anything.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let handle = runtime.handle().clone();
    explore(move || {
        let runs = Arc::new(AtomicUsize::new(0));
        let effect = {
            let runs = runs.clone();
            Effect::Disposer(Box::new(move || {
                runs.fetch_add(1, Ordering::SeqCst);
                Err(Error::from(ErrorKind::PluginFailed("cleanup".into())))
            }))
        };
        let record = EffectRecord::from(
            effect,
            "model".into(),
            crate::effect::Origin::Other,
            Weak::new(),
        );
        let callers: Vec<_> = (0..3)
            .map(|_| {
                let (record, handle) = (record.clone(), handle.clone());
                thread::spawn(move || {
                    if let Some(mut cleanups) = record.claim() {
                        let record = record.clone();
                        thread::spawn(move || {
                            let error = block_on(run_cleanups(&mut cleanups, &handle));
                            record.finish(error);
                        });
                    }
                    block_on(record.join())
                })
            })
            .collect();
        let results: Vec<_> = callers
            .into_iter()
            .map(|caller| caller.join().unwrap().expect_err("the cleanup failed"))
            .collect();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the cleanup ran more than once"
        );
        for result in &results[1..] {
            assert!(
                Arc::ptr_eq(result, &results[0]),
                "callers got different errors"
            );
        }
    });
}

/// Model 4, the subtree-close marker pass (#1025): a closer collects a fixed
/// tree, writes every `lineage_closed` marker, and only then publishes the
/// existing `closing` flags. A reader that sees an ancestor closing while its
/// own marker is still clear would admit a call after the close boundary.
///
/// The model runs the kernel's own [`mark_subtree_lineage`] /
/// [`publish_subtree_closing`] through [`CloseTree`], so changing the
/// two-phase order in the kernel changes this model too. The deliberate
/// mutation for review: call `publish_subtree_closing` before
/// `mark_subtree_lineage` (or publish closing per member inside the marker
/// loop); the readers then report an ancestor-closed/child-live window.
#[test]
fn shuttle_lineage_marker_precedes_closing() {
    explore(|| {
        let (root, nodes) = model_tree();
        let done = Arc::new(SyncBool::new(false));
        let readers: Vec<_> = nodes
            .iter()
            .map(|node| {
                let node = node.clone();
                let done = done.clone();
                thread::spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        if let Some(why) = close_window(&node) {
                            panic!("{why}");
                        }
                        switch_point();
                    }
                })
            })
            .collect();

        let closer = {
            let root = root.clone();
            let done = done.clone();
            thread::spawn(move || {
                switch_point();
                let subtree = mark_subtree_lineage(&root);
                // The kernel publishes `Shared.closing` between the two
                // passes for a root. A subtree has no shared flag; this
                // switch point stands in for the end of the marker pass.
                switch_point();
                publish_subtree_closing(&subtree);
                done.store(true, Ordering::SeqCst);
            })
        };

        closer.join().unwrap();
        for reader in readers {
            reader.join().unwrap();
        }
        for node in &nodes {
            assert!(
                node.lineage_closed.load(Ordering::SeqCst),
                "a member of the fixed subtree was not marked"
            );
            assert!(
                node.closing.load(Ordering::SeqCst),
                "a member of the fixed subtree was not closed"
            );
        }
    });
}

/// The model's fiber: parent link, children, and the two markers the kernel
/// publishes (`crate::sync` makes the atomics switch points under shuttle).
struct ModelFiber {
    parent: Option<Weak<ModelFiber>>,
    children: SyncMutex<Vec<Arc<ModelFiber>>>,
    lineage_closed: SyncBool,
    closing: SyncBool,
}

impl ModelFiber {
    fn new(parent: Option<Weak<ModelFiber>>) -> Self {
        Self {
            parent,
            children: SyncMutex::new(Vec::new()),
            lineage_closed: SyncBool::new(false),
            closing: SyncBool::new(false),
        }
    }
}

impl CloseTree for ModelFiber {
    fn close_children(&self) -> Vec<Arc<Self>> {
        self.children.lock().unwrap().clone()
    }

    fn mark_lineage(&self) {
        self.lineage_closed.store(true, Ordering::SeqCst);
    }

    fn publish_closing(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }
}

fn model_tree() -> (Arc<ModelFiber>, Vec<Arc<ModelFiber>>) {
    let root = Arc::new(ModelFiber::new(None));
    let left = Arc::new(ModelFiber::new(Some(Arc::downgrade(&root))));
    let right = Arc::new(ModelFiber::new(Some(Arc::downgrade(&root))));
    let nested = Arc::new(ModelFiber::new(Some(Arc::downgrade(&left))));
    root.children
        .lock()
        .unwrap()
        .extend([left.clone(), right.clone()]);
    left.children.lock().unwrap().push(nested.clone());
    (root.clone(), vec![root, left, right, nested])
}

/// `Some(reason)` when this node could admit a call after a close boundary:
/// its own closing flag or an ancestor's close is visible, and its own marker
/// is still clear when read after that observation. The marker is re-read
/// after the close flags, so a node marked concurrently does not look like a
/// window.
fn close_window(node: &Arc<ModelFiber>) -> Option<String> {
    let own_closing = node.closing.load(Ordering::SeqCst);
    let mut ancestor_closed = false;
    let mut current = node.parent.as_ref().and_then(Weak::upgrade);
    while let Some(parent) = current {
        if parent.closing.load(Ordering::SeqCst) || parent.lineage_closed.load(Ordering::SeqCst) {
            ancestor_closed = true;
            break;
        }
        current = parent.parent.as_ref().and_then(Weak::upgrade);
    }
    if (own_closing || ancestor_closed) && !node.lineage_closed.load(Ordering::SeqCst) {
        return Some(if own_closing {
            "this node is closing while its lineage marker is clear".to_string()
        } else {
            "an ancestor is closed while this node's marker is clear".to_string()
        });
    }
    None
}
