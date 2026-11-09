//! Trace validation for the session channel (#624, `docs/formal-trace.md`
//! §2, §3.1–§3.8, §4): an execution is a trace, and a trace is allowed only
//! if every event in it is one the previous events allow.
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
//!   carrying the prefix of the session that issued it, and each session
//!   numbering its own calls (a replacement numbers them from 1 again); a
//!   reply only for a call the same session's far end opened, at most once;
//!   a cancel for a call this side does not have is the one dangling name
//!   allowed (it may cross the reply it cancels).
//!
//! What frames grant and carry is checked with them:
//!
//! - §3.3, references: a `call`/`get`/`await` names a reference the far end
//!   granted and has not seen fully released; a `release` counts against
//!   those grants and no further; a granted identity is one a session
//!   allocates (1 to 2^53−1); a reference sent home names one its session
//!   granted. Each is a session-level fault when broken, not a call's.
//! - §3.5, failures: a `throw` carries a name and a message, and a graph
//!   whose root names a node — the code and data a program decides by. A
//!   graph that does not resolve is taken as `bad graph` and fails only its
//!   call; the frame stays in the trace, the session goes on.
//! - §3.6, limits: a frame is within the channel's message limit, because a
//!   session checks before writing it. So an over-limit value fails its
//!   call and leaves no frame; a message over the limit that reaches the
//!   carrier ends the channel, which the channel trace shows.
//! - §3.8, replacement: the replaced channel closes before the new session
//!   greets, and a reply arriving on the replacement cannot settle a call
//!   of the session it replaced.
//!
//! Every allowed trace here was produced by a real channel or session, but
//! the repeated close and the repeated end, which no run here makes. Every
//! forbidden one is that same trace with one event changed, or a literal
//! sequence. The product trace stays size-only: the fixture decodes frames,
//! the trace does not, because messages carry configuration. Each
//! forbidden case is one this validator's rule for it rejects: delete the
//! rule and the case is accepted, failing the test.
//!
//! Not checked here, and gaps of #624: path forwarding, the wait graph, the
//! outbound backlog limit (`SessionOptions::outbound_limit`, of a session
//! the host makes, not of a channel), which reference kinds a frame may
//! name (a `get` of a function is the far end's own refusal), and the
//! access point's own log (`specs/remote-lease/AccessPoint.tla`). 4002 and
//! 4003 are checked through a memory channel, whose `Close` maps them; the
//! WebSocket close codes themselves are `crates/rutis-bridge/tests/
//! websocket.rs` (feature `websocket`, not built here).

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

/// One reference a frame's value carries: granted to the far end
/// (`home: false`), or sent back to the session it belongs to
/// (`home: true`, §3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Carried {
    id: u64,
    home: bool,
}

/// One frame the fixture decoded: which end sent it, its `op`, the call or
/// value or reference it names, the endpoint and protocol of its greeting,
/// the references its value carries, the failure it throws, and its length
/// on the wire.
#[derive(Clone, Debug, PartialEq)]
struct Frame {
    from: String,
    op: String,
    id: Option<String>,
    version: Option<u64>,
    endpoint: Option<String>,
    bytes: usize,
    /// The `reference` a `call`, `get`, `await` or `release` names.
    reference: Option<u64>,
    /// The `count` a `release` says.
    count: Option<u64>,
    /// References the frame's `args` or `value` carries (§3.3).
    carries: Vec<Carried>,
    /// The failure a `throw` carries (§3.5).
    failure: Option<Failure>,
    /// Why the frame carries no value protocol 4 has, when it does not.
    malformed: Option<String>,
}

/// What a failure's `graph` is to the receiving side (`Error::code` and
/// `Error::data`, `session/mod.rs`).
#[derive(Clone, Debug, PartialEq)]
enum Graph {
    /// No graph at all: a `SyncWaitCycle`, a refusal.
    None,
    /// A graph that does not resolve: not `{root: {type: "reference",
    /// value: n}, nodes: [...]}` with a node at `n`. A malformed graph
    /// fails only the call that got it (§3.5), never the session.
    Bad,
    /// A graph whose root names a node, with the code and data it says.
    Node {
        code: Option<String>,
        data: Option<Json>,
    },
}

/// The failure a `throw` carries, read as the receiving side reads it.
#[derive(Clone, Debug, PartialEq)]
struct Failure {
    name: String,
    message: String,
    graph: Graph,
}

