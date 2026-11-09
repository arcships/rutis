//! Trace validation for the session channel (#624, `docs/formal-trace.md`
//! §2, §3.1, §3.2, §3.4, §3.7, §4): an execution is a trace, and a trace is
//! allowed only if every event in it is one the previous events allow.
//!
//! Two sequences are checked here.
//!
//! - The channel trace `channel::trace` writes: direction, length and how a
//!   channel ended, never its bytes. A close ends both directions of this
//!   end; a far end that ended its stream is not followed by another
//!   message; a failed send ends sending; a failed receive ends the message
//!   stream.
//! - The frames themselves, decoded by the fixture that wraps a channel:
//!   a greeting first and once, protocol 4, no frame before it; a call id
//!   carrying the prefix of the session that issued it; a reply only for a
//!   call a frame opened, at most once; a cancel for a call this side does
//!   not have is the one dangling name allowed (it may cross the reply it
//!   cancels).
//!
//! Every allowed trace here was produced by a real channel or session, but
//! the repeated close and the repeated end, which no run here makes. Every
//! forbidden one is that same trace with one event changed, or a literal
//! sequence. The product trace stays size-only: the fixture decodes frames,
//! the trace does not, because messages carry configuration.
//!
//! Not checked here, and gaps of #624: whether a `release` names a
//! reference a frame granted, path forwarding, the wait graph, size limits
//! and the access point's own log (`specs/remote-lease/AccessPoint.tla`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use rutis_bridge::channel::trace::{trace, Sink};
use rutis_bridge::channel::{Channel, ChannelError, Close, PeerId, Receiver};
use rutis_bridge::session::{
    Connection, Dispatch, Endpoint, Error, Reply, Value, ENDPOINT_PROTOCOL,
};
use rutis_test_support::{until, within};
use serde_json::{json, Value as Json};

fn id(name: &str) -> PeerId {
    PeerId::new(name).unwrap()
}

// ---------------------------------------------------------------- the lines

/// Trace lines from every channel of one run, in the order they were made:
/// two channels on one host write to one log, so their lines interleave and
/// the validator has to keep them apart by label.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn sink(&self) -> Sink {
        let log = self.clone();
        Arc::new(move |line: &str| log.0.lock().unwrap().push(line.to_owned()))
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// One event of `docs/formal-trace.md` §2.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Sent(usize),
    Received(usize),
    Ended,
    SendFailed(String),
    ReceiveFailed(String),
    ClosedDone,
    ClosedReplaced,
    ClosedRejected(String),
}

struct Line {
    label: String,
    event: Event,
}

/// `<label>: <event>`: the label is up to the first `: ` that leaves an
/// event the format has; a reason may itself hold `: `, which is why the
/// label is not split off by the last one.
fn parse_line(line: &str) -> Result<Line, String> {
    for (at, _) in line.match_indices(": ") {
        if at == 0 {
            continue;
        }
        if let Some(event) = parse_event(&line[at + 2..]) {
            return Ok(Line {
                label: line[..at].to_owned(),
                event,
            });
        }
    }
    Err(format!(
        "{line:?} is not a line of the trace format (docs/formal-trace.md §2)"
    ))
}

fn parse_event(rest: &str) -> Option<Event> {
    if let Some(bytes) = rest
        .strip_prefix("sent ")
        .and_then(|rest| rest.strip_suffix(" bytes"))
    {
        return bytes.parse().ok().map(Event::Sent);
    }
    if let Some(bytes) = rest
        .strip_prefix("received ")
        .and_then(|rest| rest.strip_suffix(" bytes"))
    {
        return bytes.parse().ok().map(Event::Received);
    }
    match rest {
        "ended" => Some(Event::Ended),
        "closed: done" => Some(Event::ClosedDone),
        "closed: replaced" => Some(Event::ClosedReplaced),
        _ => {
            if let Some(reason) = rest
                .strip_prefix("send failed: ")
                .filter(|reason| !reason.is_empty())
            {
                return Some(Event::SendFailed(reason.to_owned()));
            }
            if let Some(reason) = rest
                .strip_prefix("receive failed: ")
                .filter(|reason| !reason.is_empty())
            {
                return Some(Event::ReceiveFailed(reason.to_owned()));
            }
            rest.strip_prefix("closed: rejected: ")
                .filter(|reason| !reason.is_empty())
                .map(|reason| Event::ClosedRejected(reason.to_owned()))
        }
    }
}

