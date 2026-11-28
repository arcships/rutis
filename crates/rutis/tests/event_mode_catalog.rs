//! PoC tests for the optional event mode catalog (#1324,
//! `docs/design-event-mode-catalog-2026-11-20.md`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rutis::events::Mode;
use rutis::host::{
    BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, ErrorSink, EventModeCatalog, EventModePolicy,
    FiberView,
};
use rutis::internal::events::{Dispatch, Payload};
use rutis::internal::{CtxInternal, EventKey};
use rutis::{Ctx, Error, Event};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Message {
    user: String,
    text: String,
}

impl Event for Message {
    const NAME: &'static str = "message";
    type Value = bool;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Unlisted(String);

impl Event for Unlisted {
    const NAME: &'static str = "user/joined";
    type Value = String;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Dynamic {
    event_name: String,
    payload: String,
}

impl Event for Dynamic {
    const NAME: &'static str = "";
    type Value = bool;

    fn name(&self) -> &str {
        &self.event_name
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ModeEvent {
    event_name: String,
}

impl Event for ModeEvent {
    const NAME: &'static str = "";
    type Value = String;

    fn name(&self) -> &str {
        &self.event_name
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FrameworkPing;

impl Event for FrameworkPing {
    const NAME: &'static str = "rutis/test-ping";
    type Value = ();
}

struct Apply<F>(&'static str, F);

impl<F> DynPlugin for Apply<F>
where
    F: Fn(&Ctx) -> Result<(), Error> + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.0
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, Error>> {
        Box::pin(async move { (self.1)(ctx).map(|()| Effect::Done) })
    }
}

async fn child(parent: &Ctx, name: &'static str) -> (FiberView, Ctx) {
    let slot: Arc<Mutex<Option<Ctx>>> = Arc::default();
    let out = slot.clone();
    let view = parent.plugin(Apply(name, move |ctx: &Ctx| {
        *out.lock().unwrap() = Some(ctx.clone());
        Ok(())
    }));
    (&view).await.unwrap();
    let ctx = slot.lock().unwrap().take().unwrap();
    (view, ctx)
}

fn root_with_sink() -> (Ctx, Arc<Mutex<Vec<Arc<ErrorKind>>>>) {
    let errors: Arc<Mutex<Vec<Arc<ErrorKind>>>> = Arc::default();
    let out = errors.clone();
    let sink: ErrorSink = Arc::new(move |error| out.lock().unwrap().push(error));
    (
        Ctx::root_with_sink(tokio::runtime::Handle::current(), sink),
        errors,
    )
}

fn sink_texts(errors: &Mutex<Vec<Arc<ErrorKind>>>) -> Vec<String> {
    errors
        .lock()
        .unwrap()
        .iter()
        .map(|error| error.to_string())
        .collect()
}

/// Blacklist scenario under default `Warn` policy: `message` is catalogued as
/// `bail`, a blacklist listener returns `true`, and `chat` dispatches with
/// `emit`. Existing dispatch behavior stays intact (listener still runs), and
/// the exact verbatim diagnostic is reported once per `(emitter, event, actual,
/// expected)`.
#[tokio::test]
async fn blacklist_warn_reports_verbatim_and_keeps_emit_behavior() {
    let (root, errors) = root_with_sink();
    root.set_event_mode_catalog(EventModeCatalog::new().expect("message", Mode::Bail))
        .unwrap();

    let (_blacklist_view, blacklist) = child(&root, "blacklist").await;
    let (_chat_view, chat) = child(&root, "chat").await;

    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    blacklist
        .on_sync::<Message>(move |msg, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Some(msg.user == "mallory"))
        })
        .unwrap();

    // `chat` mistakenly emits `message`: dispatch still proceeds so existing
    // plugins do not break, and ErrorSink receives the verbatim warning.
    chat.emit(Message {
        user: "mallory".into(),
        text: "spam".into(),
    })
    .unwrap();
    rutis_test_support::until(|| calls.load(Ordering::SeqCst) == 1, "listener ran").await;

    assert_eq!(
        sink_texts(&errors),
        ["chat: event message dispatched with emit, expected bail: use bail instead"]
    );
    assert!(matches!(
        &*errors.lock().unwrap()[0],
        ErrorKind::EventModeMismatch {
            name,
            actual: Mode::Emit,
            expected: Mode::Bail,
            emitter,
        } if name == "message" && emitter == "chat"
    ));

    // A second mismatched emit from `chat` is deduplicated while the catalog is
    // unchanged, and still runs the listener.
    chat.emit(Message {
        user: "mallory".into(),
        text: "more spam".into(),
    })
    .unwrap();
    rutis_test_support::until(|| calls.load(Ordering::SeqCst) == 2, "second emit ran").await;
    assert_eq!(errors.lock().unwrap().len(), 1);

    // Another emitter (`the host` root) still gets its own warning.
    root.emit(Message {
        user: "mallory".into(),
        text: "from host".into(),
    })
    .unwrap();
    rutis_test_support::until(|| calls.load(Ordering::SeqCst) == 3, "host emit ran").await;
    assert_eq!(
        sink_texts(&errors),
        [
            "chat: event message dispatched with emit, expected bail: use bail instead",
            "the host: event message dispatched with emit, expected bail: use bail instead",
        ]
    );

    // Dispatching with a different wrong mode (`serial`) from `chat` also warns
    // once for that `(emitter, event, actual, expected)` tuple.
    let intercepted = chat
        .serial(Message {
            user: "mallory".into(),
            text: "via serial".into(),
        })
        .await
        .unwrap();
    assert_eq!(intercepted, Some(true));
    assert_eq!(
        sink_texts(&errors)[2],
        "chat: event message dispatched with serial, expected bail: use bail instead"
    );

    root.shutdown().await.unwrap();
}

/// Blacklist scenario under strict `Error` policy: mismatched dispatch is
/// rejected with `ErrorKind::EventModeMismatch` before any listener runs,
/// while the matching mode succeeds and returns the blacklist decision.
#[tokio::test]
async fn blacklist_error_rejects_mismatched_dispatch_before_listeners_run() {
    let (root, errors) = root_with_sink();
    root.set_event_mode_catalog(
        EventModeCatalog::new()
            .with_policy(EventModePolicy::Error)
            .expect("message", Mode::Bail),
    )
    .unwrap();

    let (_blacklist_view, blacklist) = child(&root, "blacklist").await;
    let (_chat_view, chat) = child(&root, "chat").await;

    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    blacklist
        .on_sync::<Message>(move |msg, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Some(msg.user == "mallory"))
        })
        .unwrap();

