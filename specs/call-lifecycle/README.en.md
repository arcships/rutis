# Calls in a session: start, run, reply, cancel, references and a closed session (TLA+)

[中文](README.md)

TLC exhausts every interleaving of a call from its start to its settlement in one session: the call number, the request arriving, the method running, the reply, a cancel, a late reply, the grant and release of a reference, and the session closing. It corresponds to the call lifecycle part of issue [#613](https://github.com/arcships/rutis/issues/613), quality standard Q11.2; the design is [session protocol](../../docs/protocol-session.en.md) §5, §6, §7, the [session trace contract](../../docs/formal-trace.en.md) §3.4, §3.5, and the [formal verification design](../../docs/design-formal-verification-2026-11-08.en.md) §3.4. This is the second block of #613: the first is [`row-lifecycle`](../row-lifecycle/README.en.md) (the rows of a session). This block only answers "one call from its start to its settlement". It does not model forwarding across runtimes, the call chain `path`, the synchronous wait graph, the frame order of events, or rows hosted by a node.

## The design and the code it stands for

| Model part | Design | Implementation |
| --- | --- | --- |
| A call number grows strictly per session and per side, from 1; one number belongs to one call, and a number that already appeared is not a new call | Session protocol §5.1 (`SESS-32`, `SESS-33`, `SESS-34`) | `Calls::admit` and `Calls::received` in `crates/rutis-bridge/src/session/rpc.rs` |
| A call is settled exactly once: a reply takes its waiter out first | Session protocol §5.4 (`SESS-41`, `SESS-42`) | `rpc.rs:2494` (`reply_target`) |
| A cancel acts on one call: one still queued is dropped without a reply, one already running finishes and replies, and the session stays open | Session protocol §7 (`SESS-56`, `SESS-57`, `SESS-60`) | `rpc.rs:2693` (`Frame::Cancel`), `rpc.rs:2038` (`Connection::cancel`) |
| The late reply of a cancelled call is discarded and counted, not a protocol error | Session protocol §7 (`SESS-57`, third bullet), trace contract §3.4 | `cancelled`/`orphans` in `reply_target` (`rpc.rs:2494`); `go/rutis/internal/peer/peer_test.go:135-170` |
| A `cancel` for a call that does not exist is ignored (a cancel that overtook the reply it meant) | Session protocol §7 (`SESS-58`) | `rpc.rs:2693`; test `a_cancel_for_an_unknown_call_is_ignored` |
| A cancel does not revoke a grant already made; when the local proxy lets go, the receiver still sends `release` | Session protocol §7 (`SESS-59`), §6.1 (`SESS-47`) | `Import`'s `Drop` (`rpc.rs:992`) writes `Frame::Release` |
| A value in a reply is decoded before the call is looked at: a discarded late reply still releases the reference it carried | Trace contract §3.4; the rollback rule of `SESS-44` | "Decode first" in `Frame::Return` (`rpc.rs:2675`) |
| Closing a session fails the calls still waiting with the session error, clears the export and import tables, and drops what was not written | Session protocol §5.4, §6.1 (`SESS-42`, `SESS-51`), trace contract §4 "references are conserved" | `Connection::close` (`rpc.rs:1634`) |
| After one side closes, the other learns it from the end of the channel and ends too | Trace contract §3.0 (a session ends by disposing, before anything else) | the read loop and `closed()` in `rpc.rs` |

## The model

- **Participants**: the two endpoints of one session. A starts calls; B admits them, runs the method and answers. Each direction is FIFO and delayed arbitrarily long: `invoke`, `cancel` and `release` go A to B, `return` goes B to A. So a reply can still be on its way when a cancel is written, and a cancel can overtake the reply it meant to cancel.
- **A (the caller)**: `AStart` takes the next number and writes the `invoke`; `ADeliverLive` settles the call the reply's number names and takes the reference the reply carries; `ACancel` gives up a call (writes `cancel`, settles the call with the caller's error); `ADeliverLate` decodes the reply of a cancelled call, releases the reference it carries and discards it, the session going on; `ARelease` gives up what it holds and writes `release`; `AClose`/`ALearn` end this side.
- **B (the callee)**: `BReadInvoke` takes the call (still queued); `BRun` starts the method; `BReply` answers and writes out one grant of the reference; `BReadCancel` handles a cancel — a queued call is dropped without an answer, a running one is left alone (it answers later, which is where late replies come from), an answered one is not touched; `BReadRelease` gives up the grants named; `BClose`/`BLearn` end this side.
- **Close**: either side may close. The closing side fails the calls still waiting, clears the reference table it holds, and drops the frames still queued in both directions; the other side does the same when the channel ends (`ALearn`/`BLearn`). `closeCause` is the ghost that records which side closed first and why.