/// How far one channel's trace has gone; what may follow follows from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Open,
    /// The far end finished sending (`recv` gave `Ok(None)`): no message
    /// follows its end.
    Ended,
    /// A send failed: the channel ended for sending.
    SendFailed,
    /// A receive failed: the message stream ended.
    ReceiveFailed,
    /// This end closed the channel: nothing successful crosses it after.
    Closed,
}

/// The state after `event`, or why the event cannot follow `state`.
fn after(state: State, event: &Event) -> Result<State, String> {
    use State::*;
    match (state, event) {
        (_, Event::ClosedDone | Event::ClosedReplaced | Event::ClosedRejected(_)) => Ok(Closed),
        (Closed, Event::Sent(_) | Event::Received(_) | Event::Ended) => {
            Err("this end closed the channel: nothing crosses it after the close".into())
        }
        // A send or receive already on a closed channel reports the close.
        (Closed, Event::SendFailed(_) | Event::ReceiveFailed(_)) => Ok(Closed),
        (ReceiveFailed, Event::Received(_) | Event::Ended) => {
            Err("a receive failed: the message stream ended there".into())
        }
        // The two directions can end apart: a send may still succeed.
        (ReceiveFailed, _) => Ok(ReceiveFailed),
        (SendFailed, Event::Sent(_)) => Err("a send failed: the channel ended for sending".into()),
        // What arrived before the far end's close can still be drained.
        (SendFailed, _) => Ok(SendFailed),
        (Ended, Event::Received(_)) => {
            Err("the far end ended the stream: no message can follow its end".into())
        }
        (Ended, _) => Ok(Ended),
        (Open, Event::Sent(_) | Event::Received(_)) => Ok(Open),
        (Open, Event::Ended) => Ok(Ended),
        (Open, Event::SendFailed(_)) => Ok(SendFailed),
        (Open, Event::ReceiveFailed(_)) => Ok(ReceiveFailed),
    }
}

/// A trace whose every event follows from the ones before it.
struct Checked {
    lines: Vec<Line>,
    /// Where each channel's trace ended.
    last: BTreeMap<String, State>,
}

impl Checked {
    fn channels(&self) -> usize {
        self.last.len()
    }

    /// The events of one channel, in the order they were recorded.
    fn events(&self, label: &str) -> Vec<Event> {
        self.lines
            .iter()
            .filter(|line| line.label == label)
            .map(|line| line.event.clone())
            .collect()
    }
}

/// The first event that does not follow, and the rule it breaks.
#[derive(Debug)]
struct Violation {
    /// 0-based index of the event that broke the rule.
    at: usize,
    /// What the validator read there.
    text: String,
    why: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (trace line {}): {}",
            self.text,
            self.at + 1,
            self.why
        )
    }
}

fn check_trace<S: AsRef<str>>(lines: &[S]) -> Result<Checked, Violation> {
    let mut parsed = Vec::with_capacity(lines.len());
    let mut states: BTreeMap<String, State> = BTreeMap::new();
    for (at, text) in lines.iter().enumerate() {
        let text = text.as_ref();
        let violation = |why: String| Violation {
            at,
            text: text.to_owned(),
            why,
        };
        let line = parse_line(text).map_err(violation)?;
        let state = states.get(&line.label).copied().unwrap_or(State::Open);
        let next = after(state, &line.event).map_err(violation)?;
        states.insert(line.label.clone(), next);
        parsed.push(line);
    }
    Ok(Checked {
        lines: parsed,
        last: states,
    })
}

fn accepted<S: AsRef<str>>(lines: &[S]) -> Checked {
    check_trace(lines)
        .unwrap_or_else(|violation| panic!("an allowed trace was rejected: {violation}"))
}

