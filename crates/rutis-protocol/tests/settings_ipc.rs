//! Adapt a published dsh-settings provider, rather than an echo implementation.
#![cfg(target_os = "linux")]
#[allow(dead_code, unused_variables, non_snake_case, clippy::redundant_closure)]
mod api {
    include!("../../../protocol/generated/settings.rs");
}
use api::*;
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};
use rutis_protocol::{
    contract::EventMode,
    error::{ErrorCode, ProtocolError, Result},
    events::{EventKey, HostEvents, ListenerResult},
    exports::Exports,
    frame::{Handler, Peer},
    identity::{Activation, Sequence},
    managed::{ActivationGate, ManagedActivation},
    sdk::*,
    services::{Bundles, NativePorts, ServiceTable},
    session::{HostObjects, RuntimeIdentity, RuntimeObjects},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    os::fd::AsRawFd,
    process::Stdio,
    sync::{Arc, Mutex},
};

fn owner(runtime: &str) -> Activation {
    Activation {
        runtime: runtime.into(),
        epoch: Sequence(1),
        activation: Sequence(1),
    }
}
fn identity(runtime: &str) -> RuntimeIdentity {
    RuntimeIdentity {
        runtime: runtime.into(),
        epoch: Sequence(1),
    }
}
fn reject() -> Handler {
    Arc::new(|_, _| {
        Box::pin(async {
            Err(ProtocolError::new(
                ErrorCode::UnsupportedCapability,
                "settings_fixture",
                "unknown operation",
            ))
        })
    })
}