impl Failure {
    /// The code a program decides by: the code of the node the graph's root
    /// names, `None` for one without.
    fn code(&self) -> Option<&str> {
        match &self.graph {
            Graph::Node { code, .. } => code.as_deref(),
            Graph::None | Graph::Bad => None,
        }
    }

    /// The data of that node.
    fn data(&self) -> Option<&Json> {
        match &self.graph {
            Graph::Node { data, .. } => data.as_ref(),
            _ => None,
        }
    }
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
    let mut frame = Frame {
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
        reference: json.get("reference").and_then(Json::as_u64),
        count: json.get("count").and_then(Json::as_u64),
        carries: Vec::new(),
        failure: None,
        malformed: None,
    };
    // The references a frame's value carries, and the failure it throws.
    for field in ["args", "value"] {
        let Some(value) = json.get(field) else {
            continue;
        };
        if let Err(why) = carried(value, &mut frame.carries) {
            frame.malformed = Some(why);
            return frame;
        }
    }
    if frame.op == "throw" {
        match json.get("error") {
            Some(error) => match failure_of(error) {
                Some(failure) => frame.failure = Some(failure),
                None => {
                    frame.malformed = Some(format!(
                        "a throw carries a failure: a name and a message, not {error}"
                    ))
                }
            },
            None => frame.malformed = Some("a throw carries no error".into()),
        }
    }
    frame
}

/// The references a wire value carries, into `out`. A value of data holds
/// none, whatever it looks like: a reference-shaped object inside `data` is
/// data.
fn carried(value: &Json, out: &mut Vec<Carried>) -> Result<(), String> {
    let tag = value
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("{value} is not a tagged value"))?;
    match tag {
        "reference" => {
            let reference = value
                .get("value")
                .ok_or_else(|| format!("{value} is a reference without its value"))?;
            let id = reference
                .get("id")
                .and_then(Json::as_u64)
                .ok_or_else(|| format!("{value} names no reference id"))?;
            let home = reference
                .get("home")
                .and_then(Json::as_bool)
                .ok_or_else(|| format!("{value} does not say whether it goes home"))?;
            out.push(Carried { id, home });
            Ok(())
        }
        "list" | "set" => {
            let Some(items) = value.get("value").and_then(Json::as_array) else {
                return Err(format!("{value} is a {tag} without items"));
            };
            for item in items {
                carried(item, out)?;
            }
            Ok(())
        }
        "map" => {
            let Some(entries) = value.get("value").and_then(Json::as_array) else {
                return Err(format!("{value} is a map without entries"));
            };
            for entry in entries {
                let pair = entry
                    .as_array()
                    .filter(|pair| pair.len() == 2)
                    .ok_or_else(|| format!("{entry} is not a map entry"))?;
                carried(&pair[0], out)?;
                carried(&pair[1], out)?;
            }
            Ok(())
        }
        "record" => {
            let Some(fields) = value.get("value").and_then(Json::as_object) else {
                return Err(format!("{value} is a record without fields"));
            };
            for field in fields.values() {
                carried(field, out)?;
            }
            Ok(())
        }
        // Data, undefined, a signal and the tagged values carry no
        // references.
        _ => Ok(()),
    }
}

/// The failure `error` (a `throw`'s `error`) carries, or `None` when it is
/// no failure: a name and a message are what make it one.
fn failure_of(error: &Json) -> Option<Failure> {
    let name = error.get("name")?.as_str()?.to_owned();
    let message = error.get("message")?.as_str()?.to_owned();
    let graph = match error.get("graph") {
        None | Some(Json::Null) => Graph::None,
        Some(graph) => match graph_root(graph) {
            Some(node) => Graph::Node {
                code: node.get("code").and_then(Json::as_str).map(str::to_owned),
                data: node.get("data").and_then(|data| graph_value(graph, data)),
            },
            None => Graph::Bad,
        },
    };
    Some(Failure {
        name,
        message,
        graph,
    })
}

/// The node a graph's root names (`session/mod.rs` `graph_root`).
fn graph_root(graph: &Json) -> Option<&serde_json::Map<String, Json>> {
    let root = graph.get("root")?;
    if root.get("type")?.as_str()? != "reference" {
        return None;
    }
    graph
        .get("nodes")?
        .get(root.get("value")?.as_u64()? as usize)?
        .as_object()
}

/// A value of an error graph as JSON (`session/mod.rs` `graph_value`).
fn graph_value(graph: &Json, value: &Json) -> Option<Json> {
    graph_value_at(graph, value, 0)
}

