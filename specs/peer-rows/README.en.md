# Node-hosted rows: each session's table, the instance key and a late generation's notification (TLA+)

[中文](README.md)

TLC exhausts the interleavings of a host running rows for a peer node: the old control session breaking, a new one taking over, each session's own table of what it loaded, a row's instance key that outlives a restart, the late `rows.ended`/`rows.failed` notices, and the import and withdrawal of a row's service. It corresponds to the `specs/peer-rows/` item of the "merged into #640" section of issue [#613](https://github.com/arcships/rutis/issues/613); quality standard Q11.2; the design is [remote plugins](../../docs/design-remote-plugins-2026-10-03.en.md) §4.4, §5.1–§5.3, [remote sessions](../../docs/design-remote-session-2026-10-28.en.md) §4.2, §4.3, and the [session trace contract](../../docs/formal-trace.en.md) §3.0.

This is the second block of the row lifecycle: [`row-lifecycle`](../row-lifecycle/README.en.md) models rows on the runtime's side (loading, unloading and reconnecting within a runtime session); this block models hosting rows for a peer: each session's own table, an instance key that does not change across restarts, the import and withdrawal of a service, and what a late notice of an old generation does to the new one.

`peer:` rows and `PeerRow` are gone from the current code: the 0.19 remote-connection design removes them (remote sessions §8), and the equivalent path is `<connection row>:<plugin>` through `run`; a run row's key is new on every load (`plugin#n`, `run.rs:168`), so two generations never share a key. This model writes the old rule of issue #613's doubt — the key is `ctx.instance()`, the same across restarts — exactly to answer what safety rests on when there is no new key per load. The conclusion below is therefore the design rule, not evidence about today's run rows, which have their own evidence (two cases in `session_rows.rs`); neither stands for the other.

## The design and the code it stands for

| Model part | Design | Implementation |
| --- | --- | --- |
| Each control session has its own table of what it loaded; one key is loaded at most once in a session | Remote plugins §4.4 (a runtime instance has one active session) | `crates/rutis-bridge/src/session/plugin/rows.rs:61` (`Server.loaded`), `:110-112` (a key already loaded is refused) |
| A row is loaded by its instance key, which is the same after a restart; the generation is the model's name for one load | Remote plugins §5.3; the doubt of issue #613 | the old `PeerRow` used `ctx.instance()`; a run row uses `plugin#n` (`run.rs:168`), and `rows.rs` keys by it |
| `rows.ended` is sent only for a generation the far end has not unloaded, and not once the session is cancelled | Remote sessions §4.3 | `rows.rs:138-175` (`follow`: `loaded.remove(&key)`, `ended && !cancelled`) |
| `rows.failed` goes out once per generation; the host keeps the row and the peer unloads it and loads a new one | Remote plugins §4.2; #688 | `crates/rutis-bridge/src/runtime/process.rs:543-568`, `run.rs:171-175` |
| `rows.unload` is idempotent: a row already gone is only answered | the row contract `rows.v2` | `rows.rs:197-209` ("Already gone: unloading is idempotent") |
| A replacement closes the old session, stops reading it, cleans its rows and services, then answers the new `hello` | Remote plugins §4.4; remote sessions §4.2 | the listener's order in every runtime; each session has its own `Server` in `rows.rs`, so the new session's table is new |
| A row stops with its session and runs again on the next one | Remote sessions §4.3, §7 | `RowHandle`/`RunRow` in `run.rs`; `crates/rutis-loader/tests/session_rows.rs::rows_run_on_the_session_of_their_connection_row`, `::a_far_session_unloading_first_leaves_the_row_waiting` |
| A row's service import follows the row: unloading, ending or losing the session withdraws it | Remote plugins §2.6 decision 1, §5.3 | `crates/rutis-bridge/src/session/plugin/services.rs:192-378` (a session's `Imports`); `process.rs:1306-1314` (`forget_exports`), `:1356-1366` (`unload_row` then `forget_exports`/`imports.forget_row`) |
| A late instruction of the old session does not act on the new lease | Remote plugins §4.4 | each session has its own handler; "Simplifications" says how the model writes this as matching a generation |

## The model

