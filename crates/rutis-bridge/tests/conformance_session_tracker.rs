//! Wire-level conformance for the session and value rules the tracking
//! table still marks partial or unverified (#639): a far end scripted frame
//! by frame on a memory channel, so what is checked is the frames and the
//! errors themselves, not an internal helper.
//!
//! Rules whose gap names an implementation other than Rust (Node, Python,
//! Go, Bun, Swift, Kotlin) stay open: these checks close the Rust column
//! only. `M1-13-3` is reproduced here as far as the wire shows it; what a
//! runtime that does not take object references does with one is tested on
//! that runtime's side.
//!
//! rule: SES-6.1-1
//! rule: SES-6.1-3
//! rule: SES-6.1-4
//! rule: SES-6.1-5
//! rule: SES-6.1-6
//! rule: SES-6.1-7
//! rule: SES-6.1-8
//! rule: SES-6.1-9
//! rule: SES-6.1-10
//! rule: SES-6.1-11
//! rule: SES-6.1-12
//! rule: SES-6.1-13
//! rule: SES-6.1-14
//! rule: SES-6.1-15
//! rule: SES-6.1-16
//! rule: SES-6.1-17
//! rule: SES-6.1-20
//! rule: SES-6.1-22
//! rule: SES-6.1-24
//! rule: SES-6.1-26
//! rule: SES-6.1-27
//! rule: SES-7.1-6
//! rule: CHN-4-4
//! rule: CHN-4-6
//! rule: CHN-4-8
//! rule: CHN-4-9
//! rule: CHN-4-10
//! rule: TAG-2-1
//! rule: M1-7.1-1
//! rule: M1-13-3
//! guarantee: bridge/value-tags
//! guarantee: bridge/object-refs

use std::sync::{mpsc, Arc};

use rutis_bridge::channel::{Channel, PeerId};
use rutis_bridge::session::{
    BigInt, Connection, Decimal, Dispatch, Endpoint, Error, Instant, Reply, Uuid, Value,
    ENDPOINT_PROTOCOL,
};
use rutis_test_support::within;
use serde_json::{json, Value as Json};

/// The capabilities of every tagged value, as a far end declares them when
/// it takes all of them.
const VALUE_CAPABILITIES: [&str; 10] = [
    "value-instant",
    "value-datetime",
    "value-date",
    "value-time",
    "value-bytes",
    "value-map",
    "value-set",
    "value-bigint",
    "value-decimal",
    "value-uuid",
];

fn id(name: &str) -> PeerId {
    PeerId::new(name).unwrap()
}

/// A far end that serves nothing.
struct Nothing;
impl Dispatch for Nothing {
    fn invoke(&self, _: &Connection, _: &str, _: &str, _: Value) -> Reply {
        Err(Error::value("main serves nothing"))
    }
}

/// A far end that answers a call with its arguments.
struct Echo;
impl Dispatch for Echo {
    fn invoke(&self, _: &Connection, _: &str, _: &str, args: Value) -> Reply {
        Ok(args)
    }
}

/// A far end that returns an instant from `instant`, and its arguments
/// otherwise.
struct Instanting;
impl Dispatch for Instanting {
    fn invoke(&self, _: &Connection, _: &str, method: &str, args: Value) -> Reply {
        match method {
            "instant" => Ok(Value::Instant(Instant::from_unix(0, 0).unwrap())),
            _ => Ok(args),
        }
    }
}

/// A far end that answers every call with `ok`: the tagged value itself is
/// what the far end scripted on the channel checks.
struct Ack;
impl Dispatch for Ack {
    fn invoke(&self, _: &Connection, _: &str, _: &str, _: Value) -> Reply {
        Ok(Value::Data(json!("ok")))
    }
}

/// A far end that hands out a function reference from `give`.
struct Giving;
impl Dispatch for Giving {
    fn invoke(&self, _: &Connection, _: &str, method: &str, _: Value) -> Reply {
        match method {
            "give" => Ok(Value::callback(|_| Ok(Value::Undefined))),
            _ => Ok(Value::Undefined),
        }
    }
}

/// A session between this side (`main`) and a far end scripted on the
/// channel, with this side's capabilities.
fn session_with(dispatch: Arc<dyn Dispatch>, endpoint: Endpoint) -> (Connection, Channel) {
    let (ours, theirs) = rutis_bridge::memory::pair();
    let peer = Connection::open(ours, dispatch, endpoint).unwrap();
    (peer, theirs)
}

/// A session between this side (`main`) and a far end scripted on the
/// channel.
fn session(dispatch: Arc<dyn Dispatch>) -> (Connection, Channel) {
    session_with(dispatch, Endpoint::rust(id("main")))
}

fn read(channel: &mut Channel) -> Json {
    match channel.receiver.recv().unwrap() {
        Some(message) => serde_json::from_slice(&message).unwrap(),
        None => panic!("the channel ended"),
    }
}

/// [`read`] for a frame nested deeper than serde_json's own limit: the
/// depth rule (SES-6.1-20) writes frames of about 130 levels.
fn read_deep(channel: &mut Channel) -> Json {
    use serde::Deserialize;
    let message = match channel.receiver.recv().unwrap() {
        Some(message) => message,
        None => panic!("the channel ended"),
    };
    let mut deserializer = serde_json::Deserializer::from_slice(&message);
    deserializer.disable_recursion_limit();
    let frame = Json::deserialize(&mut deserializer).expect("a frame");
    deserializer.end().expect("one frame");
    frame
}

fn send(channel: &mut Channel, frame: Json) {
    channel
        .sender
        .send(&serde_json::to_vec(&frame).unwrap())
        .unwrap();
}

/// Answer this side's greeting as `endpoint`, declaring `capabilities`, so
/// the session is ready.
fn greet(channel: &mut Channel, endpoint: &str, capabilities: &[&str]) {
    let hello = read(channel);
    assert_eq!(hello["op"], json!("hello"), "{hello}");
    assert_eq!(hello["version"], json!(ENDPOINT_PROTOCOL), "{hello}");
    assert_eq!(hello["endpoint"], json!("main"), "{hello}");
    // The session protocol has no `objects` capability: what a far end takes
    // by reference is not declared, so nothing can be checked before sending
    // an object reference (M1-13-3).
    let declared = hello["capabilities"].as_array().expect("capabilities");
    assert!(
        declared.iter().all(|capability| capability != "objects"),
        "{hello}"
    );
    send(
        channel,
        json!({
            "op": "hello",
            "version": ENDPOINT_PROTOCOL,
            "endpoint": endpoint,
            "capabilities": capabilities,
        }),
    );
}

/// The `id` of a frame the far end reads.
fn frame_id(frame: &Json) -> String {
    frame["id"].as_str().expect("an id").to_owned()
}

/// A call from the far end into this side, with `args` as the protocol's
/// value shape.
fn invoke(id: &str, target: &str, method: &str, args: Json) -> Json {
    json!({
        "op": "invoke",
        "id": id,
        "path": [],
        "target": target,
        "method": method,
        "args": args,
    })
}

/// One argument of a call, as the protocol carries an argument list.
fn one(arg: &Json) -> Json {
    json!({ "type": "list", "value": [arg] })
}

/// A call of `echo` with `arg` as its only argument; the reply the far end
/// reads.
fn echo(far: &mut Channel, id: &str, arg: &Json) -> Json {
    send(far, invoke(id, "svc", "echo", one(arg)));
    let reply = read(far);
    assert_eq!(reply["id"], json!(id), "{reply}");
    reply
}

/// `arg` crosses and comes back exactly as it was sent.
fn accepted(far: &mut Channel, id: &str, arg: &Json) {
    let reply = echo(far, id, arg);
    assert_eq!(reply["op"], json!("return"), "{arg}: {reply}");
    assert_eq!(reply["value"], one(arg), "{arg}: {reply}");
}

/// The error a far end reads in a `throw`: `(name, message, code, data)`.
fn thrown(frame: &Json) -> (String, String, Option<String>, Option<Json>) {
    assert_eq!(frame["op"], json!("throw"), "{frame}");
    let error = &frame["error"];
    let root = &error["graph"]["nodes"][0];
    let data = root.get("data").map(|data| match data.get("value") {
        Some(value) => value.clone(),
        None => data.clone(),
    });
    (
        error["name"].as_str().expect("a name").to_owned(),
        error["message"].as_str().expect("a message").to_owned(),
        root["code"].as_str().map(str::to_owned),
        data,
    )
}

