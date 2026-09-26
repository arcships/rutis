use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, PluginFactory, TypeKey};
use rutis_protocol::{
    contract::{FAMILY, VERSION},
    error::{ErrorCode, ProtocolError},
    factories::{StaticFactories, StaticFactory},
    frame::Peer,
    identity::{Activation, Sequence},
    lifecycle::*,
    managed::ManagedActivation,
    prepare::{digest, RuntimeKind},
    runner_image::{FactoryCatalog, RunnerCatalog},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;
const SCHEMA: &str = include_str!("../../../protocol/fixtures/plugin.config.json");
#[tokio::test]
async fn shared_lifecycle_hello_corpus_matches_both_sdks() {
    let corpus: Vec<Value> = serde_json::from_str(include_str!(
        "../../../protocol/fixtures/lifecycle-corpus.json"
    ))
    .unwrap();
    for case in corpus {
        let root = Ctx::root().unwrap();
        let runner = Runner::new(NativeProbeDriver {
            root: root.clone(),
            observed: Arc::new(Observed::default()),
            slow_cleanup: false,
            bad_services: false,
        });
        let result = runner.handler()("runtime/hello".into(), case["hello"].clone()).await;
        if case["error"].is_null() {
            assert!(result.is_ok(), "{}: {result:?}", case["name"]);
        } else {
            assert_eq!(
                serde_json::to_value(result.unwrap_err().code).unwrap(),
                case["error"],
                "{}",
                case["name"]
            );
        }
        for stopped in runner.close() {
            stopped.await.unwrap();
        }
        root.shutdown().await.unwrap();
    }
}
fn contract() -> FactoryCatalog {
    FactoryCatalog {
        config_sha256: digest(SCHEMA.as_bytes()),
        provides: BTreeMap::new(),
        requires: BTreeMap::new(),
    }
}
fn plan() -> Hello {
    Hello {
        protocol_family: FAMILY.into(),
        protocol_version: VERSION.into(),
        identity: RuntimeIdentity {
            runtime: "native".into(),
            epoch: Sequence(1),
            kind: RuntimeKind::RustRutis,
            framework_version: "0.3.0".into(),
            environment_sha256: digest(b"native-env"),
            code_sha256: digest(b"native-code"),
            capabilities: BTreeSet::new(),
        },
        members: ["slow", "fast"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    Member {
                        entry: Entry::Rust {
                            factory: "probe".into(),
                        },
                        config: json!({"label":name}),
                        config_schema: SCHEMA.into(),
                        contracts: contract(),
                    },
                )
            })
            .collect(),
    }
}
fn id(n: u64) -> Activation {
    Activation {
        runtime: "native".into(),
        epoch: Sequence(1),
        activation: Sequence(n),
    }
}

