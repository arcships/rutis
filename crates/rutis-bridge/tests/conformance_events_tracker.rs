//! Wire-level conformance for the event rules the tracking table still marks
//! partly or not verified (`docs/formal-trace/rules/protocol-events.md`): a
//! host on a memory session with a far end scripted frame by frame, so what
//! is checked is the wire itself, not an internal helper.
//!
//! The far end is a fake runtime: it registers listeners, dispatches, and
//! answers `events.call` as the test tells it to. Rules that name another
//! implementation (Node's Cordis `internal/dispatch`, Go's filter errors, a
//! JavaScript listener's signal), need a real runtime, or need a linked node
//! stay open; the PR body lists them.
//!
//! rule: EVP-3-2
//! rule: EVP-3-5
//! rule: EVP-4.1-6
//! rule: EVP-4.2-2
//! rule: EVP-4.2-4
//! rule: EVP-4.2-5
//! rule: EVP-4.2-10
//! rule: EVP-5.3-6
//! rule: EVP-5.3-7
//! rule: EVP-5.4-2
//! rule: EVP-5.4-14
//! rule: EVP-5.7-4
//! rule: EVP-6-6
//! rule: EVP-8-3
//! rule: EVP-8-7
//!
//! guarantee: events/hub-reserved
//! guarantee: events/hub-flat
//! guarantee: events/hub-filter
//! guarantee: events/hub-dispatch
//! guarantee: events/remote
//! guarantee: events/framework

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rutis::events::{Call, Scope};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect};
use rutis::internal::CtxInternal;
use rutis::{Ctx, Event};
use rutis_bridge::channel::PeerId;
use rutis_bridge::events::{ArgValue, Args};
use rutis_bridge::runtime::Process;
use rutis_bridge::session::{
    host_key, settle, Connection, Dispatch, Endpoint, Error, HostDispatch, Reference, Reply, Value,
};
use rutis_test_support::{until, within};
use serde_json::{json, Value as Json};

mod common;

fn id(name: &str) -> PeerId {
    PeerId::new(name).unwrap()
}

/// What a fake listener does when called.
type Behavior = Arc<dyn Fn(&BTreeMap<String, Value>) -> Reply + Send + Sync>;

/// A runtime that mounts nothing, scripted by the test: it records the rows
/// it is told to load and every `events.call` it gets, and answers by its
/// listener's behavior.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<BTreeMap<String, Value>>>,
    listeners: Mutex<BTreeMap<String, Behavior>>,
    loads: Mutex<Vec<Vec<Value>>>,
}

impl Dispatch for Fake {
    fn invoke(&self, _: &Connection, target: &str, method: &str, args: Value) -> Reply {
        let first = || -> Result<Value, Error> {
            Ok(args
                .clone()
                .list()?
                .into_iter()
                .next()
                .unwrap_or(Value::Undefined))
        };
        match (target, method) {
            ("", "mount") => Ok(Value::Data(
                json!({ "services": {}, "features": ["rows.v2", "hosts"] }),
            )),
            ("", "rows.load") => {
                self.loads.lock().unwrap().push(args.list()?);
                Ok(Value::Undefined)
            }
            ("", "rows.unload") => Ok(Value::Undefined),
            ("", "ping") => Ok(Value::Data(json!("pong"))),
            ("", "events.call") => {
                let Value::Record(fields) = first()? else {
                    return Err(Error::value("events.call needs a record"));
                };
                let listener = fields["listener"].clone().json()?;
                let behavior = self
                    .listeners
                    .lock()
                    .unwrap()
                    .get(listener.as_str().unwrap())
                    .cloned();
                self.calls.lock().unwrap().push(fields.clone());
                match behavior {
                    Some(behavior) => behavior(&fields),
                    None => Err(Error::Remote {
                        name: "BindingError".into(),
                        message: format!("listener {listener} is withdrawn"),
                        graph: None,
                    }),
                }
            }
            _ => Err(Error::value(format!("no method {method}"))),
        }
    }
}

/// The far end's channel, the frames it reads recorded in order: the wire
/// itself, before any plugin code runs.
struct Recorded {
    inner: Box<dyn rutis_bridge::channel::Receiver>,
    frames: Arc<Mutex<Vec<Json>>>,
}