/// `arg` is refused only for its call: a `BindingError` with code
/// `bad-value`, the empty data of a received value, and `said` in its
/// message. The session goes on.
fn refused(far: &mut Channel, id: &str, arg: &Json, said: &str) {
    let reply = echo(far, id, arg);
    let (name, message, code, data) = thrown(&reply);
    assert_eq!(name, "BindingError", "{arg}: {reply}");
    assert_eq!(code.as_deref(), Some("bad-value"), "{arg}: {reply}");
    assert_eq!(data, Some(json!({})), "{arg}: {reply}");
    assert!(message.contains(said), "{arg}: {message}");
}

/// `arg` is not a value frame at all: the session ends as a transport
/// failure saying `said`, and a call after it fails.
async fn malformed(arg: &Json, said: &str) {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    send(&mut far, invoke("node:1", "svc", "echo", one(arg)));
    within(said, peer.closed()).await;
    match peer.close_reason() {
        Some(Error::Transport(message)) => assert!(message.contains(said), "{arg}: {message}"),
        other => panic!("{arg}: a transport error expected, got {other:?}"),
    }
    assert!(
        peer.invoke("svc", "echo", Value::Undefined).is_err(),
        "{arg}: a call after the end fails"
    );
}

/// `frame` is not a frame this side can read: the session ends as a
/// transport failure saying `said`, without a reply, and a call after it
/// fails.
async fn unreadable(frame: Json, said: &str) {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &[]);
    peer.ready().await.unwrap();
    send(&mut far, frame.clone());
    within(said, peer.closed()).await;
    match peer.close_reason() {
        Some(Error::Transport(message)) => assert!(message.contains(said), "{frame}: {message}"),
        other => panic!("{frame}: a transport error expected, got {other:?}"),
    }
    assert!(
        matches!(far.receiver.recv(), Ok(None)),
        "{frame}: the channel ends without a reply"
    );
    assert!(
        peer.invoke("svc", "echo", Value::Undefined).is_err(),
        "{frame}: a call after the end fails"
    );
}

/// A value is `{type: tag, value: …}` (SES-6.1-1): data, lists, records and
/// `undefined` cross in that shape, `undefined` kept as a whole argument
/// and result.
/// rule: SES-6.1-1
#[tokio::test(flavor = "multi_thread")]
async fn values_cross_in_the_protocols_tagged_shape() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();

    let samples = [
        json!({ "type": "data", "value": { "a": [1, true, null] } }),
        json!({ "type": "list", "value": [{ "type": "data", "value": 1 }, { "type": "data", "value": "x" }] }),
        json!({ "type": "record", "value": { "a": { "type": "data", "value": 1 } } }),
    ];
    for (n, arg) in samples.iter().enumerate() {
        accepted(&mut far, &format!("node:{}", n + 1), arg);
    }
    // A whole argument `undefined` stays `undefined`; the call's argument
    // list is a `list` value.
    send(
        &mut far,
        invoke("node:9", "svc", "echo", json!({ "type": "undefined" })),
    );
    assert_eq!(
        read(&mut far),
        json!({ "id": "node:9", "op": "return", "value": { "type": "undefined" } })
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// An ordinary JSON field is never read as a type (SES-6.1-3): data whose
/// content has the shape of a marker crosses as data, and the `type` and
/// `value` of a record are fields.
/// rule: SES-6.1-3
#[tokio::test(flavor = "multi_thread")]
async fn data_shaped_like_a_marker_stays_data() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    accepted(
        &mut far,
        "node:1",
        &json!({
            "type": "data",
            "value": { "type": "instant", "value": "2026-10-15T03:22:15Z" },
        }),
    );
    accepted(
        &mut far,
        "node:2",
        &json!({
            "type": "list",
            "value": [
                { "type": "data", "value": { "type": "date", "value": "2024-02-29" } },
                { "type": "data", "value": { "type": "bytes", "value": "AA==" } },
            ],
        }),
    );
    accepted(
        &mut far,
        "node:3",
        &json!({
            "type": "record",
            "value": {
                "type": { "type": "data", "value": "instant" },
                "value": { "type": "data", "value": "2026-10-15T03:22:15Z" },
            },
        }),
    );
    // The same shape from this side goes as data, not as a tag.
    let far_end = std::thread::spawn(move || {
        let call = read(&mut far);
        assert_eq!(
            call["args"],
            json!({ "type": "data", "value": { "type": "instant", "value": "2026-10-15T03:22:15Z" } }),
            "{call}"
        );
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "ok" } }),
        );
        far
    });
    let answer = peer.invoke(
        "svc",
        "take",
        Value::Data(json!({ "type": "instant", "value": "2026-10-15T03:22:15Z" })),
    );
    assert_eq!(answer.unwrap().json().unwrap(), json!("ok"));
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `instant` is RFC 3339 in UTC: `YYYY-MM-DDTHH:MM:SS`, an optional 1–9
/// digit fraction, uppercase `T` and `Z`, years 0001–9999, seconds 00–59
/// (SES-6.1-4). Only the canonical text crosses.
/// rule: SES-6.1-4
#[tokio::test(flavor = "multi_thread")]
async fn instants_are_rfc3339_utc_text() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    for (n, text) in [
        "2026-10-15T03:22:15Z",
        "2026-10-15T03:22:15.5Z",
        "2026-10-15T03:22:15.123456789Z",
        "0001-01-01T00:00:00Z",
        "9999-12-31T23:59:59.999999999Z",
    ]
    .into_iter()
    .enumerate()
    {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "instant", "value": text }),
        );
    }
    let refused_texts = [
        "2026-10-15T03:22:15z",
        "2026-10-15T03:22:15+00:00",
        "2026-10-15 03:22:15Z",
        "2026-02-30T00:00:00Z",
        "2026-10-15T03:22:60Z",
        "0000-01-01T00:00:00Z",
        "2026-10-15T03:22:15.1234567890Z",
    ];
    for (n, text) in refused_texts.into_iter().enumerate() {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "instant", "value": text }),
            "invalid instant",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "instant", "value": "2026-10-15T03:22:15.5Z" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `datetime` is a local date and time, `YYYY-MM-DDTHH:MM:SS` with an
/// optional fraction and no zone (SES-6.1-5): it crosses unchanged, never
/// read in a time zone and never as an `instant`.
/// rule: SES-6.1-5
#[tokio::test(flavor = "multi_thread")]
async fn datetimes_are_local_text_without_a_zone() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    for (n, text) in [
        "2026-10-15T03:22:15",
        "2026-10-15T03:22:15.5",
        "2026-10-15T03:22:15.123456789",
        "0001-01-01T00:00:00",
        "9999-12-31T23:59:59.999999999",
        "2026-10-15T00:00:00",
    ]
    .into_iter()
    .enumerate()
    {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "datetime", "value": text }),
        );
    }
    let refused_texts = [
        "2026-10-15T03:22:15Z",
        "2026-10-15T03:22:15+00:00",
        "2026-10-15T03:22:15-05:00",
        "2026-10-15 03:22:15",
        "2026-02-30T00:00:00",
        "2026-10-15T24:00:00",
        "2026-10-15T03:22:60",
        "2026-10-15T03:22:15.1234567890",
        "0000-01-01T00:00:00",
    ];
    for (n, text) in refused_texts.into_iter().enumerate() {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "datetime", "value": text }),
            "invalid datetime",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "datetime", "value": "2026-10-15T03:22:15.5" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `date` is `YYYY-MM-DD`, years 0001–9999, and must be a day that exists
/// (SES-6.1-6).
/// rule: SES-6.1-6
#[tokio::test(flavor = "multi_thread")]
async fn dates_must_be_days_that_exist() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    for (n, text) in ["2024-02-29", "0001-01-01", "9999-12-31", "2026-04-30"]
        .into_iter()
        .enumerate()
    {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "date", "value": text }),
        );
    }
    for (n, text) in [
        "2026-02-29",
        "2026-04-31",
        "2026-13-01",
        "2026-00-10",
        "0000-01-01",
    ]
    .into_iter()
    .enumerate()
    {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "date", "value": text }),
            "invalid date",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "date", "value": "2024-02-29" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `time` is `HH:MM:SS`, 24-hour 00–23, an optional 1–9 digit fraction and
