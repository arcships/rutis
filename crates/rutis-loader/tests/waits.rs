//! `Loader::waits`: what a waiting row waits for, and why each service is
//! not there (#676): a misspelt name, a provider that failed or is
//! disabled, two rows that inject each other.
//! guarantee: loader/wait-words
#![cfg(feature = "runtimes")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rutis::host::{CtxHost, ErrorKind, ErrorSink, FiberState};
use rutis::{Ctx, Error};
use rutis_bridge::session::{host_key, HostDispatch, Reply, Value as RpcValue};
use rutis_loader::report::{CycleLink, EntryInfo, EntryStatus, Wait, WaitReason};
use rutis_loader::resolve::Declared;
use rutis_loader::status::describe_waits;
use rutis_loader::{Builtins, Layer, Loader, LoaderOptions, LoaderPlugin, Patch};
use rutis_test_support::{Guard, HANG_GUARD};
use serde_json::{json, Value};

/// A service that answers nothing.
struct Nothing;

impl HostDispatch for Nothing {
    fn invoke(&self, _: &str, _: RpcValue) -> Reply {
        Ok(RpcValue::Undefined)
    }
}

/// Provides each name in its config's `provides`, or fails when `fail`.
struct Provider {
    provides: Vec<String>,
    fail: bool,
}

impl rutis::Plugin for Provider {
    type Deps = ();

    fn name(&self) -> &str {
        "provider"
    }

    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        if self.fail {
            return Err(Error::msg("the model is gone"));
        }
        for name in &self.provides {
            ctx.provide_at(host_key(name), Arc::new(Nothing) as Arc<dyn HostDispatch>)?;
        }
        Ok(())
    }
}

fn names(config: &Value, field: &str) -> Vec<String> {
    config[field]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect()
}

/// A loader with the plugin `provider`, which declares what its config
/// says it provides; every name is shared by name, as in rutis-host.
async fn loader() -> (Ctx, Loader) {
    loader_in(Ctx::root().unwrap()).await
}

/// [`loader`] mounted at `root`.
async fn loader_in(root: Ctx) -> (Ctx, Loader) {
    let mut builtins = Builtins::new();
    builtins.register("provider", |config: &Value| {
        Ok(Provider {
            provides: names(config, "provides"),
            fail: config["fail"] == true,
        })
    });
    builtins.declare(
        "provider",
        Arc::new(|config: &Value| {
            Ok(Declared {
                provides: names(config, "provides"),
                announces: config["announces"] == true,
                ..Declared::default()
            })
        }),
    );
    // The same plugin under a name only a disabled row uses: never
    // resolved, as a disabled runtime row's module is not.
    builtins.register("unused", |config: &Value| {
        Ok(Provider {
            provides: names(config, "provides"),
            fail: false,
        })
    });
    builtins.declare(
        "unused",
        Arc::new(|config: &Value| {
            Ok(Declared {
                provides: names(config, "provides"),
                ..Declared::default()
            })
        }),
    );
    let plugin = LoaderPlugin::new(builtins, LoaderOptions::default());
    let loader = plugin.handle();
    root.plugin(plugin).await.unwrap();
    (root, loader)
}

fn layer(rows: Value) -> Vec<Layer> {
    let patches: Vec<Patch> = serde_json::from_value(json!([{ "insert": rows }])).unwrap();
    vec![Layer::new("rows", patches)]
}

fn entry(loader: &Loader, id: &str) -> EntryInfo {
    loader
        .entries()
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

/// What `id` waits for, once it waits and `ready` holds of it.
async fn waits(loader: &Loader, id: &str, ready: impl Fn(&[Wait]) -> bool) -> Vec<Wait> {
    Guard(HANG_GUARD)
        .eventually_with(
            || {
                let entry = entry(loader, id);
                let pending = matches!(
                    &entry.status,
                    EntryStatus::Running(snapshot) if snapshot.state == FiberState::Pending
                );
                Some(loader.waits(&entry)).filter(|waits| pending && ready(waits))
            },
            &format!("{id} to wait"),
            || {
                format!(
                    "{:?}: {:?}",
                    entry(loader, id),
                    loader.waits(&entry(loader, id))
                )
            },
        )
        .await
}

fn reasons(waits: &[Wait]) -> Vec<(&str, &WaitReason)> {
    waits
        .iter()
        .map(|wait| (wait.service.as_str(), &wait.reason))
        .collect()
}

/// A name no row provides, close to one a row does: `did you mean`.
/// guarantee: host/row-state-lines
#[tokio::test(flavor = "multi_thread")]
async fn a_misspelt_name_is_waited_for_with_the_name_it_meant() {
    let (root, loader) = loader().await;
    let rows = json!([
        { "id": "llm", "name": "provider", "config": { "provides": ["llm"] } },
        { "id": "weather", "name": "provider", "inject": ["lmm"] },
        { "id": "store", "name": "provider", "inject": ["blob"] },
    ]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let weather = waits(&loader, "weather", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&weather),
        [(
            "lmm",
            &WaitReason::NoProvider {
                like: Some("llm".into())
            }
        )]
    );
    let store = waits(&loader, "store", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&store),
        [("blob", &WaitReason::NoProvider { like: None })]
    );
    root.shutdown().await.unwrap();
}