fn rejected<S: AsRef<str>>(lines: &[S]) -> Violation {
    match check_trace(lines) {
        Ok(_) => panic!(
            "a forbidden trace was accepted: {:?}",
            lines
                .iter()
                .map(|line| line.as_ref().to_owned())
                .collect::<Vec<_>>()
        ),
        Err(violation) => violation,
    }
}

// --------------------------------------------------------------- the frames

/// One frame the fixture decoded: which end sent it, its `op`, the call or
/// value it names (if any), the endpoint and protocol of its greeting, and
/// its length on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    from: String,
    op: String,
    id: Option<String>,
    version: Option<u64>,
    endpoint: Option<String>,
    bytes: usize,
}

/// Frames decoded by the fixture, in the order the readers saw them. The
/// order across the two streams is theirs; each stream keeps its own.
#[derive(Clone, Default)]
struct Decoded(Arc<Mutex<Vec<Frame>>>);

impl Decoded {
    fn push(&self, frame: Frame) {
        self.0.lock().unwrap().push(frame);
    }

    fn all(&self) -> Vec<Frame> {
        self.0.lock().unwrap().clone()
    }

    /// What `end` sent, in its order.
    fn from(&self, end: &str) -> Vec<Frame> {
        self.all()
            .into_iter()
            .filter(|frame| frame.from == end)
            .collect()
    }
}

/// A receiver that decodes what the far end sent before handing it on. Only
/// a fixture does this; `channel::trace` records sizes and never bytes.
struct Decoding {
    inner: Box<dyn Receiver>,
    /// The end that sent what this receiver reads.
    from: &'static str,
    frames: Decoded,
}

impl Receiver for Decoding {
    fn recv(&mut self) -> Result<Option<Vec<u8>>, ChannelError> {
        let message = self.inner.recv()?;
        if let Some(message) = &message {
            self.frames.push(decode(self.from, message));
        }
        Ok(message)
    }
}

fn decode(from: &str, message: &[u8]) -> Frame {
    let json: Json = serde_json::from_slice(message).unwrap_or(Json::Null);
    Frame {
        from: from.to_owned(),
        op: json
            .get("op")
            .and_then(Json::as_str)
            .unwrap_or("<not a frame>")
            .to_owned(),
        id: json.get("id").and_then(Json::as_str).map(str::to_owned),
        version: json.get("version").and_then(Json::as_u64),
        endpoint: json
            .get("endpoint")
            .and_then(Json::as_str)
            .map(str::to_owned),
        bytes: message.len(),
    }
}

/// `channel` traced as `label`, its receiver decoding what the far end
/// (`from`) sends.
fn instrumented(
    channel: Channel,
    label: &'static str,
    from: &'static str,
    log: &Log,
    frames: &Decoded,
) -> Channel {
    let mut channel = trace(channel, label, log.sink());
    channel.receiver = Box::new(Decoding {
        inner: channel.receiver,
        from,
        frames: frames.clone(),
    });
    channel
}

const CALL_OPS: [&str; 4] = ["invoke", "call", "get", "await"];
const REPLY_OPS: [&str; 2] = ["return", "throw"];
/// The `Frame` vocabulary of protocol 4 (`session/protocol.rs`).
const FRAME_OPS: [&str; 9] = [
    "hello", "invoke", "call", "get", "await", "return", "throw", "release", "cancel",
];

struct FrameChecked {
    /// Calls a call frame opened, and whether a reply settled them.
    calls: BTreeMap<String, bool>,
}

impl FrameChecked {
    fn unanswered(&self) -> usize {
        self.calls.values().filter(|settled| !**settled).count()
    }
}