/// no zone (SES-6.1-7).
/// rule: SES-6.1-7
#[tokio::test(flavor = "multi_thread")]
async fn times_are_24_hour_text_without_a_zone() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    for (n, text) in [
        "00:00:00",
        "23:59:59",
        "14:30:05.25",
        "00:00:00.000000001",
        "12:34:56.789",
    ]
    .into_iter()
    .enumerate()
    {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "time", "value": text }),
        );
    }
    let refused_texts = [
        "24:00:00",
        "14:60:00",
        "14:30:60",
        "14:30:05Z",
        "14:30:05+01:00",
        "14:30:05+0100",
        "1:30:05",
        "14:30",
        "14:30:05.",
        "14:30:05.1234567890",
        " 14:30:05",
    ];
    for (n, text) in refused_texts.into_iter().enumerate() {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "time", "value": text }),
            "invalid time",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "time", "value": "14:30:05.25" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `bytes` is RFC 4648 §4 base64, padded, without whitespace, the unused
/// low bits zero: only the canonical text crosses (SES-6.1-8).
/// rule: SES-6.1-8
#[tokio::test(flavor = "multi_thread")]
async fn bytes_are_canonical_padded_base64() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    for (n, text) in ["", "AA==", "AAEC", "aGVsbG8="].into_iter().enumerate() {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "bytes", "value": text }),
        );
    }
    for (n, text) in [
        "AA",     // no padding
        "AA===",  // too much padding
        "A A==",  // whitespace
        "AB==",   // unused low bits not zero
        "-_==",   // URL alphabet
        "AA==\n", // whitespace
    ]
    .into_iter()
    .enumerate()
    {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "bytes", "value": text }),
            "invalid bytes",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "bytes", "value": "AA==" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `map` is `[[key, value], …]` in the sender's order (SES-6.1-9): the
/// order crosses both ways, not sorted or regrouped.
/// rule: SES-6.1-9
#[tokio::test(flavor = "multi_thread")]
async fn maps_keep_the_senders_order() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    accepted(
        &mut far,
        "node:1",
        &json!({ "type": "map", "value": [
            [{ "type": "data", "value": "z" }, { "type": "data", "value": 3 }],
            [{ "type": "data", "value": "a" }, { "type": "data", "value": 1 }],
            [{ "type": "data", "value": "m" }, { "type": "data", "value": 2 }],
            [{ "type": "bigint", "value": "9007199254740993" }, { "type": "data", "value": "big" }],
        ] }),
    );
    let far_end = std::thread::spawn(move || {
        let call = read(&mut far);
        assert_eq!(
            call["args"],
            json!({ "type": "map", "value": [
                [{ "type": "data", "value": "z" }, { "type": "data", "value": 3 }],
                [{ "type": "data", "value": "a" }, { "type": "data", "value": 1 }],
                [{ "type": "data", "value": "m" }, { "type": "data", "value": 2 }],
                [{ "type": "bigint", "value": "9007199254740993" }, { "type": "data", "value": "big" }],
            ] }),
            "{call}"
        );
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "ok" } }),
        );
        far
    });
    let sent = Value::Map(vec![
        (Value::Data(json!("z")), Value::Data(json!(3))),
        (Value::Data(json!("a")), Value::Data(json!(1))),
        (Value::Data(json!("m")), Value::Data(json!(2))),
        (
            Value::BigInt("9007199254740993".parse::<BigInt>().unwrap()),
            Value::Data(json!("big")),
        ),
    ]);
    assert_eq!(
        peer.invoke("svc", "take", sent).unwrap().json().unwrap(),
        json!("ok")
    );
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `set` is `[element, …]` in the sender's order (SES-6.1-10).
/// rule: SES-6.1-10
#[tokio::test(flavor = "multi_thread")]
async fn sets_keep_the_senders_order() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    accepted(
        &mut far,
        "node:1",
        &json!({ "type": "set", "value": [
            { "type": "data", "value": "z" },
            { "type": "data", "value": "a" },
            { "type": "data", "value": "m" },
        ] }),
    );
    let far_end = std::thread::spawn(move || {
        let call = read(&mut far);
        assert_eq!(
            call["args"],
            json!({ "type": "set", "value": [
                { "type": "data", "value": "z" },
                { "type": "data", "value": "a" },
                { "type": "data", "value": "m" },
            ] }),
            "{call}"
        );
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "ok" } }),
        );
        far
    });
    let sent = Value::Set(vec![
        Value::Data(json!("z")),
        Value::Data(json!("a")),
        Value::Data(json!("m")),
    ]);
    assert_eq!(
        peer.invoke("svc", "take", sent).unwrap().json().unwrap(),
        json!("ok")
    );
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `bigint` is a decimal integer, an optional `-`, no leading zeros, no
/// `-0`, at most 4300 digits (SES-6.1-11).
/// rule: SES-6.1-11
#[tokio::test(flavor = "multi_thread")]
async fn bigints_have_no_leading_zero_and_at_most_4300_digits() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    let longest = "1".repeat(4300);
    for (n, text) in ["0", "-5", "123456789012345678901234567890", &longest]
        .into_iter()
        .enumerate()
    {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "bigint", "value": text }),
        );
    }
    let too_long = "1".repeat(4301);
    for (n, text) in ["007", "-0", "+5", "", &too_long].into_iter().enumerate() {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "bigint", "value": text }),
            "invalid bigint",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "bigint", "value": "0" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `decimal` is `-?(0|[1-9][0-9]*)(\.[0-9]+)?`: no exponent, the trailing
/// zeros kept, at most 4300 digits (SES-6.1-12).
/// rule: SES-6.1-12
#[tokio::test(flavor = "multi_thread")]
async fn decimals_are_decimal_text_with_their_trailing_zeros() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    for (n, text) in ["0", "-0.0", "1.50", "123.456"].into_iter().enumerate() {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "decimal", "value": text }),
        );
    }
    for (n, text) in ["1e5", "1.", ".5", "01", "+1", "NaN", "1.0e5"]
        .into_iter()
        .enumerate()
    {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "decimal", "value": text }),
            "invalid decimal",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "decimal", "value": "1.50" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// `uuid` is lowercase `8-4-4-4-12` hex (SES-6.1-13): uppercase, braces