/// The row that provides the service failed, or is disabled: named, with
/// its error.
/// guarantee: host/row-state-lines
#[tokio::test(flavor = "multi_thread")]
async fn a_provider_that_failed_or_is_disabled_is_named() {
    let (root, loader) = loader().await;
    let rows = json!([
        { "id": "model", "name": "provider", "config": { "provides": ["llm"], "fail": true } },
        { "id": "weather", "name": "provider", "inject": ["llm"] },
        { "id": "mailer", "name": "provider", "disabled": true, "config": { "provides": ["mail"] } },
        { "id": "reader", "name": "provider", "inject": ["mail"] },
    ]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let weather = waits(&loader, "weather", |waits| {
        waits
            .iter()
            .any(|wait| matches!(wait.reason, WaitReason::ProviderFailed { .. }))
    })
    .await;
    match &weather[..] {
        [Wait {
            service,
            reason: WaitReason::ProviderFailed { row, error },
            ..
        }] => {
            assert_eq!((service.as_str(), row.as_str()), ("llm", "model"));
            assert!(error.contains("the model is gone"), "{error}");
        }
        other => panic!("{other:?}"),
    }
    let reader = waits(&loader, "reader", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&reader),
        [(
            "mail",
            &WaitReason::ProviderDisabled {
                row: "mailer".into()
            }
        )]
    );
    root.shutdown().await.unwrap();
}

/// Two rows that inject each other's services: each says so.
/// guarantee: host/row-state-lines
#[tokio::test(flavor = "multi_thread")]
async fn rows_that_inject_each_other_say_so() {
    let (root, loader) = loader().await;
    let rows = json!([
        { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"] } },
        { "id": "b", "name": "provider", "inject": ["asvc"], "config": { "provides": ["bsvc"] } },
        { "id": "c", "name": "provider", "inject": ["asvc"] },
    ]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let a = waits(&loader, "a", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&a),
        [(
            "bsvc",
            &WaitReason::Cycle {
                row: "b".into(),
                service: "asvc".into()
            }
        )]
    );
    // A third row waiting on one of them: that one waits too.
    let c = waits(&loader, "c", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&c),
        [(
            "asvc",
            &WaitReason::ProviderWaiting {
                row: "a".into(),
                services: vec!["bsvc".into()]
            }
        )]
    );
    root.shutdown().await.unwrap();
}

