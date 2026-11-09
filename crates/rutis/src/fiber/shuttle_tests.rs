//! Shuttle models of the three places in the kernel where an interleaving
//! has gone wrong before (#179; standard Q6.1.3, risk K2). Shuttle runs the
//! threads below one at a time and picks who runs next at every sync
//! operation it controls, so each iteration tries one interleaving, and a
//! failing one is printed as a schedule that `shuttle::replay` runs again.
//!
//! The models run the kernel's own code, not a copy of it: `TransitionTask`
//! and `join_task`, [`post_join_to`] (what `FiberInner::post_join` runs),
//! `EffectRecord::{claim, finish, join}` (what `EffectRecord::drain` runs),
//! and — for registration, event flights and the close passes — a real `Ctx`
//! built on a tokio runtime handle, so `register_effect_named`,
//! `Entry::start`, `set_state` and `wait_events` are the kernel's too. Models
//! 7–9 are reference models: their comments say which bookkeeping cannot be
//! run here and what stands in for it. Built with `--cfg rutis_shuttle`, `crate::sync` makes
//! the driver's `alive` flag, a fiber's `lineage_closed` marker and an effect
//! record's state shuttle primitives; the tokio channels in between (`watch`,
//! `mpsc`, `Notify`) need no runtime and are single steps to shuttle. Shuttle
//! switches just before an operation on its primitives, so the windows the
//! code guards that no primitive opens are marked with
//! `crate::sync::switch_point()` (nothing in a plain build): between
//! `post_join`'s check and its send, between an effect join's check and its
//! wait, and inside an effect factory, between the two registration checks.
//! What stands in for the rest of the fiber (the driver's exit, `drain()`'s
//! spawned runner, the unload's async steps) is written out below, each next
//! to the code it follows.
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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool as StdBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use shuttle::future::block_on;
use shuttle::sync::atomic::{AtomicBool, AtomicUsize as ShuttleUsize};
use shuttle::thread;
use tokio::sync::{mpsc, Notify};

use super::{
    complete_intent, join_task, mark_subtree_lineage, post_join_to, publish_subtree_closing,
    CloseTree, FiberInner, FiberState, Intent, PluginId, TransitionTask,
};
use crate::bus::run::{Inv, SyncNext};
use crate::bus::table::{Body, Entry};
use crate::effect::{run_cleanups, Effect, EffectRecord, Origin};
use crate::error::{Error, ErrorKind};
use crate::events::{ErasedSync, ListenerInfo, Value};
use crate::host::CtxHost;
use crate::internal::CtxInternal;
use crate::key::{ScopeId, TypeKey};
use crate::sync::{switch_point, AtomicBool as SyncBool, Mutex as SyncMutex};
use crate::Ctx;

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

/// Model 5, a registration and the dispose of its fiber (#622, "登记准入与
/// 卸载"): `Ctx::effect` registers a cleanup through
/// `Ctx::register_effect_named`, which checks the lifecycle under
/// `Shared.admission`, runs the plugin's factory **outside** that lock, and
/// checks again under the transition lock before it commits the record to
/// `FiberInner::effects`. A dispose that unloads the fiber in that window must
/// not lose the record: whichever way the race goes, the cleanup the factory
/// made runs exactly once — through the unload's `drain_effects`, or through the
/// registration's own drain of a record it refuses after the factory ran.
///
/// The model runs `register_effect_named` itself (via `effect_named`) and the
/// kernel's own `EffectRecord::claim`/`finish`/`join`; the dispose side is
/// written out where it does not exist as a callable piece: what
/// `FiberInner::unload` does under `Shared.admission` and the transition lock
/// (state change, which is what refuses a late registration), then what
/// `drain_effects` does with the records it takes. The refused record's drain
/// is a `handle.spawn` in the kernel, so the model runs the current-thread
/// runtime's queue after both threads: that is the task the host's runtime
/// would run. The factory is the window between the two checks, so it counts
/// itself.
///
/// Mutations this model is meant to catch: dropping the second check (the
/// record is committed after `drain_effects` took the list: the cleanup never
/// runs, `runs` stays 0 for one factory), and dropping the refusal's
/// `record.drain` (same count, no cleanup). Both make the `runs == factories`
/// assertion fail; the switch points are the factory's own `switch_point` and
/// `EffectRecord`'s state, so a mutation that only moves `push_effect` out of
/// the transition critical section has no switch point between the check and
/// the push and stays invisible here — that is a lock-order question, not an
/// interleaving one.
#[test]
fn shuttle_effect_registration_and_dispose_clean_up_exactly_once() {
    explore(|| {
        // The kernel drains a refused record on the runtime (`handle.spawn`):
        // a current-thread runtime runs that task when the model pumps its
        // queue, as a host's runtime would on its own.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        let handle = runtime.handle().clone();
        let ctx = Ctx::root_with(handle.clone());
        let fiber = ctx.root_view().expect("the root view").inner.clone();

        let factories = Arc::new(AtomicUsize::new(0));
        let runs = Arc::new(AtomicUsize::new(0));

        let registrar = {
            let (ctx, factories, runs) = (ctx.clone(), factories.clone(), runs.clone());
            thread::spawn(move || {
                // The dispose may land before the preflight; then the
                // registration is refused before its factory runs.
                switch_point();
                let registered = ctx.effect_named("model", move || {
                    factories.fetch_add(1, Ordering::SeqCst);
                    // `f` runs outside the admission lock: this is the window
                    // between the factory and the state re-check under the
                    // transition lock (`register_effect_named`).
                    switch_point();
                    Effect::Disposer(Box::new(move || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }))
                });
                registered.is_ok()
            })
        };

        let dispose = {
            let (ctx, fiber) = (ctx.clone(), fiber.clone());
            thread::spawn(move || {
                switch_point();
                // `FiberInner::unload` with `NextState::Disposed`: the state
                // change under both locks. It is what a registration that has
                // not committed yet sees.
                {
                    let _admission = ctx.shared().admission.lock().unwrap();
                    let mut tr = fiber.transition.lock().unwrap();
                    fiber.set_state(&mut tr, FiberState::Unloading);
                }
                // `drain_effects`: the records this unload takes, last
                // registered first, each drained by exactly one caller
                // (`EffectRecord::drain` without its spawned runner: the
                // cleanup is synchronous, so this thread runs it).
                let taken: Vec<Arc<EffectRecord>> =
                    std::mem::take(&mut *fiber.effects.lock().unwrap());
                for record in taken.into_iter().rev() {
                    if let Some(mut cleanups) = record.claim() {
                        let error = block_on(run_cleanups(&mut cleanups, &handle));
                        record.finish(error);
                    }
                    let _ = block_on(record.join());
                }
            })
        };

        let registered = registrar.join().unwrap();
        dispose.join().unwrap();
        // The refused record's drain is the task `register_effect_named`
        // spawned; run the runtime's queue.
        runtime.block_on(async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        });

        let made = factories.load(Ordering::SeqCst);
        let ran = runs.load(Ordering::SeqCst);
        assert_eq!(
            ran, made,
            "a registration made an effect whose cleanup did not run exactly once \
             ({ran} runs for {made} factories; the registration returned {registered})"
        );
        assert!(made <= 1, "the factory ran more than once");
        assert!(
            fiber.effects.lock().unwrap().is_empty(),
            "a record was left in the fiber's effect list"
        );
        assert!(
            fiber.effect_index.lock().unwrap().is_empty(),
            "a record was left in the fiber's effect index"
        );
        assert_eq!(
            *fiber.event_flights.lock().unwrap(),
            0,
            "the registration's event flight was never released"
        );
    });
}

/// The body of the flight model's listener: the model runs the call body
/// itself, so the kernel's erased sync listener is never called.
struct NeverCalled;

impl ErasedSync for NeverCalled {
    fn call(&self, _: &Ctx, _: &Inv<'_>, _: Option<SyncNext<'_>>) -> Result<Option<Value>, Error> {
        unreachable!("the model runs the listener body itself")
    }
}

