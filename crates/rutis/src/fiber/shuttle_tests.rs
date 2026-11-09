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
//! `Entry::start`, `set_state` and `wait_events` are the kernel's too. Model 7
//! is the one reference model: its comment says why the slot bookkeeping
//! cannot be run here. Built with `--cfg rutis_shuttle`, `crate::sync` makes
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

use std::sync::atomic::{AtomicBool as StdBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use shuttle::future::block_on;
use shuttle::sync::atomic::{AtomicBool, AtomicUsize as ShuttleUsize};
use shuttle::thread;
use tokio::sync::mpsc;

use super::{
    complete_intent, join_task, mark_subtree_lineage, post_join_to, publish_subtree_closing,
    CloseTree, FiberInner, FiberState, Intent, TransitionTask,
};
use crate::bus::run::{Inv, SyncNext};
use crate::bus::table::{Body, Entry};
use crate::effect::{run_cleanups, Effect, EffectRecord};
use crate::error::{Error, ErrorKind};
use crate::events::{ErasedSync, ListenerInfo, Value};
use crate::host::CtxHost;
use crate::internal::CtxInternal;
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