fn graph_value_at(graph: &Json, value: &Json, depth: usize) -> Option<Json> {
    if depth > 64 {
        return None;
    }
    match value.get("type")?.as_str()? {
        "undefined" => Some(Json::Null),
        "data" => value.get("value").cloned(),
        "bigint" | "number" => {
            let text = value.get("value")?.as_str()?;
            Some(
                text.parse::<serde_json::Number>()
                    .map(Json::Number)
                    .unwrap_or_else(|_| Json::String(text.to_owned())),
            )
        }
        "reference" => {
            let node = graph
                .get("nodes")?
                .get(value.get("value")?.as_u64()? as usize)?;
            match node.get("type")?.as_str()? {
                "array" => node
                    .get("values")?
                    .as_array()?
                    .iter()
                    .map(|item| graph_value_at(graph, item, depth + 1))
                    .collect::<Option<Vec<_>>>()
                    .map(Json::Array),
                "object" => node
                    .get("values")?
                    .as_array()?
                    .iter()
                    .map(|entry| {
                        let key = entry.get(0)?.as_str()?.to_owned();
                        Some((key, graph_value_at(graph, entry.get(1)?, depth + 1)?))
                    })
                    .collect::<Option<serde_json::Map<_, _>>>()
                    .map(Json::Object),
                _ => None,
            }
        }
        _ => None,
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

/// One frame a scripted far end read.
fn read_frame(channel: &mut Channel) -> Json {
    let message = channel
        .receiver
        .recv()
        .expect("the far end reads")
        .expect("a frame, not the end");
    serde_json::from_slice(&message).expect("a frame")
}

/// One frame a scripted far end writes.
fn write_frame(channel: &mut Channel, frame: Json) {
    channel
        .sender
        .send(&serde_json::to_vec(&frame).unwrap())
        .expect("the far end writes");
}

const CALL_OPS: [&str; 4] = ["invoke", "call", "get", "await"];
const REPLY_OPS: [&str; 2] = ["return", "throw"];
/// The `Frame` vocabulary of protocol 4 (`session/protocol.rs`).
const FRAME_OPS: [&str; 9] = [
    "hello", "invoke", "call", "get", "await", "return", "throw", "release", "cancel",
];
/// The call ops that name a reference of the far end (§3.3).
const REFERENCE_CALL_OPS: [&str; 3] = ["call", "get", "await"];
/// The largest reference id a session allocates (2^53−1): beyond it, a
/// number cannot cross as one (`session/rpc.rs` `decode`, "invalid
/// reference identity").
const MAX_REFERENCE: u64 = 9_007_199_254_740_991;

/// The two ends of one session, as the fixture labelled them: the labels
/// that greet each other. A session's call ids are its own, so a
/// replacement session numbers its calls from 1 again (§3.8).
type Ends<'a> = (&'a str, &'a str);

/// The other end of the session `end` is in.
fn peer_of<'a>(sessions: &[Ends<'a>], end: &str) -> Option<&'a str> {
    sessions.iter().find_map(|(a, b)| {
        if *a == end {
            Some(*b)
        } else if *b == end {
            Some(*a)
        } else {
            None
        }
    })
}

/// One call a frame opened.
struct Call {
    /// Whether a reply settled it.
    settled: bool,
    /// The failure the reply carried, when it was a `throw`.
    failure: Option<Failure>,
}

struct FrameChecked {
    /// Calls a call frame opened, by the end that opened it and its id.
    calls: BTreeMap<(String, String), Call>,
    /// Grants outstanding, by the end that granted and the reference id.
    outstanding: BTreeMap<(String, u64), u64>,
}

impl FrameChecked {
    fn unanswered(&self) -> usize {
        self.calls.values().filter(|call| !call.settled).count()
    }

    /// How many of `id` the end `owner` granted and has not seen released.
    fn outstanding(&self, owner: &str, id: u64) -> u64 {
        self.outstanding
            .get(&(owner.to_owned(), id))
            .copied()
            .unwrap_or(0)
    }

    /// The failure that settled the call `id`, when `issuer` opened it and
    /// the reply was a `throw` (§3.5).
    fn failure(&self, issuer: &str, id: &str) -> Option<&Failure> {
        self.calls
            .get(&(issuer.to_owned(), id.to_owned()))?
            .failure
            .as_ref()
    }
}