/// Check a run's frames in the order they were recorded: a greeting first
/// and once, a call id carrying the prefix of the session that issued it,
/// and every reply settling a call a frame opened.
fn check_frames(frames: &[Frame]) -> Result<FrameChecked, Violation> {
    let mut greeted: BTreeSet<String> = BTreeSet::new();
    let mut endpoints: BTreeMap<String, String> = BTreeMap::new();
    let mut calls: BTreeMap<String, bool> = BTreeMap::new();
    for (at, frame) in frames.iter().enumerate() {
        let violation = |why: String| Violation {
            at,
            text: format!("{} sent `{}`", frame.from, frame.op),
            why,
        };
        if !FRAME_OPS.contains(&frame.op.as_str()) {
            return Err(violation(format!(
                "`{}` is not an op of protocol {ENDPOINT_PROTOCOL}",
                frame.op
            )));
        }
        if frame.op == "hello" {
            if !greeted.insert(frame.from.clone()) {
                return Err(violation("a session greets once".into()));
            }
            if frame.version != Some(ENDPOINT_PROTOCOL.into()) {
                return Err(violation(format!(
                    "a greeting speaks protocol {ENDPOINT_PROTOCOL}, not {:?}",
                    frame.version
                )));
            }
            if let Some(endpoint) = &frame.endpoint {
                endpoints.insert(frame.from.clone(), endpoint.clone());
            }
            continue;
        }
        // §4: no frame before the greeting.
        if !greeted.contains(&frame.from) {
            return Err(violation(
                "a frame reached the session before its greeting".into(),
            ));
        }
        if CALL_OPS.contains(&frame.op.as_str()) {
            let id = frame
                .id
                .as_ref()
                .ok_or_else(|| violation("a call names no id".into()))?;
            // §4 identity: the id carries the prefix of the session that
            // issued the call, so an id of another session cannot name it.
            if let Some(endpoint) = endpoints.get(&frame.from) {
                let prefix = format!("{endpoint}:");
                if !id.starts_with(&prefix) {
                    return Err(violation(format!(
                        "the call id {id:?} does not carry the issuer's prefix {prefix:?}"
                    )));
                }
            }
            if calls.insert(id.clone(), false).is_some() {
                return Err(violation(format!("the call id {id:?} is opened twice")));
            }
        }
        if REPLY_OPS.contains(&frame.op.as_str()) {
            let id = frame
                .id
                .as_ref()
                .ok_or_else(|| violation("a reply names no id".into()))?;
            match calls.get_mut(id) {
                None => {
                    return Err(violation(format!(
                        "a reply to the call {id:?}, which no frame opened"
                    )))
                }
                Some(settled) if *settled => {
                    return Err(violation(format!("the call {id:?} is answered twice")))
                }
                Some(settled) => *settled = true,
            }
        }
        // `release` names a reference, tracked with the values that granted
        // it (a gap of this validator, #624). `cancel` is the one dangling
        // name the protocol allows (§3.1): it may cross the reply it
        // cancels, so an unknown one is not a fault.
    }
    Ok(FrameChecked { calls })
}

fn decoded(rows: &[(&str, &str)]) -> Vec<Frame> {
    rows.iter()
        .map(|(from, text)| decode(from, text.as_bytes()))
        .collect()
}

fn checked_frames(frames: &[Frame]) -> FrameChecked {
    check_frames(frames)
        .unwrap_or_else(|violation| panic!("an allowed frame sequence was rejected: {violation}"))
}

fn rejected_frames(frames: &[Frame]) -> Violation {
    match check_frames(frames) {
        Ok(_) => panic!("a forbidden frame sequence was accepted: {frames:?}"),
        Err(violation) => violation,
    }
}

fn ops(frames: &[Frame]) -> Vec<(&str, Option<&str>)> {
    frames
        .iter()
        .map(|frame| (frame.op.as_str(), frame.id.as_deref()))
        .collect()
}

// ---------------------------------------------------- allowed channel traces

/// A memory pair, the near end traced under `label`.
fn traced(label: &str) -> (Channel, Channel, Log) {
    let (near, far) = rutis_bridge::memory::pair();
    let log = Log::default();
    (
        rutis_bridge::channel::trace::trace(near, label, log.sink()),
        far,
        log,
    )
}