/// A listener entry owned by the model's fiber: the kernel's own [`Entry`],
/// generation 0 (the root fiber's live generation), so `Entry::live` answers
/// from the owner's markers.
fn model_entry(ctx: &Ctx, fiber: &Arc<FiberInner>) -> Entry {
    Entry {
        id: 0,
        position: 0,
        name: Arc::from("model"),
        prefix: false,
        once: false,
        prepend: false,
        info: Arc::new(ListenerInfo {
            name: "model".to_string(),
            prefix: false,
            plugin: "model".to_string(),
            row: None,
            runtime: None,
            node: None,
            scope: Vec::new(),
            global: true,
        }),
        origin: None,
        body: Body::Sync(Arc::new(NeverCalled)),
        ctx: ctx.clone(),
        owner: Arc::downgrade(fiber),
        generation: 0,
        withdrawn: StdBool::new(false),
        claimed: StdBool::new(false),
        calls: AtomicU64::new(0),
    }
}

/// Model 6, an unload waits for the calls in flight on the fiber (#622,
/// "事件调用的在途计数"): `Entry::start` counts the call on its owner
/// (`CallFlight` → `FiberInner::begin_event`) **before** the final liveness
/// check, and the unload publishes `Unloading` (`set_state`, which also
/// publishes `NOT_LIVE`) under `Shared.admission` before it waits
/// (`wait_events`). So the wait returns only when no call body is running: a
/// call that was admitted is counted and waited for, and a call that arrives
/// after the publication is skipped by `live()` and never runs a body. The
/// count must come back to zero, so the drain does not hang on a missed
/// wakeup.
///
/// The model runs the kernel's `Entry::start`, `CallFlight`, `set_state` and
/// `wait_events` (with its deadline thread, which stays out of the way here:
/// the calls finish long before it fires). What is written out is only the
/// call body itself — a dispatch runs the listener between `Entry::start` and
/// dropping the flight — and the unloader's two steps (the state change, then
/// the wait), which are `FiberInner::unload`'s.
///
/// Mutations this model catches: dropping the final `Entry::live` check in
/// `Entry::start` (a dispatcher that begins a flight after the wait returned
/// runs a body, so `running` is non-zero when `wait_events` returns), and
/// making `wait_events` return without waiting for the count (a body is still
/// running when it returns, or a flight outlives the wait). The reordering
/// that has no switch point — checking `live()` before counting the flight —
/// is invisible here: `begin_event` uses std primitives, so shuttle cannot
/// stop between the two.
#[test]
fn shuttle_unload_waits_for_in_flight_listener_calls() {
    explore(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        let ctx = Ctx::root_with(runtime.handle().clone());
        let fiber = ctx.root_view().expect("the root view").inner.clone();
        let entry = Arc::new(model_entry(&ctx, &fiber));

        let running = Arc::new(ShuttleUsize::new(0));

        let dispatchers: Vec<_> = (0..2)
            .map(|_| {
                let (entry, running) = (entry.clone(), running.clone());
                thread::spawn(move || {
                    for _ in 0..2 {
                        switch_point();
                        // The call is counted before its liveness check: a
                        // flight the kernel admits is what the unload waits
                        // for, even if the check then skips the call.
                        let Some(flight) = entry.start() else {
                            continue;
                        };
                        // The listener body, while the flight is held.
                        running.fetch_add(1, Ordering::SeqCst);
                        // A body is user code and yields; the wait must not
                        // pass it.
                        switch_point();
                        running.fetch_sub(1, Ordering::SeqCst);
                        drop(flight);
                    }
                })
            })
            .collect();

        let unloader = {
            let (ctx, fiber, running) = (ctx.clone(), fiber.clone(), running.clone());
            thread::spawn(move || {
                switch_point();
                // `FiberInner::unload`: `Unloading` under the admission and
                // transition locks, publishing `NOT_LIVE` before the wait.
                {
                    let _admission = ctx.shared().admission.lock().unwrap();
                    let mut tr = fiber.transition.lock().unwrap();
                    fiber.set_state(&mut tr, FiberState::Unloading);
                }
                block_on(fiber.wait_events());
                let still = running.load(Ordering::SeqCst);
                assert_eq!(
                    still, 0,
                    "the unload's wait returned while {still} listener call(s) were running"
                );
            })
        };

        for dispatcher in dispatchers {
            dispatcher.join().unwrap();
        }
        unloader.join().unwrap();
        assert_eq!(
            *fiber.event_flights.lock().unwrap(),
            0,
            "an event flight was never finished"
        );
        assert_eq!(
            running.load(Ordering::SeqCst),
            0,
            "a listener body was still running at the end"
        );
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

/// The model's fiber for the shutdown bookkeeping: the tree links and markers
/// [`CloseTree`] publishes, and what `FiberView::shutdown` and
/// `begin_subtree_shutdown_locked` keep — the task every caller of this
/// fiber's shutdown gets, the task its driver completes, its mailbox, and the
/// cleanups the driver runs.
struct ShutdownFiber {
    children: SyncMutex<Vec<Arc<ShutdownFiber>>>,
    lineage_closed: SyncBool,
    closing: SyncBool,
    /// `FiberInner::shutdown_task`: one task for every caller.
    shutdown_task: SyncMutex<Option<Arc<TransitionTask>>>,
    /// `FiberInner::terminal_task`: the task the driver completes, created
    /// once, so the driver is posted one `Intent::Shutdown` only.
    terminal_task: SyncMutex<Option<Arc<TransitionTask>>>,
    intents: mpsc::UnboundedSender<Intent>,
    cleanups: AtomicUsize,
    posts: AtomicUsize,
}

impl CloseTree for ShutdownFiber {
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

/// A node and its driver: the driver handles the one intent a shutdown posts
/// to it (`FiberInner::unload`: one cleanup pass, here) and completes the task
/// that intent carries.
fn shutdown_node(children: &[Arc<ShutdownFiber>]) -> (Arc<ShutdownFiber>, thread::JoinHandle<()>) {
    let (intents, mut mailbox) = mpsc::unbounded_channel::<Intent>();
    let fiber = Arc::new(ShutdownFiber {
        children: SyncMutex::new(children.to_vec()),
        lineage_closed: SyncBool::new(false),
        closing: SyncBool::new(false),
        shutdown_task: SyncMutex::new(None),
        terminal_task: SyncMutex::new(None),
        intents,
        cleanups: AtomicUsize::new(0),
        posts: AtomicUsize::new(0),
    });
    let driver = {
        let fiber = fiber.clone();
        thread::spawn(move || {
            if let Some(intent) = block_on(mailbox.recv()) {
                fiber.cleanups.fetch_add(1, Ordering::SeqCst);
                switch_point();
                complete_intent(&intent, None);
            }
        })
    };
    (fiber, driver)
}

/// `begin_subtree_shutdown_locked`, mirrored: called with the admission lock
/// held, every member of `fiber`'s subtree is shut down once, and every caller
/// of the same fiber gets the same task. The kernel's slot checks, its
/// `Intent::Shutdown` post and the joins are the kernel's own types; what is
/// written out is the `handle.spawn`ed joiner (a shuttle thread here) and the
/// join of the driver's handle, which only collects a panic.
fn begin_shutdown(fiber: &Arc<ShutdownFiber>) -> Arc<TransitionTask> {
    if let Some(task) = fiber.shutdown_task.lock().unwrap().as_ref() {
        return task.clone();
    }
    let public = TransitionTask::new();
    *fiber.shutdown_task.lock().unwrap() = Some(public.clone());

    let child_tasks: Vec<Arc<TransitionTask>> =
        fiber.close_children().iter().map(begin_shutdown).collect();

    let (inner, needs_post) = {
        let mut slot = fiber.terminal_task.lock().unwrap();
        match slot.as_ref() {
            Some(task) => (task.clone(), false),
            None => {
                let task = TransitionTask::new();
                *slot = Some(task.clone());
                (task, true)
            }
        }
    };
    if needs_post {
        fiber.posts.fetch_add(1, Ordering::SeqCst);
        // `FiberInner::post`: a driver that already exited completes the
        // intent's task at once.
        let intent = Intent::Shutdown(inner.clone());
        if fiber.intents.send(intent).is_err() {
            inner.complete(Some(Arc::new(ErrorKind::Closed)));
        }
    }
    let result = public.clone();
    thread::spawn(move || {
        let mut errors = Vec::new();
        if let Err(error) = block_on(join_task(&inner)) {
            errors.push(error);
        }
        // The kernel joins the child tasks after its own task: a subtree
        // shutdown returns once the whole subtree has cleaned up.
        for child in child_tasks {
            if let Err(error) = block_on(join_task(&child)) {
                errors.push(error);
            }
        }
        result.complete(crate::error::aggregate_arcs(errors));
    });
    public
}

/// `FiberView::shutdown`'s synchronous part (#1025, #622): under the admission
/// lock, the marker pass, then the existing `closing` flags, then the task
/// bookkeeping.
fn request_shutdown(fiber: &Arc<ShutdownFiber>, admission: &SyncMutex<()>) -> Arc<TransitionTask> {
    let _admission = admission.lock().unwrap();
    let subtree = mark_subtree_lineage(fiber);
    publish_subtree_closing(&subtree);
    begin_shutdown(fiber)
}

/// Model 7, concurrent parent, child and sibling subtree shutdowns (#622,
/// "根关闭与根 dispose" / "子树关闭"): three callers shut down the root, a child
/// and a sibling at once, and one of them twice. Every caller returns after
/// the whole subtree it named has run its cleanups — each fiber's cleanup
/// exactly once — no caller waits forever (the drivers are posted once, and
/// every task is completed), and two callers of the same fiber get the same
/// task, so they cannot disagree about the result.
///
/// This is the one reference model in this file: the kernel's
/// `begin_subtree_shutdown_locked` needs a real `FiberInner` and spawns its
/// joiner on a tokio runtime, and a real `FiberView::shutdown` cannot run from
/// two shuttle threads — it holds `Shared.admission`, a **std** mutex, while
/// `mark_subtree_lineage` stores a shuttle `lineage_closed` atomic, so the
/// second caller would block in a std mutex while the first is parked in
/// shuttle (a model deadlock, not a kernel one). The marker and closing passes
/// and every task are the kernel's own; the two slots and the post mirror
/// `begin_subtree_shutdown_locked` line for line.
///
/// Mutations this model catches: dropping the `shutdown_task` early return
/// (two callers of one fiber get different tasks: the `Arc::ptr_eq` assertion
/// fails) and completing the public task before the child tasks (a caller
/// returns before a child's cleanup ran: its check right after the join sees a
/// zero). The cleanups are std atomics on purpose: a shuttle load is a switch
/// point, and the check after the join must not be one, or a driver could run
/// inside it and hide an early return. The `terminal_task` slot is not
/// exercised here: concurrent shutdowns alone already serialize on
/// `shutdown_task`, so only a dispose racing a shutdown can post twice — that
/// belongs to the "root close and root dispose" model, which does not exist
/// yet (#622).
#[test]
fn shuttle_concurrent_subtree_shutdowns_run_each_cleanup_once() {
    explore(|| {
        let admission = Arc::new(SyncMutex::new(()));
        let (nested, nested_driver) = shutdown_node(&[]);
        let (left, left_driver) = shutdown_node(&[nested.clone()]);
        let (right, right_driver) = shutdown_node(&[]);
        let (root, root_driver) = shutdown_node(&[left.clone(), right.clone()]);

        // Root, child, the same child again, sibling: the parent's subtree
        // shutdown and the child's own one race, and the child's two callers
        // must share one task. Each caller gets the members of the subtree it
        // names up front: its check after the join must not take a lock or
        // store a marker, or shuttle could run a driver inside it.
        let members: Vec<Vec<Arc<ShutdownFiber>>> =
            [root.clone(), left.clone(), left.clone(), right.clone()]
                .iter()
                .map(|fiber| {
                    let mut out = Vec::new();
                    let mut pending = vec![fiber.clone()];
                    while let Some(node) = pending.pop() {
                        pending.extend(node.children.lock().unwrap().iter().cloned());
                        out.push(node);
                    }
                    out
                })
                .collect();
        let callers: Vec<_> = [root.clone(), left.clone(), left.clone(), right.clone()]
            .into_iter()
            .zip(members)
            .enumerate()
            .map(|(index, (fiber, members))| {
                let admission = admission.clone();
                thread::spawn(move || {
                    switch_point();
                    let task = request_shutdown(&fiber, &admission);
                    let result = block_on(join_task(&task));
                    // Right after the join, with no yield in between:
                    // returning means the whole subtree it named has cleaned
                    // up. The counters are std atomics for that reason.
                    let cleaned: Vec<usize> = members
                        .iter()
                        .map(|member| member.cleanups.load(Ordering::SeqCst))
                        .collect();
                    (index, task, result, cleaned)
                })
            })
            .collect();

        let mut results = Vec::new();
        for caller in callers {
            let (index, task, result, cleaned) = caller.join().unwrap();
            assert!(result.is_ok(), "a shutdown returned an error");
            assert_eq!(
                cleaned,
                vec![1; cleaned.len()],
                "a subtree shutdown returned before one of its cleanups ran"
            );
            results.push((index, task));
        }
        assert!(
            Arc::ptr_eq(&results[1].1, &results[2].1),
            "two shutdowns of the same fiber returned different tasks"
        );

        for driver in [root_driver, left_driver, right_driver, nested_driver] {
            driver.join().unwrap();
        }
        for node in [&root, &left, &right, &nested] {
            let cleanups = node.cleanups.load(Ordering::SeqCst);
            let posts = node.posts.load(Ordering::SeqCst);
            assert_eq!(
                cleanups, 1,
                "a fiber's cleanup did not run exactly once ({cleanups} runs)"
            );
            assert_eq!(
                posts, 1,
                "a fiber's driver was posted {posts} intents, not one"
            );
            assert!(
                node.lineage_closed.load(Ordering::SeqCst),
                "a member of a closed subtree was not marked"
            );
            assert!(
                node.closing.load(Ordering::SeqCst),
                "a member of a closed subtree was not closed"
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Model 8: root dispose, root shutdown and restart (#622, "根关闭与根 dispose")

/// The transition bookkeeping of the model's root: the fields of `Trans` the
/// close paths touch.
struct RootTrans {
    state: FiberState,
    generation: u64,
    /// `Trans::terminal_task`: what a dispose registers and the driver
    /// completes; a shutdown also completes it if a dispose came first.
    terminal_task: Option<Arc<TransitionTask>>,
    pending_restarts: usize,
    pre_cancelled: bool,
    error: Option<Arc<ErrorKind>>,
}

/// The root of the reference model: the slots `FiberView::dispose` and
/// `CtxHost::shutdown` share, its driver's mailbox, and the effect records
/// its `unload` drains. One effect is registered at the start, as a host's
/// `ctx.effect` on the root would be.
struct RootFiber {
    handle: tokio::runtime::Handle,
    /// `Shared.admission`. A **std** mutex in the kernel; shuttle's here, so
    /// the other close caller can run between the two calls the kernel makes
    /// as separate steps (see the model's comment).
    admission: SyncMutex<()>,
    /// `Shared.closing`.
    shared_closing: SyncBool,
    /// `Shared.shutdown_task`.
    shared_shutdown_task: SyncMutex<Option<Arc<TransitionTask>>>,
    transition: SyncMutex<RootTrans>,
    /// `FiberInner::closing`.
    closing: SyncBool,
    lineage_closed: SyncBool,
    alive: SyncBool,
    intents: mpsc::UnboundedSender<Intent>,
    effects: SyncMutex<Vec<Arc<EffectRecord>>>,
    /// Std counters: read after the joins, where a shuttle load would itself
    /// be a switch point and could run a driver inside the check.
    cleanups: Arc<AtomicUsize>,
    dispose_posts: AtomicUsize,
    shutdown_posts: AtomicUsize,
    restart_posts: AtomicUsize,
}

impl CloseTree for RootFiber {
    fn close_children(&self) -> Vec<Arc<Self>> {
        // The root's subtree is the root: child subtrees are model 7's.
        Vec::new()
    }

    fn mark_lineage(&self) {
        self.lineage_closed.store(true, Ordering::SeqCst);
    }

    fn publish_closing(&self) {
        self.closing.store(true, Ordering::SeqCst);
        // `FiberInner::publish_closing` pre-cancels the generation as well.
        self.transition.lock().unwrap().pre_cancelled = true;
    }
}

fn set_root_state(tr: &mut RootTrans, new: FiberState) {
    // `set_state`: only a change is a state change (the watch publication is
    // not part of this model).
    if tr.state != new {
        tr.state = new;
    }
}

fn stopped_error_root(root: &RootFiber) -> Option<Arc<ErrorKind>> {
    root.transition.lock().unwrap().error.clone().or_else(|| {
        root.shared_closing
            .load(Ordering::SeqCst)
            .then(|| Arc::new(ErrorKind::Closed))
    })
}

/// `FiberInner::post` for the model's root: refuse and complete the intent
/// once the driver stopped, and count the intents the driver will see.
fn post_root(root: &Arc<RootFiber>, intent: Intent) -> bool {
    if !root.alive.load(Ordering::SeqCst) {
        complete_intent(&intent, stopped_error_root(root));
        return false;
    }
    let counter = match &intent {
        Intent::Dispose => Some(&root.dispose_posts),
        Intent::Shutdown(_) => Some(&root.shutdown_posts),
        _ => None,
    };
    match root.intents.send(intent) {
        Ok(()) => {
            if let Some(counter) = counter {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            true
        }
        Err(error) => {
            complete_intent(&error.0, stopped_error_root(root));
            false
        }
    }
}

fn new_root(handle: tokio::runtime::Handle) -> (Arc<RootFiber>, mpsc::UnboundedReceiver<Intent>) {
    let (intents, mailbox) = mpsc::unbounded_channel();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let record = EffectRecord::from(
        Effect::Disposer(Box::new({
            let cleanups = cleanups.clone();
            move || {
                cleanups.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })),
        "model root effect".into(),
        Origin::Other,
        Weak::new(),
    );
    let root = Arc::new(RootFiber {
        handle,
        admission: SyncMutex::new(()),
        shared_closing: SyncBool::new(false),
        shared_shutdown_task: SyncMutex::new(None),
        transition: SyncMutex::new(RootTrans {
            state: FiberState::Active,
            generation: 0,
            terminal_task: None,
            pending_restarts: 0,
            pre_cancelled: false,
            error: None,
        }),
        closing: SyncBool::new(false),
        lineage_closed: SyncBool::new(false),
        alive: SyncBool::new(true),
        intents,
        effects: SyncMutex::new(vec![record]),
        cleanups,
        dispose_posts: AtomicUsize::new(0),
        shutdown_posts: AtomicUsize::new(0),
        restart_posts: AtomicUsize::new(0),
    });
    (root, mailbox)
}

/// `CtxHost::shutdown` for the root, mirrored: under the admission lock, one
/// shared task, the marker pass, `Shared.closing`, the existing `closing`
/// flags, then one `Intent::Shutdown`.
fn root_shutdown(root: &Arc<RootFiber>) -> Arc<TransitionTask> {
    let _admission = root.admission.lock().unwrap();
    let mut slot = root.shared_shutdown_task.lock().unwrap();
    if let Some(task) = slot.as_ref() {
        return task.clone();
    }
    let task = TransitionTask::new();
    *slot = Some(task.clone());
    let subtree = mark_subtree_lineage(root);
    root.shared_closing.store(true, Ordering::SeqCst);
    publish_subtree_closing(&subtree);
    post_root(root, Intent::Shutdown(task.clone()));
    task
}

/// `FiberView::dispose`'s root branch, mirrored: deciding and registering
/// share the admission lock; after `Shared.closing` the dispose joins the
/// shutdown's task instead of registering a second one.
fn root_dispose(root: &Arc<RootFiber>) -> (Arc<TransitionTask>, bool) {
    let _admission = root.admission.lock().unwrap();
    if root.shared_closing.load(Ordering::SeqCst) {
        let task = root
            .shared_shutdown_task
            .lock()
            .unwrap()
            .clone()
            .expect("shutdown stores its task before closing admission");
        return (task, true);
    }
    // A scheduling point inside the admission critical section: the kernel's
    // thread can be preempted here, but every other root-close caller then
    // blocks on the admission lock. A restart does not take it, so it may
    // run.
    switch_point();
    (register_root_dispose(root), false)
}

/// `register_root_dispose`, mirrored: one terminal task, one `Intent::Dispose`.
fn register_root_dispose(root: &Arc<RootFiber>) -> Arc<TransitionTask> {
    let (task, newly_registered) = {
        let mut tr = root.transition.lock().unwrap();
        match &tr.terminal_task {
            Some(task) => (task.clone(), false),
            None => {
                tr.pre_cancelled = true;
                let task = TransitionTask::new();
                tr.terminal_task = Some(task.clone());
                (task, true)
            }
        }
    };
    if newly_registered {
        post_root(root, Intent::Dispose);
        if !root.alive.load(Ordering::SeqCst) {
            task.complete(stopped_error_root(root));
        }
    }
    task
}

/// `FiberView::restart`, mirrored for the root: the closing checks, the
/// pre-cancel, then `post_join` and the join.
fn root_restart(root: &Arc<RootFiber>) -> Result<(), Arc<ErrorKind>> {
    {
        let mut tr = root.transition.lock().unwrap();
        if root.shared_closing.load(Ordering::SeqCst) || root.closing.load(Ordering::SeqCst) {
            return Err(Arc::new(ErrorKind::Closed));
        }
        tr.pending_restarts += 1;
        tr.pre_cancelled = true;
    }
    let task = TransitionTask::new();
    let sent = post_join_to(
        &root.alive,
        &root.intents,
        task.clone(),
        Intent::Restart,
        || stopped_error_root(root),
    );
    if sent {
        root.restart_posts.fetch_add(1, Ordering::SeqCst);
    }
    block_on(join_task(&task))
}

/// What `unload` moves to: `NextState` from the kernel.
enum RootNext {
    Pending,
    Disposed,
}

/// `drain_effects` for the model's one record: take the list, claim each
/// record once, run it, finish it.
fn drain_root_effects(root: &Arc<RootFiber>) {
    let effects: Vec<Arc<EffectRecord>> = std::mem::take(&mut *root.effects.lock().unwrap());
    for record in effects.into_iter().rev() {
        if let Some(mut cleanups) = record.claim() {
            let error = block_on(run_cleanups(&mut cleanups, &root.handle));
            record.finish(error);
        }
        let _ = block_on(record.join());
    }
}

/// `FiberInner::unload` for the root: `Unloading` under the admission and
/// transition locks, the (empty) event wait, the cleanup pass, then the next
/// state.
fn unload_root(root: &Arc<RootFiber>, next: RootNext) {
    {
        let _admission = root.admission.lock().unwrap();
        let mut tr = root.transition.lock().unwrap();
        set_root_state(&mut tr, FiberState::Unloading);
    }
    drain_root_effects(root);
    let mut tr = root.transition.lock().unwrap();
    match next {
        RootNext::Pending => set_root_state(&mut tr, FiberState::Pending),
        RootNext::Disposed => {
            tr.error = None;
            set_root_state(&mut tr, FiberState::Disposed);
        }
    }
}

/// The root's driver, mirroring `drive`'s `Dispose`, `Shutdown` and `Restart`
/// arms. The Shutdown arm is the one that stops the driver: it drains the
/// mailbox (a restart queued behind it gets `Closed`), completes the
/// terminal task and the shutdown task, then returns.
fn drive_root(root: Arc<RootFiber>, mut mailbox: mpsc::UnboundedReceiver<Intent>) {
    while let Some(intent) = block_on(mailbox.recv()) {
        match intent {
            Intent::Dispose => {
                if root.transition.lock().unwrap().state != FiberState::Disposed {
                    unload_root(&root, RootNext::Disposed);
                }
                let (task, error) = {
                    let tr = root.transition.lock().unwrap();
                    (tr.terminal_task.clone(), tr.error.clone())
                };
                if let Some(task) = task {
                    task.complete(error);
                }
            }
            Intent::Shutdown(task) => {
                if root.transition.lock().unwrap().state != FiberState::Disposed {
                    unload_root(&root, RootNext::Disposed);
                }
                root.alive.store(false, Ordering::SeqCst);
                let (terminal, error) = {
                    let tr = root.transition.lock().unwrap();
                    (tr.terminal_task.clone(), tr.error.clone())
                };
                while let Ok(intent) = mailbox.try_recv() {
                    let completion = match &intent {
                        Intent::Settle(_) => None,
                        Intent::Shutdown(_) => error.clone(),
                        _ => Some(Arc::new(ErrorKind::Closed)),
                    };
                    complete_intent(&intent, completion);
                }
                if let Some(terminal) = terminal {
                    terminal.complete(error.clone());
                }
                task.complete(error);
                return;
            }
            Intent::Restart(task) => {
                {
                    let mut tr = root.transition.lock().unwrap();
                    tr.pending_restarts = tr.pending_restarts.saturating_sub(1);
                }
                // The take-up's own linearization point: a restart taken up
                // after the close is refused here; one taken up before it may
                // unload/reactivate, and the queued Shutdown disposes it.
                let closing = root.shared_closing.load(Ordering::SeqCst)
                    || root.closing.load(Ordering::SeqCst);
                if closing {
                    task.complete(Some(Arc::new(ErrorKind::Closed)));
                    continue;
                }
                let state = root.transition.lock().unwrap().state;
                if state == FiberState::Disposed {
                    // The guard above is load-bearing: a restart whose take-up
                    // saw the close must never reactivate the root.
                    assert!(
                        !closing,
                        "a restart after the close committed reactivated the root"
                    );
                    {
                        let mut tr = root.transition.lock().unwrap();
                        tr.error = None;
                        tr.terminal_task = None;
                        set_root_state(&mut tr, FiberState::Active);
                    }
                    let error = root.transition.lock().unwrap().error.clone();
                    task.complete(error);
                    continue;
                }
                if state != FiberState::Pending {
                    unload_root(&root, RootNext::Pending);
                }
                // `refresh_deps` returns at its plugin check for the root:
                // no generation starts, so the restart reports the fiber's
                // error (none here), as `complete_task` does.
                let error = root.transition.lock().unwrap().error.clone();
                task.complete(error);
            }
            _ => {}
        }
    }
}

/// Model 8, root dispose × root shutdown × restart (#622, "根关闭与根
/// dispose"): two shutdown callers, one dispose and one restart race. Every
/// caller must return (no lost wakeup), the two shutdown callers must share
/// one task, the dispose must join either that same task or its own terminal
/// task and must never report `Closed` (it registered before admission
/// closed), the root's one cleanup must run exactly once, and a restart whose
/// take-up saw the close must never reactivate the root (a restart ordered
/// before the close may, and the queued shutdown then disposes it).
///
/// This is a reference model. `CtxHost::shutdown` and the root branch of
/// `FiberView::dispose` take `Shared.admission`, a **std** mutex, and then
/// write the shuttle-visible `lineage_closed` markers; two shuttle threads
/// entering them would deadlock in the std mutex while the first is parked at
/// a marker (the same reason model 7 mirrors the subtree shutdown). The
/// mirror keeps the kernel's line order and runs the kernel's own
/// `mark_subtree_lineage` / `publish_subtree_closing` through `CloseTree`,
/// `TransitionTask`, `post_join_to`, `join_task` and `complete_intent`; the
/// driver mirrors `drive`'s close and restart arms and `drain_effects`. The
/// root has no plugin, so `refresh_deps` returns at its plugin check: the
/// model asserts the close guards, not a load.
///
/// Mutations this model catches: dropping the admission lock from the root
/// dispose path (a dispose that saw `closing == false` registers after the
/// shutdown finished and joins a task completed with `Closed`) and dropping
/// the `shared_shutdown_task` early return (two shutdown callers post two
/// `Intent::Shutdown`s and get different tasks; `shutdown_posts == 1` and
/// `Arc::ptr_eq` fail). Run: both fail at the default 1000 iterations, with
/// `a dispose registered before admission closed got Err(Closed)` and `two
/// shutdown callers got different tasks`. The shutdown path's own admission
/// lock has no separately observable mutation in this root-only model: the
/// slot is stored before `Shared.closing`, so a dispose that sees `closing`
/// still finds it.
#[test]
fn shuttle_root_dispose_and_shutdown_share_one_close() {
    // `run_cleanups` takes a runtime handle; the model's cleanup is
    // synchronous, so the runtime never runs anything.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");
    let handle = runtime.handle().clone();
    explore(move || {
        let (root, mailbox) = new_root(handle.clone());
        let driver = {
            let root = root.clone();
            thread::spawn(move || drive_root(root, mailbox))
        };

        let shutdowns: Vec<_> = (0..2)
            .map(|_| {
                let root = root.clone();
                thread::spawn(move || {
                    switch_point();
                    let task = root_shutdown(&root);
                    let result = block_on(join_task(&task));
                    (task, result)
                })
            })
            .collect();
        let disposer = {
            let root = root.clone();
            thread::spawn(move || {
                switch_point();
                let (task, joined_shutdown) = root_dispose(&root);
                let result = block_on(join_task(&task));
                (task, joined_shutdown, result)
            })
        };
        let restarter = {
            let root = root.clone();
            thread::spawn(move || {
                switch_point();
                root_restart(&root)
            })
        };

        let mut shutdown_tasks: Vec<Arc<TransitionTask>> = Vec::new();
        for (task, result) in shutdowns.into_iter().map(|caller| caller.join().unwrap()) {
            assert!(result.is_ok(), "a shutdown returned {result:?}");
            shutdown_tasks.push(task);
        }
        assert!(
            Arc::ptr_eq(&shutdown_tasks[0], &shutdown_tasks[1]),
            "two shutdown callers got different tasks"
        );

        let (dispose_task, joined_shutdown, dispose_result) = disposer.join().unwrap();
        assert!(
            dispose_result.is_ok(),
            "a dispose registered before admission closed got {dispose_result:?}"
        );
        if joined_shutdown {
            assert!(
                Arc::ptr_eq(&dispose_task, &shutdown_tasks[0]),
                "a dispose that joined the shutdown got another task"
            );
        }
        match restarter.join().unwrap() {
            Ok(()) => {}
            Err(error) => assert!(
                matches!(&*error, ErrorKind::Closed),
                "a restart returned {error:?}"
            ),
        }

        assert_eq!(
            root.shutdown_posts.load(Ordering::SeqCst),
            1,
            "the root's driver was posted more than one shutdown"
        );
        assert!(
            root.dispose_posts.load(Ordering::SeqCst) <= 1,
            "the root's driver was posted more than one dispose"
        );
        assert!(
            root.restart_posts.load(Ordering::SeqCst) <= 1,
            "one restart posted more than one intent"
        );
        assert_eq!(
            root.cleanups.load(Ordering::SeqCst),
            1,
            "the root's cleanup did not run exactly once"
        );
        {
            let tr = root.transition.lock().unwrap();
            assert_eq!(
                tr.state,
                FiberState::Disposed,
                "the root did not end disposed"
            );
            assert_eq!(tr.generation, 0, "a close started a root generation");
        }
        assert!(
            root.closing.load(Ordering::SeqCst) && root.lineage_closed.load(Ordering::SeqCst),
            "the root was not marked and closed"
        );
        driver.join().unwrap();

        // A restart after the close is refused, posts nothing and runs no
        // cleanup.
        let posts = root.restart_posts.load(Ordering::SeqCst);
        let late = root_restart(&root);
        assert!(
            matches!(
                &*late.expect_err("a closed root restarted"),
                ErrorKind::Closed
            ),
            "a restart after the close did not report Closed"
        );
        assert_eq!(root.restart_posts.load(Ordering::SeqCst), posts);
        assert_eq!(root.cleanups.load(Ordering::SeqCst), 1);
        assert_eq!(
            root.transition.lock().unwrap().state,
            FiberState::Disposed,
            "a restart after the close changed the root's state"
        );
    });
}

// ---------------------------------------------------------------------------
// Model 9: dependency eviction (#622, Bug B / #620)

/// `(provider, provider generation, key, scope)`: the dependency identity
/// `last_deps` records and a withdrawal matches exactly (`registry.rs`).
type DepQuad = (PluginId, u64, TypeKey, Option<ScopeId>);

/// The model's provider: one identity whose state is the gate (`resolve_dep`
/// answers only an active provider), and whose next generation provides the
/// same key again.
struct DepProvider {
    id: PluginId,
    state: SyncMutex<FiberState>,
}

impl DepProvider {
    fn state(&self) -> FiberState {
        *self.state.lock().unwrap()
    }

    fn set(&self, state: FiberState) {
        *self.state.lock().unwrap() = state;
    }
}

/// One service binding, mirroring `Binding`: provider, generation and the
/// `removing` flag a withdrawal sets before it looks for consumers.
struct DepBinding {
    provider: Weak<DepProvider>,
    provider_id: PluginId,
    provider_gen: u64,
    removing: SyncBool,
}

/// The model's registry: the binding slot and the injectors of the one key.
struct DepRegistry {
    bindings: SyncMutex<HashMap<(TypeKey, Option<ScopeId>), Arc<DepBinding>>>,
    injectors: SyncMutex<Vec<Weak<Consumer>>>,
}

impl DepRegistry {
    fn new() -> Self {
        Self {
            bindings: SyncMutex::new(HashMap::new()),
            injectors: SyncMutex::new(Vec::new()),
        }
    }

    /// `Registry::insert_binding`: a binding still removing may be replaced;
    /// one that is live is `ServiceExists`.
    fn insert(
        &self,
        key: &TypeKey,
        provider: &Arc<DepProvider>,
        provider_gen: u64,
    ) -> Result<Arc<DepBinding>, Error> {
        let mut bindings = self.bindings.lock().unwrap();
        let entry = (key.clone(), None);
        if let Some(existing) = bindings.get(&entry) {
            if !existing.removing.load(Ordering::SeqCst) {
                return Err(Error::from(ErrorKind::ServiceExists("model key".into())));
            }
        }
        let binding = Arc::new(DepBinding {
            provider: Arc::downgrade(provider),
            provider_id: provider.id,
            provider_gen,
            removing: SyncBool::new(false),
        });
        bindings.insert(entry, binding.clone());
        Ok(binding)
    }

    fn lookup(&self, key: &TypeKey) -> Option<Arc<DepBinding>> {
        self.bindings
            .lock()
            .unwrap()
            .get(&(key.clone(), None))
            .cloned()
    }

    /// `Registry::mark_removing_if`: only while the slot still holds this
    /// binding.
    fn mark_removing_if(&self, key: &TypeKey, expected: &Arc<DepBinding>) {
        let bindings = self.bindings.lock().unwrap();
        if bindings
            .get(&(key.clone(), None))
            .is_some_and(|binding| Arc::ptr_eq(binding, expected))
        {
            expected.removing.store(true, Ordering::SeqCst);
        }
    }

    /// `Registry::finalize_binding_if`: a slot a new provide took over stays.
    fn finalize_if(&self, key: &TypeKey, expected: &Arc<DepBinding>) -> bool {
        let mut bindings = self.bindings.lock().unwrap();
        let entry = (key.clone(), None);
        let still_old = bindings
            .get(&entry)
            .is_some_and(|binding| Arc::ptr_eq(binding, expected));
        if still_old {
            bindings.remove(&entry);
        }
        still_old
    }

    /// `Registry::consumers_of`: the injectors whose `last_deps` contains
    /// exactly this quad.
    fn consumers_of(&self, key: &TypeKey, quad: DepQuad) -> Vec<Arc<Consumer>> {
        let index = self.injectors.lock().unwrap();
        index
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|consumer| consumer.inner.declared.contains(key))
            .filter(|consumer| {
                consumer
                    .inner
                    .last_deps
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|deps| deps.contains(&quad))
            })
            .collect()
    }

    fn register_inject(&self, key: &TypeKey, consumer: &Arc<Consumer>) {
        assert!(consumer.inner.declared.contains(key), "an undeclared key");
        self.injectors
            .lock()
            .unwrap()
            .push(Arc::downgrade(consumer));
    }

    /// `Registry::notify_key_changed`: post `RefreshDeps` to every injector
    /// of this key.
    fn notify(&self, key: &TypeKey) {
        let fibers: Vec<Arc<Consumer>> = {
            let index = self.injectors.lock().unwrap();
            index
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|consumer| consumer.inner.declared.contains(key))
                .collect()
        };
        for fiber in fibers {
            post_consumer(&fiber, Intent::RefreshDeps);
        }
    }

    /// `Registry::resolve_dep`: a live binding of an active provider.
    fn resolve_dep(&self, key: &TypeKey) -> Option<(PluginId, u64)> {
        let binding = self.lookup(key)?;
        if binding.removing.load(Ordering::SeqCst) {
            return None;
        }
        let provider = binding.provider.upgrade()?;
        if provider.state() != FiberState::Active {
            return None;
        }
        Some((binding.provider_id, binding.provider_gen))
    }
}

/// The model consumer's transition bookkeeping (`Trans`).
struct ConsumerTrans {
    state: FiberState,
    generation: u64,
    terminal_task: Option<Arc<TransitionTask>>,
    pending_restarts: usize,
    pre_cancelled: bool,
    error: Option<Arc<ErrorKind>>,
}

/// Everything the model's driver needs, without the intent sender, so the
/// driver can outlive the `Consumer` handles and stop when they are dropped.
struct ConsumerInner {
    transition: SyncMutex<ConsumerTrans>,
    /// `FiberInner::last_deps`.
    last_deps: SyncMutex<Option<HashSet<DepQuad>>>,
    /// `FiberInner::effects`: what this generation's `apply` registered.
    effects: SyncMutex<Vec<Arc<EffectRecord>>>,
    alive: SyncBool,
    closing: SyncBool,
    applies: Arc<AtomicUsize>,
    cleanups: Arc<AtomicUsize>,
    handle: tokio::runtime::Handle,
    declared: Vec<TypeKey>,
}

impl ConsumerInner {
    fn state(&self) -> FiberState {
        self.transition.lock().unwrap().state
    }
}

/// A consumer handle: the inner state plus the driver's mailbox sender, so
/// dropping every handle (after the driver is done) closes the mailbox.
struct Consumer {
    inner: Arc<ConsumerInner>,
    intents: mpsc::UnboundedSender<Intent>,
}

/// The registry, the admission lock `load`/`unload` take, the one key and
/// the runtime handle the model's cleanups would use.
struct DepWorld {
    key: TypeKey,
    registry: DepRegistry,
    admission: SyncMutex<()>,
    handle: tokio::runtime::Handle,
}

fn set_consumer_state(tr: &mut ConsumerTrans, new: FiberState) {
    if tr.state != new {
        tr.state = new;
    }
}

fn consumer_stopped_error(inner: &ConsumerInner) -> Option<Arc<ErrorKind>> {
    inner.transition.lock().unwrap().error.clone().or_else(|| {
        inner
            .closing
            .load(Ordering::SeqCst)
            .then(|| Arc::new(ErrorKind::Closed))
    })
}

/// `FiberInner::post` for the model's consumer.
fn post_consumer(consumer: &Arc<Consumer>, intent: Intent) -> bool {
    let inner = &consumer.inner;
    if !inner.alive.load(Ordering::SeqCst) {
        complete_intent(&intent, consumer_stopped_error(inner));
        return false;
    }
    match consumer.intents.send(intent) {
        Ok(()) => true,
        Err(error) => {
            complete_intent(&error.0, consumer_stopped_error(inner));
            false
        }
    }
}

fn new_consumer(world: &DepWorld) -> (Arc<Consumer>, mpsc::UnboundedReceiver<Intent>) {
    let (intents, mailbox) = mpsc::unbounded_channel();
    let inner = Arc::new(ConsumerInner {
        transition: SyncMutex::new(ConsumerTrans {
            state: FiberState::Pending,
            generation: 0,
            terminal_task: None,
            pending_restarts: 0,
            pre_cancelled: false,
            error: None,
        }),
        last_deps: SyncMutex::new(None),
        effects: SyncMutex::new(Vec::new()),
        alive: SyncBool::new(true),
        closing: SyncBool::new(false),
        applies: Arc::new(AtomicUsize::new(0)),
        cleanups: Arc::new(AtomicUsize::new(0)),
        handle: world.handle.clone(),
        declared: vec![world.key.clone()],
    });
    (Arc::new(Consumer { inner, intents }), mailbox)
}

/// `FiberInner::resolve_deps`: the declared keys whose gate is satisfied,
/// and those that are not. The model has one key, one scope and no instance
/// checks; every key it declares is the same one.
fn resolve_deps(world: &DepWorld, inner: &ConsumerInner) -> (HashSet<DepQuad>, Vec<TypeKey>) {
    let mut satisfied = HashSet::new();
    let mut missing = Vec::new();
    for key in &inner.declared {
        match world.registry.resolve_dep(key) {
            Some((provider, generation)) => {
                satisfied.insert((provider, generation, key.clone(), None));
            }
            None => missing.push(key.clone()),
        }
    }
    (satisfied, missing)
}

/// What `unload` moves to.
enum ConsumerNext {
    Pending,
    Disposed,
}

/// `drain_effects` for the model: take the list, drain each record (claim →
/// run → finish), clear `last_deps`, and return the cleanup errors.
fn drain_consumer_effects(inner: &Arc<ConsumerInner>) -> Vec<Arc<ErrorKind>> {
    let effects: Vec<Arc<EffectRecord>> = std::mem::take(&mut *inner.effects.lock().unwrap());
    let mut errors = Vec::new();
    for record in effects.into_iter().rev() {
        if let Some(mut cleanups) = record.claim() {
            let error = block_on(run_cleanups(&mut cleanups, &inner.handle));
            record.finish(error);
        }
        if let Err(error) = block_on(record.join()) {
            errors.push(error);
        }
    }
    *inner.last_deps.lock().unwrap() = None;
    errors
}

/// `FiberInner::unload`: `Unloading` under the admission and transition
/// locks, cancel (unobservable here), the event wait (no calls are counted),
/// the cleanup pass, then `Pending` or `Disposed`.
fn unload(world: &DepWorld, inner: &Arc<ConsumerInner>, next: ConsumerNext) {
    {
        let _admission = world.admission.lock().unwrap();
        let mut tr = inner.transition.lock().unwrap();
        set_consumer_state(&mut tr, FiberState::Unloading);
    }
    let errors = drain_consumer_effects(inner);
    let mut tr = inner.transition.lock().unwrap();
    match next {
        ConsumerNext::Pending => set_consumer_state(&mut tr, FiberState::Pending),
        ConsumerNext::Disposed => {
            tr.error = crate::error::aggregate_arcs(errors);
            set_consumer_state(&mut tr, FiberState::Disposed);
        }
    }
}

/// `FiberInner::load`, mirrored where the two gate checks and the
/// `last_deps` write are: the state change and the new generation under the
/// admission and transition locks, then `last_deps` **before** the second
/// resolve, then `apply` (the model runs it synchronously, with a scheduling
/// point where user code could await), then the fresh-deps adoption and
/// `Active`. The first resolve uses the dependencies passed in (what
/// `refresh_deps` resolved); the second reads the registry again.
fn load(world: &DepWorld, inner: &Arc<ConsumerInner>, deps: HashSet<DepQuad>) {
    let _admission = world.admission.lock().unwrap();
    if inner.closing.load(Ordering::SeqCst) {
        return;
    }
    let generation = {
        let mut tr = inner.transition.lock().unwrap();
        if tr.terminal_task.is_some() {
            return;
        }
        tr.pre_cancelled = tr.pending_restarts > 0;
        tr.generation += 1;
        tr.error = None;
        set_consumer_state(&mut tr, FiberState::Loading);
        tr.generation
    };
    drop(_admission);
    // `#620`'s protocol: the write and the resolve are ordered so that a
    // withdrawal (`mark_removing` then `consumers_of`) and this load see each
    // other — either it finds this fiber in `last_deps`, or the second
    // resolve below sees the removal.
    *inner.last_deps.lock().unwrap() = Some(deps.clone());
    let (now, missing) = resolve_deps(world, inner);
    if !missing.is_empty() || now != deps {
        unload(world, inner, ConsumerNext::Pending);
        return;
    }
    inner.applies.fetch_add(1, Ordering::SeqCst);
    let record = EffectRecord::from(
        Effect::Disposer(Box::new({
            let cleanups = inner.cleanups.clone();
            move || {
                cleanups.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })),
        format!("model apply generation {generation}"),
        Origin::Apply,
        Weak::new(),
    );
    inner.effects.lock().unwrap().push(record);
    // `apply` is user code and may await here.
    switch_point();
    if inner.closing.load(Ordering::SeqCst) {
        unload(world, inner, ConsumerNext::Disposed);
        return;
    }
    let (fresh, missing) = resolve_deps(world, inner);
    if missing.is_empty() {
        *inner.last_deps.lock().unwrap() = Some(fresh);
    }
    {
        let mut tr = inner.transition.lock().unwrap();
        tr.error = None;
        set_consumer_state(&mut tr, FiberState::Active);
    }
}

/// `FiberInner::refresh_deps`: the gate, the equal-set merge, and the unload
/// back to `Pending` before a new load.
fn refresh_deps(world: &DepWorld, inner: &Arc<ConsumerInner>) {
    if inner.closing.load(Ordering::SeqCst) {
        return;
    }
    let state = inner.transition.lock().unwrap().state;
    if state == FiberState::Disposed {
        return;
    }
    if inner.transition.lock().unwrap().terminal_task.is_some() {
        unload(world, inner, ConsumerNext::Disposed);
        return;
    }
    let (satisfied, missing) = resolve_deps(world, inner);
    let state = inner.transition.lock().unwrap().state;
    let loaded = !matches!(state, FiberState::Pending | FiberState::Disposed);
    if !missing.is_empty() {
        if matches!(state, FiberState::Active | FiberState::Loading) {
            unload(world, inner, ConsumerNext::Pending);
        }
        return;
    }
    let unchanged = {
        let last = inner.last_deps.lock().unwrap();
        last.as_ref() == Some(&satisfied)
    };
    if unchanged {
        return;
    }
    if loaded {
        unload(world, inner, ConsumerNext::Pending);
    }
    load(world, inner, satisfied);
}

fn complete_consumer_task(inner: &ConsumerInner, task: Arc<TransitionTask>) {
    let error = inner.transition.lock().unwrap().error.clone();
    task.complete(error);
}

/// The consumer's driver: one intent at a time, so a load and an unload
/// never overlap and the notifications queue in the mailbox order.
fn drive_consumer(
    world: Arc<DepWorld>,
    inner: Arc<ConsumerInner>,
    mut mailbox: mpsc::UnboundedReceiver<Intent>,
) {
    while let Some(intent) = block_on(mailbox.recv()) {
        match intent {
            Intent::RefreshDeps => refresh_deps(&world, &inner),
            Intent::RefreshDepsJoin(task) => {
                refresh_deps(&world, &inner);
                complete_consumer_task(&inner, task);
            }
            Intent::Settle(task) => {
                let error = {
                    let tr = inner.transition.lock().unwrap();
                    (tr.state == FiberState::Failed)
                        .then(|| tr.error.clone())
                        .flatten()
                };
                task.complete(error);
            }
            Intent::Dispose => {
                // The non-root `Dispose` arm: unload to `Disposed`, stop the
                // driver, drain what is queued (a later `Settle` reports no
                // failure; every other join gets the terminal error).
                if inner.state() != FiberState::Disposed {
                    unload(&world, &inner, ConsumerNext::Disposed);
                }
                inner.alive.store(false, Ordering::SeqCst);
                let error = inner.transition.lock().unwrap().error.clone();
                while let Ok(intent) = mailbox.try_recv() {
                    let completion = if matches!(intent, Intent::Settle(_)) {
                        None
                    } else {
                        error.clone()
                    };
                    complete_intent(&intent, completion);
                }
                return;
            }
            _ => {}
        }
    }
}

fn pre_cancel(inner: &ConsumerInner) {
    inner.transition.lock().unwrap().pre_cancelled = true;
}

/// `evict_and_finalize`, mirrored: publish the provider inactive (its
/// `unload` does that before the provide effect runs), mark the binding
/// removing, find the consumers bound to exactly this quad, pre-cancel them,
/// have each re-check (`RefreshDepsJoin`) and join them, notify the rest,
/// then remove the binding only if the slot still holds it. The model has
/// no `provided` table and no scopes (the quad's scope is always `None`).
fn withdraw(
    world: &Arc<DepWorld>,
    provider: &Arc<DepProvider>,
    binding: &Arc<DepBinding>,
    removal: &Notify,
) {
    provider.set(FiberState::Unloading);
    world.registry.mark_removing_if(&world.key, binding);
    removal.notify_one();
    let old_quad = (provider.id, binding.provider_gen, world.key.clone(), None);
    let consumers = world.registry.consumers_of(&world.key, old_quad);
    let mut tasks = Vec::new();
    for consumer in &consumers {
        pre_cancel(&consumer.inner);
        let task = TransitionTask::new();
        // `post_join` returns false once the driver stopped; the task is
        // then complete already, and joining it still returns.
        post_join_to(
            &consumer.inner.alive,
            &consumer.intents,
            task.clone(),
            Intent::RefreshDepsJoin,
            || consumer_stopped_error(&consumer.inner),
        );
        tasks.push(task);
    }
    // A scheduling point between finding the consumers and notifying the
    // rest: the kernel runs these as separate steps and can be preempted.
    switch_point();
    world.registry.notify(&world.key);
    for task in tasks {
        let _ = block_on(join_task(&task));
    }
    world.registry.finalize_if(&world.key, binding);
}

/// The provider's next generation provides the same key again. Like `load`'s
/// provide path, the binding appears while the provider is still `Loading`,
/// and the notify follows `Active`: consumers resolve only an active
/// provider.
fn provide(
    world: &Arc<DepWorld>,
    provider: &Arc<DepProvider>,
    provider_gen: u64,
) -> Arc<DepBinding> {
    provider.set(FiberState::Loading);
    let binding = world
        .registry
        .insert(&world.key, provider, provider_gen)
        .expect("the next generation provides the same key");
    provider.set(FiberState::Active);
    world.registry.notify(&world.key);
    binding
}

/// Model 9, dependency eviction (#622, Bug B / #620): a provider leaves
/// while its consumer is between `Pending` and `Loading`, and comes back
/// (replacing the removing binding) while the consumer is still settling.
/// The consumer must not still be running on the withdrawn generation when
/// the withdrawal returns, the withdrawal must not remove the replacement,
/// the consumer must reload on the returned generation, and each applied
/// generation's cleanup must run exactly once.
///
/// This is a reference model: the real gate window runs no plugin code and
/// touches std locks only (`FiberInner::load`'s `last_deps` write and
/// `resolve_deps`, `Registry`'s two locks), so shuttle has no switch point
/// inside it. The model runs `TransitionTask`, `post_join_to`, `join_task`,
/// `EffectRecord` and `complete_intent` from the kernel; `load`,
/// `refresh_deps`, `unload`, the registry methods and `evict_and_finalize`
/// are mirrored line for line with shuttle locks, which is what lets the
/// scheduler stop in that window. One key, one scope and one provider
/// generation per provide; the consumer has no dispose, events or child
/// fibers.
///
/// Mutations this model catches: dropping the second gate check in `load`
/// (a consumer that passed the gate with the old quad writes `last_deps`
/// after the withdrawal read it and still reaches `Active`; the check after
/// `withdraw` sees it running on the withdrawn generation), dropping the
/// consumer join loop in `withdraw` (same check, now with the consumer
/// legitimately found), and dropping the `removing` or provider-state check
/// in `resolve_dep` (the re-check stays `Active`, so the withdrawal returns
/// on a dead dependency). Run: removing the second gate check fails at the
/// default 1000 iterations with `the withdrawal returned while the consumer
/// still ran on the withdrawn generation`.
#[test]
fn shuttle_dependency_withdrawal_stops_the_loading_consumer() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");
    let handle = runtime.handle().clone();
    explore(move || {
        let key = TypeKey::of::<u32>();
        let world = Arc::new(DepWorld {
            key: key.clone(),
            registry: DepRegistry::new(),
            admission: SyncMutex::new(()),
            handle: handle.clone(),
        });
        let provider = Arc::new(DepProvider {
            id: PluginId(7),
            state: SyncMutex::new(FiberState::Active),
        });
        // The provider's first generation provides the key.
        provider.set(FiberState::Loading);
        let first = world
            .registry
            .insert(&key, &provider, 1)
            .expect("the first provide");
        provider.set(FiberState::Active);

        let (consumer, mailbox) = new_consumer(&world);
        world.registry.register_inject(&key, &consumer);
        let driver = {
            let (world, inner) = (world.clone(), consumer.inner.clone());
            thread::spawn(move || drive_consumer(world, inner, mailbox))
        };
        // The mount posts the first refresh.
        post_consumer(&consumer, Intent::RefreshDeps);

        let removal = Arc::new(Notify::new());
        let leaver = {
            let (world, provider, first, consumer, removal) = (
                world.clone(),
                provider.clone(),
                first.clone(),
                consumer.clone(),
                removal.clone(),
            );
            thread::spawn(move || {
                switch_point();
                withdraw(&world, &provider, &first, &removal);
                let state = consumer.inner.state();
                let stale = {
                    let last = consumer.inner.last_deps.lock().unwrap();
                    last.as_ref().is_some_and(|deps| {
                        deps.contains(&(provider.id, 1, world.key.clone(), None))
                    })
                };
                assert!(
                    !(state == FiberState::Active && stale),
                    "the withdrawal returned while the consumer still ran on the \
                     withdrawn generation"
                );
            })
        };
        let returner = {
            let (world, provider, removal) = (world.clone(), provider.clone(), removal.clone());
            thread::spawn(move || {
                // `notify_one`'s permit is kept when it fires before the wait,
                // so the return never starts before the removal is marked.
                block_on(removal.notified());
                provide(&world, &provider, 2)
            })
        };

        leaver.join().unwrap();
        let returned = returner.join().unwrap();

        // Settle after every notification the two threads posted: the
        // consumer handles what is queued before the barrier.
        let settle = TransitionTask::new();
        let sent = post_join_to(
            &consumer.inner.alive,
            &consumer.intents,
            settle.clone(),
            Intent::Settle,
            || consumer_stopped_error(&consumer.inner),
        );
        assert!(sent, "the consumer's driver was not alive");
        assert!(block_on(join_task(&settle)).is_ok());

        let inner = consumer.inner.clone();
        let applies = inner.applies.load(Ordering::SeqCst);
        let cleanups = inner.cleanups.load(Ordering::SeqCst);
        assert!(applies >= 1, "the consumer never loaded");
        assert_eq!(
            inner.state(),
            FiberState::Active,
            "the consumer did not reload on the returned generation"
        );
        // The generation that is still Active has not been unloaded: its
        // record is still in the list and its cleanup is not due. Every
        // earlier generation's cleanup ran exactly once.
        assert_eq!(
            cleanups,
            applies - 1,
            "an unloaded generation's cleanup did not run exactly once ({cleanups} \
             cleanups for {applies} applies, one still active)"
        );
        assert_eq!(
            inner.effects.lock().unwrap().len(),
            1,
            "the active generation's record is not the only one left"
        );
        let expected: HashSet<DepQuad> =
            std::iter::once((provider.id, 2, key.clone(), None)).collect();
        assert_eq!(
            inner.last_deps.lock().unwrap().as_ref(),
            Some(&expected),
            "the consumer is not bound to the returned generation"
        );
        let now = world.registry.lookup(&key).expect("the returned binding");
        assert!(
            Arc::ptr_eq(&now, &returned),
            "the withdrawal removed or replaced the new binding"
        );
        assert_eq!(now.provider_gen, 2);
        assert!(!now.removing.load(Ordering::SeqCst));

        // Dispose the loaded generation too: its cleanup must run exactly
        // once, and the record list must end empty. The driver handles
        // `Dispose` and exits (the non-root `drive` arm), so the settle that
        // follows may complete during the drain.
        post_consumer(&consumer, Intent::Dispose);
        let settle = TransitionTask::new();
        let _ = post_join_to(
            &inner.alive,
            &consumer.intents,
            settle.clone(),
            Intent::Settle,
            || consumer_stopped_error(&inner),
        );
        assert!(block_on(join_task(&settle)).is_ok());
        drop(consumer);
        driver.join().unwrap();
        assert_eq!(
            inner.state(),
            FiberState::Disposed,
            "the dispose did not end"
        );
        assert_eq!(
            inner.cleanups.load(Ordering::SeqCst),
            applies,
            "the active generation's cleanup did not run exactly once"
        );
        assert!(
            inner.effects.lock().unwrap().is_empty(),
            "a generation's effect record was left behind"
        );
    });
}
