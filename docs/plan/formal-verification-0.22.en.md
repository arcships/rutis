# 0.22 formal-coverage execution plan

Status: active. Date: 2026-11-09.
Scope decision: 0.23 is merged into 0.22. The 0.23 milestone issues #613, #622, #624, #630, #631, #634, #635, #639 and #776 join #1025 in 0.22's release scope. #1040 stays deferred and is not brought back by the merge.
Basis: [formal-verification redesign](../design-formal-verification-2026-11-08.en.md), [0.22 roadmap](../roadmap-0.22-2026-11-08.en.md), [quality standard](../quality-standard.en.md) §3, §8, §9.4, §11.

This document only arranges work and gaps; it changes no behaviour. Nothing new is visible to developers. When an item is implemented, its design document's "What developers see" is the acceptance text.

## 1. Workstreams

| ID | Scope | Issues | Deliverable | Depends on | Status |
| --- | --- | --- | --- | --- | --- |
| WS0 | Baseline, roadmap merge, gap register | #776, #634 | This plan; roadmap and quality-status revisions | — | In progress |
| WS1 | #1025 event close admission: marker, publication, `Entry::live`, race tests, benchmark | #1025, #622 | Code, deterministic tests, benchmark result | WS0 | Core done, verifying |
| WS2 | Current session protocol specification: frames, handshake, calls, references, cancellation, malformed frames, limits, unknown fields | #635 | `docs/protocol-session.md` and `.en.md`; old designs point to it; gap list | WS0 | First version done; unknown fields decided (call-scoped) |
| WS3 | Rule list and tracking table: source, plain promise, evidence, mutation, status, gap | #634 | `docs/formal-trace/` and a statistics script; `guarantees.md` linked | WS2 partly | First version done, 1817 rows (including 1368 protocol rules); not yet rechecked against the current specification |
| WS4 | TLA+ models of the protocol design: event close admission, cross-runtime sync, remote lease, row and call lifecycle | #613 | New `specs/` model or reconciled old models, `expected.txt`, `run.sh`, README | WS2 | All nine models are expanded to the design scope: event-close, the four events modules, cross-runtime-sync, remote-lease, row-lifecycle, call-lifecycle and peer-rows; about 215 configurations, all new ones run; implementation fixes and model follow-up remain |
| WS5 | Real-execution trace and conformance: channel/frame trace validation, applicable implementation columns | #624 | Validator, positive/negative cases, gap list | WS2, WS4 | First slice done: 16 cases; limits, leases, host-gone, sync-cycles remain |
| WS6 | Controlled scheduling and static checks for non-Rust runtimes | #630 | Evidence per applicable language; reasons for not applicable | WS2 | First slice done |
| WS7 | Property tests, fuzzing, Kani, Miri, virtual clocks | #631, #639 | Property tests, fuzz targets, timing cases, Kani/Miri conclusions | WS2 | First slice done: six grant-rollback + five value/frame/bad-frame properties; Kani/Miri/fuzz not run |
| WS8 | Close-out: tracking table to zero gaps, release gate, CI reports | #634 | Release check result and unverified list | All | In progress: tracker check is in CI; release gate not started |

## 2. Order and gates

First stage (now, in parallel):

1. WS1 lands the race test and model before the core change; the core change must be independently revertible.
2. WS2 writes the current specification first. When specification and code disagree, the specification changes first, not the code.
3. WS3 builds the table skeleton and links the existing `guarantees.md`; rules are extracted from specifications and the guarantee register, not from memory.
4. WS4 reconciles `specs/cross-runtime-sync` and `specs/remote-lease` first, then adds the event-close model.
5. WS5 starts with allowed-sequence validation for the channel trace; frame level and cross-implementation columns follow.
6. WS6 and WS7 start with languages and tools available on this machine; Swift/Kotlin and dedicated-runner items are recorded as environment gaps.

Second stage (after the first stage has runnable evidence):

- WS4 fills in the event, row-lifecycle, call-lifecycle and node-hosted-row models;
- WS5 connects real traces to leases, host-gone, sync-cycles, AccessEvent and cross-language conformance;
- WS7 covers every timing promise with virtual clocks;
- WS8 closes the tracking table item by item.

Release gate: per design §10 and quality standard Q13.1, every applicable rule in the tracking table has a current source, implementation evidence, applicability matrix and status; the number of unverified core promises is zero; model, trace, conformance, property/fuzz, virtual-clock, platform and long-run results all exist.

## 3. Gaps and blockers