/// A normal end: the far end finishes, then this end closes. The four lines
/// are the ones `docs/formal-trace.md` §2 and §5 fix, with the 18-byte
/// secret of `channel/trace.rs`, which the trace must not carry.
#[test]
fn a_normal_end_is_an_allowed_trace() {
    let (mut near, mut far, log) = traced("memory peer mac");
    near.sender.send(b"{\"token\":\"secret\"}").unwrap();
    far.receiver.recv().unwrap().unwrap();
    far.sender.send(b"{\"token\":\"secret\"}").unwrap();
    near.receiver.recv().unwrap().unwrap();
    far.closer.close(Close::Done);
    assert_eq!(near.receiver.recv().unwrap(), None);
    near.closer.close(Close::Done);

    let lines = log.lines();
    assert_eq!(
        lines,
        [
            "memory peer mac: sent 18 bytes",
            "memory peer mac: received 18 bytes",
            "memory peer mac: ended",
            "memory peer mac: closed: done",
        ]
    );
    let checked = accepted(&lines);
    assert_eq!(checked.channels(), 1);
    assert_eq!(checked.last["memory peer mac"], State::Closed);

    // The same run with a message after the close is not a trace.
    let mut changed = lines.clone();
    changed.push("memory peer mac: sent 1 bytes".into());
    assert_eq!(rejected(&changed).at, 4);
}

/// A newer connection of the same far end: this end's receive and send both
/// report the replacement, then this end closes as replaced.
#[test]
fn a_replaced_channel_is_an_allowed_trace() {
    let (mut near, mut far, log) = traced("peer");
    near.sender.send(b"ping").unwrap();
    far.receiver.recv().unwrap().unwrap();
    far.closer.close(Close::Replaced);
    match near.receiver.recv() {
        Err(ChannelError::Closed { reason }) => assert_eq!(reason, "replaced by a new connection"),
        other => panic!("a replaced channel receives an error, got {other:?}"),
    }
    match near.sender.send(b"ping") {
        Err(ChannelError::Closed { reason }) => assert_eq!(reason, "replaced by a new connection"),
        other => panic!("a replaced channel sends an error, got {other:?}"),
    }
    near.closer.close(Close::Replaced);

    let lines = log.lines();
    assert_eq!(
        lines,
        [
            "peer: sent 4 bytes",
            "peer: receive failed: replaced by a new connection",
            "peer: send failed: replaced by a new connection",
            "peer: closed: replaced",
        ]
    );
    let checked = accepted(&lines);
    assert_eq!(checked.last["peer"], State::Closed);

    // A message where the receive failed is not a trace.
    let mut changed = lines.clone();
    changed.insert(2, "peer: received 4 bytes".into());
    assert_eq!(rejected(&changed).at, 2);
}

/// A refused identity: `Close::Rejected(reason)` reaches this end as a
/// rejected channel, with its reason, in the `receive failed` line and in
/// the `closed: rejected` line.
#[test]
fn a_rejected_channel_is_an_allowed_trace() {
    let (mut near, far, log) = traced("peer");
    far.closer.close(Close::Rejected("unknown identity".into()));
    match near.receiver.recv() {
        Err(ChannelError::Rejected { reason }) => assert_eq!(reason, "rejected: unknown identity"),
        other => panic!("a rejected channel receives a rejection, got {other:?}"),
    }
    near.closer
        .close(Close::Rejected("unknown identity".into()));

    let lines = log.lines();
    assert_eq!(
        lines,
        [
            "peer: receive failed: rejected: unknown identity",
            "peer: closed: rejected: unknown identity",
        ]
    );
    let checked = accepted(&lines);
    assert_eq!(checked.last["peer"], State::Closed);

    // The end of the stream cannot come after a failed receive.
    let mut changed = lines.clone();
    changed.insert(1, "peer: ended".into());
    assert_eq!(rejected(&changed).at, 1);
}