impl rutis_bridge::channel::Receiver for Recorded {
    fn recv(&mut self) -> Result<Option<Vec<u8>>, rutis_bridge::channel::ChannelError> {
        let message = self.inner.recv()?;
        if let Some(message) = &message {
            if let Ok(frame) = serde_json::from_slice::<Json>(message) {
                self.frames.lock().unwrap().push(frame);
            }
        }
        Ok(message)
    }
}

/// The host (`root`'s bus) and the fake runtime `name` mounted on it.
struct Rig {
    root: Ctx,
    process: Arc<Process>,
    fake: Arc<Fake>,
    far: Connection,
    /// Every frame the host wrote to `far`, in order.
    frames: Arc<Mutex<Vec<Json>>>,
}

async fn rig_on(root: Ctx, name: &str) -> Rig {
    let (a, b) = rutis_bridge::memory::pair();
    let fake = Arc::new(Fake::default());
    let frames: Arc<Mutex<Vec<Json>>> = Arc::default();
    let far = rutis_bridge::Channel {
        sender: b.sender,
        receiver: Box::new(Recorded {
            inner: b.receiver,
            frames: frames.clone(),
        }),
        closer: b.closer,
    };
    // A language runtime, which the session mounts a process on.
    let mut endpoint = Endpoint::rust(id(name));
    endpoint.capabilities.push("runtime".into());
    let far = Connection::open(far, fake.clone(), endpoint).unwrap();
    let process = common::runtime::attached(&root, name, a)
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    Rig {
        root,
        process,
        fake,
        far,
        frames,
    }
}

async fn rig() -> Rig {
    rig_on(common::runtime::root(), "rt").await
}

impl Rig {
    async fn send(&self, method: &str, argument: Value) -> Result<Value, Error> {
        let reply = self
            .far
            .invoke_async("", method, Value::List(vec![argument]))
            .await?;
        settle(reply).await
    }

    /// Queue the frame now and hand back its reply: dropping the reply
    /// cancels the call, which is what an emitter that gives up does.
    fn started(
        &self,
        method: &str,
        argument: Value,
    ) -> Result<impl Future<Output = Reply> + Send + 'static, Error> {
        self.far
            .invoke_started("", method, Value::List(vec![argument]))
    }

    /// Load row `key`, named `id` (its configuration's id) when given, in
    /// `scope`.
    async fn load(&self, key: &str, id: Option<&str>, scope: &[Scope]) {
        if let Some(id) = id {
            self.process.name_row(key, id);
        }
        self.process
            .load_row(key, Path::new("fake"), json!({}), &[], &[], scope)
            .await
            .unwrap();
    }

    /// Register listener `id` of `name` from row `row` for the fake.
    async fn register(
        &self,
        id: &str,
        name: &str,
        row: Option<&str>,
        plugin: &str,
        kind: &str,
    ) -> Result<Value, Error> {
        self.register_with(id, name, row, plugin, kind, quiet())
            .await
    }

    async fn register_with(
        &self,
        id: &str,
        name: &str,
        row: Option<&str>,
        plugin: &str,
        kind: &str,
        behavior: Behavior,
    ) -> Result<Value, Error> {
        self.fake
            .listeners
            .lock()
            .unwrap()
            .insert(id.into(), behavior);
        self.send(
            "events.on",
            Value::Data(json!({
                "id": id, "name": name, "row": row, "plugin": plugin,
                "kind": kind, "prepend": false, "global": false,
            })),
        )
        .await
    }

    /// `events.dispatch` from `row`, waiting for the answer.
    async fn dispatch(
        &self,
        row: Option<&str>,
        name: &str,
        mode: &str,
        args: Vec<Value>,
    ) -> Result<Value, Error> {
        let fields = request(row, name, mode, args);
        self.send("events.dispatch", Value::Record(fields)).await
    }

    fn dispatch_started(
        &self,
        row: Option<&str>,
        name: &str,
        mode: &str,
        args: Vec<Value>,
        filter: Option<Value>,
    ) -> Result<impl Future<Output = Reply> + Send + 'static, Error> {
        let mut fields = request(row, name, mode, args);
        if let Some(filter) = filter {
            fields.insert("filter".into(), filter);
        }
        self.started("events.dispatch", Value::Record(fields))
    }
}

