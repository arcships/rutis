//! The session protocol's pure logic under Miri (#631): each fixture is
//! read as a frame, its tagged values decoded, and the frame written and
//! read again, exactly as the `session_frame` fuzz target does
//! (`rutis_bridge::fuzzing::session_frame`). Malformed and too-deep input
//! must fail alone, and a frame that reads must write back unchanged.
//!
//! The same run covers the two other in-memory fuzz targets, on the seeds
//! in `fuzz/corpus/`: the local channels' line framing as the stream
//! arrives in pieces (`local_lines`, `Mutex`, `Arc` and atomics), and an
//! error graph from the far end read, printed and thrown on
//! (`error_graph`).
//!
//! Run, from the repository root:
//!
//!     RUSTFLAGS="--cfg fuzzing" \
//!         cargo +nightly miri run --manifest-path fuzz/miri/Cargo.toml
//!
//! `--cfg fuzzing` exposes the fuzz entry points in `rutis-bridge`; nothing
//! here reads files, clocks or the network, so Miri's default isolation is
//! enough (no `MIRIFLAGS` needed). The same entry points are the cargo-fuzz
//! targets in `fuzz/Cargo.toml`, on nightly.
//!
//! guarantee: bridge/value-roundtrip, bridge/value-tags,
//! bridge/large-and-deep-values, bridge/error-report-bounded

/// A frame the reader must accept; `session_frame` writes each back and
/// reads it again.
const FIXTURES: &[(&str, &str)] = &[
    (
        "hello",
        r#"{"op":"hello","version":4,"endpoint":"e1","implementation":{"name":"node","version":"1.2.3"},"capabilities":["value-bytes","sync-stack"]}"#,
    ),
    (
        "invoke: list, data, record, reference, signal, undefined",
        r#"{"op":"invoke","id":"1","path":["app"],"target":"svc","method":"m","args":{"type":"list","value":[{"type":"data","value":{"n":[1,2.5,null,true],"s":"\u0000x"}},{"type":"record","value":{"k":{"type":"reference","id":9,"home":false,"kind":"object","origin":["x"]}}},{"type":"reference","id":2,"home":true,"kind":"function","origin":[]},{"type":"signal"},{"type":"undefined"}]},"sync":true}"#,
    ),
    (
        "call: method",
        r#"{"op":"call","id":"2","path":["app","p"],"reference":7,"method":"read","args":{"type":"data","value":"x"}}"#,
    ),
    (
        "get",
        r#"{"op":"get","id":"3","path":[],"reference":7,"property":"p","sync":true}"#,
    ),
    (
        "await",
        r#"{"op":"await","id":"4","path":["a"],"reference":8}"#,
    ),
    ("release", r#"{"op":"release","reference":7,"count":2}"#),
    ("cancel", r#"{"op":"cancel","id":"1"}"#),
    (
        "return: every tagged value",
        r#"{"op":"return","id":"5","value":{"type":"list","value":[
            {"type":"instant","value":"2026-11-11T00:00:00Z"},
            {"type":"datetime","value":"2026-11-11T00:00:00"},
            {"type":"date","value":"2026-11-11"},
            {"type":"time","value":"00:00:00.5"},
            {"type":"bytes","value":"AAEC"},
            {"type":"bigint","value":"-123"},
            {"type":"decimal","value":"1.5"},
            {"type":"uuid","value":"00000000-0000-0000-0000-000000000000"},
            {"type":"map","value":[[{"type":"data","value":"k"},{"type":"data","value":1}],[{"type":"data","value":"k2"},{"type":"undefined"}]]},
            {"type":"set","value":[{"type":"data","value":1},{"type":"data","value":"x"}]}
        ]}}"#,
    ),
    (
        "throw: graph and code",
        r#"{"op":"throw","id":"6","error":{"name":"BindingError","message":"no such method","graph":{"root":{"value":0},"nodes":[]}}}"#,
    ),
    ("empty input", ""),
    ("truncated json", r#"{"op":"invoke","id":"1","#),
    ("unknown frame", r#"{"op":"nope"}"#),
    (
        "bad values fail alone",
        r#"{"op":"return","id":"7","value":{"type":"list","value":[{"type":"date","value":"2026-02-30"},{"type":"bytes","value":"A"},{"type":"time","value":"24:00:00"},{"type":"data","value":1}]}}"#,
    ),
];

/// The `local_lines` seeds: a limit, a piece size and the stream. Each is
/// read back line by line through a memory channel, and what was sent back
/// must be the stream up to the last complete line.
const LOCAL_LINES: &[(&str, &[u8])] = &[
    (
        "empty-limit",
        include_bytes!("../../corpus/local_lines/empty-limit"),
    ),
    (
        "no-newline",
        include_bytes!("../../corpus/local_lines/no-newline"),
    ),
    (
        "over-limit",
        include_bytes!("../../corpus/local_lines/over-limit"),
    ),
    (
        "truncated",
        include_bytes!("../../corpus/local_lines/truncated"),
    ),
    (
        "two-lines",
        include_bytes!("../../corpus/local_lines/two-lines"),
    ),
    // A limit of 0 accepts only empty lines, one byte at a time.
    ("limit 0", &[0, 0, b'\n', b'\n']),
    // A piece size of 0 reads one byte at a time (`local_lines` makes it
    // 1); the limit is the full buffer here, so no line is too long.
    ("one byte a read", &[255, 0, b'a', b'\n', b'b', b'\n']),
    // Fewer than two bytes: the target reads nothing.
    ("empty input", &[]),
    ("limit only", &[8]),
];

/// The `error_graph` seeds: an error graph from the far end, as JSON text.
/// Reading it, printing it and throwing it on must not panic, and what is
/// printed must start with the error's own text.
const ERROR_GRAPHS: &[(&str, &[u8])] = &[
    ("cycle", include_bytes!("../../corpus/error_graph/cycle")),
    (
        "dangling",
        include_bytes!("../../corpus/error_graph/dangling"),
    ),
    (
        "method-newline",
        include_bytes!("../../corpus/error_graph/method-newline"),
    ),
    (
        "plugin-error",
        include_bytes!("../../corpus/error_graph/plugin-error"),
    ),
    // Not JSON, and not UTF-8: the target returns before reading a graph.
    ("truncated json", b"{\"root\":{"),
    ("not utf-8", &[0xff, 0xfe, b'{', b'}']),
    ("empty input", b""),
];

fn main() {
    for (name, fixture) in FIXTURES {
        println!("miri: {name}");
        rutis_bridge::fuzzing::session_frame(fixture.as_bytes());
    }
    // A frame past the reader's depth limit is refused before parsing: the
    // check itself is what runs here, and it must not recurse or panic.
    let deep = format!(
        r#"{{"op":"return","id":"8","value":{}{}}}"#,
        "[".repeat(400),
        "]".repeat(400)
    );
    println!("miri: too deep");
    rutis_bridge::fuzzing::session_frame(deep.as_bytes());
    for (name, fixture) in LOCAL_LINES {
        println!("miri: lines {name}");
        rutis_bridge::fuzzing::local_lines(fixture);
    }
    for (name, fixture) in ERROR_GRAPHS {
        println!("miri: error {name}");
        rutis_bridge::fuzzing::error_graph(fixture);
    }
    println!("miri: ok");
}