/// The far end gone: this end's send fails; what arrived before is drained,
/// then the stream ends.
#[test]
fn a_send_failure_is_an_allowed_trace() {
    let (mut near, far, log) = traced("peer");
    drop(far);
    assert!(near.sender.send(b"ping").is_err());
    assert_eq!(near.receiver.recv().unwrap(), None);
    near.closer.close(Close::Done);

    let lines = log.lines();
    assert_eq!(
        lines,
        [
            "peer: send failed: the channel is closed",
            "peer: ended",
            "peer: closed: done",
        ]
    );
    let checked = accepted(&lines);
    assert_eq!(checked.last["peer"], State::Closed);

    // A send cannot succeed after a failed one.
    let mut changed = lines.clone();
    changed.insert(1, "peer: sent 4 bytes".into());
    assert_eq!(rejected(&changed).at, 1);
}

/// A receive that fails ends the message stream: no message and no end can
/// follow it, only the failures and the close.
#[test]
fn a_receive_failure_is_an_allowed_trace() {
    let (mut near, far, log) = traced("peer");
    far.closer.close(Close::Rejected("bad token".into()));
    assert!(near.receiver.recv().is_err());
    near.closer.close(Close::Done);

    let lines = log.lines();
    assert_eq!(
        lines,
        [
            "peer: receive failed: rejected: bad token",
            "peer: closed: done",
        ]
    );
    accepted(&lines);

    let mut changed = lines.clone();
    changed.push("peer: received 4 bytes".into());
    assert_eq!(rejected(&changed).at, 2);
}

/// The far end finished sending, not receiving: sending after its end is
/// allowed, and so is the end reported twice (the channel contract checks a
/// receive after the end) and a close repeated (closing is idempotent).
#[test]
fn sending_after_the_fars_end_is_allowed() {
    let (mut near, far, log) = traced("peer");
    let Channel {
        sender,
        receiver,
        closer,
    } = far;
    drop(sender);
    assert_eq!(near.receiver.recv().unwrap(), None);
    near.sender.send(b"ping").unwrap();
    drop((receiver, closer));
    near.closer.close(Close::Done);

    let lines = log.lines();
    assert_eq!(
        lines,
        ["peer: ended", "peer: sent 4 bytes", "peer: closed: done",]
    );
    accepted(&lines);

    let checked = accepted(&[
        "peer: ended",
        "peer: ended",
        "peer: closed: done",
        "peer: closed: replaced",
    ]);
    assert_eq!(checked.last["peer"], State::Closed);
}

/// Two channels' lines in one log: each is checked on its own, and a
/// violation of one is still caught among the other's lines.
#[test]
fn channels_are_checked_apart() {
    let checked = accepted(&[
        "main: sent 18 bytes",
        "node: sent 18 bytes",
        "main: received 18 bytes",
        "node: received 18 bytes",
        "node: ended",
        "main: closed: done",
        "node: closed: done",
    ]);
    assert_eq!(checked.channels(), 2);
    assert_eq!(
        checked.events("node"),
        [
            Event::Sent(18),
            Event::Received(18),
            Event::Ended,
            Event::ClosedDone,
        ]
    );

    let violation = rejected(&[
        "main: sent 18 bytes",
        "node: sent 18 bytes",
        "node: closed: done",
        "main: received 18 bytes",
        "node: received 18 bytes",
    ]);
    assert_eq!(violation.at, 4);
}

/// Events no channel produces: an op the format does not have, a size that
/// is not a number, a close or a failure without its reason, no label, an
/// empty one.
#[test]
fn lines_outside_the_format_are_rejected() {
    let cases: [(&str, &[&str], usize); 8] = [
        ("an unknown event", &["peer: frobbed 3 bytes"], 0),
        (
            "a size that is not a number",
            &["peer: sent three bytes"],
            0,
        ),
        ("a close without a reason", &["peer: closed: rejected"], 0),
        ("a failure without a reason", &["peer: send failed: "], 0),
        ("no label", &["sent 3 bytes"], 0),
        ("an empty label", &[": sent 3 bytes"], 0),
        ("a label alone", &["peer:"], 0),
        (
            "an unknown event after an allowed one",
            &["peer: ended", "peer: went away"],
            1,
        ),
    ];
    for (what, lines, at) in cases {
        let violation = rejected(lines);
        assert_eq!(violation.at, at, "{what}: {violation}");
    }
}