## Simplifications

- The scale is deliberately finite: one caller, one callee, two calls, at most one cancel, one reference (granted once per reply, so at most two units of grants). No reconnect, no second session, no multi-hop forwarding.
- The handshake is not modelled: the model starts with both sides greeted and the session usable. The version, the endpoint, the capabilities, the order of `hello` and the 10-second deadline are not modelled (`remote-lease` covers the takeover and the rest of the handshake); this model starts at the first call.
- The frames are `invoke`, `cancel` and `release` (A to B) and `return` (B to A). `throw` settles as `return` does and is not modelled separately; `call`, `get`, `await`, `events.*` and `rows.*` are not modelled.
- Only "grows, never reused" of a call number is modelled: the prefix, the canonical decimal form, the 2^53 bound and "not in the frame's `path`" are not. The receiver's refusal of a number that does not grow has no reachable path either — a correct sender never repeats a number, and a repeated number is exactly the `reuseId` mutation, so the check would be dead code here.
- Protocol errors such as a `release` of a reference this side does not hold, a release count above the grants, or `call`/`get`/`await` on a reference that does not exist are not modelled: a correct side never sends them, and every `release` in the model is within the grants (the session errors have their cases in trace contract §3.3, §3.5).
- The call chain `path`, `rebase`, the session tags, the synchronous wait and `SyncWaitCycle` are not modelled: that is the scope of forwarding across runtimes (the `cross-runtime-sync` model and its later blocks).
- Values, error graphs, `bad-value`, `bad-frame`, the 16 MiB limit and the outbound backlog are not modelled. Each fails one call and follows the same settlement path as `return`; they are covered by trace contract §3.5–§3.7 and the property tests.
- The duration of a method is not modelled: a call goes from `queued` to `running` to its reply, and the reply may be written at any later point, a cancel included. There is no timing and no promise about the order calls run in.
- After a close the queues in both directions are cleared and neither side reads: this is a simplification. In the implementation a frame already written to the channel may still be read before the channel ends; that interleaving is not in this model, and the conclusion below does not claim it.
- The cause of a close is not a protocol frame: each side ends its own session and the other learns it from the end of the channel. `closeCause` only tells the wrong design ("a cancel closes the session") apart.
- Memory order, real scheduling and frame encoding are not modelled; every action is atomic.

## Invariants and liveness

| Name | Content |
| --- | --- |
| `CancelKeepsSession` | A cancel never ends the session: a close comes from the caller, the callee or the end of the channel (`closeCause ≠ "cancel"`) |
| `SettlesAtMostOnce` | A call is settled at most once: a reply, a cancel and the session ending never settle the same call twice |
| `LateReplyDropped` | A late reply is discarded: a cancelled call keeps the outcome the cancel gave it and the reply does not settle it again; each call has at most one reply to discard |
| `OwnReply` | The reply that settles a call is the reply of that call |
| `NoIdReuse` | A call number belongs to one call: no new call takes it again |
| `NoOverRelease` | The caller gives back only what it was granted: a grant is released, dropped or cleared once, never twice |
| `ClosedClearsTables` | A closed side holds no reference: A's imports are empty, B's exports are empty |
| `FailUnfinishedOnClose` | A close leaves no call unfinished on either side: no call of A still waits, and none of B is queued or running |
| `WaitingSettles` | Liveness (checked only in the configuration without a close): a call A waits for is answered or cancelled, so the callee answers what it took |

## Results

TLC 2.19 (tla2tools v1.7.4), Java 21.0.12 (Temurin), 32 cores, `-workers auto`. `ok` means the exhaustive search completed and every listed property holds; a violated configuration stops at the first counterexample, so its state count depends on the search order and may differ from run to run. The whole directory takes about 5 seconds.

| Configuration | Content | Result | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `call-healthy` | Two calls, at most one cancel, both sides may close; every safety invariant | ok | 1,155 | 1 s |
| `call-progress` | No close; only the liveness property (`WaitingSettles`) | ok | 409 | 1 s |
| `call-mut-cancel-closes` | A cancel closes the session | violated `CancelKeepsSession` | 11 (stops at the counterexample) | 1 s |
| `call-mut-late-reply-twice` | A late reply is taken for a live one and settles the call again | violated `SettlesAtMostOnce` | 530 (stops at the counterexample) | 1 s |
| `call-mut-close-leaks` | Closing keeps the export table | violated `ClosedClearsTables` | 352 (stops at the counterexample) | 0–1 s |
| `call-mut-reuse-id` | A new call may take a call number again | violated `NoIdReuse` | 11 (stops at the counterexample) | 0–1 s |
| `call-mut-reuse-id-settles` | The second half of the same mutation: the reused number lets a reply settle another call | violated `OwnReply` | 570 (stops at the counterexample) | 0–1 s |