/// Check a run's frames in the order they were recorded: a greeting first
/// and once, a call id carrying the prefix of the session that issued it,
/// every reply settling a call the same session's far end opened, what
/// frames grant and release (§3.3), the failure a `throw` carries (§3.5),
/// and that no frame is over the channel's `limit` (§3.6).
fn check_frames(
    frames: &[Frame],
    sessions: &[Ends<'_>],
    limit: Option<usize>,
) -> Result<FrameChecked, Violation> {
    let mut greeted: BTreeSet<String> = BTreeSet::new();
    let mut endpoints: BTreeMap<String, String> = BTreeMap::new();
    let mut calls: BTreeMap<(String, String), Call> = BTreeMap::new();
    let mut granted: BTreeMap<(String, u64), u64> = BTreeMap::new();
    for (at, frame) in frames.iter().enumerate() {
        let violation = |why: String| Violation {
            at,
            text: format!("{} sent `{}`", frame.from, frame.op),
            why,
        };
        let peer = peer_of(sessions, &frame.from)
            .ok_or_else(|| violation(format!("no session here has an end {:?}", frame.from)))?
            .to_owned();
        if let Some(why) = &frame.malformed {
            return Err(violation(why.clone()));
        }
        if !FRAME_OPS.contains(&frame.op.as_str()) {
            return Err(violation(format!(
                "`{}` is not an op of protocol {ENDPOINT_PROTOCOL}",
                frame.op
            )));
        }
        // §3.6: a session checks a frame against the channel's limit before
        // writing it, so a frame the channel carried is within it; one over
        // it ends the channel, which is not how a value fails its call.
        if let Some(limit) = limit {
            if frame.bytes > limit {
                return Err(violation(format!(
                    "a frame of {} bytes is over the channel's limit of {limit} bytes: an \
                     over-limit value fails its call before the frame is written, so no frame \
                     over the limit is in an allowed trace (#675)",
                    frame.bytes
                )));
            }
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
            if calls
                .insert(
                    (frame.from.clone(), id.clone()),
                    Call {
                        settled: false,
                        failure: None,
                    },
                )
                .is_some()
            {
                return Err(violation(format!(
                    "the call id {id:?} is opened twice in this session"
                )));
            }
        }
        if REPLY_OPS.contains(&frame.op.as_str()) {
            let id = frame
                .id
                .as_ref()
                .ok_or_else(|| violation("a reply names no id".into()))?;
            // §3.8: an id belongs to the session that issued it, so a reply
            // on the replacement session cannot settle the call of the one
            // it replaced.
            match calls.get_mut(&(peer.clone(), id.clone())) {
                None => {
                    return Err(violation(format!(
                        "a reply to the call {id:?}, which no frame of this session opened"
                    )))
                }
                Some(call) if call.settled => {
                    return Err(violation(format!("the call {id:?} is answered twice")))
                }
                Some(call) => {
                    call.settled = true;
                    call.failure = frame.failure.clone();
                }
            }
        }
        // §3.3: what the frame carries, then what it names. `cancel` is the
        // one dangling name the protocol allows (§3.1): it may cross the
        // reply it cancels, so an unknown one is not a fault.
        for carried in &frame.carries {
            if carried.id == 0 || carried.id > MAX_REFERENCE {
                return Err(violation(format!(
                    "the reference {} is no identity a session allocates (1 to {MAX_REFERENCE}): \
                     one outside it ends the session",
                    carried.id
                )));
            }
            if carried.home {
                // A reference sent home names one this side granted: the
                // object belongs to its session, and comes back to it.
                if granted
                    .get(&(peer.clone(), carried.id))
                    .copied()
                    .unwrap_or(0)
                    == 0
                {
                    return Err(violation(format!(
                        "the reference {} goes home to {peer}, which never granted it to {} (or \
                         saw it fully released)",
                        carried.id, frame.from
                    )));
                }
            } else {
                *granted.entry((frame.from.clone(), carried.id)).or_insert(0) += 1;
            }
        }
        if REFERENCE_CALL_OPS.contains(&frame.op.as_str()) {
            let reference = frame
                .reference
                .ok_or_else(|| violation("a call names no reference".into()))?;
            if granted
                .get(&(peer.clone(), reference))
                .copied()
                .unwrap_or(0)
                == 0
            {
                return Err(violation(format!(
                    "the call names the reference {reference}, which {peer} never granted to {} \
                     (or fully released): an unknown reference ends the session (§3.3)",
                    frame.from
                )));
            }
        }
        if frame.op == "release" {
            let reference = frame
                .reference
                .ok_or_else(|| violation("a release names no reference".into()))?;
            let count = frame
                .count
                .ok_or_else(|| violation("a release says no count".into()))?;
            let left = {
                let outstanding = granted.entry((peer.clone(), reference)).or_insert(0);
                if count == 0 || count > *outstanding {
                    return Err(violation(format!(
                        "a release of {count} of the reference {reference}: {peer} has \
                         {outstanding} of it granted (an invalid release count ends the session)"
                    )));
                }
                *outstanding -= count;
                *outstanding
            };
            if left == 0 {
                granted.remove(&(peer, reference));
            }
        }
    }
    Ok(FrameChecked {
        calls,
        outstanding: granted,
    })
}

fn decoded(rows: &[(&str, &str)]) -> Vec<Frame> {
    rows.iter()
        .map(|(from, text)| decode(from, text.as_bytes()))
        .collect()
}

fn checked_frames(frames: &[Frame], sessions: &[Ends<'_>], limit: Option<usize>) -> FrameChecked {
    check_frames(frames, sessions, limit)
        .unwrap_or_else(|violation| panic!("an allowed frame sequence was rejected: {violation}"))
}

fn rejected_frames(frames: &[Frame], sessions: &[Ends<'_>], limit: Option<usize>) -> Violation {
    match check_frames(frames, sessions, limit) {
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
    let checked = checked_frames(&all, &[("main", "node")], None);
    assert_eq!(checked.unanswered(), 0, "every call was answered");
}

/// The greetings and the call frames the literal traces below use: a call
/// `main` opens, and a reply the far end sends for it.
const HELLO_MAIN: &str = r#"{"op":"hello","version":4,"endpoint":"main"}"#;
const HELLO_NODE: &str = r#"{"op":"hello","version":4,"endpoint":"node"}"#;
const INVOKE: &str = r#"{"op":"invoke","id":"main:1","path":[],"target":"t","method":"m","args":{"type":"undefined"}}"#;
const RETURN: &str = r#"{"op":"return","id":"main:1","value":{"type":"undefined"}}"#;

/// Frames protocol 4 does not allow: an op outside the vocabulary, a message
/// that is not a frame, a frame before the greeting, a call or reply without
/// an id, a call carrying another session's id prefix, a second greeting,
/// another protocol version, a reply to a call no frame opened, a call
/// answered twice, an id opened twice. The one dangling name allowed is a
/// cancel for a call this side does not have.
#[test]
fn frames_outside_the_protocol_are_rejected() {
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
        let violation = rejected_frames(&decoded(rows), &[("main", "node")], None);
        assert_eq!(violation.at, at, "{what}: {violation}");
    }

    // §3.1 boundary: a cancel may cross the reply it cancels.
    let checked = checked_frames(
        &decoded(&[
            ("main", HELLO_MAIN),
            ("node", HELLO_NODE),
            ("main", r#"{"op":"cancel","id":"node:7"}"#),
            ("main", INVOKE),
            ("node", RETURN),
        ]),
        &[("main", "node")],
        None,
    );
    assert_eq!(checked.unanswered(), 0);
}

// ------------------------------------------------------ §3.3, the references

/// A reference this side grants, the far end calls it, and the release that
/// counts: one callback granted twice in one call, called once, and released
/// with the count it was granted by, after which the session goes on.
#[tokio::test(flavor = "multi_thread")]
async fn a_reference_is_granted_by_count_called_and_released() {
    struct Callee;
    impl Dispatch for Callee {
        fn invoke(&self, _: &Connection, _: &str, method: &str, args: Value) -> Reply {
            match method {
                "use" => {
                    let mut args = args.list()?;
                    let first = args.remove(0).reference()?;
                    let second = args.remove(0).reference()?;
                    let answer = first.call(Value::List(vec![Value::Data(json!(41))]))?;
                    // Let go of both references: one release, counting both
                    // grants of the same object (`Drop for Import`).
                    drop(second);
                    Ok(answer)
                }
                _ => Ok(Value::Data(json!("still here"))),
            }
        }
    }
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
        Arc::new(Callee),
        Endpoint::rust(id("node")).expect(id("main")),
    )
    .unwrap();
    within("the caller's greeting", caller.ready())
        .await
        .unwrap();
    within("the callee's greeting", callee.ready())
        .await
        .unwrap();

    let callback = Value::callback(|args| {
        let n = args.list()?.remove(0).json()?;
        Ok(Value::Data(json!(n.as_i64().unwrap() + 1)))
    });
    let answer = within(
        "the call granting one reference twice",
        caller.invoke_async(
            "svc",
            "use",
            Value::List(vec![callback.clone(), callback.clone()]),
        ),
    )
    .await
    .unwrap();
    assert_eq!(answer.json().unwrap(), json!(42));
    let after = within(
        "the call after the release",
        caller.invoke_async("svc", "after", Value::Undefined),
    )
    .await
    .unwrap();
    assert_eq!(after.json().unwrap(), json!("still here"));
    caller.close(Error::Transport("done".into()));
    within("the callee ending", callee.closed()).await;
    within("the caller ending", caller.closed()).await;

    let lines = log.lines();
    let checked = accepted(&lines);
    assert_eq!(checked.last["main"], State::Closed);
    assert_eq!(checked.last["node"], State::Closed);

    let all = frames.all();
    let checked = checked_frames(&all, &[("main", "node")], None);
    assert_eq!(checked.unanswered(), 0, "{all:?}");
    assert_eq!(
        checked.outstanding("main", 1),
        0,
        "the release cleared both grants: {all:?}"
    );

    // The frame that granted the same object twice, the call of it, and one
    // release counting both grants.
    let main = frames.from("main");
    let grant = main
        .iter()
        .find(|frame| frame.op == "invoke")
        .expect("the invoke that granted the reference");
    assert_eq!(
        grant.carries,
        [
            Carried { id: 1, home: false },
            Carried { id: 1, home: false }
        ],
        "the same object sent twice is one reference granted twice"
    );
    let node = frames.from("node");
    let call = node
        .iter()
        .find(|frame| frame.op == "call")
        .expect("the call of the granted reference");
    assert_eq!(call.reference, Some(1));
    let releases: Vec<&Frame> = node.iter().filter(|frame| frame.op == "release").collect();
    assert_eq!(releases.len(), 1, "{node:?}");
    assert_eq!(
        (releases[0].reference, releases[0].count),
        (Some(1), Some(2)),
        "one release, counting both grants"
    );
}

/// What a frame may name (§3.3): a `call`/`get`/`await` names a grant of the
/// far end still standing, a `release` counts no more than was granted, and
/// a reference's identity is one a session allocates. Each broken is a
/// session-level fault, not a call-level one.
#[test]
fn references_outside_the_grants_are_rejected() {
    const GRANT: &str = r#"{"op":"invoke","id":"main:1","path":[],"target":"svc","method":"use","args":{"type":"list","value":[{"type":"reference","value":{"id":7,"kind":"function","home":false,"origin":[]}}]}}"#;
    const CALL: &str =
        r#"{"op":"call","id":"node:1","path":[],"reference":7,"args":{"type":"list","value":[]}}"#;
    const HOME: &str = r#"{"op":"return","id":"main:1","value":{"type":"reference","value":{"id":7,"kind":"function","home":true,"origin":[]}}}"#;

    // The allowed trace: granted, called, released once.
    let checked = checked_frames(
        &decoded(&[
            ("main", HELLO_MAIN),
            ("node", HELLO_NODE),
            ("main", GRANT),
            ("node", CALL),
            ("node", r#"{"op":"release","reference":7,"count":1}"#),
        ]),
        &[("main", "node")],
        None,
    );
    assert_eq!(checked.outstanding("main", 7), 0);

    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], usize);
    let cases: [Case; 9] = [
        (
            "a call of a reference nothing granted",
            &[("main", HELLO_MAIN), ("node", HELLO_NODE), ("node", CALL)],
            2,
        ),
        (
            "a get of a reference nothing granted",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                (
                    "node",
                    r#"{"op":"get","id":"node:1","path":[],"reference":7,"property":"x"}"#,
                ),
            ],
            2,
        ),
        (
            "an await of a reference nothing granted",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                (
                    "node",
                    r#"{"op":"await","id":"node:1","path":[],"reference":7}"#,
                ),
            ],
            2,
        ),
        (
            "a release of a reference nothing granted",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                ("node", r#"{"op":"release","reference":7,"count":1}"#),
            ],
            2,
        ),
        (
            "a release of more than was granted",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                ("main", GRANT),
                ("node", r#"{"op":"release","reference":7,"count":2}"#),
            ],
            3,
        ),
        (
            "a release of nothing",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                ("main", GRANT),
                ("node", r#"{"op":"release","reference":7,"count":0}"#),
            ],
            3,
        ),
        (
            "a call of a reference fully released",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                ("main", GRANT),
                ("node", r#"{"op":"release","reference":7,"count":1}"#),
                ("node", CALL),
            ],
            4,
        ),
        (
            "a reference sent home to a session that granted nothing",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                ("main", INVOKE),
                ("node", HOME),
            ],
            3,
        ),
        (
            "a grant of an identity no session allocates",
            &[
                ("main", HELLO_MAIN),
                ("node", HELLO_NODE),
                (
                    "main",
                    r#"{"op":"invoke","id":"main:1","path":[],"target":"svc","method":"use","args":{"type":"list","value":[{"type":"reference","value":{"id":0,"kind":"function","home":false,"origin":[]}}]}}"#,
                ),
            ],
            2,
        ),
    ];
    for (what, rows, at) in cases {
        let violation = rejected_frames(&decoded(rows), &[("main", "node")], None);
        assert_eq!(violation.at, at, "{what}: {violation}");
    }
}