fn request(row: Option<&str>, name: &str, mode: &str, args: Vec<Value>) -> BTreeMap<String, Value> {
    let mut fields = BTreeMap::new();
    fields.insert("row".into(), Value::Data(json!(row)));
    fields.insert("name".into(), Value::Data(json!(name)));
    fields.insert("mode".into(), Value::Data(json!(mode)));
    fields.insert("args".into(), Value::List(args));
    fields
}

/// A listener that answers nothing.
fn quiet() -> Behavior {
    Arc::new(|_| Ok(Value::Undefined))
}

fn scope(chain: &[(&str, &str)]) -> Vec<Scope> {
    chain
        .iter()
        .map(|(id, group)| Scope {
            id: (*id).into(),
            group: (*group).into(),
        })
        .collect()
}

/// The listener id an `events.call` frame names.
fn called(frame: &Json) -> String {
    frame["args"]["value"][0]["value"]["listener"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("an events.call frame: {frame:#?}"))
        .to_owned()
}

/// The number of a dispatch id (`d<number>`), or `None`.
fn dispatch_number(value: &Value) -> Option<u64> {
    match value {
        Value::Data(Json::String(id)) => id.strip_prefix('d')?.parse().ok(),
        _ => None,
    }
}

/// The `name` and `message` of a `BindingError` the host threw.
fn binding(error: &Error) -> (&str, &str) {
    match error {
        Error::Remote { name, message, .. } => (name, message),
        other => panic!("expected a BindingError, got {other:?}"),
    }
}

/// A typed event of this host, for a dispatch that does not come from a
/// session.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Happened(String);

impl Event for Happened {
    const NAME: &'static str = "t/id";
    type Value = ();
}

/// A host listener that answers nothing.
async fn hear(_: Arc<Args>, _: Call<Args>) -> Result<Option<ArgValue>, rutis::Error> {
    Ok(None)
}

/// EVP-4.2-2, EVP-8-7
///
/// `internal/*` belongs to the runtime that names it: the host refuses to
/// register such a listener, and refuses a dispatch of such a name, with
/// the protocol's one message. The far end never gets an entry into the
/// host's table for one.
#[tokio::test(flavor = "multi_thread")]
async fn internal_names_are_refused_and_never_registered() {
    within("internal names", async {
        let rig = rig().await;
        rig.load("chat", None, &[]).await;
        let refused = rig
            .register("1", "internal/state", Some("chat"), "fake", "any")
            .await
            .unwrap_err();
        assert_eq!(
            binding(&refused),
            (
                "BindingError",
                "event internal/state is internal to its runtime"
            )
        );
        let refused = rig
            .dispatch(Some("chat"), "internal/state", "serial", vec![])
            .await
            .unwrap_err();
        assert_eq!(
            binding(&refused),
            (
                "BindingError",
                "event internal/state is internal to its runtime"
            )
        );
        assert!(
            rig.fake.calls.lock().unwrap().is_empty(),
            "no listener of the name is on the bus"
        );
    })
    .await;
}

/// EVP-3-5
///
/// The host tells every row its scope chain and its id in the seventh
/// `rows.load` argument: seven arguments always, `exports` as `{}` when the
/// row exports none, and the configured id (the key when it has none).
#[tokio::test(flavor = "multi_thread")]
async fn rows_load_carries_seven_arguments_with_the_scope_and_the_id() {
    within("rows.load", async {
        let rig = rig().await;
        rig.load("plain", None, &[]).await;
        let chain = scope(&[("17", "tenant"), ("42", "session")]);
        rig.load("chat", Some("chat-ui"), &chain).await;
        let loads = rig.fake.loads.lock().unwrap().clone();
        assert_eq!(loads.len(), 2, "{loads:?}");
        let plain = &loads[0];
        assert_eq!(plain.len(), 7, "{plain:?}");
        assert_eq!(plain[0], Value::Data(json!("plain")));
        assert_eq!(plain[3], Value::Data(json!([])), "no isolate: {plain:?}");
        assert_eq!(plain[4], Value::Data(json!([])), "no inject: {plain:?}");
        assert_eq!(plain[5], Value::Data(json!({})), "no exports: {plain:?}");
        assert_eq!(
            plain[6],
            Value::Data(json!({ "scope": [], "id": "plain" })),
            "{plain:?}"
        );
        let named = &loads[1];
        assert_eq!(named.len(), 7, "{named:?}");
        assert_eq!(
            named[6],
            Value::Data(json!({
                "scope": [
                    { "id": "17", "group": "tenant" },
                    { "id": "42", "group": "session" },
                ],
                "id": "chat-ui",
            })),
            "{named:?}"
        );
    })
    .await;
}

