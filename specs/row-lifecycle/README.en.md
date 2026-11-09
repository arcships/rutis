# Rows in a session: breaks, reconnecting with the same id, cleanup (TLA+)

[中文](README.md)

TLC exhaustively checks the lifecycle of a runtime session's rows, from load to cleanup, and their interleavings with a break and a reconnect of the same id. Issue [#613](https://github.com/arcships/rutis/issues/613), its row-lifecycle part; quality standard Q11.2; design: [remote sessions](../../docs/design-remote-session-2026-10-28.md) §4.2, §4.3, [remote plugins](../../docs/design-remote-plugins-2026-10-03.md) §4.4, [session trace contract](../../docs/formal-trace.md) §3.0. This is the first block of the row-lifecycle model: it answers only "the rows of one session, a break, a reconnect, and the runtime's replacement". It does not model generations, refusals, service announcements or host service leases (#625's scope), nor event registration and the frame order of `rows.ended` (#613's event table), the call lifecycle, or rows hosted by a node.

## The design and code it corresponds to

| Model part | Design | Implementation |
| --- | --- | --- |
| The host has one session at a time and redials only after losing it; rows load through the session and wait for the next one when it ends | remote sessions §4.2; remote plugins §4.4 | `crates/rutis-host/src/connect/dial.rs` `Dialing::run` (redials after `installed.unloaded().await`); `crates/rutis-loader/src/runtime/sessions.rs` `SessionRowsPlugin` (rows wait for the session, stop when it ends) |
| The host keys a load by its generation: every load gets a new key, and a late reply of an earlier generation does not act on a newer one | quality standard §11.2; #625's generation rule (only the key is used here) | `crates/rutis-loader/src/runtime.rs:719-727` (`ctx.instance()` + `GENERATION`); `on_row_ended` / `on_row_failed` in `crates/rutis-bridge/src/runtime/process.rs` (keyed, removed by `unload_row`) |
| `rows.ended`: the row ends by itself; the host ends it and unloads it (it is not loaded again) | remote sessions §4.3; `loader/self-dispose` | `crates/rutis-bridge/src/runtime/process.rs:531-541`; `node/rutis-runtime/src/runner.mjs:379-385` (`endRow`: `unloadRow` first, then `rows.ended`) |
| `rows.failed`: the row's code failed; the host restarts it (unload the old generation, load a new one), the runtime keeps the row loaded | remote sessions §4.3; #688 | `crates/rutis-bridge/src/runtime/process.rs:543-568`; `node/rutis-runtime/src/runner.mjs:1120-1135` |
| `rows.unload` is idempotent: a row already gone is only answered | the `rows.v2` row contract | `crates/rutis-bridge/src/session/plugin/rows.rs:197-207`; `node/rutis-runtime/src/runner.mjs:743-746` |
| Replacement: close the old session, stop reading its frames, clean all its rows, then answer the new hello | remote plugins §4.4; remote sessions §4.2 | the runtimes' listener side (one order in Python `serve`, Go `listen.go`, Node `serve.mjs`); the model follows Node's row handling (`runner.mjs`) |
| A session ending (a break or a replacement) cleans all its rows, each once | remote plugins §4.4; `loader/remote-runtime-rows`, `loader/row-unload-leaves-nothing` | the runtimes' `serve`/`listen`; `Server` in `crates/rutis-bridge/src/session/plugin/rows.rs` (one loaded table per session) |

## The model

- **Participants**: one host (1–2 loader rows and a dial row) and one runtime (the listener and its row table). The host has one connection at a time and redials the same id after a break; the runtime serves one session at a time, a new connection replacing the old one.
- **Connections**: `1..MaxConns`, at most 2 reconnects. The host dials in order; each direction of a connection is FIFO and delayed arbitrarily long; a side that closed a connection reads nothing more on it, and frames sent to it are dropped.
- **Host** (`HDial`, `HGreeted`, `HLoad`, `HLoaded`, `HUnloaded`, `HEnded`, `HFailed`, `HLose`):
  - `HLoad(r)` sends `rows.load` with the key `<<session, row, generation>>`; a row loads at most `MaxLoads` times in a session (a restart uses one).
  - `HEnded` ends a row only for the key that is the row's current generation, and sends `rows.unload` as the real unload stack does (the runtime already disposed it and only answers); `HFailed` marks the current generation unloading and waits for the reply before a new generation may load.
  - `HLose` breaks the current connection: its rows return to `idle` (they stop and wait for the next session) and the host may redial; a row already `gone` (it ended itself) stays `gone`.