/// A long wait's report (the kernel's, for an application embedding the
/// loader) names what a row waits for as rows do: the service, not the
/// key's type.
/// guarantee: core/features.still-waiting
#[tokio::test(flavor = "multi_thread")]
async fn a_long_wait_is_reported_by_service_name() {
    let sunk: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink: ErrorSink = {
        let sunk = sunk.clone();
        Arc::new(move |error: Arc<ErrorKind>| {
            if matches!(*error, ErrorKind::StillWaiting { .. }) {
                sunk.lock().unwrap().push(error.to_string());
            }
        })
    };
    let root = Ctx::root_with_sink(tokio::runtime::Handle::current(), sink);
    root.report_waiting_after(Some(Duration::from_millis(100)));
    let (root, loader) = loader_in(root).await;
    let rows = json!([{ "id": "weather", "name": "provider", "inject": ["lmm"] }]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let said = Guard(HANG_GUARD)
        .eventually_with(
            || sunk.lock().unwrap().first().cloned(),
            "the report",
            || format!("{:?}", sunk.lock().unwrap()),
        )
        .await;
    assert_eq!(said, "provider: still waiting after 0.1s for lmm");
    root.shutdown().await.unwrap();
}

/// A row that cannot start reaches the error sink by its row name (stderr
/// in an embedding, by default): one whose plugin failed, from the kernel,
/// and one refused before it ran (its plugin unknown), from the loader;
/// each once, not again when nothing changed.
/// guarantee: core/features.default-output
#[tokio::test(flavor = "multi_thread")]
async fn a_row_that_cannot_start_reaches_the_error_sink_by_its_name() {
    let sunk: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink: ErrorSink = {
        let sunk = sunk.clone();
        Arc::new(move |error: Arc<ErrorKind>| {
            if matches!(*error, ErrorKind::StartFailed { .. }) {
                sunk.lock().unwrap().push(error.to_string());
            }
        })
    };
    let root = Ctx::root_with_sink(tokio::runtime::Handle::current(), sink);
    let (root, loader) = loader_in(root).await;
    let rows = json!([
        { "id": "broken", "name": "provider", "config": { "fail": true } },
        { "id": "missing", "name": "nope" },
    ]);
    loader.reconcile(layer(rows.clone()), None).await.unwrap();
    loader.reconcile(layer(rows), None).await.unwrap();
    let mut said = sunk.lock().unwrap().clone();
    said.sort();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].starts_with("broken: failed: "), "{said:?}");
    assert!(said[1].starts_with("missing: failed: "), "{said:?}");
    assert!(said[1].contains("nope"), "{said:?}");
    root.shutdown().await.unwrap();
}

/// A disabled row that was never resolved may provide what a row waits
/// for: not "no row provides it", but that no enabled row does. A disabled
/// row whose plugin is known (another row resolved it) is named.
/// guarantee: host/row-state-lines
#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_row_not_resolved_is_not_taken_for_no_provider() {
    let (root, loader) = loader().await;
    let rows = json!([
        { "id": "off", "name": "unused", "disabled": true, "config": { "provides": ["mail"] } },
        { "id": "reader", "name": "provider", "inject": ["mail"] },
    ]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let reader = waits(&loader, "reader", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&reader),
        [("mail", &WaitReason::NoEnabledProvider { like: None })]
    );
    root.shutdown().await.unwrap();
}

fn formatted(id: &str, waits: &[Wait]) -> String {
    format!("{id}: {}", describe_waits(waits, &|_| None))
}