#[tokio::test]
async fn stop_handler_closes_native_context_before_its_confirmation_future_is_polled() {
    let root = Ctx::root().unwrap();
    let observed = Arc::new(Observed::default());
    let runner = Runner::new(NativeProbeDriver {
        root: root.clone(),
        observed: observed.clone(),
        slow_cleanup: false,
        bad_services: false,
    });
    runner
        .handle("runtime/hello", serde_json::to_value(plan()).unwrap())
        .await
        .unwrap();
    runner
        .handle(
            "plugin/start",
            json!({"instance":"fast","activation":id(1)}),
        )
        .await
        .unwrap();
    let original = observed.seen.lock().unwrap()[0].1.clone();
    let stopped = runner.handle("plugin/stop", json!({"activation":id(1)}));
    assert!(original.cancellation_token().is_cancelled());
    assert!(original.effect(|| Effect::Done).is_err());
    assert!(original.provide(1_u64).is_err());
    drop(stopped);
    runner
        .handle("plugin/stop", json!({"activation":id(1)}))
        .await
        .unwrap();
    root.shutdown().await.unwrap();
}
#[derive(Default)]
struct Observed {
    mounts: Mutex<Vec<(String, Activation)>>,
    seen: Mutex<Vec<(String, Ctx)>>,
    cleaned: Mutex<Vec<String>>,
    entered: Notify,
    load: Notify,
    cleanup_entered: Notify,
    cleanup: Notify,
}
struct Probe {
    label: String,
    observed: Arc<Observed>,
    slow: bool,
    slow_cleanup: bool,
}
impl Plugin for Probe {
    fn name(&self) -> &str {
        "native-lifecycle"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            self.observed
                .seen
                .lock()
                .unwrap()
                .push((self.label.clone(), ctx.clone()));
            let observed = self.observed.clone();
            let label = self.label.clone();
            let slow = self.slow_cleanup;
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        if slow {
                            observed.cleanup_entered.notify_one();
                            observed.cleanup.notified().await;
                        }
                        observed.cleaned.lock().unwrap().push(label);
                        Ok(())
                    })
                }))
            })?;
            if self.slow {
                self.observed.entered.notify_one();
                self.observed.load.notified().await;
            }
            Ok(Effect::Done)
        })
    }
}
struct NativeProbeDriver {
    root: Ctx,
    observed: Arc<Observed>,
    slow_cleanup: bool,
    bad_services: bool,
}
impl Driver for NativeProbeDriver {
    fn admit(&self, _: &Hello) -> rutis_protocol::error::Result<()> {
        Ok(())
    }
    fn mount(
        &self,
        request: MountRequest,
        admission: rutis_protocol::managed::ActivationGate,
    ) -> rutis_protocol::error::Result<Mounted> {
        let member = request.member;
        self.observed
            .mounts
            .lock()
            .unwrap()
            .push((request.instance, request.activation));
        let label = member.config["label"].as_str().unwrap().to_owned();
        let native = ManagedActivation::mount_gated(
            &self.root,
            Probe {
                slow: label == "slow",
                label,
                observed: self.observed.clone(),
                slow_cleanup: self.slow_cleanup,
            },
            admission,
        )
        .unwrap();
        let bad = self.bad_services;
        Ok(Mounted {
            native,
            services: Box::pin(async move {
                Ok(if bad {
                    [("undeclared".into(), Value::Null)].into_iter().collect()
                } else {
                    BTreeMap::new()
                })
            }),
        })
    }
}
fn connect(
    root: &Ctx,
    observed: Arc<Observed>,
    slow_cleanup: bool,
    bad_services: bool,
) -> (Arc<Runner>, Peer, Peer) {
    let runner = Runner::new(NativeProbeDriver {
        root: root.clone(),
        observed,
        slow_cleanup,
        bad_services,
    });
    let (a, b) = tokio::io::duplex(8192);
    let server = Peer::start(a, runner.handler());
    runner.attach(&server).unwrap();
    let client = Peer::start(b, Arc::new(|_, _| Box::pin(async { Ok(Value::Null) })));
    (runner, server, client)
}

