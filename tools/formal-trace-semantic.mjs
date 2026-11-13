// Semantic re-extraction of the one partition that has been rewritten
// (2026-11-11): the 44 trace rows of `rules/trace-contract.md`.
//
// The re-check in `formal-trace-recheck.mjs` maps a rule onto a current source;
// it does not rewrite the rule. This table is the rewrite: for every row, the
// section of the current source (`source`, a file plus a section), the
// `SESS-*` anchors of `docs/protocol-session.md` §11 that carry the rule, the
// rule text in force in English (`en`; the Chinese row in the file is the same
// rule) and its English gap (`gapEn`). `anchors: []` requires
// `whyNoAnchor`/`whyNoAnchorEn`: the session specification does not cover the
// rule, and the row says where it lives instead. `extra` names a second current
// source the row also cites.
//
// This file is the single source of the generated English re-check page
// (`docs/formal-trace/recheck-0.22.en.md`) and of the TRC section of the
// Chinese re-check page. `formal-trace.mjs --check` verifies: every row of the
// file has one entry and the reverse, the statuses agree, every anchor is in
// the session specification and in the row's 现行来源 cell, every `source` (and
// `extra`) section heading exists in its document, and both generated pages
// match. The protocol partitions have no entries here: they are re-checked, not
// re-extracted.
export const SEMANTIC_DOCS = {
  'formal-trace.md': { doc: 'docs/formal-trace.md', label: '会话追踪契约', labelEn: 'Session trace contract', kind: 'spec' },
  'protocol-session.md': { doc: 'docs/protocol-session.md', label: '会话协议规格', labelEn: 'Session protocol specification', kind: 'spec' },
  'design-remote-session-2026-10-28.md': { doc: 'docs/design-remote-session-2026-10-28.md', label: '远程连接设计', labelEn: 'Remote session design', kind: 'design' },
}