## Mutations and counterexamples

Every checked property has a deliberate error next to it, and TLC must find the counterexample (`violated:<name>` in `expected.txt`):

| Configuration | Mutation | Violated | Counterexample |
| --- | --- | --- | --- |
| `call-mut-cancel-closes` | `ACancel` closes A's session right there (§7's "a cancel acts on one call" replaced by "a cancel ends the session") | `CancelKeepsSession` | A starts a call and cancels it: `acall = "settled"`, `aclosed = TRUE`, `closeCause = "cancel"` — one cancel ended the whole session |
| `call-mut-late-reply-twice` | `ADeliverLate` takes the reply of a cancelled call for a result still waited for | `SettlesAtMostOnce` | A cancels call 1 (B was already running it) and B answers; `settles["c1"] = 2`: the cancel gave one outcome and the reply a second |
| `call-mut-close-leaks` | `CloseB` keeps `bgrants` (§6.1's "a session close clears the exports" removed) | `ClosedClearsTables` | A cancels call 1, B answers as usual and grants the reference once; A closes, B learns it and closes, and `bgrants = 1` stays in a closed session |
| `call-mut-reuse-id` | `AStart` does not advance `anext`: a new call takes a number an older call already took | `NoIdReuse` | Call 1 takes number 1, then call 2 takes number 1 as well: `aid = [c1 \|-> 1, c2 \|-> 1]` |
| `call-mut-reuse-id-settles` | The same mutation, looking at what follows | `OwnReply` | Both calls are numbered 1; call 2 is cancelled and B answers call 2 (number 1); A settles call 1 with that answer: `settledBy["c1"] = "c2"` |

The two configurations of the reused number are two conclusions of one mutation: first the rule itself is broken (one number for two calls), then the wrong settlement that follows (a reply landing on another call). They are separate so that each counterexample carries one conclusion (as `event-close` and `row-lifecycle` do).

## Conclusion

- With the rules of protocol §5–§7 — a number only grows and is never reused, a call settles once, a cancel drops the call and not the session, a late reply is discarded, a close fails the unfinished calls and clears the reference tables — the following holds in the exhausted range: no call is settled twice; a cancel never closes the session; the late reply of a cancelled call does not change its outcome and the session handles the next call; a close on one side ends the other side too, and neither keeps an unfinished call or a reference; every grant made before the close is given back once.
- The five mutation configurations pin five premises: a cancel must not end the session (`CancelKeepsSession`); a late reply must be discarded (`SettlesAtMostOnce`); a close must clear the export table (`ClosedClearsTables`); a new call must not take a number again (`NoIdReuse`), and the direct consequence of reusing it is a reply settling the wrong call (`OwnReply`). None of the properties is empty: `call-progress` demands that the callee really answers when no close is possible, and taking `BReply` out of `Next` makes it fail at once.
- What the model does not prove: the handshake and authentication; forwarding across runtimes, relays, call chains and the synchronous wait graph; the frame order of events and rows (`row-lifecycle` and the later blocks); value encoding, error graphs, limits and the outbound backlog; multi-hop cancel propagation (the relay part of `SESS-59`); frames still on the channel after a close; real scheduling, memory order and timing. This directory is only the first block of the #613 call lifecycle and is not evidence for the whole call protocol.

## How to run it

Java 11 or newer and `tla2tools.jar` are needed ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh                    # every configuration
TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh call-healthy       # one configuration
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh      # limit TLC's threads
```

`run.sh` compares each configuration's outcome with `expected.txt` and exits 0 when they all match; the logs and counterexamples go to `TLC_OUT` (a temporary directory by default). The `specs` job of CI carries `call-lifecycle` in its matrix (see [CI](../../docs/ci.en.md) §4).

## Maintenance

When session protocol §5, §6 or §7 changes, or when `Calls::admit`, `reply_target`, the handling of `Frame::Cancel`/`Frame::Release`, `Connection::cancel` or `Connection::close` in `crates/rutis-bridge/src/session/rpc.rs` changes, or when a runtime's cancel and reference-release behaviour (for example `node/rutis-runtime/src/session.mjs`) changes, change this model and `expected.txt` with them (standard Q11.2.3).