// ---------------------------------------------------------- §3.5, the errors

/// An error crosses with the code a program decides by: `no-method` and
/// `bad-value` reach the caller as `BindingError`s whose graph carries the
/// code and data, and each fails only its own call (§3.5).
#[tokio::test(flavor = "multi_thread")]
async fn error_graphs_carry_their_codes_across_a_session() {
    struct Throws;
    impl Dispatch for Throws {
        fn invoke(&self, _: &Connection, _: &str, method: &str, _: Value) -> Reply {
            match method {
                "tomorow" => Err(Error::no_method(
                    "weather",
                    "tomorow",
                    vec!["today", "tomorrow"],
                )),
                "sparse" => Err(Error::value("a sparse array")),
                _ => Ok(Value::Data(json!("ok"))),
            }
        }
    }
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
        Arc::new(Throws),
        Endpoint::rust(id("node")).expect(id("main")),
    )
    .unwrap();
    within("the caller's greeting", caller.ready())
        .await
        .unwrap();
    within("the callee's greeting", callee.ready())
        .await
        .unwrap();

    let no_method = within(
        "the call of a method that is not there",
        caller.invoke_async("weather", "tomorow", Value::Undefined),
    )
    .await
    .unwrap_err();
    assert_eq!(
        no_method.to_string(),
        "weather has no method tomorow (methods: today, tomorrow)"
    );
    assert_eq!(no_method.code(), Some("no-method"));
    assert_eq!(
        no_method.data(),
        Some(json!({
            "service": "weather",
            "method": "tomorow",
            "methods": ["today", "tomorrow"],
        }))
    );
    let bad_value = within(
        "the call of a value that cannot cross",
        caller.invoke_async("svc", "sparse", Value::Undefined),
    )
    .await
    .unwrap_err();
    assert_eq!(bad_value.code(), Some("bad-value"));
    // The next call is unaffected: an error belongs to one call.
    let after = within(
        "the call after them",
        caller.invoke_async("svc", "fine", Value::Undefined),
    )
    .await
    .unwrap();
    assert_eq!(after.json().unwrap(), json!("ok"));
    caller.close(Error::Transport("done".into()));
    within("the callee ending", callee.closed()).await;
    within("the caller ending", caller.closed()).await;

    let lines = log.lines();
    accepted(&lines);
    let all = frames.all();
    let checked = checked_frames(&all, &[("main", "node")], None);
    assert_eq!(checked.unanswered(), 0, "{all:?}");

    let no_method = checked
        .failure("main", "main:1")
        .expect("the throw that answered the first call");
    assert_eq!(no_method.name, "BindingError");
    assert_eq!(no_method.code(), Some("no-method"));
    assert_eq!(
        no_method.data(),
        Some(&json!({
            "service": "weather",
            "method": "tomorow",
            "methods": ["today", "tomorrow"],
        }))
    );
    let bad_value = checked
        .failure("main", "main:2")
        .expect("the throw that answered the second call");
    assert_eq!(bad_value.name, "BindingError");
    assert_eq!(bad_value.code(), Some("bad-value"));
}