struct Visitor {
    original: Ctx,
    section: InterfaceSettingsSectionClient,
}
impl BorrowCallback0Service for Visitor {
    fn call(
        &self,
        context: CallContext,
        value: BTreeMap<String, Value>,
    ) -> RpcFuture<BTreeMap<String, Value>> {
        assert!(context.native().unwrap().is_within(&self.original));
        let section = self.section.clone();
        Box::pin(async move {
            assert_eq!(
                section.read(()).await?,
                value,
                "callback reentered the original native scope"
            );
            let mut value = value;
            value.insert("callback".into(), Value::Bool(true));
            Ok(value)
        })
    }
}
struct Consumer {
    runtime: Arc<RuntimeObjects>,
    gate: ActivationGate,
    owner: Activation,
    keys: Vec<TypeKey>,
    saved: Arc<Mutex<Option<InterfaceSettingsSectionClient>>>,
    exports: Arc<Mutex<Option<Exports>>>,
    hub: Arc<HostEvents>,
    event: EventKey,
}
struct Publisher {
    original: Ctx,
    owner: Activation,
    hub: Arc<HostEvents>,
    event: EventKey,
}
impl InterfaceEventPublisherService for Publisher {
    fn dispatch(
        &self,
        context: CallContext,
        params: InterfaceEventPublisherMethod0ParamsParamRecord,
    ) -> RpcFuture<InterfaceEventPublisherMethod0ResultResultRecord> {
        assert!(context.native().unwrap().is_within(&self.original));
        let mode = match params.mode {
            InterfaceEventPublisherMethod0ParamsField0::V0 => EventMode::Parallel,
            InterfaceEventPublisherMethod0ParamsField0::V1 => EventMode::Serial,
        };
        let payload = Outbound::Record(BTreeMap::from([
            (
                "section".into(),
                Outbound::Foreign(params.payload.section.client().proxy().clone()),
            ),
            (
                "value".into(),
                Outbound::json(params.payload.value).unwrap(),
            ),
        ]));
        let dispatch = self.hub.dispatch(&self.owner, &self.event, mode, payload);
        Box::pin(async move {
            let report = dispatch.await?;
            Ok(InterfaceEventPublisherMethod0ResultResultRecord {
                returned: report.returned.is_some(),
                value: report.returned,
                errors: report
                    .errors
                    .into_iter()
                    .map(|error| {
                        BTreeMap::from([
                            ("code".into(), json!(error.code)),
                            ("message".into(), json!(error.message)),
                        ])
                    })
                    .collect(),
            })
        })
    }
}
impl Plugin for Consumer {
    fn name(&self) -> &str {
        "native-settings-consumer"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.keys
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let result: Result<Effect> = async {
                let exports = Exports::managed(
                    ctx,
                    self.owner.clone(),
                    self.runtime.ids(),
                    self.gate.clone(),
                )
                .unwrap();
                self.runtime
                    .bind(&self.owner, ctx.clone(), exports.clone())?;
                self.hub.bind_publisher(
                    ctx,
                    exports.clone(),
                    std::collections::BTreeSet::from([self.event.clone()]),
                )?;
                *self.exports.lock().unwrap() = Some(exports);
                let publisher: Arc<dyn InterfaceEventPublisherService> = Arc::new(Publisher {
                    original: ctx.clone(),
                    owner: self.owner.clone(),
                    hub: self.hub.clone(),
                    event: self.event.clone(),
                });
                ctx.provide_as::<dyn InterfaceEventPublisherService>(
                    TypeKey::of::<dyn InterfaceEventPublisherService>(),
                    publisher,
                )
                .unwrap();
                let settings = ctx
                    .require_as::<InterfaceSettingsClient>(TypeKey::of::<InterfaceSettingsClient>())
                    .unwrap();
                let first = settings.open("protocol-example".into()).await?;
                let second = settings.open("protocol-example".into()).await?;
                assert!(first.client().proxy().same_object(second.client().proxy()));
                assert_eq!(first.namespace()?, "protocol-example");
                assert_eq!(first.read(()).await?["count"], 2);
                first
                    .write(BTreeMap::from([("count".into(), json!(7))]))
                    .await?;
                assert_eq!(second.read(()).await?["count"], 7);
                assert_eq!(settings.inspect(first.clone()).await?["count"], 7);
                assert_eq!(
                    first
                        .write(BTreeMap::from([("count".into(), json!(-1))]))
                        .await
                        .unwrap_err()
                        .code,
                    ErrorCode::Business
                );
                assert_eq!(
                    first.read(()).await?["count"],
                    7,
                    "native validation rejected the write atomically"
                );
                let visitor: Arc<dyn BorrowCallback0Service> = Arc::new(Visitor {
                    original: ctx.clone(),
                    section: first.clone(),
                });
                self.runtime
                    .caller(&self.owner)
                    .bind_native(ctx, exportBorrowCallback0(visitor.clone()))?;
                assert_eq!(first.visit(visitor).await?["callback"], true);
                first.replace(BTreeMap::new()).await?;
                assert_eq!(
                    first.read(()).await?["count"],
                    2,
                    "native schema defaults survive replacement"
                );
                assert_eq!(
                    settings
                        .open("private-namespace".into())
                        .await
                        .err()
                        .unwrap()
                        .code,
                    ErrorCode::CapabilityDenied
                );
                *self.saved.lock().unwrap() = Some(first);
                Ok(Effect::Done)
            }
            .await;
            result.map_err(|error| CordisError::PluginFailed(Box::new(error)))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_settings_scope_keeps_native_behavior_across_private_ipc() {
    tokio::time::timeout(std::time::Duration::from_secs(30), scenario())
        .await
        .unwrap();
}
async fn scenario() {
    let bundles =
        Bundles::admit([
            include_bytes!("../../../protocol/fixtures/settings.bundle.json").to_vec(),
        ])
        .unwrap();
    let host = HostObjects::new(bundles.clone());
    host.reserve(owner("rust")).unwrap();
    host.reserve(owner("settings")).unwrap();
    let runtime = RuntimeObjects::new(identity("rust"), bundles.clone());
    let gate = ActivationGate::default();
    runtime.reserve(owner("rust"), gate.clone()).unwrap();
    let (host_stream, runtime_stream) = tokio::net::UnixStream::pair().unwrap();
    let rust_host_peer = Peer::start(host_stream, host.handler(identity("rust"), reject()));
    host.attach(identity("rust"), &rust_host_peer).unwrap();
    let rust_peer = Peer::start(runtime_stream, runtime.handler(reject()));
    runtime.attach(&rust_peer).unwrap();
    let (parent, child_socket) = tokio::net::UnixStream::pair().unwrap();
    let child_socket = child_socket.into_std().unwrap();
    let fd = child_socket.as_raw_fd();
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = tokio::process::Command::new("node");
    command
        .current_dir(workspace)
        .args([
            "--import",
            "./host/node_modules/tsx/dist/loader.mjs",
            "host/tests/fixtures/protocol-settings-peer.ts",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(child_socket);
    let mut stderr = child.stderr.take().unwrap();
    let diagnostics = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut text = String::new();
        stderr.read_to_string(&mut text).await.unwrap();
        text
    });
    let node_peer = Peer::start(parent, host.handler(identity("settings"), reject()));
    host.attach(identity("settings"), &node_peer).unwrap();
    let start = node_peer.request("fixture/start", Value::Null).await;
    let start = match start {
        Ok(start) => start,
        Err(error) => {
            child.kill().await.unwrap();
            panic!("{error}: {}", diagnostics.await.unwrap());
        }
    };
    let table: ServiceTable = serde_json::from_value(start["table"].clone()).unwrap();
    let contracts = serde_json::from_value(start["contracts"].clone()).unwrap();
    host.publish(&owner("settings")).unwrap();
    let values = host
        .offer_services(&owner("settings"), &table, &contracts, &owner("rust"))
        .await
        .unwrap()
        .receive(&runtime)
        .await
        .unwrap();
    let root = Ctx::root().unwrap();
    let mut ports = NativePorts::default();
    ports
        .require::<InterfaceSettingsClient>(
            "settings",
            TypeKey::of::<InterfaceSettingsClient>(),
            &bundles,
        )
        .unwrap();
    ports
        .require::<InterfaceEventListenerClient>(
            "events",
            TypeKey::of::<InterfaceEventListenerClient>(),
            &bundles,
        )
        .unwrap();
    ports
        .provide::<dyn InterfaceEventPublisherService, InterfaceEventPublisherClient>(
            "publisher",
            TypeKey::of::<dyn InterfaceEventPublisherService>(),
            &bundles,
            exportInterfaceEventPublisher,
        )
        .unwrap();
    let mut bindings = ports.scope(&root, owner("rust"), gate.clone());
    ports
        .install(&mut bindings, values, runtime.caller(&owner("rust")))
        .unwrap();
    let saved = Arc::new(Mutex::new(None));
    let exports = Arc::new(Mutex::new(None));
    let hub = HostEvents::new(bundles.clone());
    let key = EventKey {
        scope: "application".into(),
        bundle: BUNDLE_SHA256.into(),
        event: "changed".into(),
    };
    let consumer = ManagedActivation::mount_gated(
        bindings.ctx(),
        Consumer {
            runtime: runtime.clone(),
            gate: gate.clone(),
            owner: owner("rust"),
            keys: ports.required_keys(),
            saved: saved.clone(),
            exports: exports.clone(),
            hub: hub.clone(),
            event: key.clone(),
        },
        gate,
    )
    .unwrap();
    consumer.view().await.unwrap();
    bindings.adopt(&consumer).unwrap();
    let staged = ports
        .stage(
            &bindings,
            bundles.clone(),
            exports.lock().unwrap().as_ref().unwrap().clone(),
        )
        .unwrap();
    let publish_contracts = staged.contracts().clone();
    let publish_table = runtime.stage_services(staged).unwrap();
    host.publish(&owner("rust")).unwrap();
    runtime.publish(&owner("rust")).unwrap();
    let native = consumer.native_context().unwrap();
    let ready = host
        .offer_services(
            &owner("rust"),
            &publish_table,
            &publish_contracts,
            &owner("settings"),
        )
        .await
        .unwrap()
        .send("fixture/publisher")
        .await
        .unwrap();
    assert_eq!(ready["ready"], true);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let local_caller = runtime.caller(&owner("rust"));
    let original_section = saved.lock().unwrap().as_ref().unwrap().clone();
    let local = hub
        .subscribe(
            &native,
            key.clone(),
            false,
            Arc::new(move |payload| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Outbound::Record(mut fields) = payload else {
                    panic!("event payload is not a record")
                };
                let Some(Outbound::Foreign(proxy)) = fields.remove("section") else {
                    panic!("event has no authorized section")
                };
                assert!(proxy.same_object(original_section.client().proxy()));
                let section = bind::<InterfaceSettingsSectionClient>(
                    rutis_protocol::graph::DecodedValue::Object(proxy),
                    local_caller.clone(),
                )
                .unwrap();
                Box::pin(async move {
                    assert_eq!(section.read(()).await?["count"], 2);
                    Ok(ListenerResult::Continue)
                })
            }),
        )
        .unwrap();
    let endpoint = native
        .get_as::<InterfaceEventListenerClient>(TypeKey::of::<InterfaceEventListenerClient>())
        .unwrap();
    let caller = runtime.caller(&owner("rust"));
    let target = endpoint.client().proxy().clone();
    let remote = hub
        .subscribe(
            &native,
            key.clone(),
            false,
            Arc::new(move |payload| {
                let future = caller.call(target.clone(), "call".into(), payload);
                Box::pin(async move {
                    let mut fields = record(future.await?)?;
                    let returned =
                        rutis_protocol::sdk::json::<bool>(field(&mut fields, "returned")?)?;
                    let value = optional(field(&mut fields, "value")?)?
                        .map(rutis_protocol::sdk::json::<Value>)
                        .transpose()?
                        .unwrap_or(Value::Null);
                    Ok(if returned {
                        ListenerResult::Return(value)
                    } else {
                        ListenerResult::Continue
                    })
                })
            }),
        )
        .unwrap();
    for result in [json!(0), json!(false), Value::Null] {
        let section = saved.lock().unwrap().as_ref().unwrap().clone();
        let payload = Outbound::Record(BTreeMap::from([
            (
                "section".into(),
                Outbound::Foreign(section.client().proxy().clone()),
            ),
            (
                "value".into(),
                Outbound::Value(json!({"count":2,"result":result})),
            ),
        ]));
        let serial = hub
            .dispatch(&owner("rust"), &key, EventMode::Serial, payload.clone())
            .await
            .unwrap();
        assert!(serial.errors.is_empty(), "{:?}", serial.errors);
        assert_eq!(serial.returned, Some(result));
        let parallel = hub
            .dispatch(&owner("rust"), &key, EventMode::Parallel, payload)
            .await
            .unwrap();
        assert!(parallel.errors.is_empty(), "{:?}", parallel.errors);
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 6);
    for result in [json!(0), json!(false), Value::Null] {
        let serial = node_peer
            .request("fixture/emit", json!({"mode":"serial","result":result}))
            .await
            .unwrap();
        assert_eq!(serial["errors"], json!([]));
        assert_eq!(serial["returned"], true);
        assert_eq!(serial["value"], result);
        let parallel = node_peer
            .request("fixture/emit", json!({"mode":"parallel","result":result}))
            .await
            .unwrap();
        assert_eq!(parallel["errors"], json!([]));
        assert_eq!(parallel["returned"], false);
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 12);
    local.unsubscribe().await.unwrap();
    remote.unsubscribe().await.unwrap();
    assert_eq!(
        node_peer
            .request("fixture/expired", Value::Null)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ScopeClosed
    );
    let stopping = node_peer
        .request("fixture/stop", Value::Null)
        .await
        .unwrap();
    assert_eq!(stopping["cleanups"], 1);
    assert_eq!(stopping["registrations"], 0);
    let section = saved.lock().unwrap().take().unwrap();
    assert!(section.read(()).await.is_err());
    assert!(section.namespace().is_err());
    consumer.stop().await.unwrap();
    host.close_member(&owner("rust")).await.unwrap();
    runtime.close();
    root.shutdown().await.unwrap();
    node_peer
        .request("fixture/close", Value::Null)
        .await
        .unwrap();
    for peer in [node_peer, rust_peer, rust_host_peer] {
        peer.close(ProtocolError::new(
            ErrorCode::Unavailable,
            "settings_fixture",
            "fixture closed",
        ));
    }
    let status = child.wait().await.unwrap();
    let output = diagnostics.await.unwrap();
    assert!(status.success(), "{status}: {output}");
}