export const SEMANTIC = {
  file: 'docs/formal-trace/rules/trace-contract.md',
  label: '会话追踪契约 TRC',
  labelEn: 'Session trace contract (TRC)',
  pageEn: 'docs/formal-trace/recheck-0.22.en.md',
  date: '2026-11-11',
  baseline: '4a1ac08b',
  rows: {
    'TRC-LIFE-1': {
      source: 'formal-trace.md §3.0',
      anchors: ['SESS-5', 'SESS-28'],
      en: 'The handshake returns a `Greeted` session and registers or mounts nothing (`SESS-28`); a replacement mounts only after the old session unloads; an ordinary reconnect pair reaches `FiberState::Active`, and a reconnect is a new session that keeps no old state (`SESS-5`); a runtime row is loaded through `rows.schema`/`rows.load`, and its load-success path announces services and awaits listener registration before it reports the row loaded. This is a sequencing fact, not a uniform cross-runtime policy.',
      gapEn: 'Rows, services and announcements are outside the session specification (multi-language M1 and the mount design); the row-failure policy is not uniform across runtimes (the source does not claim it is); no trace validator yet (#624).',
    },
    'TRC-LIFE-2': {
      source: 'formal-trace.md §3.0',
      anchors: ['SESS-42'],
      en: '`rows.unload` is idempotent: a loaded row is disposed and its services are withdrawn; when the far end leaves, the session is disposed, in-flight calls fail once without a retry (`SESS-42`), and the far plugins are removed; a row that ends by itself reports `rows.ended` only when the session was not cancelled, and reports `rows.failed` once per failed state.',
      gapEn: '`rows.*` is outside the session specification (multi-language M1 and the mount design); announcement errors are handled per SDK with no uniform assertion; no trace validator yet (#624).',
    },
    'TRC-LIFE-3': {
      source: 'formal-trace.md §3.0',
      anchors: ['SESS-5', 'SESS-28', 'SESS-29'],
      en: 'A reconnect is a new session: the old session and its rows are disposed before the new pair becomes active, and the old calls, references and late frames do not enter the new session (`SESS-5`); a handshake-only session shares no plugins or services (`SESS-28`); the transport thread\'s `Ok(())`/`Ok(None)` and the local session `Disposed` are two observations (`SESS-29`), not one handshake result.',
      gapEn: 'Rust tests only; the replacement order has no equivalent test in Go, Node, Bun or Python (#624).',
    },
    'TRC-FRAME-1': {
      source: 'formal-trace.md §3.1',
      anchors: ['SESS-6', 'SESS-2', 'SESS-18'],
      en: 'A call is an `invoke` frame with `id`, `path`, `target`, `method`, `args` and the optional `sync`; its result is a `return` with the same `id` or a `throw` carrying a `Failure` (`SESS-6`); one UTF-8 JSON frame per message, read one at a time in arrival order (`SESS-2`); a channel handles frames one at a time, while calls may run concurrently on their own tasks, threads or stacks (`SESS-18`).',
      gapEn: 'Order is covered indirectly by the Go recorder and the Rust frame test; there is no cross-implementation test asserting "one frame at a time, in order" (#624).',
    },
    'TRC-FRAME-2': {
      source: 'formal-trace.md §3.1',
      anchors: ['SESS-41', 'SESS-63'],
      en: 'When this end is not waiting for that `id`, a `return`/`throw` for it is a protocol error (`response for unknown call`): the session ends with a transport error, not a call-level error (`SESS-41`, `SESS-63`); local calls then fail because the session is closed.',
      gapEn: 'Rust matrix only; the dangling-reply cases of the other implementations are not listed (#635, #624).',
    },
    'TRC-FRAME-3': {
      source: 'formal-trace.md §3.1',
      anchors: ['SESS-58', 'SESS-60'],
      en: 'A `cancel` for an id that does not exist is ignored on purpose, because it may have overtaken the reply it cancels; this is the only tolerated dangling call id (`SESS-58`); the cancel itself does not close the session, and a later valid call still works (`SESS-60`).',
      gapEn: 'Rust test only; the mutation (treating an unknown `cancel` as fatal) is not caught automatically (#631).',
    },
    'TRC-GREET-1': {
      source: 'formal-trace.md §3.2',
      anchors: ['SESS-6', 'SESS-20', 'SESS-28', 'SESS-78'],
      en: 'Each end sends a `hello` with `version`, endpoint, implementation (optional) and capabilities; both ends write `hello` before reading, and a pair is ready only when both report `Ready()` (`SESS-20`); a greeting alone mounts nothing: `Session::handshake` returns `Greeted` and registers nothing (`SESS-28`); `implementation` may be omitted and the receiver does not require it (`SESS-78`).',
      gapEn: 'Rust and Go tests only; Node, Bun, Python, Swift and Kotlin have no equivalent handshake test (#624).',
    },
    'TRC-GREET-2': {
      source: 'formal-trace.md §3.2',
      anchors: ['SESS-23', 'SESS-24', 'SESS-26', 'SESS-29'],
      en: 'A far end that speaks protocol 3 gets `incompatible session: the far end speaks protocol 3, this side 4: upgrade the far end`, naming which end to upgrade (`SESS-23`); a missing or invalid `hello.endpoint` fails the handshake (`SESS-24`); no greeting within 10 seconds gets `the far end did not greet in time` and closes the session (`SESS-26`); a host without a `LocalId` fails with `the host provides no id for its sessions` and sends no greeting; a failed handshake closes the session and cancels the waiting calls (`SESS-29`).',
      gapEn: 'The 10-second deadline has a Rust virtual-clock test only (`start_paused`); 10 seconds is a scheduling deadline, not a wall-clock promise (`SESS-31`); the other implementations have no deterministic timing for the same deadline (#639).',
    },
    'TRC-GREET-3': {
      source: 'formal-trace.md §3.2',
      anchors: ['SESS-25', 'SESS-15', 'SESS-17'],
      en: 'Capabilities are declarations, not requirements: no implementation offers an entry point for a required-capabilities list, so the handshake does not refuse a far end that lacks one (`SESS-25` is not guaranteed); a declaration grants no permission and is not verified, and unknown capability names are ignored (`SESS-15`, `SESS-17`).',
      gapEn: 'Rust\'s `Greet.require` had one production call passing an empty list and is removed; Go\'s `peer.Options.Require` sits in `internal/peer` with no caller; Node, Bun, Python, Swift and Kotlin have no such check (#1111).',
    },
    'TRC-REF-1': {
      source: 'formal-trace.md §3.3',
      anchors: ['SESS-46', 'SESS-50', 'SESS-52'],
      en: 'A reference is tagged `{type:"reference", value:{id, home, kind, origin}}`; the same object sent again is the same reference, and two grants to one session share the id with the count increased (`SESS-46`); a reference sent home becomes the original value with its identity (`SESS-50`); an object with methods travels by reference and every protocol-4 implementation accepts object references (`SESS-52`).',
      gapEn: 'Only a Go test covers the two-hop identity round trip; the Rust side has structure and relay tests only (#624).',
    },
    'TRC-REF-2': {
      source: 'formal-trace.md §3.3',
      anchors: ['SESS-48', 'SESS-63'],
      en: 'A `call`, `get`, `await` or `release` naming a reference this session does not hold closes the session with a transport error (`unknown or released reference`, `release of unknown reference`); this is a malformed far-end trace, not a recoverable `no-method` call error (`SESS-48`, `SESS-63`).',
      gapEn: 'Rust tests only; the unknown-reference behaviour of the other implementations is not listed (#635).',
    },
    'TRC-REF-3': {
      source: 'formal-trace.md §3.3',
      anchors: ['SESS-47', 'SESS-48', 'SESS-51'],
      en: '`release {reference, count}` releases by count; `count` must be greater than 0 and no larger than the grants made, or the session ends with `invalid reference release count` (`SESS-47`); after a normal release the export count drops by one and the session stays open; closing the session clears the export and import tables, and unreleased references go with it (`SESS-51`).',
      gapEn: 'No property test for reference conservation (no leak over any grant/release sequence, #631).',
    },
    'TRC-CANCEL-1': {
      source: 'formal-trace.md §3.4',
      anchors: ['SESS-56', 'SESS-57', 'SESS-60'],
      en: 'The cancel frame is `{"op":"cancel","id":...}`, sent when the caller drops the future of an unfinished asynchronous call (`SESS-56`); the far end sends `call` and then `cancel`, the object method observes the context cancellation, a late reply for the cancelled id is dropped and counted (`SESS-57`), and the next `invoke` succeeds; the cancel does not close the session (`SESS-60`).',
      gapEn: 'Only a Go test covers end-to-end cancellation; Rust has its own synchronous-cancel test but not the same set (#624).',
    },
    'TRC-CANCEL-2': {
      source: 'formal-trace.md §3.4',
      anchors: ['SESS-57', 'SESS-60', 'SESS-61'],
      en: 'A method may reply late: the host receives and drops the reply, counts it (`orphans()`), and then handles the next call in order; the caller gets its context error and the session stays open (`SESS-57`, `SESS-60`); whether the callee replies after a cancel is still not a uniform promise (`SESS-61`).',
      gapEn: 'Go test only; the reply rule after a cancel waits for #626 to write it back into the specification (#624, #626).',
    },
    'TRC-CANCEL-3': {
      source: 'formal-trace.md §3.4',
      anchors: ['SESS-58', 'SESS-59', 'SESS-60'],
      en: 'A cancel may overtake the reply, and an unknown `cancel` is ignored; a reply winning the race must not close the session (`SESS-58`); cancellation propagates hop by hop and does not undo reference grants already made, so a released proxy still sends `release` (`SESS-59`); the cancel does not close the session (`SESS-60`).',
      gapEn: 'The race (reply and cancel arriving together) has no test with controlled scheduling (#622, #630).',
    },
    'TRC-ERR-1': {
      source: 'formal-trace.md §3.5',
      anchors: ['SESS-43'],
      en: 'A `Failure` carries `name`, `message` and an optional `graph`; rutis\'s `no-method` becomes a `BindingError` with code `no-method` and `data` across the end, as do `bad-value` and transport errors; the exact message is `weather has no method tomorow (methods: today, tomorrow)`; a business error affects its call only, and a malformed error graph is not a protocol error (`SESS-43`).',
      gapEn: 'No property test for the error-graph round trip (#631); the error-code columns of Go, Node, Bun, Python, Swift and Kotlin have no shared test (#639).',
    },
    'TRC-ERR-2': {
      source: 'formal-trace.md §3.5',
      anchors: ['SESS-44', 'SESS-71'],
      en: 'An error belongs to one call: a value that cannot cross is refused before the frame is sent (`bad-value`), only that call fails and grants already made are rolled back (`SESS-44`); the next call succeeds and the refused method\'s `invoke` is not recorded; an over-limit result has the prefix `conformance.big: the result cannot be sent: the message would be ` with data `{"limit":16777216,"part":"result"}` (`SESS-71`).',
      gapEn: 'Only the Go test asserts the failure scope; the Rust over-limit test lives in the session tests, and the two are not the same set (#624).',
    },
    'TRC-ERR-3': {
      source: 'formal-trace.md §3.5',
      anchors: ['SESS-43', 'SESS-63'],
      en: 'A malformed error graph surfaces as a received `TypeError` with message `bad graph`; it can be thrown again and does not end the session (`SESS-43`); it is deliberately narrower than a malformed frame — bad frame syntax or a dangling id closes the session (`SESS-63`).',
      gapEn: 'The two failure scopes (graph vs frame) have tests only in Go and Rust respectively; no comparison within one implementation (#624).',
    },
    'TRC-LIMIT-1': {
      source: 'formal-trace.md §3.6',
      anchors: ['SESS-70', 'SESS-71'],
      en: 'The session records `Sender::max_message()` when it opens and compares the size of every frame before writing it; a fitting message is queued in order and gets its reply (`SESS-71`); the limit is a transport rule: process pipes and WebSocket default to 16 MiB, in-memory channels have no limit (`SESS-70`).',
      gapEn: 'The limit differs per implementation (16 MiB is the default for process pipes and WebSocket); no test fixes one limit across implementations (#635).',
    },
    'TRC-LIMIT-2': {
      source: 'formal-trace.md §3.6',
      anchors: ['SESS-71', 'SESS-44'],
      en: 'A value over 16 MiB fails only that call: `svc.take: argument 2 cannot be sent: the message would be over the limit of 16777216 bytes (16 MiB)`, code `bad-value`, data `{"argument":2,"limit":16777216,"part":"argument"}`; the message never reaches the host, the callback export is released, and later calls succeed (`SESS-71`, `SESS-44`).',
      gapEn: 'Only the Go test is end to end; the Rust call-level tests cover the same rule without a host; the mutation is not caught automatically (#631).',
    },
    'TRC-LIMIT-3': {
      source: 'formal-trace.md §3.6',
      anchors: ['SESS-75', 'SESS-76'],
      en: '`SessionOptions::outbound_limit` bounds the bytes queued behind the one frame the writer holds; a limit of `0` still accepts one empty-queue frame; on overflow the session ends with `the far end is not reading: <n> messages, <bytes> bytes waited for it` (`peer-gone`) and the queued messages go with it (`SESS-75`); the default `None` is unbounded and explicitly not guaranteed (`SESS-76`).',
      gapEn: 'Rust tests only; `None` being unbounded is an explicit non-guarantee (`TRC-NG-7`).',
    },
    'TRC-MALFORMED-1': {
      source: 'formal-trace.md §3.7',
      anchors: ['SESS-58', 'SESS-63'],
      en: 'The only tolerated malformed- or dangling-looking input is an unknown `cancel` (reply race); unknown fields and over-deep frames that can be attributed to a call fail only that call (`SESS-58`, `SESS-63`); a later valid `invoke` gets a normal result.',
      gapEn: 'Rust tests only (#624).',
    },
    'TRC-MALFORMED-2': {
      source: 'formal-trace.md §3.7',
      anchors: ['SESS-63', 'SESS-64', 'SESS-65'],
      en: 'A malformed or dangling frame fails closed: malformed JSON, an empty message, a leading BOM, invalid UTF-8, a non-object, an unknown `op`, wrong field types, missing fields, unknown wire tags, a call/await/release for an unknown reference, and a reply to an unknown call; every case waits for `peer.closed()`, requires `Error::Transport(_)` and verifies that later calls are refused (`SESS-63`); the shared matrix has 13 cases that all seven implementations run (`SESS-64`), and the rules outside it are executed by code (`SESS-65`).',
      gapEn: 'The shared matrix has 13 cases; §14 G2 lists the nine missing ones (`SESS-65`); the other implementations have no per-case tests outside the matrix (#635, #624).',
    },
    'TRC-MALFORMED-3': {
      source: 'formal-trace.md §3.7',
      anchors: ['SESS-66', 'SESS-67', 'SESS-69'],
      en: 'Unknown fields are not an extension mechanism (`SESS-69` explicitly does not guarantee forward compatibility), but they no longer end the whole session: `hello` fails the handshake; `invoke`/`call`/`get`/`await` fail only that call (`bad-frame`, data `{op, fields}`); `return`/`throw` fail only the call waiting for the reply; `release`/`cancel` ignore them; a malformed or dangling frame that cannot be attributed to a call still ends the session (`SESS-66`); all seven implementations reject at call level (`SESS-67`).',
      gapEn: 'The `SESS-67` row of §11 and §14 G1/G5 still say six runtimes ignore unknown fields, contradicting `SESS-66`/`SESS-67` in §9; #635 has to fix the specification. The behaviour of `release`/`cancel` after a failed call has unit branches only, no cross-implementation test.',
    },
    'TRC-NET-1': {
      source: 'formal-trace.md §3.8',
      anchors: ['SESS-31'],
      extra: 'design-remote-session-2026-10-28.md §三',
      en: 'Dial retry parsing keeps the defaults when the override is malformed; a valid `100,400,60000,300` override changes the initial, maximum, stable and refusal delays while keeping the jitter of `0.2` and the 10-second handshake; the backoff produces exactly `0.5s 1s 2s 4s 8s 16s 30s 30s 30s` and then resets to `500ms`, with jitter bounds tested at `400ms` and `600ms`; the reconnect numbers (0.5 → 30 seconds, auth-rejected every 30 seconds, incompatible stops, reset after 60 seconds) are the shared convention of every dialer (remote session design §三; the session specification leaves retries to the connection layer, `SESS-31`).',
      gapEn: 'Go tests only; the other implementations have no equivalent backoff set (#624).',
    },
    'TRC-NET-2': {
      source: 'formal-trace.md §3.8',
      anchors: ['SESS-31'],
      extra: 'design-remote-session-2026-10-28.md §三',
      en: 'A session failure is classified by error type, not by text (`ConnectError` has three classes): rejected → `authRejected`, incompatible → `incompatible`, replaced/host-gone → `retryable`; an incompatible connection stops retrying until the configuration changes, and an authentication failure retries slowly every 30 seconds; the classification table includes a misleading-text case, proving the decision does not match text (remote session design §三; `SESS-31`).',
      gapEn: 'Go test only (#624).',
    },
    'TRC-NET-3': {
      source: 'formal-trace.md §3.8',
      anchors: ['SESS-3', 'SESS-5', 'SESS-28'],
      extra: 'design-remote-session-2026-10-28.md §三',
      en: 'The host listener accepts a token with the WebSocket subprotocol `rutis.4` (`SESS-3`); for an id that already has a connection, the new connection replaces the old one only after the handshake: the old session unloads, closes as `Replaced` (4002), and only then the new session is installed; a failed handshake changes nothing (`SESS-5`, `SESS-28`); a late frame cannot reach the replaced session.',
      gapEn: 'No independent end-to-end test for the listener and the replacement order; it is covered only by the session-plugin tests (#624).',
    },
    'TRC-INV-1': {
      source: 'formal-trace.md §4',
      anchors: ['SESS-18'],
      en: 'Frames on one channel are handled one at a time in arrival order (`SESS-18`); calls may run concurrently on their own tasks, threads or stacks, while frame reading and call-id admission are serial.',
      gapEn: 'Order is not asserted per implementation; the Go recorder records but does not judge (#624).',
    },
    'TRC-INV-2': {
      source: 'formal-trace.md §4',
      anchors: ['SESS-21', 'SESS-27'],
      en: 'No call frame is accepted before `ready`: a local call before `ready` fails, `Connection::send` returns `Transport("protocol handshake incomplete")` and the frame is not written (`SESS-27`); any other frame from a far end that has not greeted ends the session with a transport error, text `request before protocol handshake` (`SESS-21`).',
      gapEn: 'Rust has the source but no test asserting that refusal text; Go asserts `before protocol handshake` and waits for the session to end; the shared-matrix case for `SESS-21` is part of the gap in §14 G2.',
    },
    'TRC-INV-3': {
      source: 'formal-trace.md §4',
      anchors: ['SESS-43', 'SESS-44', 'SESS-63'],
      en: 'Encoding, value and limit errors fail only their call; malformed frames, dangling references or calls, and transport failures close the session (`SESS-43`, `SESS-44`, `SESS-63`).',
      gapEn: 'The two failure scopes have tests in Rust and Go respectively; no comparison matrix within one implementation (#624).',
    },
    'TRC-INV-4': {
      source: 'formal-trace.md §4',
      anchors: ['SESS-4', 'SESS-33', 'SESS-36', 'SESS-5'],
      en: 'An endpoint-prefixed id names the session that made the call; a call id must carry the far end\'s endpoint id and `:`, increase, and not appear in the frame\'s `path` (`SESS-4`, `SESS-33`); forwarding rebuilds the chain from session tags (`SESS-36`), and a reconnect is a new session, so a late id from the old session cannot name a call in the new one (`SESS-5`).',
      gapEn: 'The "late id from the old session" clause has only the replacement-order test; no test sends an old id straight to the new session (#635).',
    },
    'TRC-INV-5': {
      source: 'formal-trace.md §4',
      anchors: ['SESS-56', 'SESS-57', 'SESS-60'],
      en: 'A cancel reaches the running call; a late reply is dropped and counted; a cancel does not close the session (`SESS-56`, `SESS-57`, `SESS-60`).',
      gapEn: 'The race has no controlled scheduling (#622, #630); the Rust async-cancel test is not aligned with Go (#624).',
    },
    'TRC-INV-6': {
      source: 'formal-trace.md §4',
      anchors: ['SESS-47', 'SESS-51'],
      en: 'Grants and releases pair up, `count` cannot exceed the grants made, and closing the session clears unfinished exports and imports (`SESS-47`, `SESS-51`).',
      gapEn: 'No property test that no grant/release sequence leaks (#631).',
    },
    'TRC-INV-7': {
      source: 'formal-trace.md §2',
      anchors: [],
      whyNoAnchor: '会话规格不规定追踪输出；规则在 formal-trace.md §2',
      whyNoAnchorEn: 'The session specification does not cover trace output; the rule is in formal-trace.md §2',
      en: 'Channel trace output carries only the label, direction, byte counts and a reason, never a payload (`crates/rutis-bridge/src/channel/trace.rs:104-133`); the validator compares event type, direction, label and byte length, and production traces never expect a payload.',
      gapEn: 'Only the Rust verbatim-output test exists; the other implementations (Go and others) have no equivalent trace-output test (#624).',
    },
    'TRC-INV-8': {
      source: 'formal-trace.md §4',
      anchors: ['SESS-5', 'SESS-28'],
      extra: 'design-remote-session-2026-10-28.md §4.2',
      en: 'Before the same id accepts a new hello, the old session is already closed: it unloads, closes as `Replaced`, and only then the new session is installed (remote session design §4.2; `SESS-5`, `SESS-28`).',
      gapEn: 'The host side has only a source comment; replacement isolation has no independent end-to-end test (#624).',
    },
    'TRC-NG-1': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-18'],
      en: 'The concrete scheduling interleaving beyond channel order is not guaranteed: only frames one at a time in arrival order and serial call-id admission are promised (`SESS-18`); a trace only answers whether one real execution belongs to the allowed trace language.',
      gapEn: 'Do not read a passing trace as safety for any interleaving (`TRC-INV-1` promises order only).',
    },
    'TRC-NG-2': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-21', 'SESS-27', 'SESS-30'],
      en: 'A successful call before `ready` is not guaranteed; under the current rule it fails and ends the session: a local call gets `protocol handshake incomplete` (`SESS-27`), and any frame from a far end that has not greeted ends the session with a transport error (`SESS-21`); `SESS-30` replaced the two old rules.',
      gapEn: 'A Go test locks in that it is not provided; the Rust side has no assertion (§14 G2); the other implementations have no equivalent test (#624).',
    },
    'TRC-NG-3': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-55'],
      en: 'Reference lifetime does not keep a runtime process alive; after the holder exits, calls to its objects fail with `peer-gone` instead of keeping the runtime running (`SESS-55`).',
      gapEn: 'No test locks this in; if anyone relies on it, a "not provided" test is needed (#635).',
    },
    'TRC-NG-4': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-52', 'SESS-53', 'SESS-79'],
      en: 'Withdrawn: "cross-runtime object-capability interop is not guaranteed" was written for the 0.18 `objects` capability. Protocol 4 has no such capability (`SESS-16`; `SESS-53` replaced the capability and its refusal branch), and every protocol-4 implementation accepts object references (`SESS-52`); cross-language object references are covered by `bridge/object-refs`; the #639 "Rust relay does not check `objects`" has no check point to trigger in protocol 4, and far ends of 0.18 or earlier are refused at the handshake (`SESS-79`).',
      gapEn: 'The 0.18 `objects` capability no longer exists in protocol 4, so this non-guarantee is withdrawn; object references are no longer sent conditionally (#1123).',
    },
    'TRC-NG-5': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-45', 'SESS-61'],
      en: 'A call deadline is not guaranteed: the protocol has no deadline and the caller implements timeouts itself; the `cancel` rules here do not include a deadline (`SESS-45`), and whether the callee replies after a cancel is decided separately (`SESS-61`).',
      gapEn: 'Timing promises belong to the virtual clock of #639; nothing may be written as a guarantee before the specification has a deadline.',
    },
    'TRC-NG-6': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-63'],
      en: 'A stable, platform-specific transport reason for malformed input is not guaranteed; tests promise only `Error::Transport(_)` and the failure scope (`SESS-63`).',
      gapEn: 'A test locks in that the reason sentence is not promised; the equivalent assertions of the other implementations are not listed (#624).',
    },
    'TRC-NG-7': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-76'],
      en: 'With `outbound_limit` at `None` (the default), an unbounded outbound backlog is not guaranteed (`SESS-76`).',
      gapEn: 'A test locks in the unbounded default; the other implementations have no equivalent test for their defaults (#635).',
    },
    'TRC-NG-8': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-66', 'SESS-69'],
      en: 'Forward compatibility for arbitrary new fields is not guaranteed: an old implementation fails that call with `bad-frame` when it can attribute the field, ends the session when it cannot, and never silently ignores it (`SESS-66`, `SESS-69`); a compatible change must be deliberate and versioned.',
      gapEn: 'All seven runtimes implement the call-level `bad-frame` refusal.',
    },
    'TRC-NG-9': {
      source: 'formal-trace.md §1',
      anchors: ['SESS-31'],
      extra: 'design-remote-session-2026-10-28.md §三',
      en: 'With jitter enabled, the exact wall-clock reconnect time is not guaranteed; only the backoff upper-bound sequence and the reset point are (remote session design §三; `SESS-31`).',
      gapEn: 'Timing promises belong to the virtual clock of #639; wall-clock time outside the jitter bounds is not a guarantee.',
    },
  },
}