    let err = chat
        .emit(Message {
            user: "mallory".into(),
            text: "spam".into(),
        })
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "chat: event message dispatched with emit, expected bail: use bail instead"
    );
    assert_eq!(err.wire_name(), Some("BindingError"));
    assert!(matches!(
        err.kind(),
        Some(ErrorKind::EventModeMismatch {
            name,
            actual: Mode::Emit,
            expected: Mode::Bail,
            emitter,
        }) if name == "message" && emitter == "chat"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "listener must not run");
    assert!(errors.lock().unwrap().is_empty(), "error went to caller");

    // Repeated mismatched dispatches are always rejected (never deduplicated).
    let second = chat
        .emit(Message {
            user: "mallory".into(),
            text: "spam again".into(),
        })
        .unwrap_err();
    assert!(matches!(
        second.kind(),
        Some(ErrorKind::EventModeMismatch { .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Other wrong modes (`parallel`, `serial`, `waterfall`) are rejected too.
    let err = chat
        .parallel(Message {
            user: "mallory".into(),
            text: "parallel".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "chat: event message dispatched with parallel, expected bail: use bail instead"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Using the expected mode (`bail`) succeeds and blocks the message.
    let blocked = chat
        .bail(Message {
            user: "mallory".into(),
            text: "spam".into(),
        })
        .unwrap();
    assert_eq!(blocked, Some(true));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(errors.lock().unwrap().is_empty());

    root.shutdown().await.unwrap();
}

/// Matching mode succeeds with zero warnings across all five dispatch modes,
/// including both asynchronous and synchronous waterfalls.
#[tokio::test]
async fn matching_mode_succeeds_with_zero_warnings_for_all_five_modes() {
    let (root, errors) = root_with_sink();
    root.set_event_mode_catalog(
        EventModeCatalog::new()
            .with_policy(EventModePolicy::Error)
            .expect("ev/emit", Mode::Emit)
            .expect("ev/parallel", Mode::Parallel)
            .expect("ev/serial", Mode::Serial)
            .expect("ev/waterfall", Mode::Waterfall)
            .expect("ev/bail", Mode::Bail),
    )
    .unwrap();

    let emit_seen = Arc::new(AtomicUsize::new(0));
    let seen = emit_seen.clone();
    root.listen::<ModeEvent>()
        .name("ev/emit")
        .on_sync(move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
        .unwrap();
    root.listen::<ModeEvent>()
        .name("ev/parallel")
        .on(|_, _| async { Ok(None) })
        .unwrap();
    root.listen::<ModeEvent>()
        .name("ev/serial")
        .on(|_, _| async { Ok(Some("serial-ok".into())) })
        .unwrap();
    root.listen::<ModeEvent>()
        .name("ev/waterfall")
        .on_sync(|_, call| {
            if call.mode() == Mode::Waterfall {
                Ok(Some("wf-ok".into()))
            } else {
                Ok(None)
            }
        })
        .unwrap();
    root.listen::<ModeEvent>()
        .name("ev/bail")
        .on_sync(|_, _| Ok(Some("bail-ok".into())))
        .unwrap();

    root.emit(ModeEvent {
        event_name: "ev/emit".into(),
    })
    .unwrap();
    rutis_test_support::until(|| emit_seen.load(Ordering::SeqCst) == 1, "emit ran").await;
    root.parallel(ModeEvent {
        event_name: "ev/parallel".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        root.serial(ModeEvent {
            event_name: "ev/serial".into()
        })
        .await
        .unwrap(),
        Some("serial-ok".into())
    );
    assert_eq!(
        root.waterfall(ModeEvent {
            event_name: "ev/waterfall".into()
        })
        .await
        .unwrap(),
        Some("wf-ok".into())
    );
    assert_eq!(
        root.waterfall(ModeEvent {
            event_name: "ev/waterfall".into()
        })
        .sync(|_| Ok(Some("terminal".into())))
        .unwrap(),
        Some("wf-ok".into())
    );
    assert_eq!(
        root.bail(ModeEvent {
            event_name: "ev/bail".into()
        })
        .unwrap(),
        Some("bail-ok".into())
    );
    assert!(errors.lock().unwrap().is_empty());

    root.shutdown().await.unwrap();
}

/// Unlisted static events and unlisted dynamic event names (including names
/// sharing a prefix with a catalogued event) produce zero warnings and zero
/// errors. A dynamic event whose runtime name matches a catalogued entry IS
/// checked.
#[tokio::test]
async fn unlisted_and_dynamic_events_have_zero_false_positives() {
    let (root, errors) = root_with_sink();
    root.set_event_mode_catalog(
        EventModeCatalog::new()
            .with_policy(EventModePolicy::Error)
            .expect("message", Mode::Bail),
    )
    .unwrap();

    // Unlisted typed event works in every mode.
    root.emit(Unlisted("a".into())).unwrap();
    root.parallel(Unlisted("b".into())).await.unwrap();
    root.serial(Unlisted("c".into())).await.unwrap();
    root.waterfall(Unlisted("d".into())).await.unwrap();
    root.bail(Unlisted("e".into())).unwrap();

    // Dynamic event names not in the catalog (even sharing a prefix like
    // `message/deleted`) produce zero warnings or errors.
    root.emit(Dynamic {
        event_name: "room/42".into(),
        payload: "hi".into(),
    })
    .unwrap();
    root.emit(Dynamic {
        event_name: "message/deleted".into(),
        payload: "id-1".into(),
    })
    .unwrap();
    assert!(errors.lock().unwrap().is_empty());

    // When a dynamic event's runtime name equals a catalogued name, it is
    // checked like any other dispatch.
    let err = root
        .emit(Dynamic {
            event_name: "message".into(),
            payload: "oops".into(),
        })
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the host: event message dispatched with emit, expected bail: use bail instead"
    );

    root.shutdown().await.unwrap();
}

/// Per-event policy overrides, catalog replacement resetting warning dedup,
/// and clearing the catalog with an empty `EventModeCatalog::new()`.
#[tokio::test]
async fn per_event_policy_and_catalog_replacement() {
    let (root, errors) = root_with_sink();
    let catalog = EventModeCatalog::new()
        .expect("message", Mode::Serial)
        .expect_with("http/request", Mode::Waterfall, EventModePolicy::Error);
    root.set_event_mode_catalog(catalog.clone()).unwrap();
    assert_eq!(root.event_mode_catalog(), catalog);

    // `message` inherits `Warn`; `http/request` overrides to `Error`.
    root.emit(Message {
        user: "alice".into(),
        text: "hi".into(),
    })
    .unwrap();
    assert_eq!(
        sink_texts(&errors),
        ["the host: event message dispatched with emit, expected serial: use serial instead"]
    );

    let err = root
        .parallel(ModeEvent {
            event_name: "http/request".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the host: event http/request dispatched with parallel, expected waterfall: use waterfall instead"
    );

    // Re-installing the catalog resets warning deduplication.
    root.set_event_mode_catalog(catalog).unwrap();
    root.emit(Message {
        user: "alice".into(),
        text: "again".into(),
    })
    .unwrap();
    assert_eq!(errors.lock().unwrap().len(), 2);

    // Clearing the catalog disables all checks.
    root.set_event_mode_catalog(EventModeCatalog::new())
        .unwrap();
    root.parallel(ModeEvent {
        event_name: "http/request".into(),
    })
    .await
    .unwrap();
    assert_eq!(errors.lock().unwrap().len(), 2);

    root.shutdown().await.unwrap();
}

/// Raw `EventBus::dispatch` / `dispatch_sync` (the path used by all six
/// language runtimes and Cordis plugins) respects `Dispatch.emitter` and the
/// catalog; framework events (`emit_framework`) bypass the catalog.
#[tokio::test]
async fn raw_dispatch_emitter_override_and_framework_bypass() {
    let (root, errors) = root_with_sink();
    root.set_event_mode_catalog(EventModeCatalog::new().expect("message", Mode::Bail))
        .unwrap();

    let mut raw = Dispatch::new(
        "message",
        Mode::Emit,
        Payload::new(Arc::new(Message {
            user: "mallory".into(),
            text: "from cordis".into(),
        })),
    );
    raw.emitter = Some("cordis-chat".into());
    root.events().dispatch(&root, raw).await.unwrap();
    assert_eq!(
        sink_texts(&errors),
        ["cordis-chat: event message dispatched with emit, expected bail: use bail instead"]
    );

    // Marked row context names the row without needing `Dispatch.emitter`.
    let (_row_view, row_ctx) = child(&root, "runtime-plugin").await;
    row_ctx.mark_row("py-bot").unwrap();
    row_ctx
        .emit(Message {
            user: "mallory".into(),
            text: "from row".into(),
        })
        .unwrap();
    assert_eq!(
        sink_texts(&errors)[1],
        "py-bot: event message dispatched with emit, expected bail: use bail instead"
    );

    // Framework events bypass the catalog.
    let key = EventKey::<FrameworkPing>::of();
    root.events()
        .emit_framework(&root, &key, Arc::new(FrameworkPing))
        .unwrap();
    assert_eq!(errors.lock().unwrap().len(), 2);

    root.shutdown().await.unwrap();
}

/// Empty, `rutis/*`, and `internal/*` event names are rejected by
/// `set_event_mode_catalog`, leaving any previously installed catalog intact.
#[tokio::test]
async fn invalid_catalog_entries_are_rejected_and_keep_existing_catalog() {
    let (root, _errors) = root_with_sink();
    let valid = EventModeCatalog::new()
        .with_policy(EventModePolicy::Error)
        .expect("message", Mode::Bail);
    root.set_event_mode_catalog(valid.clone()).unwrap();

    let invalid = EventModeCatalog::new()
        .expect("", Mode::Emit)
        .expect("rutis/row", Mode::Emit)
        .expect("internal/dispatch", Mode::Emit);
    let err = root.set_event_mode_catalog(invalid).unwrap_err();
    match err.kind() {
        Some(ErrorKind::Validation { issues }) => {
            assert_eq!(
                issues,
                &[
                    "events: event name cannot be empty",
                    "events.internal/dispatch: internal/* events stay inside their runtime and cannot be configured here",
                    "events.rutis/row: rutis/* events are owned by the host and cannot be configured here",
                ]
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }

    // Previously installed valid catalog remains active.
    assert_eq!(root.event_mode_catalog(), valid);
    assert!(root
        .emit(Message {
            user: "mallory".into(),
            text: "still strict".into(),
        })
        .is_err());

    root.shutdown().await.unwrap();
}

/// Mismatches are reported or rejected even when zero listeners are currently
/// mounted for the catalogued event.
#[tokio::test]
async fn mismatch_is_caught_with_zero_listeners() {
    let (root, errors) = root_with_sink();
    root.set_event_mode_catalog(EventModeCatalog::new().expect("message", Mode::Serial))
        .unwrap();

    root.emit(Message {
        user: "mallory".into(),
        text: "no listeners yet".into(),
    })
    .unwrap();
    assert_eq!(
        sink_texts(&errors),
        ["the host: event message dispatched with emit, expected serial: use serial instead"]
    );

    root.set_event_mode_catalog(
        EventModeCatalog::new()
            .with_policy(EventModePolicy::Error)
            .expect("message", Mode::Serial),
    )
    .unwrap();
    let err = root
        .emit(Message {
            user: "mallory".into(),
            text: "still no listeners".into(),
        })
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the host: event message dispatched with emit, expected serial: use serial instead"
    );

    root.shutdown().await.unwrap();
}

/// Multi-threaded concurrency stress test: concurrent `set_event_mode_catalog`
/// calls (alternating strict non-empty and empty catalogs) racing with
/// concurrent `emit` dispatches must never leave the fast-path flag out of sync
/// with `event_mode_catalog().is_empty()`, and a subsequent fresh `Warn` epoch
/// must deduplicate cleanly without double or lost warnings.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_catalog_swaps_and_emits_stay_consistent() {
    use std::sync::Barrier;

    let (root, errors) = root_with_sink();
    let strict = EventModeCatalog::new()
        .with_policy(EventModePolicy::Error)
        .expect("message", Mode::Bail);
    let warn = EventModeCatalog::new()
        .with_policy(EventModePolicy::Warn)
        .expect("message", Mode::Bail);
    let empty = EventModeCatalog::new();

    for round in 0..25 {
        let barrier = Arc::new(Barrier::new(6));
        std::thread::scope(|scope| {
            // 3 writer threads rapidly alternating strict / empty (and warn on
            // odd rounds) catalogs.
            for tid in 0..3 {
                let root = root.clone();
                let barrier = barrier.clone();
                let strict = strict.clone();
                let warn = warn.clone();
                let empty = empty.clone();
                scope.spawn(move || {
                    barrier.wait();
                    for step in 0..100 {
                        let catalog = if round % 2 == 0 {
                            if (tid + step) % 2 == 0 {
                                strict.clone()
                            } else {
                                empty.clone()
                            }
                        } else if (tid + step) % 3 == 0 {
                            warn.clone()
                        } else if (tid + step) % 3 == 1 {
                            strict.clone()
                        } else {
                            empty.clone()
                        };
                        root.set_event_mode_catalog(catalog).unwrap();
                    }
                });
            }

            // 3 emitter threads concurrently dispatching mismatched `emit`s.
            for _ in 0..3 {
                let root = root.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..100 {
                        let res = root.emit(Message {
                            user: "mallory".into(),
                            text: "race".into(),
                        });
                        if let Err(err) = res {
                            assert!(matches!(
                                err.kind(),
                                Some(ErrorKind::EventModeMismatch {
                                    name,
                                    actual: Mode::Emit,
                                    expected: Mode::Bail,
                                    emitter,
                                }) if name == "message" && emitter == "the host"
                            ));
                        }
                    }
                });
            }
        });

        // Once writers are quiescent, `has_catalog` MUST agree with
        // `event_mode_catalog().is_empty()`: if the strict catalog won, a
        // mismatched emit must be rejected; if empty won, it must succeed.
        let installed = root.event_mode_catalog();
        let probe = root.emit(Message {
            user: "mallory".into(),
            text: "probe".into(),
        });
        if installed.is_empty() {
            assert!(probe.is_ok(), "empty catalog must allow emit");
        } else if installed.policy() == EventModePolicy::Error {
            assert!(
                matches!(
                    probe.unwrap_err().kind(),
                    Some(ErrorKind::EventModeMismatch { .. })
                ),
                "strict catalog must reject emit (has_catalog out of sync!)"
            );
        } else {
            assert!(probe.is_ok(), "warn catalog must allow emit");
        }
    }

    // Install a fresh `Warn` catalog and hammer it concurrently from multiple
    // threads: the mismatch must be reported exactly once in this epoch.
    errors.lock().unwrap().clear();
    root.set_event_mode_catalog(warn).unwrap();
    let barrier = Arc::new(Barrier::new(4));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let root = root.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..50 {
                    root.emit(Message {
                        user: "mallory".into(),
                        text: "warn-dedup".into(),
                    })
                    .unwrap();
                }
            });
        }
    });
    assert_eq!(
        sink_texts(&errors),
        ["the host: event message dispatched with emit, expected bail: use bail instead"]
    );

    root.shutdown().await.unwrap();
}