| ID | Gap/blocker | Impact | Handling |
| --- | --- | --- | --- |
| G1 | Old unknown-field behaviour: Rust ended the whole session, the other six ignored the field | The error scope was too large, or implementations disagreed | Decided B: reject, but fail only that call; all seven runtimes implement it (`bad-frame`, `hello` fails the handshake, `release`/`cancel` ignore) |
| G11 | Swift and Kotlin still silently ignore unknown fields; they have no call-level `bad-frame` refusal | The same frame with a new field behaves differently across implementations | Resolved: Swift and Kotlin implement it and run in CI; all seven runtimes agree |
| G2 | #634's 1368-rule inventory exists only in issue comments; the repository has no extractable source | The tracking table cannot claim completeness | WS3 builds the schema and a full `guarantees.md` mapping; rule extraction becomes a script or data file; until then the status is "partial" |
| G3 | `release_transient` removes a child outside `Shared.admission` | The marker set can miss a member | WS1 moves the detach under admission; keeping it lock-free requires proving `alive=false` is set first plus a concurrent test |
| G4 | No shuttle model for event close admission; a std atomic marker gives shuttle no switch point | Cannot explore marker-publication/`Entry::live` races | WS1 uses `crate::sync::AtomicBool` for the marker, or adds `switch_point()` at the read; the model must fail when the order is wrong |
| G5 | This machine has no Swift or Kotlin toolchain; Fray/Lincheck, TSan and simulators need their runners | WS6 cannot produce Swift/Kotlin evidence here | WS6 added four Node frame-pump cases; Bun/Python/Go are inventory-only; Swift/Kotlin are recorded as CI/device gaps, never as passing |
| G6 | `formal-trace.md` has only channel-level trace (direction, length, end), no frame payload | WS5 cannot reuse production trace for frame-level validation | Test fixtures decode frame bytes; production trace keeps sizes only, per design §1 |
| G7 | Existing TLA+ models cover only `cross-runtime-sync` and `remote-lease`; no event or row-lifecycle models | WS4 cannot claim complete coverage | Reconcile old models first, then add a minimal `specs/event-close/`; event models follow #613 in batches |
| G8 | 0.22's release scope grows from one performance change to the whole four-layer programme | The release date no longer follows one performance change | The roadmap moves to staged gates; WS8 judges only by the tracking table, not by "tests ran" |
| G9 | In Node/Bun, if a business frame was admitted but not run when the session closes, `drain()` can never settle | A runtime process keeps a hung promise, diverging from Python | WS6 reproduced it; fix by `#finish()` on every discarded task in `close()` or by settling `#draining`, with a regression test |
| G10 | `formal-trace.md` §1/§3.0 still treats the `objects` capability as a non-guarantee for passing objects; the new session specification says protocol 4 has no `objects` capability and all six implementations accept objects | Two current documents give different rules for the same behaviour; the tracker cannot decide a status | WS2 rechecks the code; after confirmation, change `formal-trace.md`, `guarantees.md` and the tracker; do not keep both statements |
| G12 | An asynchronous call has no protocol signal for "will await the result later"; 854bef5d can only record a conservative edge, which refuses a call that would finish, while removing it misses a real wait | An async call on a marked chain can be falsely refused, or a real cycle can be missed | Issue #1098 filed; keep the conservative behaviour until the protocol carries a signal; `async-policy-b` is recorded as a known false alarm |
| G13 | A queued call cancelled before it starts never gets a reply, so its `Calls.cancelled` entry lives until the session ends | A long session cancelling many queued calls grows memory without bound; clearing it blindly turns a legitimate late reply into an unknown reply and ends the session | Issue #1099 filed; wait for the cancel-ack/late-reply rule before changing code |
| G14 | The model keys peer rows by `ctx.instance()`, while the code behaves like `plugin#n` per load | The same instance across restarts/two sessions is not the same identity in model and code, so coverage cannot be counted directly | Issue #1100 filed; settle the #625/#640 row identity before aligning code and model |
| G15 | The peer-row `unload` of an unknown key only answers and records no tombstone; if it arrives before `load`, the later load may leave an orphan | `specs/peer-rows` models it; the current code may leave an orphan | Issue #1101 filed; add a deterministic implementation test, then a tombstone or a host-side order guarantee |
| G16 | The Kotlin heartbeat starts at `FrameConnection` construction/accept; a slow handshake can be closed before hello | `lease-mut-heartbeat-from-accept` in `specs/remote-lease` locks the current behaviour | Issue #1102 filed; confirm with a controllable clock, then start after hello or add a separate handshake deadline |
| G17 | With one host worker, the host's own synchronous call occupies it and service calls queue; policy B cannot see the host thread | `policy-b-workers1` in `specs/cross-runtime-sync` expects a deadlock, but the premise needs implementation checking | Issue #1103 filed; confirm the worker count and queue, then fix or write an explicit non-guarantee |

## 4. File ownership (to avoid conflicts in parallel work)