/// 2-row cycle keeps the exact existing `WaitReason::Cycle` variant and
/// verbatim `loader/wait-words` wording.
#[tokio::test(flavor = "multi_thread")]
async fn two_row_cycle_keeps_existing_reason_and_wording() {
    let (root, loader) = loader().await;
    let rows = json!([
        { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"] } },
        { "id": "b", "name": "provider", "inject": ["asvc"], "config": { "provides": ["bsvc"] } },
    ]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let a = waits(&loader, "a", |waits| !waits.is_empty()).await;
    let b = waits(&loader, "b", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&a),
        [(
            "bsvc",
            &WaitReason::Cycle {
                row: "b".into(),
                service: "asvc".into(),
            }
        )]
    );
    assert_eq!(
        formatted("a", &a),
        "a: waiting for bsvc (row b waits for asvc too: they inject each other)"
    );
    assert_eq!(
        formatted("b", &b),
        "b: waiting for asvc (row a waits for bsvc too: they inject each other)"
    );
    root.shutdown().await.unwrap();
}

/// 3-row cycle (`a -> b -> c -> a`): each row on the cycle reports
/// `WaitReason::CycleChain` starting from itself, with the exact verbatim
/// wording from the design doc's "What developers see".
#[tokio::test(flavor = "multi_thread")]
async fn three_row_cycle_reports_cycle_chain_with_exact_wording() {
    let (root, loader) = loader().await;
    let rows = json!([
        { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"] } },
        { "id": "b", "name": "provider", "inject": ["csvc"], "config": { "provides": ["bsvc"] } },
        { "id": "c", "name": "provider", "inject": ["asvc"], "config": { "provides": ["csvc"] } },
    ]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let a = waits(&loader, "a", |waits| !waits.is_empty()).await;
    let b = waits(&loader, "b", |waits| !waits.is_empty()).await;
    let c = waits(&loader, "c", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&a),
        [(
            "bsvc",
            &WaitReason::CycleChain {
                links: vec![
                    CycleLink::new("a", "bsvc", "b"),
                    CycleLink::new("b", "csvc", "c"),
                    CycleLink::new("c", "asvc", "a"),
                ],
            }
        )]
    );
    assert_eq!(
        formatted("a", &a),
        "a: waiting for bsvc (rows a, b, c wait for each other: \
         a waits for bsvc from b, b waits for csvc from c, c waits for asvc from a; \
         break the cycle in their inject)"
    );
    assert_eq!(
        formatted("b", &b),
        "b: waiting for csvc (rows b, c, a wait for each other: \
         b waits for csvc from c, c waits for asvc from a, a waits for bsvc from b; \
         break the cycle in their inject)"
    );
    assert_eq!(
        formatted("c", &c),
        "c: waiting for asvc (rows c, a, b wait for each other: \
         c waits for asvc from a, a waits for bsvc from b, b waits for csvc from c; \
         break the cycle in their inject)"
    );
    root.shutdown().await.unwrap();
}

/// Lead-in row waiting on a provider that itself is part of a downstream
/// 3-row cycle: traversal terminates without looping, the lead-in row is
/// NOT accused of being in the cycle (`ProviderWaiting`), while the cycle
/// members each report `CycleChain`.
#[tokio::test(flavor = "multi_thread")]
async fn lead_in_row_waiting_on_a_downstream_cycle_stays_provider_waiting() {
    let (root, loader) = loader().await;
    let rows = json!([
        { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"] } },
        { "id": "b", "name": "provider", "inject": ["csvc"], "config": { "provides": ["bsvc"] } },
        { "id": "c", "name": "provider", "inject": ["asvc"], "config": { "provides": ["csvc"] } },
        { "id": "d", "name": "provider", "inject": ["asvc"] },
    ]);
    loader.reconcile(layer(rows), None).await.unwrap();
    let d = waits(&loader, "d", |waits| !waits.is_empty()).await;
    let a = waits(&loader, "a", |waits| !waits.is_empty()).await;
    assert_eq!(
        reasons(&d),
        [(
            "asvc",
            &WaitReason::ProviderWaiting {
                row: "a".into(),
                services: vec!["bsvc".into()],
            }
        )]
    );
    assert_eq!(
        formatted("d", &d),
        "d: waiting for asvc (row a waits too, for bsvc)"
    );
    assert!(
        matches!(a.as_slice(), [Wait { reason: WaitReason::CycleChain { links }, .. }] if links.len() == 3),
        "{a:?}"
    );
    root.shutdown().await.unwrap();
}

/// Direct wait and multi-hop wait chain hitting an unknown / unannounced
/// provider (a Pending row with `announces: true`): never misreported as a
/// proved cycle, and names the wait chain and unstarted candidate row.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_unannounced_provider_reports_direct_and_chained_waits() {
    // Case 1: dynamic deadlock candidate (`a` provides `asvc` and waits for
    // undeclared `bsvc`; `b` injects `asvc` and has `announces: true`).
    {
        let (root, loader) = loader().await;
        let rows = json!([
            { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"] } },
            { "id": "b", "name": "provider", "inject": ["asvc"], "config": { "announces": true } },
        ]);
        loader.reconcile(layer(rows), None).await.unwrap();
        let a = waits(&loader, "a", |waits| !waits.is_empty()).await;
        let b = waits(&loader, "b", |waits| !waits.is_empty()).await;
        assert_eq!(
            reasons(&a),
            [(
                "bsvc",
                &WaitReason::UnknownProvider {
                    chain: vec![],
                    missing: "bsvc".into(),
                    candidates: vec!["b".into()],
                }
            )]
        );
        assert_eq!(
            formatted("a", &a),
            "a: waiting for bsvc (nothing declares it yet; \
             unstarted row b may provide services once it runs; \
             declare provides or check why b has not started)"
        );
        assert_eq!(
            reasons(&b),
            [(
                "asvc",
                &WaitReason::UnknownProvider {
                    chain: vec![CycleLink::new("b", "asvc", "a")],
                    missing: "bsvc".into(),
                    candidates: vec!["b".into()],
                }
            )]
        );
        assert_eq!(
            formatted("b", &b),
            "b: waiting for asvc (b waits for asvc from a, a waits for bsvc; \
             nothing declares bsvc yet; \
             this row has not started and may provide services once it runs; \
             declare provides on this row or break the inject chain)"
        );
        root.shutdown().await.unwrap();
    }

    // Case 2: self-candidate alongside another unstarted announcing row `u`
    // (`b` sees `unstarted rows this row, u ...`).
    {
        let (root, loader) = loader().await;
        let rows = json!([
            { "id": "u", "name": "provider", "inject": ["other"], "config": { "announces": true } },
            { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"] } },
            { "id": "b", "name": "provider", "inject": ["asvc"], "config": { "announces": true } },
        ]);
        loader.reconcile(layer(rows), None).await.unwrap();
        let b = waits(&loader, "b", |waits| !waits.is_empty()).await;
        assert_eq!(
            reasons(&b),
            [(
                "asvc",
                &WaitReason::UnknownProvider {
                    chain: vec![CycleLink::new("b", "asvc", "a")],
                    missing: "bsvc".into(),
                    candidates: vec!["b".into(), "u".into()],
                }
            )]
        );
        assert_eq!(
            formatted("b", &b),
            "b: waiting for asvc (b waits for asvc from a, a waits for bsvc; \
             nothing declares bsvc yet; \
             unstarted rows this row, u may provide services once they run; \
             declare provides or check why they have not started)"
        );
        root.shutdown().await.unwrap();
    }

    // Case 3: 3-row chain (`c -> asvc -> a -> bsvc -> b -> storage`) with an
    // unstarted announcing row `u` (waiting on `asvc`), plus an unrelated
    // announcing row `z` — `u` ranks before `z` because `u` reaches `b`.
    {
        let (root, loader) = loader().await;
        let rows = json!([
            { "id": "z", "name": "provider", "inject": ["other"], "config": { "announces": true } },
            { "id": "c", "name": "provider", "inject": ["asvc"] },
            { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"] } },
            { "id": "b", "name": "provider", "inject": ["storage"], "config": { "provides": ["bsvc"] } },
            { "id": "u", "name": "provider", "inject": ["asvc"], "config": { "announces": true } },
        ]);
        loader.reconcile(layer(rows), None).await.unwrap();
        let c = waits(&loader, "c", |waits| !waits.is_empty()).await;
        assert_eq!(
            reasons(&c),
            [(
                "asvc",
                &WaitReason::UnknownProvider {
                    chain: vec![
                        CycleLink::new("c", "asvc", "a"),
                        CycleLink::new("a", "bsvc", "b"),
                    ],
                    missing: "storage".into(),
                    candidates: vec!["u".into(), "z".into()],
                }
            )]
        );
        assert_eq!(
            formatted("c", &c),
            "c: waiting for asvc (c waits for asvc from a, a waits for bsvc from b, b waits for storage; \
             nothing declares storage yet; \
             unstarted rows u, z may provide services once they run; \
             declare provides or check why they have not started)"
        );
        root.shutdown().await.unwrap();
    }
}

/// Plugin that stays in `FiberState::Loading` until `gate` is notified.
struct SlowProvider {
    provides: Vec<String>,
    started: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Notify>,
}

impl rutis::Plugin for SlowProvider {
    type Deps = ();

    fn name(&self) -> &str {
        "slow"
    }

    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        self.started.notify_waiters();
        self.gate.notified().await;
        for name in &self.provides {
            ctx.provide_at(host_key(name), Arc::new(Nothing) as Arc<dyn HostDispatch>)?;
        }
        Ok(())
    }
}

/// Ordinary non-cycle waits produce zero false cycles and zero false
/// `UnknownProvider` reports:
/// - normal startup chain where the downstream provider is `Loading`,
/// - ordinary missing service when an `announces: true` row is already `Active`,
/// - misspelt name (`did you mean`) even when another Pending row has `announces: true`,
/// - cross-`isolate` rows declaring each other's names in different scopes.
#[tokio::test(flavor = "multi_thread")]
async fn ordinary_waits_and_cross_isolate_rows_produce_no_false_positives() {
    // 1. Normal startup wait (`a -> bsvc -> b -> csvc -> c`, with `c` Loading).
    {
        let started = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let root = Ctx::root().unwrap();
        let mut builtins = Builtins::new();
        builtins.register("provider", |config: &Value| {
            Ok(Provider {
                provides: names(config, "provides"),
                fail: config["fail"] == true,
            })
        });
        builtins.declare(
            "provider",
            Arc::new(|config: &Value| {
                Ok(Declared {
                    provides: names(config, "provides"),
                    announces: config["announces"] == true,
                    ..Declared::default()
                })
            }),
        );
        {
            let started = started.clone();
            let gate = gate.clone();
            builtins.register("slow", move |config: &Value| {
                Ok(SlowProvider {
                    provides: names(config, "provides"),
                    started: started.clone(),
                    gate: gate.clone(),
                })
            });
        }
        builtins.declare(
            "slow",
            Arc::new(|config: &Value| {
                Ok(Declared {
                    provides: names(config, "provides"),
                    announces: true,
                    ..Declared::default()
                })
            }),
        );
        let plugin = LoaderPlugin::new(builtins, LoaderOptions::default());
        let loader = plugin.handle();
        root.plugin(plugin).await.unwrap();

        let rows = json!([
            { "id": "c", "name": "slow", "config": { "provides": ["csvc"] } },
            { "id": "b", "name": "provider", "inject": ["csvc"], "config": { "provides": ["bsvc"], "announces": true } },
            { "id": "a", "name": "provider", "inject": ["bsvc"], "config": { "provides": ["asvc"], "announces": true } },
        ]);
        let loader_bg = loader.clone();
        let reconciling = tokio::spawn(async move { loader_bg.reconcile(layer(rows), None).await });
        started.notified().await;
        let b = waits(&loader, "b", |waits| {
            waits
                .iter()
                .any(|w| matches!(w.reason, WaitReason::ProviderStarting { .. }))
        })
        .await;
        let a = waits(&loader, "a", |waits| !waits.is_empty()).await;
        assert_eq!(
            formatted("b", &b),
            "b: waiting for csvc (row c is starting)"
        );
        assert_eq!(
            reasons(&a),
            [(
                "bsvc",
                &WaitReason::ProviderWaiting {
                    row: "b".into(),
                    services: vec!["csvc".into()],
                }
            )]
        );
        assert_eq!(
            formatted("a", &a),
            "a: waiting for bsvc (row b waits too, for csvc)"
        );
        gate.notify_waiters();
        reconciling.await.unwrap().unwrap();
        root.shutdown().await.unwrap();
    }

    // 2. Ordinary missing service when an `announces: true` row is already Active,
    //    plus a misspelt service name even alongside an unstarted announcing row.
    {
        let (root, loader) = loader().await;
        let rows = json!([
            { "id": "llm", "name": "provider", "config": { "provides": ["llm"], "announces": true } },
            { "id": "store", "name": "provider", "inject": ["blob"], "config": { "announces": true } },
            { "id": "weather", "name": "provider", "inject": ["lmm"] },
        ]);
        loader.reconcile(layer(rows), None).await.unwrap();
        let store = waits(&loader, "store", |waits| !waits.is_empty()).await;
        let weather = waits(&loader, "weather", |waits| !waits.is_empty()).await;
        assert_eq!(
            reasons(&store),
            [("blob", &WaitReason::NoProvider { like: None })]
        );
        assert_eq!(
            formatted("store", &store),
            "store: waiting for blob (nothing provides it yet)"
        );
        assert_eq!(
            reasons(&weather),
            [(
                "lmm",
                &WaitReason::NoProvider {
                    like: Some("llm".into()),
                }
            )]
        );
        assert_eq!(
            formatted("weather", &weather),
            "weather: waiting for lmm (nothing provides it yet; did you mean llm?)"
        );
        root.shutdown().await.unwrap();
    }

    // 3. Cross-isolate rows: `a` isolates `bsvc` privately, so `b` (outside
    //    that isolate) cannot provide `bsvc` to `a` — must NOT report a cycle.
    {
        let (root, loader) = loader().await;
        let rows = json!([
            { "id": "a", "name": "provider", "inject": ["bsvc"], "isolate": { "bsvc": true }, "config": { "provides": ["asvc"] } },
            { "id": "b", "name": "provider", "inject": ["asvc"], "config": { "provides": ["bsvc"] } },
        ]);
        loader.reconcile(layer(rows), None).await.unwrap();
        let a = waits(&loader, "a", |waits| !waits.is_empty()).await;
        let b = waits(&loader, "b", |waits| !waits.is_empty()).await;
        assert_eq!(
            reasons(&a),
            [("bsvc", &WaitReason::NoProvider { like: None })]
        );
        assert_eq!(
            formatted("a", &a),
            "a: waiting for bsvc (nothing provides it yet)"
        );
        assert_eq!(
            reasons(&b),
            [(
                "asvc",
                &WaitReason::ProviderWaiting {
                    row: "a".into(),
                    services: vec!["bsvc".into()],
                }
            )]
        );
        assert_eq!(
            formatted("b", &b),
            "b: waiting for asvc (row a waits too, for bsvc)"
        );
        root.shutdown().await.unwrap();
    }
}