/// and any other layout are refused, and the session goes on.
/// rule: SES-6.1-13
#[tokio::test(flavor = "multi_thread")]
async fn uuids_are_lowercase_in_the_canonical_layout() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    for (n, text) in [
        "00000000-0000-0000-0000-000000000000",
        "0f8fad5b-d9cb-469f-a165-70867728950e",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
    ]
    .into_iter()
    .enumerate()
    {
        accepted(
            &mut far,
            &format!("node:{}", n + 1),
            &json!({ "type": "uuid", "value": text }),
        );
    }
    for (n, text) in [
        "0F8FAD5B-D9CB-469F-A165-70867728950E",
        "0f8fad5b-d9cb-469f-a165-70867728950g",
        "{0f8fad5b-d9cb-469f-a165-70867728950e}",
        "0f8fad5bd9cb469fa16570867728950e",
        "0f8fad5b-d9cb-469f-a165-70867728950",
        "0f8fad5b-d9cb-469f-a165-70867728950e0",
        "0f8fad5b-d9cb-469f-a165-7086772895-0e",
    ]
    .into_iter()
    .enumerate()
    {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "uuid", "value": text }),
            "invalid uuid",
        );
    }
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "uuid", "value": "0f8fad5b-d9cb-469f-a165-70867728950e" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A map key and a set element may only be a string, a finite number, a
/// boolean, or a bigint, datetime, date, time, decimal or uuid value
/// (SES-6.1-14); anything else fails only that call.
/// rule: SES-6.1-14
#[tokio::test(flavor = "multi_thread")]
async fn map_keys_and_set_elements_are_limited_to_key_kinds() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    let allowed = json!([
        [{ "type": "data", "value": "s" }, { "type": "data", "value": 0 }],
        [{ "type": "data", "value": 1 }, { "type": "data", "value": 1 }],
        [{ "type": "data", "value": true }, { "type": "data", "value": 2 }],
        [{ "type": "bigint", "value": "9007199254740993" }, { "type": "data", "value": 3 }],
        [{ "type": "datetime", "value": "2026-10-15T03:22:15" }, { "type": "data", "value": 4 }],
        [{ "type": "date", "value": "2026-10-15" }, { "type": "data", "value": 5 }],
        [{ "type": "time", "value": "03:22:15" }, { "type": "data", "value": 6 }],
        [{ "type": "decimal", "value": "1.50" }, { "type": "data", "value": 7 }],
        [{ "type": "uuid", "value": "0f8fad5b-d9cb-469f-a165-70867728950e" }, { "type": "data", "value": 8 }],
    ]);
    accepted(
        &mut far,
        "node:1",
        &json!({ "type": "map", "value": allowed }),
    );
    accepted(
        &mut far,
        "node:2",
        &json!({ "type": "set", "value": [
            { "type": "data", "value": "s" },
            { "type": "data", "value": 1 },
            { "type": "data", "value": true },
            { "type": "bigint", "value": "9007199254740993" },
            { "type": "datetime", "value": "2026-10-15T03:22:15" },
            { "type": "date", "value": "2026-10-15" },
            { "type": "time", "value": "03:22:15" },
            { "type": "decimal", "value": "1.50" },
            { "type": "uuid", "value": "0f8fad5b-d9cb-469f-a165-70867728950e" },
        ] }),
    );
    let refused_keys = [
        json!({ "type": "instant", "value": "2026-10-15T03:22:15Z" }),
        json!({ "type": "bytes", "value": "AA==" }),
        json!({ "type": "data", "value": null }),
        json!({ "type": "undefined" }),
        json!({ "type": "list", "value": [] }),
        json!({ "type": "record", "value": {} }),
        json!({ "type": "map", "value": [] }),
        json!({ "type": "set", "value": [] }),
        json!({ "type": "signal" }),
    ];
    for (n, key) in refused_keys.iter().enumerate() {
        refused(
            &mut far,
            &format!("node:{}", n + 10),
            &json!({ "type": "map", "value": [[key, { "type": "data", "value": 1 }]] }),
            "a map key cannot be ",
        );
    }
    refused(
        &mut far,
        "node:30",
        &json!({ "type": "set", "value": [
            { "type": "instant", "value": "2026-10-15T03:22:15Z" },
        ] }),
        "a set element cannot be an instant",
    );
    accepted(
        &mut far,
        "node:99",
        &json!({ "type": "map", "value": [
            [{ "type": "data", "value": "k" }, { "type": "data", "value": 1 }],
        ] }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// An integer beyond ±(2^53−1) is a `bigint` key, equal to the same
/// `bigint`, and a whole-number float beyond that range cannot be a key
/// (SES-6.1-15).
/// rule: SES-6.1-15
#[tokio::test(flavor = "multi_thread")]
async fn bigint_keys_equal_the_same_value_and_float_keys_are_refused() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    accepted(
        &mut far,
        "node:1",
        &json!({ "type": "map", "value": [
            [{ "type": "bigint", "value": "9007199254740993" }, { "type": "data", "value": "a" }],
            [{ "type": "bigint", "value": "-9007199254740993" }, { "type": "data", "value": "b" }],
        ] }),
    );
    // The same integer as data crosses as a bigint key: the two are one key.
    refused(
        &mut far,
        "node:2",
        &json!({ "type": "map", "value": [
            [{ "type": "bigint", "value": "9007199254740993" }, { "type": "data", "value": "a" }],
            [{ "type": "data", "value": 9007199254740993u64 }, { "type": "data", "value": "b" }],
        ] }),
        "duplicate map key 9007199254740993",
    );
    // A whole-number float beyond the range is not exact in every
    // language, so it cannot be a key at all.
    refused(
        &mut far,
        "node:3",
        &json!({ "type": "map", "value": [
            [{ "type": "data", "value": 9007199254740992.0 }, { "type": "data", "value": "a" }],
        ] }),
        "a map key cannot be the float",
    );
    accepted(
        &mut far,
        "node:4",
        &json!({ "type": "map", "value": [
            [{ "type": "bigint", "value": "5" }, { "type": "data", "value": "small" }],
        ] }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A sender checks the keys of a map and the elements of a set before it
/// sends them (SES-6.1-17): a duplicate or a kind no language takes as a
/// key fails that call before any frame is written, naming the argument.
/// rule: SES-6.1-17
#[tokio::test(flavor = "multi_thread")]
async fn map_keys_are_checked_before_the_frame_is_written() {
    let (peer, mut far) = session(Arc::new(Ack));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    let duplicates = Value::Map(vec![
        (Value::Data(json!(1)), Value::Data(json!("a"))),
        (Value::Data(json!(1.0)), Value::Data(json!("b"))),
    ]);
    let refused_by_key = Value::Map(vec![(
        Value::Instant(Instant::from_unix(0, 0).unwrap()),
        Value::Data(json!("a")),
    )]);
    let refused_by_element = Value::Set(vec![Value::Data(json!("x")), Value::Data(json!("x"))]);
    for (n, (value, said)) in [
        (duplicates, "duplicate map key 1"),
        (refused_by_key, "a map key cannot be an instant"),
        (refused_by_element, "duplicate set element"),
    ]
    .into_iter()
    .enumerate()
    {
        let error = peer
            .invoke("svc", "take", Value::List(vec![value]))
            .expect_err("the value is refused before it is sent");
        assert_eq!(error.code(), Some("bad-value"), "{n}: {error}");
        assert_eq!(
            error.data(),
            Some(json!({ "part": "argument", "argument": 1 })),
            "{n}: {error}"
        );
        assert!(error.to_string().contains(said), "{n}: {error}");
    }
    // Nothing of the refused calls was written: the far end's next frame is
    // the call that is sent.
    let far_end = std::thread::spawn(move || {
        let call = read(&mut far);
        assert_eq!(call["op"], json!("invoke"), "{call}");
        assert_eq!(
            call["args"],
            json!({ "type": "list", "value": [{ "type": "map", "value": [
                [{ "type": "data", "value": "k" }, { "type": "data", "value": 1 }],
            ] }] }),
            "{call}"
        );
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "ok" } }),
        );
        far
    });
    let sent = Value::List(vec![Value::Map(vec![(
        Value::Data(json!("k")),
        Value::Data(json!(1)),
    )])]);
    assert_eq!(
        peer.invoke("svc", "take", sent).unwrap().json().unwrap(),
        json!("ok")
    );
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// Two keys of one map, or two elements of one set, may not be equal:
/// numbers by value (`1` and `1.0`), the rest by kind and text
/// (SES-6.1-16).
/// rule: SES-6.1-16
#[tokio::test(flavor = "multi_thread")]
async fn equal_map_keys_and_set_elements_are_refused() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    refused(
        &mut far,
        "node:1",
        &json!({ "type": "map", "value": [
            [{ "type": "data", "value": 1 }, { "type": "data", "value": "a" }],
            [{ "type": "data", "value": 1.0 }, { "type": "data", "value": "b" }],
        ] }),
        "duplicate map key",
    );
    refused(
        &mut far,
        "node:2",
        &json!({ "type": "set", "value": [
            { "type": "data", "value": "x" },
            { "type": "data", "value": "x" },
        ] }),
        "duplicate set element",
    );
    accepted(
        &mut far,
        "node:3",
        &json!({ "type": "map", "value": [
            [{ "type": "data", "value": 1 }, { "type": "data", "value": "a" }],
            [{ "type": "data", "value": 2 }, { "type": "data", "value": "b" }],
        ] }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A `map` and a `set` count one of the 64 levels a value may nest
/// (SES-6.1-20), like a list: a value 64 levels deep crosses, 65 is
/// refused before it is sent, naming the argument and `too-deep`.
/// rule: SES-6.1-20
#[tokio::test(flavor = "multi_thread")]
async fn a_map_or_set_counts_one_of_the_sixty_four_levels() {
    fn lists(levels: usize, inner: Value) -> Value {
        (0..levels).fold(inner, |inner, _| Value::List(vec![inner]))
    }
    fn map() -> Value {
        Value::Map(vec![(Value::Data(json!("k")), Value::Data(json!(1)))])
    }
    fn set() -> Value {
        Value::Set(vec![Value::Data(json!("x"))])
    }
    let (peer, mut far) = session(Arc::new(Ack));
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    // One level too many: the map or set is the 65th.
    for (n, value) in [lists(64, map()), lists(64, set())].into_iter().enumerate() {
        let error = peer
            .invoke("svc", "take", Value::List(vec![value]))
            .expect_err("the value nests deeper than 64 levels");
        assert_eq!(error.code(), Some("bad-value"), "{n}: {error}");
        assert_eq!(
            error.data(),
            Some(json!({ "part": "argument", "argument": 1, "reason": "too-deep" })),
            "{n}: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("the value nests deeper than 64 levels"),
            "{n}: {error}"
        );
    }
    let far_end = std::thread::spawn(move || {
        for (n, kind) in ["map", "set"].into_iter().enumerate() {
            let call = read_deep(&mut far);
            // The argument list, then 63 lists: the map or set is the 64th
            // level.
            let mut value = &call["args"];
            for _ in 0..64 {
                assert_eq!(value["type"], json!("list"), "{n}: {call}");
                value = &value["value"][0];
            }
            assert_eq!(value["type"], json!(kind), "{n}: {call}");
            send(
                &mut far,
                json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "ok" } }),
            );
        }
        far
    });
    for (n, value) in [lists(63, map()), lists(63, set())].into_iter().enumerate() {
        assert_eq!(
            peer.invoke("svc", "take", Value::List(vec![value]))
                .unwrap()
                .json()
                .unwrap(),
            json!("ok"),
            "{n}"
        );
    }
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// Each tag has one capability, `value-<tag>`, and a far end may declare
/// only some of them (SES-6.1-22): a value whose tag it declared crosses,
/// one it did not fails before it is sent, naming the tag and the
/// capability; the rest of the session goes on.
/// rule: SES-6.1-22
#[tokio::test(flavor = "multi_thread")]
async fn a_partial_declaration_takes_only_the_declared_tags() {
    let declared: Vec<&str> = VALUE_CAPABILITIES
        .iter()
        .copied()
        .filter(|capability| !matches!(*capability, "value-instant" | "value-bytes"))
        .collect();
    let (peer, mut far) = session(Arc::new(Ack));
    greet(&mut far, "node", &declared);
    peer.ready().await.unwrap();
    // An undeclared tag fails before the frame is written.
    for (n, (tag, capability, value)) in [
        (
            "instant",
            "value-instant",
            Value::Instant(Instant::from_unix(0, 0).unwrap()),
        ),
        ("bytes", "value-bytes", Value::Bytes(vec![0, 1])),
    ]
    .into_iter()
    .enumerate()
    {
        let error = peer
            .invoke("svc", "take", Value::List(vec![value]))
            .expect_err("the far end did not declare the tag");
        assert_eq!(error.code(), Some("bad-value"), "{n}: {error}");
        assert_eq!(
            error.data(),
            Some(json!({ "part": "argument", "argument": 1 })),
            "{n}: {error}"
        );
        assert!(
            error.to_string().contains(&format!(
                "the far end does not take {tag} values (it did not declare the capability {capability})"
            )),
            "{n}: {error}"
        );
    }
    // Every declared tag crosses, unchanged.
    let far_end = std::thread::spawn(move || {
        for (tag, text) in [
            ("datetime", "2026-10-15T03:22:15"),
            ("date", "2026-10-15"),
            ("time", "03:22:15"),
            (
                "map",
                "[[{\"type\":\"data\",\"value\":\"k\"},{\"type\":\"data\",\"value\":1}]]",
            ),
            ("set", "[{\"type\":\"data\",\"value\":\"x\"}]"),
            ("bigint", "9007199254740993"),
            ("decimal", "1.50"),
            ("uuid", "0f8fad5b-d9cb-469f-a165-70867728950e"),
        ]
        .into_iter()
        {
            let call = read(&mut far);
            let argument = &call["args"]["value"][0];
            assert_eq!(argument["type"], json!(tag), "{call}");
            if tag == "map" || tag == "set" {
                assert_eq!(
                    argument["value"],
                    serde_json::from_str::<Json>(text).unwrap(),
                    "{call}"
                );
            } else {
                assert_eq!(argument["value"], json!(text), "{call}");
            }
            send(
                &mut far,
                json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "ok" } }),
            );
        }
        let call = read(&mut far);
        assert_eq!(
            call["args"],
            json!({ "type": "list", "value": [{ "type": "data", "value": "after" }] }),
            "{call}"
        );
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "ok" } }),
        );
        far
    });
    let values = [
        Value::DateTime("2026-10-15T03:22:15".parse().unwrap()),
        Value::Date("2026-10-15".parse().unwrap()),
        Value::Time("03:22:15".parse().unwrap()),
        Value::Map(vec![(Value::Data(json!("k")), Value::Data(json!(1)))]),
        Value::Set(vec![Value::Data(json!("x"))]),
        Value::BigInt("9007199254740993".parse::<BigInt>().unwrap()),
        Value::Decimal("1.50".parse::<Decimal>().unwrap()),
        Value::Uuid(
            "0f8fad5b-d9cb-469f-a165-70867728950e"
                .parse::<Uuid>()
                .unwrap(),
        ),
    ];
    for (n, value) in values.into_iter().enumerate() {
        assert_eq!(
            peer.invoke("svc", "take", Value::List(vec![value]))
                .unwrap()
                .json()
                .unwrap(),
            json!("ok"),
            "{n}"
        );
    }
    assert_eq!(
        peer.invoke(
            "svc",
            "take",
            Value::List(vec![Value::Data(json!("after"))])
        )
        .unwrap()
        .json()
        .unwrap(),
        json!("ok")
    );
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A tagged value goes only to a far end that declared `value-<tag>`
/// (SES-6.1-24): an argument fails before it is sent, a result answers the
/// far end's call with a `throw`; the error names the value and the
/// capability, and the session goes on.
/// rule: SES-6.1-24
#[tokio::test(flavor = "multi_thread")]
async fn a_tagged_value_to_a_far_end_without_its_capability_fails_only_that_call() {
    let (peer, mut far) = session(Arc::new(Instanting));
    // The far end takes no `instant` values.
    greet(&mut far, "node", &[]);
    peer.ready().await.unwrap();
    // A result it cannot take: this side's answer is a `throw` naming the
    // result and the capability.
    send(
        &mut far,
        invoke("node:1", "svc", "instant", json!({ "type": "undefined" })),
    );
    let reply = read(&mut far);
    let (name, message, code, data) = thrown(&reply);
    assert_eq!(name, "BindingError");
    assert_eq!(code.as_deref(), Some("bad-value"));
    assert_eq!(data, Some(json!({ "part": "result" })));
    assert_eq!(
        message,
        "svc.instant: the result cannot be sent: cannot send an instant: \
         the far end does not take instant values (it did not declare the \
         capability value-instant)"
    );
    // An argument it cannot take: this side's call fails before anything is
    // sent, with code and data.
    let error = peer
        .invoke(
            "svc",
            "take",
            Value::List(vec![Value::Instant(Instant::from_unix(0, 0).unwrap())]),
        )
        .expect_err("the far end does not take instants");
    assert_eq!(error.code(), Some("bad-value"));
    assert_eq!(
        error.data(),
        Some(json!({ "part": "argument", "argument": 1 }))
    );
    assert!(
        error
            .to_string()
            .contains("the far end does not take instant values (it did not declare the capability value-instant)"),
        "{error}"
    );
    // Nothing of the refused call was sent: the next frame is the next call,
    // and both calls are answered.
    send(
        &mut far,
        invoke(
            "node:2",
            "svc",
            "echo",
            json!({ "type": "data", "value": "after" }),
        ),
    );
    assert_eq!(
        read(&mut far),
        json!({ "id": "node:2", "op": "return", "value": { "type": "data", "value": "after" } })
    );
    let answering = std::thread::spawn(move || {
        let next = read(&mut far);
        assert_eq!(frame_id(&next), "main:2", "{next}");
        assert_eq!(
            next["args"],
            json!({ "type": "data", "value": 1 }),
            "{next}"
        );
        send(
            &mut far,
            json!({ "op": "return", "id": "main:2", "value": { "type": "data", "value": "plain" } }),
        );
        far
    });
    let plain = peer.invoke("svc", "echo", Value::Data(json!(1)));
    assert_eq!(plain.unwrap().json().unwrap(), json!("plain"));
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || answering.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A received value this side does not take, one not in its format, or one
/// whose keys collide fails only its call: the far end reads a `throw` with
/// code `bad-value`; the session goes on (SES-6.1-26).
/// rule: SES-6.1-26
#[tokio::test(flavor = "multi_thread")]
async fn a_received_value_this_side_refuses_fails_only_its_call() {
    // This side takes every tagged value but `uuid`.
    let mut endpoint = Endpoint::rust(id("main"));
    endpoint.capabilities.retain(|c| c != "value-uuid");
    let (peer, mut far) = session_with(Arc::new(Echo), endpoint);
    greet(&mut far, "node", &VALUE_CAPABILITIES);
    peer.ready().await.unwrap();
    // A tag this side did not declare: the far end reads its call's error.
    refused(
        &mut far,
        "node:1",
        &json!({ "type": "uuid", "value": "0f8fad5b-d9cb-469f-a165-70867728950e" }),
        "received a uuid value, which this side does not take \
         (it did not declare the capability value-uuid)",
    );
    // A value not in its format, and keys that collide.
    refused(
        &mut far,
        "node:2",
        &json!({ "type": "instant", "value": "2026-10-15T03:22:15+02:00" }),
        "invalid instant",
    );
    refused(
        &mut far,
        "node:3",
        &json!({ "type": "map", "value": [
            [{ "type": "data", "value": 1 }, { "type": "data", "value": "a" }],
            [{ "type": "data", "value": 1.0 }, { "type": "data", "value": "b" }],
        ] }),
        "duplicate map key",
    );
    // The session goes on: a value it takes crosses.
    accepted(
        &mut far,
        "node:4",
        &json!({ "type": "instant", "value": "2026-10-15T03:22:15Z" }),
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A `value` whose JSON shape is wrong (an `instant` that is not a string,
/// a `map` entry that is not a pair) is a malformed frame: the session
/// ends, and a call after it fails (SES-6.1-27).
/// rule: SES-6.1-27
#[tokio::test(flavor = "multi_thread")]
async fn a_value_of_the_wrong_json_shape_ends_the_session() {
    malformed(
        &json!({ "type": "instant", "value": 5 }),
        "invalid type: integer `5`, expected a string",
    )
    .await;
    malformed(
        &json!({ "type": "map", "value": [1] }),
        "invalid type: integer `1`, expected a tuple of size 2",
    )
    .await;
    malformed(
        &json!({ "type": "map", "value": [[{ "type": "data", "value": 1 }]] }),
        "invalid length 1, expected a tuple of size 2",
    )
    .await;
    malformed(
        &json!({ "type": "set", "value": 5 }),
        "invalid type: integer `5`, expected a sequence",
    )
    .await;
    malformed(
        &json!({ "type": "record", "value": [] }),
        "invalid type: sequence, expected a map",
    )
    .await;
    malformed(&json!({ "type": "data" }), "missing field `value`").await;
    malformed(
        &json!({ "type": "bogus", "value": 1 }),
        "unknown variant `bogus`",
    )
    .await;
}

/// A call frame with a field of the wrong type, or a missing one
/// (CHN-4-4): the frame cannot be read, the session ends as a transport
/// failure naming the field, without a reply.
/// rule: CHN-4-4
#[tokio::test(flavor = "multi_thread")]
async fn a_frame_with_a_mistyped_field_ends_the_session() {
    let cases = [
        (
            json!({ "op": "invoke", "id": "node:1", "path": [], "target": 7,
                    "method": "echo", "args": { "type": "undefined" } }),
            "invalid type: integer `7`, expected a string",
        ),
        (
            json!({ "op": "invoke", "id": "node:1", "path": [], "target": "svc",
                    "args": { "type": "undefined" } }),
            "missing field `method`",
        ),
        (
            json!({ "op": "get", "id": "node:1", "path": [], "reference": 7,
                    "property": 5 }),
            "invalid type: integer `5`, expected a string",
        ),
        (
            json!({ "op": "cancel", "id": 5 }),
            "invalid type: integer `5`, expected a string",
        ),
        (
            json!({ "op": "release", "reference": 7, "count": "1" }),
            "invalid type: string \"1\", expected u64",
        ),
        (
            json!({ "op": "throw", "id": "node:1",
                    "error": { "name": 7, "message": "x" } }),
            "invalid type: integer `7`, expected a string",
        ),
        (
            json!({ "op": "throw", "id": "node:1",
                    "error": { "name": "Error", "message": 7 } }),
            "invalid type: integer `7`, expected a string",
        ),
    ];
    for (frame, said) in cases {
        unreadable(frame, said).await;
    }
}

/// A call, a property read or an await naming a reference this side does
/// not have, or already released, and a release of an unknown reference or
/// with a count of 0 or over granted (CHN-4-6): the frame is malformed,
/// the session ends as a transport failure, and a call after it fails.
/// rule: CHN-4-6
#[tokio::test(flavor = "multi_thread")]
async fn unknown_or_released_references_end_the_session() {
    /// A session with a reference this side exported (`give`); the far end
    /// sends `frames(reference)` and the session ends saying `said`.
    async fn ending(said: &str, frames: impl FnOnce(u64) -> Vec<Json>) {
        let (peer, mut far) = session(Arc::new(Giving));
        greet(&mut far, "node", &[]);
        peer.ready().await.unwrap();
        send(
            &mut far,
            invoke("node:1", "svc", "give", json!({ "type": "undefined" })),
        );
        let reply = read(&mut far);
        let reference = reply["value"]["value"]["id"]
            .as_u64()
            .expect("a reference id");
        for frame in frames(reference) {
            send(&mut far, frame);
        }
        within(said, peer.closed()).await;
        match peer.close_reason() {
            Some(Error::Transport(message)) => assert!(message.contains(said), "{message}"),
            other => panic!("a transport error expected, got {other:?}"),
        }
        assert!(
            peer.invoke("svc", "echo", Value::Undefined).is_err(),
            "a call after the end fails"
        );
    }
    ending("unknown or released reference", |_| {
        vec![
            json!({ "op": "call", "id": "node:2", "path": [], "reference": 99,
                      "args": { "type": "undefined" } }),
        ]
    })
    .await;
    ending("unknown or released reference", |_| {
        vec![
            json!({ "op": "get", "id": "node:3", "path": [], "reference": 99,
                      "property": "name" }),
        ]
    })
    .await;
    ending("unknown or released reference", |_| {
        vec![json!({ "op": "await", "id": "node:4", "path": [], "reference": 99 })]
    })
    .await;
    ending("release of unknown reference", |_| {
        vec![json!({ "op": "release", "reference": 99, "count": 1 })]
    })
    .await;
    ending("invalid reference release count", |reference| {
        vec![json!({ "op": "release", "reference": reference, "count": 0 })]
    })
    .await;
    ending("invalid reference release count", |reference| {
        vec![json!({ "op": "release", "reference": reference, "count": 2 })]
    })
    .await;
    // Released once: the next call names a reference that is gone.
    ending("unknown or released reference", |reference| {
        vec![
            json!({ "op": "release", "reference": reference, "count": 1 }),
            json!({ "op": "call", "id": "node:5", "path": [], "reference": reference,
                    "args": { "type": "undefined" } }),
        ]
    })
    .await;
    ending("unknown or released reference", |reference| {
        vec![
            json!({ "op": "release", "reference": reference, "count": 1 }),
            json!({ "op": "get", "id": "node:6", "path": [], "reference": reference,
                    "property": "name" }),
        ]
    })
    .await;
    ending("release of unknown reference", |reference| {
        vec![
            json!({ "op": "release", "reference": reference, "count": 1 }),
            json!({ "op": "release", "reference": reference, "count": 1 }),
        ]
    })
    .await;
}

/// A request before the handshake, and a second `hello`, are protocol
/// failures: the session ends, and no call is answered (CHN-4-8).
/// rule: CHN-4-8
#[tokio::test(flavor = "multi_thread")]
async fn a_request_before_the_handshake_or_a_second_hello_ends_the_session() {
    {
        let (peer, mut far) = session(Arc::new(Echo));
        let hello = read(&mut far);
        assert_eq!(hello["op"], json!("hello"));
        // A call before this side's greeting ends the session.
        send(
            &mut far,
            invoke("node:1", "svc", "echo", json!({ "type": "undefined" })),
        );
        within("the session's end", peer.closed()).await;
        match peer.close_reason() {
            Some(Error::Transport(message)) => {
                assert_eq!(message, "request before protocol handshake")
            }
            other => panic!("a transport error expected, got {other:?}"),
        }
        assert!(
            matches!(far.receiver.recv(), Ok(None)),
            "the channel ends without a reply"
        );
    }
    {
        let (peer, mut far) = session(Arc::new(Echo));
        greet(&mut far, "node", &[]);
        // A second greeting, after the first.
        send(
            &mut far,
            json!({ "op": "hello", "version": ENDPOINT_PROTOCOL, "endpoint": "node" }),
        );
        within("the session's end", peer.closed()).await;
        match peer.close_reason() {
            Some(Error::Transport(message)) => {
                assert_eq!(message, "duplicate protocol handshake")
            }
            other => panic!("a transport error expected, got {other:?}"),
        }
    }
}

/// A reply to a call this side gave up on is dropped, counted, and does not
/// end the session (CHN-4-9).
/// rule: CHN-4-9
#[tokio::test(flavor = "multi_thread")]
async fn a_late_reply_to_a_cancelled_call_is_dropped() {
    let (peer, mut far) = session(Arc::new(Nothing));
    greet(&mut far, "node", &[]);
    peer.ready().await.unwrap();
    let (seen, was_seen) = mpsc::channel();
    let far_end = std::thread::spawn(move || {
        let call = read(&mut far);
        let id = frame_id(&call);
        seen.send(id.clone()).unwrap();
        // The caller gave up: a cancel for that call.
        assert_eq!(read(&mut far), json!({ "op": "cancel", "id": id }));
        // A late reply for the cancelled call: dropped, not a protocol
        // failure.
        send(
            &mut far,
            json!({ "op": "return", "id": id, "value": { "type": "data", "value": "late" } }),
        );
        // A call after it is answered.
        let next = read(&mut far);
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&next), "value": { "type": "data", "value": "after" } }),
        );
        far
    });
    let mut call = Box::pin(peer.invoke_async("svc", "slow", Value::Undefined));
    tokio::select! {
        biased;
        answer = &mut call => panic!("the call only ends when given up: {answer:?}"),
        received = tokio::task::spawn_blocking(move || was_seen.recv().unwrap()) => {
            received.unwrap();
        }
    }
    drop(call);
    let reply = peer.invoke_async("svc", "echo", Value::Data(json!(1)));
    assert_eq!(
        within("a call after the late reply", reply)
            .await
            .unwrap()
            .json()
            .unwrap(),
        json!("after")
    );
    let _far = within(
        "the far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A `cancel` for a call this side has not got is ignored (it may have
/// overtaken its own reply), and the session goes on (CHN-4-10).
/// rule: CHN-4-10
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_for_an_unknown_call_is_ignored() {
    let (peer, mut far) = session(Arc::new(Echo));
    greet(&mut far, "node", &[]);
    peer.ready().await.unwrap();
    send(&mut far, json!({ "op": "cancel", "id": "node:99" }));
    send(
        &mut far,
        invoke(
            "node:1",
            "svc",
            "echo",
            json!({ "type": "data", "value": "after" }),
        ),
    );
    assert_eq!(
        read(&mut far),
        json!({ "id": "node:1", "op": "return", "value": { "type": "data", "value": "after" } })
    );
    assert!(peer.close_reason().is_none(), "the session goes on");
}

/// A call frame whose `path` already holds its own `id` is an illegal
/// frame (TAG-2-1): the session ends as a transport failure, without a
/// reply; a path holding another call's id is not.
/// rule: TAG-2-1
#[tokio::test(flavor = "multi_thread")]
async fn a_path_holding_the_frames_own_id_ends_the_session() {
    {
        let (peer, mut far) = session(Arc::new(Echo));
        greet(&mut far, "node", &[]);
        peer.ready().await.unwrap();
        // Only the frame's own id in its path is illegal.
        let mut looped = invoke("node:1", "svc", "echo", json!({ "type": "undefined" }));
        looped["path"] = json!(["node:1"]);
        send(&mut far, looped);
        within("the session's end", peer.closed()).await;
        match peer.close_reason() {
            Some(Error::Transport(message)) => {
                assert_eq!(message, "invalid invocation identity/path")
            }
            other => panic!("a transport error expected, got {other:?}"),
        }
        assert!(
            matches!(far.receiver.recv(), Ok(None)),
            "the channel ends without a reply"
        );
        assert!(peer.invoke("svc", "echo", Value::Undefined).is_err());
    }
    {
        // A path with another call's id crosses.
        let (peer, mut far) = session(Arc::new(Echo));
        greet(&mut far, "node", &[]);
        peer.ready().await.unwrap();
        let mut chained = invoke(
            "node:2",
            "svc",
            "echo",
            json!({ "type": "data", "value": "after" }),
        );
        chained["path"] = json!(["node:1"]);
        send(&mut far, chained);
        assert_eq!(
            read(&mut far),
            json!({ "id": "node:2", "op": "return",
                    "value": { "type": "data", "value": "after" } })
        );
        assert!(peer.close_reason().is_none(), "the session goes on");
    }
    {
        // A `call` on an exported reference checks its path the same way.
        let (peer, mut far) = session(Arc::new(Giving));
        greet(&mut far, "node", &[]);
        peer.ready().await.unwrap();
        send(
            &mut far,
            invoke("node:1", "svc", "give", json!({ "type": "undefined" })),
        );
        let reply = read(&mut far);
        let reference = reply["value"]["value"]["id"]
            .as_u64()
            .expect("a reference id");
        send(
            &mut far,
            json!({ "op": "call", "id": "node:2", "path": ["node:2"],
                    "reference": reference, "args": { "type": "undefined" } }),
        );
        within("the session's end", peer.closed()).await;
        match peer.close_reason() {
            Some(Error::Transport(message)) => {
                assert_eq!(message, "invalid invocation identity/path")
            }
            other => panic!("a transport error expected, got {other:?}"),
        }
    }
}

/// A business `close()` and a proxy release are separate (SES-7.1-6):
/// dropping an imported object reference writes a `release` frame and
/// calls no business method on its owner; the session goes on.
/// rule: SES-7.1-6
#[tokio::test(flavor = "multi_thread")]
async fn releasing_a_proxy_calls_no_business_method() {
    let (owner, mut far) = session(Arc::new(Nothing));
    greet(&mut far, "a", &[]);
    owner.ready().await.unwrap();
    let far_end = std::thread::spawn(move || {
        // A hands out an object reference.
        let call = read(&mut far);
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&call), "value": {
                "type": "list",
                "value": [{ "type": "reference", "value": {
                    "id": 7, "home": false, "kind": "object", "origin": [],
                } }],
            } }),
        );
        // Releasing the proxy is only a release: no `call`, no `get`, no
        // business `close()`.
        assert_eq!(
            read(&mut far),
            json!({ "op": "release", "reference": 7, "count": 1 })
        );
        // The next frame is the next call, answered as usual.
        let call = read(&mut far);
        assert_eq!(call["op"], json!("invoke"), "{call}");
        send(
            &mut far,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "alive" } }),
        );
        far
    });
    let object = owner
        .invoke("svc", "give", Value::Undefined)
        .unwrap()
        .list()
        .unwrap()
        .remove(0)
        .reference()
        .unwrap();
    drop(object);
    assert_eq!(
        owner
            .invoke("svc", "still", Value::Undefined)
            .unwrap()
            .json()
            .unwrap(),
        json!("alive")
    );
    let _far = within(
        "the owner's far end",
        tokio::task::spawn_blocking(move || far_end.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(owner.close_reason().is_none(), "the session goes on");
}

/// A reference another session exported is relayed to a third session as a
/// live reference: one reference however often it is sent; its methods and
/// properties are forwarded to its owner with the chain rebased; releasing
/// the relay releases it there (M1-7.1-1).
/// rule: M1-7.1-1
#[tokio::test(flavor = "multi_thread")]
async fn an_object_relayed_to_another_session_reaches_its_owner() {
    let (a, ra) = session(Arc::new(Nothing));
    let (b, rb) = session(Arc::new(Nothing));
    let mut ra = ra;
    let mut rb = rb;
    greet(&mut ra, "a", &[]);
    greet(&mut rb, "b", &[]);
    a.ready().await.unwrap();
    b.ready().await.unwrap();

    let a_far = std::thread::spawn(move || {
        // A owns the object: it hands out reference 7 of its own session.
        let call = read(&mut ra);
        send(
            &mut ra,
            json!({ "op": "return", "id": frame_id(&call), "value": {
                "type": "list",
                "value": [{ "type": "reference", "value": {
                    "id": 7, "home": false, "kind": "object", "origin": [],
                } }],
            } }),
        );
        // The relayed method call: the owner's own id, and a chain rebased
        // to the session that made the call.
        let call = read(&mut ra);
        assert_eq!(call["op"], json!("call"), "{call}");
        assert_eq!(call["reference"], json!(7), "{call}");
        assert_eq!(call["method"], json!("greet"), "{call}");
        assert_eq!(
            call["args"],
            json!({ "type": "list", "value": [] }),
            "{call}"
        );
        let path = call["path"].as_array().expect("a path");
        assert_eq!(path.len(), 1, "{call}");
        assert!(
            path[0].as_str().unwrap().ends_with("/b:1"),
            "the chain is rebased to the caller's session: {call}"
        );
        send(
            &mut ra,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "hello" } }),
        );
        // The relayed property read.
        let call = read(&mut ra);
        assert_eq!(call["op"], json!("get"), "{call}");
        assert_eq!(call["reference"], json!(7), "{call}");
        assert_eq!(call["property"], json!("name"), "{call}");
        send(
            &mut ra,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "cache" } }),
        );
        // The relay was released: the owner's own reference is released.
        assert_eq!(
            read(&mut ra),
            json!({ "op": "release", "reference": 7, "count": 1 })
        );
        ra
    });
    let b_far = std::thread::spawn(move || {
        // The object handed over, as this session's reference.
        let call = read(&mut rb);
        let reference = call["args"]["value"][0]["value"].clone();
        assert_eq!(reference["kind"], json!("object"), "{call}");
        assert_eq!(reference["home"], json!(false), "{call}");
        let reference = reference["id"].as_u64().expect("a reference id");
        send(
            &mut rb,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "taken" } }),
        );
        // Handed over again: the same reference.
        let call = read(&mut rb);
        assert_eq!(
            call["args"]["value"][0]["value"]["id"],
            json!(reference),
            "{call}"
        );
        send(
            &mut rb,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "again" } }),
        );
        // Its method and property reach the owner.
        send(
            &mut rb,
            json!({ "op": "call", "id": "b:1", "path": [], "reference": reference,
                    "method": "greet", "args": { "type": "list", "value": [] }, "sync": false }),
        );
        assert_eq!(
            read(&mut rb),
            json!({ "id": "b:1", "op": "return", "value": { "type": "data", "value": "hello" } })
        );
        send(
            &mut rb,
            json!({ "op": "get", "id": "b:2", "path": [], "reference": reference,
                    "property": "name", "sync": false }),
        );
        assert_eq!(
            read(&mut rb),
            json!({ "id": "b:2", "op": "return", "value": { "type": "data", "value": "cache" } })
        );
        // Both grants released.
        send(
            &mut rb,
            json!({ "op": "release", "reference": reference, "count": 2 }),
        );
        rb
    });

    let object = a
        .invoke("svc", "give", Value::Undefined)
        .unwrap()
        .list()
        .unwrap()
        .remove(0)
        .reference()
        .unwrap();
    let handed = Value::List(vec![Value::Reference(object.clone())]);
    assert_eq!(
        b.invoke("svc", "take", handed.clone())
            .unwrap()
            .json()
            .unwrap(),
        json!("taken")
    );
    assert_eq!(
        b.invoke("svc", "take", handed).unwrap().json().unwrap(),
        json!("again")
    );
    drop(object);
    let _rb = within(
        "b's far end",
        tokio::task::spawn_blocking(move || b_far.join().unwrap()),
    )
    .await
    .unwrap();
    let _ra = within(
        "a's far end",
        tokio::task::spawn_blocking(move || a_far.join().unwrap()),
    )
    .await
    .unwrap();
    assert!(a.close_reason().is_none(), "the owner's session goes on");
    assert!(b.close_reason().is_none(), "the holder's session goes on");
}