- **Runtime** (`TakeUp`, `Answer`, `EndSession`, `RLoad`, `RUnload`, `EndSelf`, `SendEnding`, `RFail`, `Sweep`, `MarkSwept`):
  - `TakeUp(s)` reads a new connection's `hello`, closes the old session (`rside` becomes `closed`, no more reads) and starts cleaning the old session's rows.
  - `Answer(s)` answers the new `hello` only once every earlier session is `Swept`, so two sessions' rows never coexist.
  - `EndSession(s)`: the runtime notices the host closed the session; `Sweep` cleans its rows one by one.
  - `RLoad` reads only from the current session while its connection is open (or, with the `readOld` mutation, from an ended session); `RUnload` is idempotent (a row still loaded is cleaned once, one already gone is only answered); `EndSelf` is the plugin ending itself: the row disappears and its `rows.ended` is still to be sent; `SendEnding` sends only while the session is up.
  - `RFail(k)` sends `rows.failed` at most once per generation; the row stays loaded until the host unloads it.
- **The replacement order** lives between `TakeUp` and `Answer`: close the old, stop reading, clean its rows (`Swept`), answer the new `hello`. `Answer`'s precondition is the rule this model checks.

## Simplifications

- The scale is deliberately finite: 1 host, 1 runtime, 1–2 rows, at most 3 connections (2 reconnects), at most 2 loads of a row per session. The real reconnect counts, backoff and the 30 s / 60 s timers are not modelled: the environment is only "a break at some time", which is more general than real timings.
- The handshake is one `hello` each way: version, capabilities, authentication and the 10 s deadline are not modelled (`remote-lease` covers the rest of the handshake and the replacement).
- The only host→runtime frames are `rows.load` and `rows.unload`; `rows.update`, `rows.schema`/`mount`, services, leases and events are not modelled. The runtime→host frames are `hello`, `loaded`, `unloaded`, `ended` and `failed`.
- A host row is one fiber: `idle | loading | loaded | unloading | gone`, keyed `<<session, row, generation>>` (the implementation keys `instance.generation`, a fresh value per load); the loader's two-stage resolution, the `SessionRows` dependency and the restart policy are not modelled. "A row ending itself disables it, never to load again" follows `loader/self-dispose`.
- The runtime disposing a row is one atomic action (`Sweep`, `RUnload` and `EndSelf` all mean "the row disappears, the cleanup count grows by one"); the per-language unload stacks, shutdown deadlines and `events.off` details are not modelled (#625's scope).
- Unread frames of an old session stay in the model (the design drops them when the connection closes); the design side is `CanRead`, which never reads a non-current session, and only the `readOld` mutation does. That gives the mutation something to act on while the correct protocol never reads them.
- The host stops reading a session once it loses it (`dial.rs` redials only after the old session unloaded), so a late `rows.ended` of an old session never reaches the host after the new session starts; the cross-session isolation is therefore the runtime's rule (don't read the old session, clean before answering). The same rule on the host side shows up as the generation race within one session: a late `rows.ended`/`rows.failed` of an earlier generation does not end the current one (`OwnFramesOnly`), which is the defect a88fb65b fixed.
- Memory order, task scheduling, real timers and the frame encoding are not modelled; every action is atomic.

## Invariants and liveness

| Name | Meaning |
| --- | --- |
| `OneRowPerName` | At most one generation of a row name is loaded at the runtime: a new session loads only after the old session's rows are cleaned; a restart waits for the old generation's unload reply |
| `NewSessionOnlyAfterSweep` | When a session's `hello` is answered, every earlier session's rows are cleaned: the old session is cleaned before the new `hello` is accepted |
| `NoRowsAfterSweep` | Once a session was swept, no frame of it loads a row again: the runtime does not read the old session's commands |
| `OwnFramesOnly` | The host acts on a `rows.ended`/`rows.failed` only for the row's current generation: a late frame of an earlier generation neither ends nor restarts the current one |
| `CleanupOnce` | A row generation is cleaned at most once: the session sweep, `rows.unload` and the row ending itself never dispose the same row twice |
| `EndedLeadsToSwept` | Liveness: a session that ended ends with its rows cleaned (after a break, no row is left in the runtime) |

## Results

TLC 2.19 (tla2tools v1.7.4), Java 21.0.12 (Temurin), 32 cores, `-workers auto`. "ok" means the exhaustive search completed and every listed property held; a violated configuration stops at the first counterexample, and its state count depends on the search order (it can differ between runs). The whole directory takes about 9 seconds.

| Configuration | Content | Result | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `row-healthy` | 2 rows, 1 connection; rows may fail and end themselves; all safety invariants | ok | 73,768 | 1–2 s |
| `row-reconnect` | 2 rows, 3 connections (2 reconnects), a break may happen; all safety invariants | ok | 191,830 | 2 s |
| `row-cleanup` | 2 rows, 1 connection, a break may happen; liveness only | ok | 110 | 1 s |
| `row-mut-accept-before-clean` | the replacement answers the new `hello` before the old session's rows are cleaned | violated `NewSessionOnlyAfterSweep` | 469 (stops at the counterexample) | 1 s |
| `row-mut-accept-before-clean-loads` | the same, then the new session loads a row of the same name | violated `OneRowPerName` | 705 (stops at the counterexample) | 1 s |
| `row-mut-read-old` | the runtime keeps reading the old session after the replacement | violated `NoRowsAfterSweep` | 449 (stops at the counterexample) | 0 s |
| `row-mut-keyed-by-row` | the host matches `rows.ended`/`rows.failed` by row name, not by key | violated `OwnFramesOnly` | 144 (stops at the counterexample) | 1 s |
| `row-mut-reclean` | an unload of a row already gone runs the cleanup again | violated `CleanupOnce` | 53 (stops at the counterexample) | 1 s |
| `row-mut-keep-rows` | the runtime closes a lost session but keeps its rows | violated liveness | 29 (stops at the counterexample) | 0 s |