| Workstream | Owned/primary files |
| --- | --- |
| WS1 | `crates/rutis/src/fiber.rs`, `crates/rutis/src/ctx.rs`, `crates/rutis/src/bus/table.rs`, `crates/rutis/src/fiber/shuttle_tests.rs`, `crates/rutis-loader/tests/close_cpu.rs`, new close-admission tests |
| WS2 | `docs/protocol-session.md` and `.en.md`, `docs/design-protocol-plugins-2026-09-25.*`, `docs/design-protocol-plugin-mount.*` |
| WS3 | `docs/formal-trace/**`, the link column in `docs/guarantees.md`, the statistics script |
| WS4 | `specs/**` |
| WS5 | `crates/rutis-bridge/tests/trace_validation.rs`, trace-reading helpers under `crates/rutis-bridge/src/channel/` (no protocol change) |
| WS6 | tests and scripts under `node/`, `bun/`, `python/`, `go/` |
| WS7 | new property-test files, `fuzz/fuzz_targets/`, virtual-clock test files |

Conflict rule: `Cargo.toml` in a shared crate, `session/protocol.rs` and `bus.rs` are edited by WS1 or the main thread. Another workstream that needs them reports first instead of editing.

## 5. Log

- 2026-11-10: three suspected implementation bugs filed: #1101 (peer-row unload before load leaves an orphan), #1102 (Kotlin heartbeat from accept), #1103 (host single-worker blind spot in the wait graph); behaviour changes wait for implementation tests.
- 2026-11-10: six model directories expanded to the design scope: events 33 configurations, cross-runtime-sync 44, remote-lease 32, row-lifecycle 36, call-lifecycle 35, peer-rows 28; every new configuration was run and passes, and `specs/README` plus the plan status are updated; implementation fixes and validation close-out remain.
- 2026-11-10: three design gaps filed: #1098 (async "will await" signal), #1099 (cancel-table cleanup and cancel reply), #1100 (peer-row key identity); the current PR only fixes implementation bugs that do not change those semantics.- 2026-11-09: the old-offer reinsertion in `events::link::accept` is fixed (a `gone` boundary and a recheck under the insertion lock, with two deterministic regression tests); the marked-chain asynchronous-call finding in `cross-runtime-sync` cannot be distinguished on the frame from "fire and forget" versus "awaited later", so the code change was reverted pending a protocol signal.
- 2026-11-09: all nine TLA+ areas landed: four `specs/events/` modules with 21 configurations, `specs/peer-rows/` 7, `row-lifecycle` 22, `call-lifecycle` 16, `remote-lease` 21, and new `cross-runtime-sync` configurations; the models found a bail-dispatch blind spot, local abandonment without cancel, a marked-chain async false edge, and an old-generation reinsertion in link accept.
- 2026-11-09: third batch: shuttle grew to nine models (root dispose/shutdown, dependency withdrawal); `specs/call-lifecycle` has seven configurations ending as expected; the trace validator has 23 cases; cargo-fuzz ran session_frame, websocket_upgrade, error_graph and local_lines with no crash.
- 2026-11-09: second code batch landed: shuttle grew to seven models (effect registration/dispose, in-flight unload, concurrent subtree shutdown); `specs/row-lifecycle` has nine configurations ending as expected; the trace validator has 16 cases; five value/frame/bad-frame properties; Node/Bun/Python/Go implement call-level `bad-frame` for unknown fields; the 1368 protocol rules are imported into the tracker (1817 rows) and the rule-tracker check is in CI.
- 2026-11-09: unknown fields were decided as "reject, but fail only that call" (`hello` fails the handshake, `release`/`cancel` ignore them). Rust implements it and returns `bad-frame`; the other six still ignore them, tracked as G11.
- 2026-11-09: 0.23 merged into 0.22; this plan created; WS1, WS2, WS3, WS4, WS5, WS6 and WS7 started in parallel.
- 2026-11-09: WS1 implemented the marker publication, `Entry::live`, root/subtree paths and the `release_transient` lock; four deterministic race cases and one shuttle model pass; the model fails when the publication order is broken. Local release `close_cpu`: 1000 instances create in 17.55 s and close in 11.15 s, thresholds pass, and the issue's ≤30s creation target is met on this machine; `queued_emits_memory` passes at about 1938 B per queued emit.
- 2026-11-09: WS2 finished the first session protocol specification (82 `SESS-*` rules, both languages); the unknown-field conflict is still undecided.
- 2026-11-09: WS3 finished the first tracker version: 264 rows (210 registry + 54 hand-written), `node tools/formal-trace.mjs --check` reports zero problems; the 1368 protocol rules are not extracted yet.
- 2026-11-09: WS4 finished the first `specs/event-close/` model; all seven configurations end as `expected.txt` says (healthy 2,263,040 distinct/21s, tree 688,580/12s, drain 31,488/1s; four mutations violate their expected invariants).
- 2026-11-09: WS5 landed the channel/frame trace validator, 11 tests pass; WS7 landed the grant-rollback property tests, 6 tests pass and mutations are caught; WS6 landed four Node frame-pump cases and reproduced the Node/Bun `drain()` hang (G9).
- 2026-11-09: all seven workstreams have their first slice done or in verification; WS8 (close-out, CI wiring, full rule extraction) has not started.