/// A graph that does not resolve fails only its call (§3.5): the receiver
/// keeps the failure as it came, the session goes on, and the next call is
/// answered. No session event follows the malformed graph: the trace is
/// allowed, and the failure is recorded as `bad graph`.
#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_error_graph_fails_only_its_call() {
    let log = Log::default();
    let frames = Decoded::default();
    let (local, far) = rutis_bridge::memory::pair();
    let local = instrumented(local, "main", "node", &log, &frames);
    // The far end is a fixture too: what it reads is decoded, so the trace
    // holds the frames of both directions.
    let mut far = instrumented(far, "node", "main", &log, &frames);
    let caller = Connection::open(
        local,
        Arc::new(Echo),
        Endpoint::rust(id("main")).expect(id("node")),
    )
    .unwrap();
    let scripted = std::thread::spawn(move || {
        let hello = read_frame(&mut far);
        assert_eq!(hello["op"], "hello");
        write_frame(
            &mut far,
            json!({ "op": "hello", "version": ENDPOINT_PROTOCOL, "endpoint": "node" }),
        );
        let first = read_frame(&mut far);
        // The root names node 3 of no nodes: a malformed graph.
        write_frame(
            &mut far,
            json!({
                "op": "throw",
                "id": first["id"],
                "error": {
                    "name": "TypeError",
                    "message": "bad graph",
                    "graph": { "root": { "type": "reference", "value": 3 }, "nodes": [] },
                },
            }),
        );
        let second = read_frame(&mut far);
        write_frame(
            &mut far,
            json!({
                "op": "return",
                "id": second["id"],
                "value": { "type": "data", "value": "after the graph" },
            }),
        );
        far
    });
    within("the greeting", caller.ready()).await.unwrap();

    let error = within(
        "the call a malformed graph answers",
        caller.invoke_async("svc", "m", Value::Undefined),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "TypeError: bad graph");
    assert_eq!(error.code(), None, "{error:?}");
    assert!(
        caller.close_reason().is_none(),
        "a malformed graph fails its call, not the session (§3.5)"
    );
    let after = within(
        "the call after it",
        caller.invoke_async("svc", "m", Value::Undefined),
    )
    .await
    .unwrap();
    assert_eq!(after.json().unwrap(), json!("after the graph"));
    caller.close(Error::Transport("done".into()));
    within("the caller ending", caller.closed()).await;
    let _far = scripted.join().unwrap();

    let lines = log.lines();
    let checked = accepted(&lines);
    assert_eq!(checked.last["main"], State::Closed, "{lines:?}");
    let all = frames.all();
    let checked = checked_frames(&all, &[("main", "node")], None);
    assert_eq!(checked.unanswered(), 0, "{all:?}");
    let failure = checked
        .failure("main", "main:1")
        .expect("the throw that answered the first call");
    assert_eq!(failure.name, "TypeError");
    assert_eq!(failure.message, "bad graph");
    assert_eq!(failure.graph, Graph::Bad);
    assert_eq!(failure.code(), None);
}