/// EVP-3-2, EVP-4.2-5, EVP-4.2-10
///
/// A filter sees each candidate's descriptor: the event name, whether it is
/// a prefix entry, the registering plugin's name, the row's configured id
/// (not its key), the runtime's name (the host's own Rust listener has
/// none), the scope chain outer first and `[]` at the root, and `global`.
/// The emitter's own entry also carries its listener id; a prefix entry of
/// this host is `prefix: true`. A `global` entry is filtered out before the
/// filter is called (§6), so its descriptor never reaches one: this checks
/// `global: false` for the entries that do.
#[tokio::test(flavor = "multi_thread")]
async fn descriptors_carry_the_registration_and_the_scope() {
    within("descriptors", async {
        let rig = rig().await;
        let chain = scope(&[("17", "tenant"), ("42", "session")]);
        rig.load("chat", Some("chat-ui"), &chain).await;
        rig.register("7", "t/desc", Some("chat"), "chat-plugin", "any")
            .await
            .unwrap();
        // A Rust listener of this host, a prefix entry, at the root.
        rig.root.listen::<Args>().prefix("t/").on(hear).unwrap();
        let seen: Arc<Mutex<Vec<Json>>> = Arc::default();
        let recorded = seen.clone();
        let selector = Value::callback(move |argument| {
            let descriptors = argument.list()?.remove(0).json()?;
            recorded.lock().unwrap().push(descriptors.clone());
            let answers = descriptors.as_array().unwrap().len();
            Ok(Value::Data(json!(vec![false; answers])))
        });
        let result = rig
            .dispatch_started(Some("chat"), "t/desc", "serial", vec![], Some(selector))
            .unwrap()
            .await;
        let result = settle(result.unwrap()).await.unwrap();
        assert_eq!(result, Value::Undefined);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        let described = &seen[0];
        assert_eq!(described[0]["name"], json!("t/desc"));
        assert_eq!(described[0]["prefix"], json!(false));
        assert_eq!(described[0]["plugin"], json!("chat-plugin"));
        assert_eq!(described[0]["row"], json!("chat-ui"), "the row id");
        assert_eq!(described[0]["runtime"], json!("rt"));
        assert_eq!(described[0]["node"], Json::Null);
        assert_eq!(
            described[0]["scope"],
            json!([
                { "id": "17", "group": "tenant" },
                { "id": "42", "group": "session" },
            ])
        );
        assert_eq!(described[0]["global"], json!(false));
        assert_eq!(described[0]["listener"], json!("7"));
        // The host's own: no runtime, no row, the root's scope; a prefix.
        assert_eq!(described[1]["name"], json!("t/"));
        assert_eq!(described[1]["prefix"], json!(true));
        assert_eq!(described[1]["runtime"], Json::Null);
        assert_eq!(described[1]["node"], Json::Null);
        assert_eq!(described[1]["row"], Json::Null);
        assert_eq!(described[1]["scope"], json!([]));
        assert_eq!(described[1]["global"], json!(false));
    })
    .await;
}

/// EVP-4.2-4
///
/// A listener belongs to a row loaded on this session, by its key; the
/// mount's own plugins write `row: null`, and their descriptor has no row.
/// A row this session does not have is refused with the protocol's message.
#[tokio::test(flavor = "multi_thread")]
async fn a_listener_belongs_to_a_row_of_this_session() {
    within("listener rows", async {
        let rig = rig().await;
        let refused = rig
            .register("1", "t/row", Some("elsewhere"), "fake", "any")
            .await
            .unwrap_err();
        assert_eq!(
            binding(&refused),
            (
                "BindingError",
                "the calling row is not loaded in this runtime"
            )
        );
        // The mount's own plugins: `row: null`.
        rig.register("2", "t/row", None, "fake", "any")
            .await
            .unwrap();
        let seen: Arc<Mutex<Vec<Json>>> = Arc::default();
        let recorded = seen.clone();
        let selector = Value::callback(move |argument| {
            let descriptors = argument.list()?.remove(0).json()?;
            recorded.lock().unwrap().push(descriptors.clone());
            Ok(Value::Data(json!([false])))
        });
        let result = rig
            .dispatch_started(None, "t/row", "serial", vec![], Some(selector))
            .unwrap()
            .await;
        assert_eq!(settle(result.unwrap()).await.unwrap(), Value::Undefined);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0][0]["row"], Json::Null, "{seen:?}");
        assert_eq!(seen[0][0]["listener"], json!("2"), "{seen:?}");
    })
    .await;
}