## Mutations and counterexamples

Every checked property has a deliberately introduced error, and TLC must find a counterexample (`violated:<name>` in `expected.txt`; `violated:liveness` for liveness):

| Configuration | Mutation | Violated | Counterexample |
| --- | --- | --- | --- |
| `row-mut-accept-before-clean` | `Answer` does not require the earlier sessions to be `Swept` (§4.4's "clean, then answer" removed) | `NewSessionOnlyAfterSweep` | Session 1 loaded row `a`, the connection broke, the host dials session 2; `TakeUp(2)` closes session 1 and `Answer(2)` answers the `hello` at once, while `Swept(1)` is still false |
| `row-mut-accept-before-clean-loads` | the same | `OneRowPerName` | the host then loads row `a` on session 2 (new key) while session 1's `a` (old key) still runs: two generations of one name |
| `row-mut-read-old` | `CanRead` allows reading an ended session's frames | `NoRowsAfterSweep` | the host sent `rows.load` on session 1 but the runtime never read it; after a break it dials session 2; session 1 is cleaned and `MarkSwept`, then the runtime reads the late `rows.load` and starts a row for the swept session |
| `row-mut-keyed-by-row` | `HEnded`/`HFailed` match by row name, ignoring the key's generation | `OwnFramesOnly` | row `a` fails and is unloaded; the plugin ends itself before the unload, so its `rows.ended` goes out after the unload reply; the host has already loaded a new generation and ends it on the old ending (`hEndedFrom` is the old key, `hkey` the new one) — the defect a88fb65b fixed |
| `row-mut-reclean` | a row already gone still runs its cleanup | `CleanupOnce` | the row fails; the plugin ends itself (one cleanup), and the following `rows.unload` finds it gone but the mutation cleans it again: `cleanCount = 2` |
| `row-mut-keep-rows` | `Sweep` is disabled (a break keeps its rows) | liveness (`EndedLeadsToSwept`) | session 1 loaded a row, the connection broke, the runtime never cleans it: `Swept(1)` never holds |

`row-mut-accept-before-clean` and `row-mut-read-old` would also fail `OneRowPerName` or `NewSessionOnlyAfterSweep` respectively; the configurations are split so each counterexample stands for one conclusion (as in `event-close`).

## Conclusion

- Under remote plugins §4.4's two rules — stop reading the old session when replacing, and answer the new `hello` only after its rows are cleaned — everything holds in the exhausted range: the runtime never has two generations of one row name; the new session's rows load only after the old session is cleaned; the old session's leftover frames never start a row after the sweep; a break's rows are cleaned. The host acts on a row's ending and failure by its key (generation), so a late frame of an earlier generation changes nothing of the current one; and a row is cleaned once.
- The five mutations pin the five premises: the answer must follow the cleanup (`NewSessionOnlyAfterSweep`, `OneRowPerName`); the runtime must stop reading the old session (`NoRowsAfterSweep`); the host must key endings and failures, not name them (`OwnFramesOnly`); the unload cleanup must be idempotent (`CleanupOnce`); a break must really clean (liveness). None of the properties is empty.
- Not proven: memory order and real scheduling; the host loader's resolution, restarts, generations, refusals, service announcements and host service leases (#625's later `specs/row-lifecycle/` model); event registration and the frame order of `rows.ended` with `events.*` (#613's event table); the call lifecycle; per-language unload stacks and shutdown deadlines; timers and reconnect backoff; the frame encoding. This directory is only the first block of #613's row lifecycle, not evidence for the whole row protocol.

## How to run

Java 11 or later and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`) are needed. Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh                    # every configuration
TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh row-reconnect      # one configuration
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh      # limit TLC's threads
```

`run.sh` compares each configuration's outcome with `expected.txt` and exits 0 when all match; logs and counterexamples go to `TLC_OUT` (a temporary directory by default). CI's `specs` job matrix includes `row-lifecycle` (see [CI](../../docs/ci.en.md) §4).

## Maintenance

When remote sessions §4.2, §4.3 or remote plugins §4.4 change, or when the row key in `crates/rutis-loader/src/runtime.rs`, `rows.ended`/`rows.failed`/`unload_row` in `crates/rutis-bridge/src/runtime/process.rs`, the runtimes' listener replacement order, or `endRow`/`unloadRow` in `node/rutis-runtime/src/runner.mjs` change, update this model and `expected.txt` (standard Q11.2.3).