/// Frames that are no failure of protocol 4: a `throw` with no `error`, with
/// no name, and with a name that is not a string. A malformed frame closes
/// the session (§3.7), unlike a malformed graph inside a well-formed one.
#[test]
fn failures_outside_the_protocol_are_rejected() {
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], usize);
    let cases: [Case; 3] = [
        (
            "a throw without an error",
            &[
                ("main", HELLO_MAIN),
                ("main", INVOKE),
                ("node", HELLO_NODE),
                ("node", r#"{"op":"throw","id":"main:1"}"#),
            ],
            3,
        ),
        (
            "a throw without a name",
            &[
                ("main", HELLO_MAIN),
                ("main", INVOKE),
                ("node", HELLO_NODE),
                (
                    "node",
                    r#"{"op":"throw","id":"main:1","error":{"message":"x"}}"#,
                ),
            ],
            3,
        ),
        (
            "a throw whose name is not a string",
            &[
                ("main", HELLO_MAIN),
                ("main", INVOKE),
                ("node", HELLO_NODE),
                (
                    "node",
                    r#"{"op":"throw","id":"main:1","error":{"name":7,"message":"x"}}"#,
                ),
            ],
            3,
        ),
    ];
    for (what, rows, at) in cases {
        let violation = rejected_frames(&decoded(rows), &[("main", "node")], None);
        assert_eq!(violation.at, at, "{what}: {violation}");
    }

    // The boundary: any graph that does not resolve is a failure of the
    // call, not of the session, so it is in an allowed trace as `bad graph`.
    let checked = checked_frames(
        &decoded(&[
            ("main", HELLO_MAIN),
            ("main", INVOKE),
            ("node", HELLO_NODE),
            (
                "node",
                r#"{"op":"throw","id":"main:1","error":{"name":"TypeError","message":"bad graph","graph":"not a graph"}}"#,
            ),
        ]),
        &[("main", "node")],
        None,
    );
    assert_eq!(
        checked
            .failure("main", "main:1")
            .map(|failure| &failure.graph),
        Some(&Graph::Bad)
    );
}