/// EVP-5.3-6, EVP-5.3-7
///
/// One dispatch names its calls alike: the same id for every listener,
/// number in one host and never reused; another dispatch gets another. The
/// same id reaches listeners of two sessions, and `own` tells them apart:
/// the dispatching session's entry is its own, the other session's and the
/// host's own emissions are not.
#[tokio::test(flavor = "multi_thread")]
async fn a_dispatch_names_its_calls_and_marks_its_own_session() {
    within("dispatch identity", async {
        let root = common::runtime::root();
        let one = rig_on(root.clone(), "rt1").await;
        let two = rig_on(root.clone(), "rt2").await;
        one.load("chat", None, &[]).await;
        two.load("chat", None, &[]).await;
        one.register("11", "t/id", Some("chat"), "one", "any")
            .await
            .unwrap();
        two.register("22", "t/id", Some("chat"), "two", "any")
            .await
            .unwrap();
        let result = one
            .dispatch(Some("chat"), "t/id", "serial", vec![json!("x").into()])
            .await
            .unwrap();
        assert_eq!(result, Value::Undefined);
        let first = one.fake.calls.lock().unwrap()[0].clone();
        let second = two.fake.calls.lock().unwrap()[0].clone();
        assert_eq!(first["own"], Value::Data(json!(true)), "{first:?}");
        assert_eq!(second["own"], Value::Data(json!(false)), "{second:?}");
        assert_eq!(
            second["dispatch"], first["dispatch"],
            "{first:?} {second:?}"
        );
        let number = dispatch_number(&first["dispatch"]).expect("d<number>");
        // Another dispatch: another id.
        one.dispatch(Some("chat"), "t/id", "serial", vec![])
            .await
            .unwrap();
        let again = one.fake.calls.lock().unwrap()[1].clone();
        assert_ne!(again["dispatch"], first["dispatch"]);
        assert_ne!(dispatch_number(&again["dispatch"]).unwrap(), number);
        // A host emission is no session's own.
        root.emit(Happened("host".into())).unwrap();
        until(
            || two.fake.calls.lock().unwrap().len() == 2,
            "the host's emit at rt2",
        )
        .await;
        let heard = two.fake.calls.lock().unwrap()[1].clone();
        assert_eq!(heard["own"], Value::Data(json!(false)), "{heard:?}");
    })
    .await;
}

/// EVP-5.4-2
///
/// An `emit` answers once every one of its calls has started, in
/// registration order, and with the plain `undefined`: the frames the far
/// end reads are the calls first, then the reply.
#[tokio::test(flavor = "multi_thread")]
async fn an_emit_answers_after_all_its_calls_started() {
    within("emit reply", async {
        let rig = rig().await;
        rig.load("a", None, &[]).await;
        rig.load("b", None, &[]).await;
        rig.register("1", "t/emit", Some("a"), "one", "any")
            .await
            .unwrap();
        rig.register("2", "t/emit", Some("b"), "two", "any")
            .await
            .unwrap();
        let result = rig
            .dispatch(Some("a"), "t/emit", "emit", vec![json!(1).into()])
            .await
            .unwrap();
        assert_eq!(result, Value::Undefined);
        // The frames the far end read, in order: this dispatch ends with
        // its two calls and then the answer, the calls first.
        let frames = rig.frames.lock().unwrap().clone();
        let tail = &frames[frames.len() - 3..];
        assert_eq!(tail[2]["op"], json!("return"), "{frames:#?}");
        assert_eq!(
            tail[2]["value"],
            json!({ "type": "undefined" }),
            "the plain undefined, not a future: {frames:#?}"
        );
        assert_eq!(called(&tail[0]), "1", "{frames:#?}");
        assert_eq!(called(&tail[1]), "2", "{frames:#?}");
    })
    .await;
}