#[tokio::test]
async fn runtime_ready_precedes_members_and_fast_member_publishes_while_sibling_loads() {
    let root = Ctx::root().unwrap();
    let observed = Arc::new(Observed::default());
    let (runner, server, peer) = connect(&root, observed.clone(), false, false);
    hello(&peer, &plan()).await.unwrap();
    assert!(observed.seen.lock().unwrap().is_empty());
    let slow = peer
        .start_request(
            "plugin/start",
            json!({"instance":"slow","activation":id(1)}),
        )
        .unwrap();
    observed.entered.notified().await;
    let ready = peer
        .request(
            "plugin/start",
            json!({"instance":"fast","activation":id(2)}),
        )
        .await
        .unwrap();
    assert_eq!(ready["services"], json!({}));
    assert!(observed
        .mounts
        .lock()
        .unwrap()
        .contains(&("fast".into(), id(2))));
    assert!(runner.require_published(&id(2)).is_err());
    peer.request("plugin/activate", json!({"activation":id(2)}))
        .await
        .unwrap();
    runner.require_published(&id(2)).unwrap();
    assert!(runner.require_published(&id(1)).is_err());
    let stop = peer
        .start_request("plugin/stop", json!({"activation":id(1)}))
        .unwrap();
    assert_eq!(
        peer.request("plugin/state", json!({"activation":id(1)}))
            .await
            .unwrap()["phase"],
        "closing"
    );
    let old = observed
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|(label, _)| label == "slow")
        .unwrap()
        .1
        .clone();
    assert!(old.cancellation_token().is_cancelled());
    observed.load.notify_one();
    assert!(slow.await.unwrap().is_err());
    assert_eq!(stop.await.unwrap().unwrap()["phase"], "stopped");
    assert!(peer
        .request("plugin/activate", json!({"activation":id(1)}))
        .await
        .is_err());
    assert!(peer
        .request(
            "plugin/start",
            json!({"instance":"slow","activation":id(1)})
        )
        .await
        .is_err());
    assert!(old.effect(|| Effect::Done).is_err());
    peer.request("runtime/stop", json!({})).await.unwrap();
    server.close(ProtocolError::new(ErrorCode::Unavailable, "test", "done"));
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_waiter_drop_keeps_cleanup_and_blocks_replacement_until_confirmed() {
    let root = Ctx::root().unwrap();
    let observed = Arc::new(Observed::default());
    let (runner, server, peer) = connect(&root, observed.clone(), true, false);
    hello(&peer, &plan()).await.unwrap();
    peer.request(
        "plugin/start",
        json!({"instance":"fast","activation":id(1)}),
    )
    .await
    .unwrap();
    peer.request("plugin/activate", json!({"activation":id(1)}))
        .await
        .unwrap();
    let waiter = peer
        .start_request("plugin/stop", json!({"activation":id(1)}))
        .unwrap();
    drop(waiter);
    observed.cleanup_entered.notified().await;
    assert!(runner.require_published(&id(1)).is_err());
    assert_eq!(
        peer.request(
            "plugin/start",
            json!({"instance":"fast","activation":id(2)})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Unavailable
    );
    let joined = peer
        .start_request("plugin/stop", json!({"activation":id(1)}))
        .unwrap();
    observed.cleanup.notify_one();
    assert_eq!(joined.await.unwrap().unwrap()["phase"], "stopped");
    peer.request(
        "plugin/start",
        json!({"instance":"fast","activation":id(2)}),
    )
    .await
    .unwrap();
    assert_eq!(observed.seen.lock().unwrap().len(), 2);
    let stop = peer
        .start_request("plugin/stop", json!({"activation":id(2)}))
        .unwrap();
    observed.cleanup_entered.notified().await;
    observed.cleanup.notify_one();
    stop.await.unwrap().unwrap();
    server.close(ProtocolError::new(ErrorCode::Unavailable, "test", "done"));
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn staging_mismatch_rolls_back_native_effects_and_hello_is_not_reusable() {
    let root = Ctx::root().unwrap();
    let observed = Arc::new(Observed::default());
    let (_runner, server, peer) = connect(&root, observed.clone(), false, true);
    hello(&peer, &plan()).await.unwrap();
    assert!(hello(&peer, &plan()).await.is_err());
    assert_eq!(
        peer.request(
            "plugin/start",
            json!({"instance":"fast","activation":id(1)})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::InterfaceMismatch
    );
    peer.request("plugin/stop", json!({"activation":id(1)}))
        .await
        .unwrap();
    assert_eq!(*observed.cleaned.lock().unwrap(), vec!["fast"]);
    assert!(observed.seen.lock().unwrap()[0]
        .1
        .effect(|| Effect::Done)
        .is_err());
    server.close(ProtocolError::new(ErrorCode::Unavailable, "test", "done"));
    root.shutdown().await.unwrap();
}

struct ReadyConsumer {
    key: TypeKey,
    seen: Arc<Mutex<Vec<Ctx>>>,
}
impl Plugin for ReadyConsumer {
    fn name(&self) -> &str {
        "runtime-ready-consumer"
    }
    fn injects(&self) -> &[TypeKey] {
        std::slice::from_ref(&self.key)
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.require_as::<RuntimeReady>(self.key.clone())?;
            self.seen.lock().unwrap().push(ctx.clone());
            Ok(Effect::Done)
        })
    }
}
#[tokio::test]
async fn native_runtime_ready_slot_invalidates_consumers_at_peer_close() {
    let host = Ctx::root().unwrap();
    let native = Ctx::root().unwrap();
    let (_runner, server, peer) = connect(&native, Arc::default(), false, false);
    let identity = hello(&peer, &plan()).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let consumer = ManagedActivation::mount(
        &host,
        ReadyConsumer {
            key: ready_key(&identity),
            seen: seen.clone(),
        },
    )
    .unwrap();
    consumer.view().await.unwrap();
    assert_eq!(consumer.view().state().state, rutis::FiberState::Pending);
    let published = publish_ready(&host, identity.clone(), peer.clone()).unwrap();
    let ready = host.get_as::<RuntimeReady>(ready_key(&identity)).unwrap();
    let _consumer_lease = ready.bind_consumer(&consumer);
    consumer.view().await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    peer.close(ProtocolError::new(
        ErrorCode::Unavailable,
        "test",
        "disconnected",
    ));
    assert!(seen.lock().unwrap()[0].cancellation_token().is_cancelled());
    assert!(!consumer.gate().is_open());
    assert!(seen.lock().unwrap()[0].effect(|| Effect::Done).is_err());
    published.dispose().await.unwrap();
    consumer.stop().await.unwrap();
    server.close(ProtocolError::new(ErrorCode::Unavailable, "test", "done"));
    host.shutdown().await.unwrap();
    native.shutdown().await.unwrap();
}

#[tokio::test]
async fn hello_and_activation_metadata_fail_before_native_mount() {
    let root = Ctx::root().unwrap();
    let observed = Arc::new(Observed::default());
    let (runner, server, peer) = connect(&root, observed.clone(), false, false);
    let mut invalid = serde_json::to_value(plan()).unwrap();
    invalid["identity"]["capabilities"] = json!(["object.scope", "object.scope"]);
    assert_eq!(
        peer.request("runtime/hello", invalid)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidParams
    );
    let mut invalid = serde_json::to_value(plan()).unwrap();
    invalid["members"]["fast"]["config_schema"] = json!("{}");
    assert_eq!(
        peer.request("runtime/hello", invalid)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InterfaceMismatch
    );
    hello(&peer, &plan()).await.unwrap();
    let mut old = id(1);
    old.epoch = Sequence(2);
    assert!(peer
        .request("plugin/start", json!({"instance":"fast","activation":old}))
        .await
        .is_err());
    assert!(peer
        .request(
            "plugin/start",
            json!({"instance":"missing","activation":id(1)})
        )
        .await
        .is_err());
    assert!(observed.seen.lock().unwrap().is_empty());
    server.close(ProtocolError::new(ErrorCode::Unavailable, "test", "done"));
    assert!(runner
        .handle(
            "plugin/start",
            json!({"instance":"fast","activation":id(1)})
        )
        .await
        .is_err());
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn independent_member_ids_can_arrive_out_of_order_but_never_rebind_a_member() {
    let root = Ctx::root().unwrap();
    let observed = Arc::new(Observed::default());
    let (runner, server, peer) = connect(&root, observed.clone(), false, false);
    let mut hello_plan = plan();
    hello_plan.members.get_mut("slow").unwrap().config = json!({"label":"other"});
    hello(&peer, &hello_plan).await.unwrap();
    peer.request(
        "plugin/start",
        json!({"instance":"fast","activation":id(2)}),
    )
    .await
    .unwrap();
    peer.request(
        "plugin/start",
        json!({"instance":"slow","activation":id(1)}),
    )
    .await
    .unwrap();
    assert_eq!(observed.seen.lock().unwrap().len(), 2);
    peer.request("plugin/stop", json!({"activation":id(2)}))
        .await
        .unwrap();
    assert!(peer
        .request(
            "plugin/start",
            json!({"instance":"fast","activation":id(1)})
        )
        .await
        .is_err());
    assert!(peer
        .request(
            "plugin/start",
            json!({"instance":"fast","activation":id(2)})
        )
        .await
        .is_err());
    peer.request(
        "plugin/start",
        json!({"instance":"fast","activation":id(3)}),
    )
    .await
    .unwrap();
    let finish = runner.close();
    for stop in finish {
        stop.await.unwrap();
    }
    server.close(ProtocolError::new(ErrorCode::Unavailable, "test", "done"));
    root.shutdown().await.unwrap();
}

#[derive(Deserialize)]
struct Config {
    label: String,
}
struct Factory;
impl PluginFactory<Config> for Factory {
    fn build(&self, config: &Config) -> Result<Box<dyn Plugin>, CordisError> {
        Ok(Box::new(Journal(config.label.clone())))
    }
}
struct Journal(String);
impl Plugin for Journal {
    fn name(&self) -> &str {
        "native-journal"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            println!("native-apply {} {}", self.0, ctx.instance());
            let label = self.0.clone();
            Ok(Effect::Disposer(Box::new(move || {
                println!("native-cleanup {label}");
                Ok(())
            })))
        })
    }
}
fn factories() -> StaticFactories {
    factories_with_counter(Arc::new(std::sync::atomic::AtomicUsize::new(0)))
}
fn factories_with_counter(constructed: Arc<std::sync::atomic::AtomicUsize>) -> StaticFactories {
    let catalog = RunnerCatalog {
        protocol_family: FAMILY.into(),
        protocol_version: VERSION.into(),
        framework_version: "0.3.0".into(),
        environment_sha256: digest(b"native-env"),
        capabilities: BTreeSet::new(),
        factories: [("probe".into(), contract())].into_iter().collect(),
    };
    StaticFactories::admit(
        &serde_json::to_vec(&catalog).unwrap(),
        [
            StaticFactory::native("probe", contract(), SCHEMA.as_bytes(), move || {
                constructed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Factory
            })
            .unwrap(),
        ],
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_during_synchronous_construction_prevents_a_late_native_apply() {
    struct Blocking {
        root: Ctx,
        observed: Arc<Observed>,
        entered: Arc<Notify>,
        finish: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    }
    impl Driver for Blocking {
        fn admit(&self, _: &Hello) -> rutis_protocol::error::Result<()> {
            Ok(())
        }
        fn mount(
            &self,
            request: MountRequest,
            gate: rutis_protocol::managed::ActivationGate,
        ) -> rutis_protocol::error::Result<Mounted> {
            let member = request.member;
            self.entered.notify_one();
            let (lock, cv) = &*self.finish;
            let mut done = lock.lock().unwrap();
            while !*done {
                done = cv.wait(done).unwrap();
            }
            Ok(Mounted {
                native: ManagedActivation::mount_gated(
                    &self.root,
                    Probe {
                        label: member.config["label"].as_str().unwrap().into(),
                        observed: self.observed.clone(),
                        slow: false,
                        slow_cleanup: false,
                    },
                    gate,
                )
                .unwrap(),
                services: Box::pin(async { Ok(BTreeMap::new()) }),
            })
        }
    }
    let root = Ctx::root().unwrap();
    let observed = Arc::new(Observed::default());
    let entered = Arc::new(Notify::new());
    let finish = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let runner = Runner::new(Blocking {
        root: root.clone(),
        observed: observed.clone(),
        entered: entered.clone(),
        finish: finish.clone(),
    });
    runner
        .handle("runtime/hello", serde_json::to_value(plan()).unwrap())
        .await
        .unwrap();
    let starting = runner.handle(
        "plugin/start",
        json!({"instance":"fast","activation":id(1)}),
    );
    entered.notified().await;
    let stopping = runner.handle("plugin/stop", json!({"activation":id(1)}));
    *finish.0.lock().unwrap() = true;
    finish.1.notify_all();
    assert!(starting.await.is_err());
    stopping.await.unwrap();
    assert!(observed.seen.lock().unwrap().is_empty());
    assert!(observed.cleaned.lock().unwrap().is_empty());
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_driver_rejects_unimplemented_transport_claims_without_constructing_factories() {
    let root = Ctx::root().unwrap();
    let constructed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runner = Runner::new(NativeDriver::new(
        root.clone(),
        factories_with_counter(constructed.clone()),
    ));
    let mut required = plan();
    required.identity.capabilities.insert("object.scope".into());
    assert_eq!(
        runner
            .handle("runtime/hello", serde_json::to_value(required).unwrap())
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    let mut invalid = plan();
    invalid
        .members
        .get_mut("fast")
        .unwrap()
        .contracts
        .config_sha256 = digest(b"changed");
    assert_eq!(
        runner
            .handle("runtime/hello", serde_json::to_value(invalid).unwrap())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InterfaceMismatch
    );
    runner
        .handle("runtime/hello", serde_json::to_value(plan()).unwrap())
        .await
        .unwrap();
    assert_eq!(constructed.load(std::sync::atomic::Ordering::Relaxed), 0);
    drop(runner.close());
    root.shutdown().await.unwrap();
}

// The parent test starts this same immutable executable with exactly one test
// selected. No external Rust compiler invocation or placeholder process is used.
#[cfg(target_os = "linux")]
#[test]
fn private_native_runner_entry() {
    if std::env::var_os("RUTIS_PROTOCOL_LIFECYCLE_CHILD").is_none() {
        return;
    }
    use std::os::fd::FromRawFd;
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(3) };
        stream.set_nonblocking(true).unwrap();
        let stream = tokio::net::UnixStream::from_std(stream).unwrap();
        let root = Ctx::root().unwrap();
        serve(stream, NativeDriver::new(root.clone(), factories()))
            .await
            .unwrap();
        root.shutdown().await.unwrap();
    });
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn private_native_process_hellos_then_mounts_two_distinct_native_contexts() {
    use std::{os::fd::AsRawFd, process::Stdio};
    let (parent, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
    parent.set_nonblocking(true).unwrap();
    let fd = child_socket.as_raw_fd();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "private_native_runner_entry", "--nocapture"])
        .env("RUTIS_PROTOCOL_LIFECYCLE_CHILD", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    drop(child_socket);
    let peer = Peer::start(
        tokio::net::UnixStream::from_std(parent).unwrap(),
        Arc::new(|_, _| Box::pin(async { Ok(Value::Null) })),
    );
    hello(&peer, &plan()).await.unwrap();
    peer.request(
        "plugin/start",
        json!({"instance":"slow","activation":id(1)}),
    )
    .await
    .unwrap();
    peer.request(
        "plugin/start",
        json!({"instance":"fast","activation":id(2)}),
    )
    .await
    .unwrap();
    for activation in [id(1), id(2)] {
        peer.request("plugin/activate", json!({"activation":activation}))
            .await
            .unwrap();
    }
    peer.request("plugin/stop", json!({"activation":id(1)}))
        .await
        .unwrap();
    assert_eq!(
        peer.request("plugin/state", json!({"activation":id(2)}))
            .await
            .unwrap()["phase"],
        "published"
    );
    peer.request("runtime/stop", json!({})).await.unwrap();
    peer.close(ProtocolError::new(ErrorCode::Unavailable, "test", "done"));
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let contexts: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("native-apply "))
        .map(|line| line.split_whitespace().last().unwrap().to_owned())
        .collect();
    assert_eq!(contexts.len(), 2, "{text}");
    assert_ne!(contexts[0], contexts[1]);
    assert!(text.contains("native-cleanup slow"));
    assert!(text.contains("native-cleanup fast"));
}
