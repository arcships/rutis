//! Managed timer helpers (`Ctx::timeout`, `Ctx::interval`) built on `effect`
//! (#1325, docs/design-managed-timers-2026-11-20.en.md).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rutis::host::{CtxHost, ErrorKind, ErrorSink};
use rutis::{Ctx, Error, Plugin};

fn collecting_sink() -> (ErrorSink, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let out = lines.clone();
    let sink: ErrorSink = Arc::new(move |error| {
        out.lock().unwrap().push(error.to_string());
    });
    (sink, lines)
}

#[tokio::test]
async fn timeout_fires_once_normally() {
    let root = Ctx::root().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(tokio::sync::Notify::new());

    let h = hits.clone();
    let n = done.clone();
    root.timeout(Duration::from_millis(15), move || async move {
        h.fetch_add(1, Ordering::SeqCst);
        n.notify_one();
        Ok(())
    })
    .unwrap();

    tokio::time::timeout(Duration::from_secs(2), done.notified())
        .await
        .expect("timeout should fire");
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeout_cancelled_early_does_not_fire() {
    let root = Ctx::root().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));

    let h = hits.clone();
    let cancel = root
        .timeout(Duration::from_millis(30), move || async move {
            h.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();

    cancel.dispose().await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeout_does_not_fire_when_plugin_unloads_before_delay() {
    struct Delayed(Arc<AtomicUsize>);
    impl Plugin for Delayed {
        type Deps = ();
        async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
            let hits = self.0.clone();
            ctx.timeout(Duration::from_millis(40), move || async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })?;
            Ok(())
        }
    }

    let root = Ctx::root().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let view = root.plugin(Delayed(hits.clone()));
    (&view).await.unwrap();

    view.dispose().await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn interval_fires_multiple_times_and_stops_on_unload() {
    struct Heartbeat {
        hits: Arc<AtomicUsize>,
        reached: Arc<tokio::sync::Notify>,
    }
    impl Plugin for Heartbeat {
        type Deps = ();
        async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
            let hits = self.hits.clone();
            let reached = self.reached.clone();
            ctx.interval(Duration::from_millis(12), move || {
                let hits = hits.clone();
                let reached = reached.clone();
                async move {
                    if hits.fetch_add(1, Ordering::SeqCst) + 1 >= 3 {
                        reached.notify_one();
                    }
                    Ok(())
                }
            })?;
            Ok(())
        }
    }

    let root = Ctx::root().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let reached = Arc::new(tokio::sync::Notify::new());
    let view = root.plugin(Heartbeat {
        hits: hits.clone(),
        reached: reached.clone(),
    });
    (&view).await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), reached.notified())
        .await
        .expect("interval should fire at least 3 times");
    view.dispose().await.unwrap();

    let count_at_unload = hits.load(Ordering::SeqCst);
    assert!(count_at_unload >= 3);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        count_at_unload,
        "zero fires after unload"
    );
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn interval_does_not_overlap_slow_async_callbacks() {
    let root = Ctx::root().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(tokio::sync::Notify::new());

    let a = active.clone();
    let m = max_active.clone();
    let f = finished.clone();
    let d = done.clone();
    let stop = root
        .interval(Duration::from_millis(10), move || {
            let a = a.clone();
            let m = m.clone();
            let f = f.clone();
            let d = d.clone();
            async move {
                let now = a.fetch_add(1, Ordering::SeqCst) + 1;
                m.fetch_max(now, Ordering::SeqCst);
                // Callback takes much longer than the 10 ms period.
                tokio::time::sleep(Duration::from_millis(35)).await;
                a.fetch_sub(1, Ordering::SeqCst);
                if f.fetch_add(1, Ordering::SeqCst) + 1 >= 3 {
                    d.notify_one();
                }
                Ok(())
            }
        })
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), done.notified())
        .await
        .expect("slow interval ticks should complete");
    stop.dispose().await.unwrap();

    assert_eq!(
        max_active.load(Ordering::SeqCst),
        1,
        "interval callbacks must never overlap"
    );
    assert!(finished.load(Ordering::SeqCst) >= 3);
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn interval_rejects_zero_period() {
    let root = Ctx::root().unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let flag = ran.clone();
    let error = root
        .interval(Duration::ZERO, move || {
            flag.store(true, Ordering::SeqCst);
            async { Ok(()) }
        })
        .unwrap_err();
    assert!(
        matches!(error.kind(), Some(ErrorKind::Validation { .. })),
        "expected Validation, got {error:?}"
    );
    assert_eq!(
        error.to_string(),
        r#"config validation failed: ["interval period must be greater than zero"]"#
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!ran.load(Ordering::SeqCst));
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn timer_registration_during_unload_and_after_dispose_is_refused() {
    struct RefusalProbe {
        kept: Arc<Mutex<Option<Ctx>>>,
        during_timeout: Arc<Mutex<Option<String>>>,
        during_interval: Arc<Mutex<Option<String>>>,
        ran: Arc<AtomicBool>,
    }
    impl Plugin for RefusalProbe {
        type Deps = ();
        async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
            *self.kept.lock().unwrap() = Some(ctx.clone());
            let c = ctx.clone();
            let during_timeout = self.during_timeout.clone();
            let during_interval = self.during_interval.clone();
            let ran = self.ran.clone();
            ctx.effect(move || async move {
                let r1 = ran.clone();
                let err1 = c
                    .timeout(Duration::from_millis(1), move || async move {
                        r1.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                    .unwrap_err();
                assert!(matches!(err1.kind(), Some(ErrorKind::InactiveEffect)));
                *during_timeout.lock().unwrap() = Some(err1.to_string());

                let r2 = ran.clone();
                let err2 = c
                    .interval(Duration::from_millis(1), move || {
                        let r2 = r2.clone();
                        async move {
                            r2.store(true, Ordering::SeqCst);
                            Ok(())
                        }
                    })
                    .unwrap_err();
                assert!(matches!(err2.kind(), Some(ErrorKind::InactiveEffect)));
                *during_interval.lock().unwrap() = Some(err2.to_string());
                Ok(())
            })?;
            Ok(())
        }
    }

    let root = Ctx::root().unwrap();
    let kept = Arc::new(Mutex::new(None));
    let during_timeout = Arc::new(Mutex::new(None));
    let during_interval = Arc::new(Mutex::new(None));
    let ran = Arc::new(AtomicBool::new(false));
    let view = root.plugin(RefusalProbe {
        kept: kept.clone(),
        during_timeout: during_timeout.clone(),
        during_interval: during_interval.clone(),
        ran: ran.clone(),
    });
    (&view).await.unwrap();
    view.dispose().await.unwrap();

    assert_eq!(
        during_timeout.lock().unwrap().as_deref(),
        Some("fiber disposed")
    );
    assert_eq!(
        during_interval.lock().unwrap().as_deref(),
        Some("fiber disposed")
    );

    // Registration after dispose is equally refused without starting any timer.
    let ctx = kept.lock().unwrap().take().unwrap();
    let r3 = ran.clone();
    let after_err = ctx
        .timeout(Duration::from_millis(1), move || async move {
            r3.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(after_err.kind(), Some(ErrorKind::InactiveEffect)));
    assert_eq!(after_err.to_string(), "fiber disposed");

    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!ran.load(Ordering::SeqCst), "refused timers must never run");
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn timer_callback_error_and_panic_are_contained_and_reported_to_error_sink() {
    struct Faulty {
        ticks: Arc<AtomicUsize>,
        done: Arc<tokio::sync::Notify>,
    }
    impl Plugin for Faulty {
        type Deps = ();
        fn name(&self) -> &str {
            "heartbeat"
        }
        async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
            ctx.timeout(Duration::from_millis(5), || async {
                Err(Error::msg("connection refused"))
            })?;
            let ticks = self.ticks.clone();
            let done = self.done.clone();
            ctx.interval(Duration::from_millis(10), move || {
                let ticks = ticks.clone();
                let done = done.clone();
                async move {
                    let n = ticks.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 1 {
                        panic!("boom in tick 1");
                    }
                    if n == 2 {
                        return Err(Error::msg("transient failure"));
                    }
                    done.notify_one();
                    Ok(())
                }
            })?;
            Ok(())
        }
    }

    let (sink, reported) = collecting_sink();
    let root = Ctx::root_with_sink(tokio::runtime::Handle::current(), sink);
    let ticks = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(tokio::sync::Notify::new());
    let view = root.plugin(Faulty {
        ticks: ticks.clone(),
        done: done.clone(),
    });
    (&view).await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), done.notified())
        .await
        .expect("interval should recover and reach tick 3 after panic and error");
    view.dispose().await.unwrap();
    root.shutdown().await.unwrap();

    let lines = reported.lock().unwrap().clone();
    assert!(
        lines.contains(&"heartbeat: timer failed: connection refused".to_owned()),
        "missing timeout error report: {lines:?}"
    );
    assert!(
        lines.contains(&"heartbeat: timer failed: boom in tick 1".to_owned()),
        "missing panic report: {lines:?}"
    );
    assert!(
        lines.contains(&"heartbeat: timer failed: transient failure".to_owned()),
        "missing interval error report: {lines:?}"
    );
    assert!(ticks.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn unload_cancels_and_awaits_inflight_timer_drop_before_earlier_lifo_cleanup() {
    struct PoolGuard {
        pool_open: Arc<AtomicBool>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Drop for PoolGuard {
        fn drop(&mut self) {
            let tag = if self.pool_open.load(Ordering::SeqCst) {
                "timer.dropped-with-open-pool"
            } else {
                "timer.USED-CLOSED-POOL"
            };
            self.events.lock().unwrap().push(tag);
        }
    }

    struct Worker {
        pool_open: Arc<AtomicBool>,
        events: Arc<Mutex<Vec<&'static str>>>,
        started: Arc<tokio::sync::Notify>,
    }
    impl Plugin for Worker {
        type Deps = ();
        async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
            let pool_open = self.pool_open.clone();
            let events = self.events.clone();
            ctx.effect(move || async move {
                pool_open.store(false, Ordering::SeqCst);
                events.lock().unwrap().push("pool.close");
                Ok(())
            })?;

            let pool_open = self.pool_open.clone();
            let events = self.events.clone();
            let started = self.started.clone();
            ctx.interval(Duration::from_millis(5), move || {
                let pool_open = pool_open.clone();
                let events = events.clone();
                let started = started.clone();
                async move {
                    let _guard = PoolGuard { pool_open, events };
                    started.notify_one();
                    std::future::pending::<()>().await;
                    Ok(())
                }
            })?;
            Ok(())
        }
    }

    let root = Ctx::root().unwrap();
    let pool_open = Arc::new(AtomicBool::new(true));
    let events = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());

    let view = root.plugin(Worker {
        pool_open: pool_open.clone(),
        events: events.clone(),
        started: started.clone(),
    });
    (&view).await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("timer callback should enter async wait");

    // Unload drops the in-flight future (running PoolGuard::drop) and waits
    // for the timer task to exit BEFORE running the earlier pool.close effect.
    view.dispose().await.unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        vec!["timer.dropped-with-open-pool", "pool.close"]
    );
    root.shutdown().await.unwrap();
}