/// EVP-5.7-4
///
/// An emitter that gives up on its `events.dispatch` (`cancel`) does not
/// cancel an `emit`: this one was queued behind another, still starts its
/// calls, and the session's emits go on after it.
///
/// This is the race that used to lose the emit (#1121): when `cancel` reaches
/// the session before its ordered task takes the queued `events.dispatch`, the
/// emit must still start. Deterministically covered in
/// `crates/rutis-bridge/src/session/rpc/tests.rs::a_cancelled_emit_dispatch_stays_queued`.
#[tokio::test(flavor = "multi_thread")]
async fn an_emit_is_not_cancelled() {
    within("emit cancel", async {
        let rig = rig().await;
        rig.load("chat", None, &[]).await;
        rig.register("1", "t/e", Some("chat"), "one", "any")
            .await
            .unwrap();
        // The first emit waits in its filter, so the second one is queued.
        let held: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<Reply>>>> = Arc::default();
        let filtering = Arc::new(AtomicBool::new(false));
        let (release, receiver) = tokio::sync::oneshot::channel::<Reply>();
        *held.lock().unwrap() = Some(receiver);
        let filter = {
            let held = held.clone();
            let filtering = filtering.clone();
            Value::callback(move |_| {
                let waiting = held.lock().unwrap().take();
                match waiting {
                    Some(receiver) => {
                        filtering.store(true, Ordering::SeqCst);
                        Ok(Value::future(async move {
                            receiver
                                .await
                                .unwrap_or_else(|_| Ok(Value::Data(json!([false]))))
                        }))
                    }
                    None => Ok(Value::Data(json!([true]))),
                }
            })
        };
        let first = rig
            .dispatch_started(Some("chat"), "t/e", "emit", vec![], Some(filter))
            .unwrap();
        until(
            || filtering.load(Ordering::SeqCst),
            "the first emit's filter",
        )
        .await;
        let second = rig
            .dispatch_started(Some("chat"), "t/e", "emit", vec![], None)
            .unwrap();
        drop(second);
        // The first keeps no listener; the second is next in line.
        release.send(Ok(Value::Data(json!([false])))).unwrap();
        let answered = settle(first.await.unwrap()).await.unwrap();
        assert_eq!(answered, Value::Undefined);
        let third = rig
            .dispatch(Some("chat"), "t/e", "emit", vec![])
            .await
            .unwrap();
        assert_eq!(third, Value::Undefined);
        until(
            || rig.fake.calls.lock().unwrap().len() == 2,
            "one call each from the second and third emits",
        )
        .await;
        let calls = rig.fake.calls.lock().unwrap().clone();
        for call in &calls {
            assert_eq!(call["listener"], Value::Data(json!("1")), "{calls:?}");
        }
        assert_ne!(calls[0]["dispatch"], calls[1]["dispatch"], "{calls:?}");
    })
    .await;
}

/// EVP-5.4-14
///
/// A waterfall's `next` runs the rest once: a second call in the same
/// waterfall fails with `WaterfallNext` ("already called"), and a call made
/// after the dispatch ended fails with it too ("after its waterfall
/// ended"), even on a handle that was never called.
#[tokio::test(flavor = "multi_thread")]
async fn next_runs_once_and_not_after_the_waterfall_ended() {
    within("next", async {
        let rig = rig().await;
        rig.load("chat", None, &[]).await;
        // The listener of `t/w` calls `next` twice; the host answers a
        // `next` call with a future, so each answer is settled.
        let misuse: Arc<Mutex<Option<(String, String)>>> = Arc::default();
        let twice = {
            let misuse = misuse.clone();
            Arc::new(move |fields: &BTreeMap<String, Value>| -> Reply {
                let next = fields["next"].clone().reference()?;
                let misuse = misuse.clone();
                Ok(Value::future(async move {
                    let first = settle(next.call(Value::List(vec![]))?).await?;
                    assert_eq!(first, Value::Undefined, "the rest is empty");
                    let refused = settle(next.call(Value::List(vec![]))?).await.unwrap_err();
                    match &refused {
                        Error::Remote { name, message, .. } => {
                            *misuse.lock().unwrap() = Some((name.clone(), message.clone()))
                        }
                        other => panic!("expected WaterfallNext, got {other:?}"),
                    }
                    Ok(Value::Undefined)
                }))
            })
        };
        rig.register_with("1", "t/w", Some("chat"), "fake", "async", twice)
            .await
            .unwrap();
        let result = rig
            .dispatch(Some("chat"), "t/w", "waterfall", vec![])
            .await
            .unwrap();
        assert_eq!(result, Value::Undefined);
        assert_eq!(
            misuse.lock().unwrap().take().unwrap(),
            (
                "WaterfallNext".to_owned(),
                "next of t/w was already called".to_owned()
            )
        );

        // Another waterfall's listener keeps its handle, never calls it, and
        // the test calls it once the dispatch ended.
        let kept: Arc<Mutex<Option<Reference>>> = Arc::default();
        let keeping = {
            let kept = kept.clone();
            Arc::new(move |fields: &BTreeMap<String, Value>| -> Reply {
                *kept.lock().unwrap() = Some(fields["next"].clone().reference()?);
                Ok(Value::Undefined)
            })
        };
        rig.register_with("2", "t/w2", Some("chat"), "fake", "async", keeping)
            .await
            .unwrap();
        let result = rig
            .dispatch(Some("chat"), "t/w2", "waterfall", vec![])
            .await
            .unwrap();
        assert_eq!(result, Value::Undefined);
        let next = kept.lock().unwrap().take().unwrap();
        let refused = settle(next.call(Value::List(vec![])).unwrap())
            .await
            .unwrap_err();
        match refused {
            Error::Remote { name, message, .. } => {
                assert_eq!(name, "WaterfallNext");
                assert_eq!(message, "next of t/w2 was called after its waterfall ended");
            }
            other => panic!("expected WaterfallNext, got {other:?}"),
        }
    })
    .await;
}