/// Transitions the channel contract forbids, each with the first event that
/// breaks it.
#[test]
fn forbidden_transitions_are_rejected() {
    let cases: [(&str, &[&str], usize); 6] = [
        (
            "a message after this end closed",
            &["peer: closed: done", "peer: sent 3 bytes"],
            1,
        ),
        (
            "a receive after this end closed",
            &["peer: closed: replaced", "peer: received 3 bytes"],
            1,
        ),
        (
            "a message after the far end's end",
            &["peer: ended", "peer: received 3 bytes"],
            1,
        ),
        (
            "a send after a failed send",
            &["peer: send failed: gone", "peer: sent 3 bytes"],
            1,
        ),
        (
            "a receive after a failed receive",
            &["peer: receive failed: gone", "peer: received 3 bytes"],
            1,
        ),
        (
            "an end after a failed receive",
            &["peer: receive failed: gone", "peer: ended"],
            1,
        ),
    ];
    for (what, lines, at) in cases {
        let violation = rejected(lines);
        assert_eq!(violation.at, at, "{what}: {violation}");
    }
}

// ------------------------------------------------- allowed session and frames

struct Echo;

impl Dispatch for Echo {
    fn invoke(&self, _: &Connection, _: &str, _method: &str, args: Value) -> Reply {
        Ok(args)
    }
}

/// Two real sessions over one memory pair, both ends traced into one log,
/// every frame decoded by the fixture: the channel trace, the frames and the
/// bytes they took agree, the payload never reaches the log, and the frames
/// are ones protocol 4 allows.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_session_run_is_an_allowed_trace() {
    let log = Log::default();
    let frames = Decoded::default();
    let (ours, theirs) = rutis_bridge::memory::pair();
    let ours = instrumented(ours, "main", "node", &log, &frames);
    let theirs = instrumented(theirs, "node", "main", &log, &frames);

    let caller = Connection::open(
        ours,
        Arc::new(Echo),
        Endpoint::rust(id("main")).expect(id("node")),
    )
    .unwrap();
    let callee = Connection::open(
        theirs,
        Arc::new(Echo),
        Endpoint::rust(id("node")).expect(id("main")),
    )
    .unwrap();
    within("the caller's greeting", caller.ready())
        .await
        .unwrap();
    within("the callee's greeting", callee.ready())
        .await
        .unwrap();
    let reply = within(
        "the call",
        caller.invoke_async("svc", "echo", Value::Data(json!({ "secret": "hunter2" }))),
    )
    .await
    .unwrap();
    assert_eq!(reply.json().unwrap(), json!({ "secret": "hunter2" }));

    caller.close(Error::Transport("done".into()));
    within("the callee ending", callee.closed()).await;
    within("the caller ending", caller.closed()).await;
    // The caller's reader reports the close it woke from as a failed
    // receive; wait for that line rather than for the close.
    until(
        || {
            log.lines()
                .iter()
                .any(|line| line == "main: receive failed: closed here")
        },
        "the caller's reader line",
    )
    .await;

    let lines = log.lines();
    assert!(
        lines.iter().all(|line| !line.contains("hunter2")),
        "the trace carries no payload: {lines:?}"
    );
    let checked = accepted(&lines);
    assert_eq!(checked.channels(), 2, "{lines:?}");
    assert_eq!(checked.last["main"], State::Closed);
    assert_eq!(checked.last["node"], State::Closed);

    let main = frames.from("main");
    let node = frames.from("node");
    assert_eq!(
        ops(&main),
        [("hello", None), ("invoke", Some("main:1"))],
        "the caller greets, then calls"
    );
    assert_eq!(
        ops(&node),
        [("hello", None), ("return", Some("main:1"))],
        "the callee greets, then answers that call"
    );
    // §2: the trace compares direction, label and byte length; the lengths
    // here are the fixture's, frame by frame.
    assert_eq!(
        checked.events("main"),
        [
            Event::Sent(main[0].bytes),
            Event::Received(node[0].bytes),
            Event::Sent(main[1].bytes),
            Event::Received(node[1].bytes),
            Event::ClosedDone,
            Event::ReceiveFailed("closed here".into()),
        ]
    );
    assert_eq!(
        checked.events("node"),
        [
            Event::Sent(node[0].bytes),
            Event::Received(main[0].bytes),
            Event::Received(main[1].bytes),
            Event::Sent(node[1].bytes),
            Event::Ended,
            Event::ClosedDone,
        ]
    );

    let all = frames.all();
    let checked = checked_frames(&all);
    assert_eq!(checked.unanswered(), 0, "every call was answered");
}