/// M1-13-3: an object reference relayed to a far end that did not declare
/// `objects`. The session protocol has no such capability, and the relay
/// sends the reference anyway; what happens next is the far end's decoder's
/// doing:
///
/// - one that refuses the value for that call only reads the reference and
///   answers with a `throw`: the call fails, both sessions go on;
/// - one that ends its session on the reference (as Node's `receive` did
///   before #687) leaves this side's call failing with `peer-gone`; the
///   session that ends is the far end's, and the owner's session goes on.
///
/// The object itself never leaves its owner.
/// rule: M1-13-3
#[tokio::test(flavor = "multi_thread")]
async fn an_object_relayed_to_a_far_end_without_objects_is_still_sent() {
    /// A session whose far end owns object 7.
    fn owner() -> (Connection, std::thread::JoinHandle<Channel>) {
        let (a, mut ra) = session(Arc::new(Nothing));
        greet(&mut ra, "a", &[]);
        let far = std::thread::spawn(move || {
            let call = read(&mut ra);
            send(
                &mut ra,
                json!({ "op": "return", "id": frame_id(&call), "value": {
                    "type": "list",
                    "value": [{ "type": "reference", "value": {
                        "id": 7, "home": false, "kind": "object", "origin": [],
                    } }],
                } }),
            );
            // Calls after the relay: the session is alive. A release may
            // arrive first when the far end's session ended and its relay
            // went with it.
            loop {
                let frame = read(&mut ra);
                if frame["op"] == json!("release") {
                    assert_eq!(frame["reference"], json!(7), "{frame}");
                    continue;
                }
                assert_eq!(frame["op"], json!("invoke"), "{frame}");
                send(
                    &mut ra,
                    json!({ "op": "return", "id": frame_id(&frame), "value": { "type": "data", "value": "alive" } }),
                );
                break;
            }
            ra
        });
        (a, far)
    }
    fn object_of(a: &Connection) -> Value {
        a.invoke("svc", "give", Value::Undefined)
            .unwrap()
            .list()
            .unwrap()
            .remove(0)
    }

    // The far end refuses the value for its call only.
    let (a, a_far) = owner();
    let (b, rb) = session(Arc::new(Nothing));
    let mut rb = rb;
    greet(&mut rb, "b", &[]);
    a.ready().await.unwrap();
    b.ready().await.unwrap();
    let b_far = std::thread::spawn(move || {
        let call = read(&mut rb);
        let reference = call["args"]["value"][0]["value"].clone();
        assert_eq!(reference["kind"], json!("object"), "{call}");
        assert_eq!(reference["home"], json!(false), "{call}");
        send(
            &mut rb,
            json!({ "op": "throw", "id": frame_id(&call),
                    "error": { "name": "TypeError", "message": "invalid reference" } }),
        );
        // The next call reaches the far end, and the session goes on.
        let call = read(&mut rb);
        assert_eq!(
            call["args"],
            json!({ "type": "data", "value": 1 }),
            "{call}"
        );
        send(
            &mut rb,
            json!({ "op": "return", "id": frame_id(&call), "value": { "type": "data", "value": "after" } }),
        );
        rb
    });
    let object = object_of(&a);
    let failed = b
        .invoke("svc", "take", Value::List(vec![object.clone()]))
        .expect_err("the far end refused the reference");
    match failed {
        Error::Remote { name, message, .. } => {
            assert_eq!(
                (name.as_str(), message.as_str()),
                ("TypeError", "invalid reference")
            )
        }
        other => panic!("the far end's error expected, got {other:?}"),
    }
    drop(object);
    let reply = b.invoke("svc", "plain", Value::Data(json!(1)));
    assert_eq!(reply.unwrap().json().unwrap(), json!("after"));
    assert!(b.close_reason().is_none(), "the far end's session goes on");
    assert!(a.close_reason().is_none(), "the owner's session goes on");
    let alive = a.invoke("svc", "still", Value::Undefined);
    assert_eq!(alive.unwrap().json().unwrap(), json!("alive"));
    let _rb = within(
        "b's far end",
        tokio::task::spawn_blocking(move || b_far.join().unwrap()),
    )
    .await
    .unwrap();
    let _ra = within(
        "a's far end",
        tokio::task::spawn_blocking(move || a_far.join().unwrap()),
    )
    .await
    .unwrap();
    drop(a);

    // The far end's decoder ends its session on the reference.
    let (a, a_far) = owner();
    let (b, rb) = session(Arc::new(Nothing));
    let mut rb = rb;
    greet(&mut rb, "b", &[]);
    a.ready().await.unwrap();
    b.ready().await.unwrap();
    let b_far = std::thread::spawn(move || {
        let call = read(&mut rb);
        let reference = call["args"]["value"][0]["value"].clone();
        assert_eq!(reference["kind"], json!("object"), "{call}");
        // The decoder cannot take it: its session ends here.
        drop(rb);
    });
    let object = object_of(&a);
    let failed = b.invoke("svc", "take", Value::List(vec![object]));
    let failed = failed.expect_err("the far end's session ended");
    assert_eq!(failed.code(), Some("peer-gone"), "{failed}");
    assert!(
        matches!(failed, Error::Transport(_)),
        "the call fails as the far end's session ends: {failed}"
    );
    within("the far end's session's end", b.closed()).await;
    assert!(b.close_reason().is_some(), "the far end's session ended");
    assert!(a.close_reason().is_none(), "the owner's session goes on");
    // A call on the owner's session is answered: the upstream session is
    // unaffected.
    let alive = a.invoke("svc", "still", Value::Undefined);
    assert_eq!(alive.unwrap().json().unwrap(), json!("alive"));
    within(
        "b's far end",
        tokio::task::spawn_blocking(move || b_far.join().unwrap()),
    )
    .await
    .unwrap();
    let _ra = within(
        "a's far end",
        tokio::task::spawn_blocking(move || a_far.join().unwrap()),
    )
    .await
    .unwrap();
}