/// EVP-6-6
///
/// A filter's answers must be one boolean per candidate: a shorter list, or
/// one with a non-boolean, fails the whole dispatch with the protocol's
/// `BindingError` and starts no call.
#[tokio::test(flavor = "multi_thread")]
async fn filter_answers_must_match_the_candidates() {
    within("filter answers", async {
        let rig = rig().await;
        rig.load("chat", None, &[]).await;
        rig.register("1", "t/f", Some("chat"), "one", "any")
            .await
            .unwrap();
        rig.register("2", "t/f", Some("chat"), "two", "any")
            .await
            .unwrap();
        let short = Value::callback(|_| Ok(Value::Data(json!([true]))));
        let answered = rig
            .dispatch_started(Some("chat"), "t/f", "serial", vec![], Some(short))
            .unwrap()
            .await
            .unwrap();
        let refused = settle(answered).await.unwrap_err();
        assert_eq!(
            binding(&refused),
            (
                "BindingError",
                "filter of t/f returned 1 answers for 2 listeners"
            )
        );
        // Not a boolean: still refused, and the message counts the answers.
        let not_boolean = Value::callback(|_| Ok(Value::Data(json!([true, "yes"]))));
        let answered = rig
            .dispatch_started(Some("chat"), "t/f", "serial", vec![], Some(not_boolean))
            .unwrap()
            .await
            .unwrap();
        let refused = settle(answered).await.unwrap_err();
        assert_eq!(
            binding(&refused),
            (
                "BindingError",
                "filter of t/f returned 2 answers for 2 listeners"
            )
        );
        assert!(rig.fake.calls.lock().unwrap().is_empty());
    })
    .await;
}

/// EVP-4.1-6
///
/// `events.off` answers once the entries have left the table: a dispatch
/// queued right after it does not call them. Ids that are not there are
/// ignored, not refused.
#[tokio::test(flavor = "multi_thread")]
async fn events_off_answers_after_the_entries_left() {
    within("events.off", async {
        let rig = rig().await;
        rig.load("chat", None, &[]).await;
        rig.register("1", "t/off", Some("chat"), "one", "any")
            .await
            .unwrap();
        let result = rig
            .dispatch(Some("chat"), "t/off", "serial", vec![])
            .await
            .unwrap();
        assert_eq!(result, Value::Undefined);
        assert_eq!(rig.fake.calls.lock().unwrap().len(), 1);
        // The off's frame is queued before the dispatch's: the entry is
        // gone by the time the dispatch runs.
        let off = rig
            .started("events.off", Value::Data(json!(["1", "no-such-listener"])))
            .unwrap();
        let next = rig
            .dispatch_started(Some("chat"), "t/off", "serial", vec![], None)
            .unwrap();
        let answered = settle(off.await.unwrap()).await.unwrap();
        assert_eq!(answered, Value::Undefined, "an unknown id is ignored");
        let result = settle(next.await.unwrap()).await.unwrap();
        assert_eq!(result, Value::Undefined);
        assert_eq!(
            rig.fake.calls.lock().unwrap().len(),
            1,
            "the withdrawn listener was not called again"
        );
    })
    .await;
}