/// Frames protocol 4 does not allow: an op outside the vocabulary, a message
/// that is not a frame, a frame before the greeting, a call or reply without
/// an id, a call carrying another session's id prefix, a second greeting,
/// another protocol version, a reply to a call no frame opened, a call
/// answered twice, an id opened twice. The one dangling name allowed is a
/// cancel for a call this side does not have.
#[test]
fn frames_outside_the_protocol_are_rejected() {
    const HELLO_MAIN: &str = r#"{"op":"hello","version":4,"endpoint":"main"}"#;
    const HELLO_NODE: &str = r#"{"op":"hello","version":4,"endpoint":"node"}"#;
    const INVOKE: &str = r#"{"op":"invoke","id":"main:1","path":[],"target":"t","method":"m","args":{"type":"undefined"}}"#;
    const RETURN: &str = r#"{"op":"return","id":"main:1","value":{"type":"undefined"}}"#;

    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], usize);
    let cases: [Case; 11] = [
        (
            "an unknown op",
            &[
                ("main", HELLO_MAIN),
                ("main", r#"{"op":"frobnicate","id":"node:1"}"#),
            ],
            1,
        ),
        (
            "a message that is not a frame",
            &[("main", HELLO_MAIN), ("main", "not a frame")],
            1,
        ),
        ("a frame before the greeting", &[("main", INVOKE)], 0),
        (
            "a call without an id",
            &[
                ("main", HELLO_MAIN),
                (
                    "main",
                    r#"{"op":"invoke","path":[],"target":"t","method":"m","args":{"type":"undefined"}}"#,
                ),
            ],
            1,
        ),
        (
            "a reply without an id",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                ("node", r#"{"op":"return","value":{"type":"undefined"}}"#),
            ],
            2,
        ),
        (
            "a call carrying another session's id prefix",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                (
                    "node",
                    r#"{"op":"invoke","id":"main:2","path":[],"target":"t","method":"m","args":{"type":"undefined"}}"#,
                ),
            ],
            2,
        ),
        (
            "a second greeting",
            &[("main", HELLO_MAIN), ("main", HELLO_MAIN)],
            1,
        ),
        (
            "a greeting of another protocol version",
            &[("main", r#"{"op":"hello","version":3,"endpoint":"main"}"#)],
            0,
        ),
        (
            "a reply to a call no frame opened",
            &[("main", HELLO_MAIN), ("node", HELLO_NODE), ("node", RETURN)],
            2,
        ),
        (
            "a call answered twice",
            &[
                ("main", HELLO_MAIN),
                ("main", INVOKE),
                ("node", HELLO_NODE),
                ("node", RETURN),
                ("node", RETURN),
            ],
            4,
        ),
        (
            "a call id opened twice",
            &[("main", HELLO_MAIN), ("main", INVOKE), ("main", INVOKE)],
            2,
        ),
    ];
    for (what, rows, at) in cases {
        let violation = rejected_frames(&decoded(rows));
        assert_eq!(violation.at, at, "{what}: {violation}");
    }

    // §3.1 boundary: a cancel may cross the reply it cancels.
    let checked = checked_frames(&decoded(&[
        ("main", HELLO_MAIN),
        ("node", HELLO_NODE),
        ("main", r#"{"op":"cancel","id":"node:7"}"#),
        ("main", INVOKE),
        ("node", RETURN),
    ]));
    assert_eq!(checked.unanswered(), 0);
}