- **Participants**: one host (it loads the peer's rows: a table per session, the ending and failure notices, the service imports), one peer node with two rows that it loads again on the next session, and the peer's two control sessions: session 1 (old) and session 2 (the replacement). Two rows, `a` and `b`, one instance key each, the key the same on every load; each row has at most two generations (one per session).
- **Sessions**: `1..MaxSessions` (2). The peer opens them in order; each direction of a session is FIFO and delayed arbitrarily long. After a break the peer reads no more of it (`PLoss`) and neither does the host (`CanRead`); a `rows.ended`/`rows.failed` already written but not read may arrive late (`late`).
- **The peer** (`PDial`, `PGreeted`, `PLoad`, `PLoaded`, `PUnload`, `PUnloaded`, `PEnded`, `PFailed`, `PLoss`, `PLateEnded`, `PLateFailed`):
  - `PLoad(r)` loads a row on the session: the key is the instance, the generation goes up by one; `prow` is `idle | loading | loaded | unloading | gone`.
  - `PEnded`/`PFailed` act only on a notice of the row's current generation: a late notice of an earlier one neither ends nor restarts the new generation.
  - `PLoss(s)`: the session broke; the rows on it stop and wait (`idle`), what it imported goes with it, and the ending or failure notices it had not delivered become late notices.
  - `PLateEnded`/`PLateFailed`: a late notice arrives. Under the protocol it belongs to no current generation and changes nothing; the `lateEnding` mutation lets it act by row key.
- **The host** (`HTakeUp`, `HAnswer`, `HEndSession`, `HReadLoad`, `HReadUnload`, `HEndSelf`, `HFail`, `HSweep`, `Break`):
  - `HTakeUp(s)` takes up a new connection's `hello`, closes the old session (reads it no more) and starts cleaning it.
  - `HAnswer(s)` answers only once every earlier session's rows are cleaned (`Swept`); this is the rule the model is about.
  - `HReadLoad` puts the row into **that session's own** table (a key already loaded is refused; that is unreachable here — see "Simplifications"); `HReadUnload` is idempotent: a row still there is disposed once and its service withdrawn, one already gone is only answered.
  - `HEndSelf` is the plugin ending on its own (a Cordis plugin disposing its fiber): it leaves the table and `rows.ended` goes out; `HFail` sends `rows.failed` once per generation and keeps the row loaded.
  - `HEndSession` is the host seeing the break (or the takeover closing the old session): the session ends and `HSweep` disposes its rows one by one; ending or failure notices already written but not read become late.
  - `Break(s)` is the environment: a connection breaks.

## Simplifications

- The scale is deliberately finite: one host, one peer, two sessions, two rows, at most two generations per row. Reconnect counts, backoff and timing are not modelled: the environment is only "the connection breaks at some point", more general than real timing.
- The two sessions are sequential: the peer opens the new one only after the old one broke. Concurrent handshakes with the same id, the old session alive during a handshake, and an access point's takeover order are not modelled (`remote-lease` and `access-*` cover them); what stays is the one interleaving that matters here — the host answers the new `hello` while the old session's rows may still be there.
- The generation is the model's name for one load: the frames carry it, a real frame carries only the key. What tells the peer two generations apart in the code is which registration (which session's `Process`) delivered the notice; the model writes that as a generation, and `lateEnding` is exactly "match the row key only".
- No `rows.ended` is sent once the session is cancelled (`!cancelled` in `rows.rs`): the row is treated as disconnected and waits for the next session. The model therefore ignores a late notice entirely (the correct `PLateEnded`/`PLateFailed` changes nothing); a late notice is what the environment may do, not an instruction the protocol allows.
- The service announcement and withdrawal are not frames with delays: the import is taken when the load is accepted and withdrawn when the row goes, each in one atomic step. The version rule (an older announcement does not restore a withdrawn service, `services.rs:239-245`) and multi-hop are not modelled.
- A load and an unload are atomic steps. A load cancelled midway and an unload arriving before its load, which leave an orphan plugin (the doubt moved from #625), are not modelled; `NoOrphan` is not in this model.
- The handshake is only "`hello` → answer": the 10-second deadline, authentication, capabilities, `link.offers` and its epoch, `plugins.describe`/`schema`, and the two-stage `PeerRows` gating are not modelled (the other half of #640).
- Events, proxies, `plugins.update`, lease upgrades, backoff and timing are not modelled; so are memory order, task scheduling and frame encoding; every action is atomic.
- A second load of the same key in one session (refused by `rows.rs`) is unreachable: the peer loads a row only while it is `idle`, once per session; the model writes that as "already loaded: ignore" and does not model the refusal's reply.

## Invariants and liveness

| Name | Content |
| --- | --- |
| `OneInstanceAtATime` | A row's instance runs in one session at a time: the new session loads it only after the old session's rows are cleaned |
| `OwnGenerationOnly` | The peer acts only on a `rows.ended`/`rows.failed` of the row's current generation: a late notice of an earlier one neither ends nor restarts the new one |
| `ServicesFollowRows` | A row's service is imported only while the generation providing it is loaded: unloading, ending on its own or losing the session withdraws it; no import outlives the row |
| `CleanupOnce` | A generation is disposed at most once: `rows.unload`, the row's own end and the session's sweep do not dispose it twice |
| `EndedRowsCleaned` | Liveness: once a session ended, the rows it loaded are cleaned (and their services withdrawn) |

## Results

TLC 2.19 (tla2tools v1.7.4), Java 21.0.12 (Temurin), 32 cores, `-workers auto`. `ok` means the exhaustive search completed and every listed property holds; a violated configuration stops at the first counterexample, so its state count depends on the search order and may differ from run to run. The whole directory takes about 10 seconds.

| Configuration | Content | Result | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `peer-healthy` | 2 rows, 2 sessions, breaks allowed; a row may end on its own; every safety invariant | ok | 36,083 | 1 s |
| `peer-failure` | 2 rows, 2 sessions, breaks allowed; a row's code may fail and the peer unloads it and loads a new one; every safety invariant | ok | 161,503 | 2 s |
| `peer-clean` | 2 rows, 2 sessions, breaks allowed; a row may end on its own; liveness only | ok | 36,083 | 1–4 s |
| `peer-mut-late-ending` | A late `rows.ended`/`rows.failed` of the old generation acts on the new one by row key | violates `OwnGenerationOnly` | 5,065–10,888 (stops at the counterexample) | 0–1 s |
| `peer-mut-two-sessions` | A replacement answers the new `hello` before the old session's rows are cleaned, and the new session loads the same instance | violates `OneInstanceAtATime` | 2,876–4,536 (stops at the counterexample) | 0–1 s |
| `peer-mut-keep-rows` | The host ends the session but keeps its rows | violates liveness | 36,083 (stops at the counterexample) | 1–3 s |
| `peer-mut-service-residue` | A row's service is not withdrawn when the row goes or its session ends | violates `ServicesFollowRows` | 171–238 (stops at the counterexample) | 0–1 s |

## Mutations and counterexamples

Every checked property has a deliberate bug that TLC must find (`violated:<name>` in `expected.txt`; `violated:liveness` for the temporal one):

| Configuration | Mutation | Violated | Counterexample |
| --- | --- | --- | --- |
| `peer-mut-late-ending` | `PLateEnded`/`PLateFailed` ignore the generation and end or restart the row by key | `OwnGenerationOnly` | On session 1, `a` is generation 1; the plugin ends on its own and `rows.ended(a,1)` is written to session 1 but never read; the connection breaks, the peer opens session 2 and loads `a` as generation 2; the late ending arrives and ends the new generation (`pEndedFrom = 1`, `pkey = 2`) |
| `peer-mut-two-sessions` | `HAnswer` does not require earlier sessions to be `Swept` (the "clean first, then answer" of §4.4 is removed) | `OneInstanceAtATime` | Session 1 has `a` as generation 1; the connection breaks; the peer opens session 2; the host closes the old session but answers without waiting for the cleanup; the peer loads `a` as generation 2 and it is accepted on session 2: `loaded[1][a] = 1` and `loaded[2][a] = 2` at once |
| `peer-mut-keep-rows` | `HSweep` is off (an ended session's rows are never cleaned) | liveness (`EndedRowsCleaned`) | Session 1 loaded a row and broke; the host ends the session (`ended[1]`), but the row stays in the table forever and `Swept(1)` never holds |
| `peer-mut-service-residue` | The service is not withdrawn when the row is unloaded, ends on its own, or its session is cleaned | `ServicesFollowRows` | Session 1 loads `a`, whose service is imported as `<<1,1>>`; the plugin ends on its own and the host removes the row but keeps the import: the row is gone, the service is not |

The `peer-mut-two-sessions` mutation also breaks "clean before answering"; the configurations are split so that each counterexample carries one conclusion (as in `row-lifecycle` and `event-close`). `CleanupOnce` has no mutation of its own: all three disposal paths first check that the generation is still in the table, so a second disposal is unreachable; any path forgetting the check fails it.

## Conclusion

- Under the rules of remote plugins §4.4 — a replacement stops reading the old session and answers the new `hello` only after the old session's rows are cleaned — the exhaustive search holds: one instance never runs in two sessions at once; the new session loads a row only after the old session's rows are cleaned; a row stops with its session and runs on the next one.
- The peer acts on `rows.ended`/`rows.failed` only by the load the notice belongs to: a late ending of an earlier generation does not end the new one, and a late failure does not restart it. With a key that is the same across restarts this must rest on the notice's registration; matching the row key alone is exactly the `lateEnding` counterexample.
- A row's service import follows the row: unloading, ending on its own and losing the session all withdraw it; `serviceResidue` shows "the import outlives its row" is caught.
- A session must really clean its rows: `keepRows` fails the liveness property.
- What the model does not prove: a hosted row cancelled while loading (the orphan doubt from #625), concurrent handshakes with the same id, `PeerRows`' resolution, gating, offline state and `link.offers` withdrawal, `plugins.describe`/`schema`, the service announcement's versions and multi-hop, events, backoff and timing, the differences of each language's listener, memory order and real scheduling. Those are the rest of #640.

## Running it

Needs Java 11+ and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/peer-rows/run.sh                      # every configuration
TLA2TOOLS=/tmp/tla2tools.jar specs/peer-rows/run.sh peer-mut-late-ending # one configuration
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/peer-rows/run.sh       # bound TLC's workers
```

`run.sh` compares each configuration's outcome with `expected.txt` and exits 0 when all match; the logs and counterexamples go to `TLC_OUT` (a temporary directory by default). CI's `specs` job does not list `peer-rows` yet (see [CI](../../docs/ci.en.md) §4; part of #640's close-out).

## Maintenance

When remote plugins §4.4, §5.1–§5.3 or remote sessions §4.2, §4.3 change, or when the per-session table of `crates/rutis-bridge/src/session/plugin/rows.rs`, the import table of `services.rs`, `rows.ended`/`rows.failed`/`unload_row` in `runtime/process.rs`, or the row key of `session/plugin/run.rs` change, change this model and `expected.txt` with them (standard Q11.2.3).