/// A host service, provided by a plugin row.
struct Clock;

impl HostDispatch for Clock {
    fn invoke(&self, method: &str, _: Value) -> Reply {
        Err(Error::value(format!("no method {method}")))
    }
}

/// A plugin that provides the host service `service` as row `row`.
struct Provides {
    row: &'static str,
    service: &'static str,
}

impl DynPlugin for Provides {
    fn name(&self) -> &str {
        "provider"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, rutis::Error>> {
        Box::pin(async move {
            ctx.mark_row(self.row)?;
            ctx.provide_at(host_key(self.service), Arc::new(Clock))?;
            Ok(Effect::Done)
        })
    }
}

/// EVP-8-3
///
/// `rutis/service` records the service's name, the scope chain, whether it
/// is available, and the providing row: `true` with the new row when the
/// provider is replaced (after the old one's `false`), and `false` with the
/// last provider's row when it goes.
#[tokio::test(flavor = "multi_thread")]
async fn a_service_provider_is_announced_replaced_and_withdrawn() {
    within("rutis/service", async {
        let rig = rig().await;
        rig.register("svc", "rutis/service", None, "fake", "any")
            .await
            .unwrap();
        let first = rig.root.plugin(Provides {
            row: "weather",
            service: "weather",
        });
        until(
            || rig.fake.calls.lock().unwrap().len() == 1,
            "the service's arrival",
        )
        .await;
        assert_eq!(
            service(&rig.fake.calls.lock().unwrap()[0]),
            (
                "weather".to_owned(),
                Vec::new(),
                true,
                Some("weather".to_owned())
            )
        );
        first.dispose().await.unwrap();
        until(
            || rig.fake.calls.lock().unwrap().len() == 2,
            "the service's withdrawal",
        )
        .await;
        assert_eq!(
            service(&rig.fake.calls.lock().unwrap()[1]),
            (
                "weather".to_owned(),
                Vec::new(),
                false,
                Some("weather".to_owned())
            )
        );
        let second = rig.root.plugin(Provides {
            row: "weather-2",
            service: "weather",
        });
        until(
            || rig.fake.calls.lock().unwrap().len() == 3,
            "the new provider",
        )
        .await;
        assert_eq!(
            service(&rig.fake.calls.lock().unwrap()[2]),
            (
                "weather".to_owned(),
                Vec::new(),
                true,
                Some("weather-2".to_owned())
            )
        );
        second.dispose().await.unwrap();
        until(
            || rig.fake.calls.lock().unwrap().len() == 4,
            "the new provider's withdrawal",
        )
        .await;
        assert_eq!(
            service(&rig.fake.calls.lock().unwrap()[3]),
            (
                "weather".to_owned(),
                Vec::new(),
                false,
                Some("weather-2".to_owned())
            )
        );
    })
    .await;
}

/// The record a `rutis/service` call carries: `(name, scope, available, row)`.
fn service(
    call: &BTreeMap<String, Value>,
) -> (String, Vec<(String, String)>, bool, Option<String>) {
    assert_eq!(
        call["name"],
        Value::Data(json!("rutis/service")),
        "{call:?}"
    );
    assert_eq!(call["mode"], Value::Data(json!("emit")), "{call:?}");
    let Value::List(arguments) = call["args"].clone() else {
        panic!("one argument: {call:?}");
    };
    assert_eq!(arguments.len(), 1, "{call:?}");
    let Json::Object(fields) = arguments[0].clone().json().unwrap() else {
        panic!("a record: {call:?}");
    };
    let name = fields["name"].as_str().unwrap().to_owned();
    let chain: Vec<(String, String)> = fields["scope"]
        .as_array()
        .unwrap()
        .iter()
        .map(|scope| {
            (
                scope["id"].as_str().unwrap().to_owned(),
                scope["group"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let available = fields["available"].as_bool().unwrap();
    let row = fields["row"].as_str().map(str::to_owned);
    (name, chain, available, row)
}
