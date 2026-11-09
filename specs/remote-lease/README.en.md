# Remote leases: takeover and reconnection (TLA+)

[中文](README.md)

TLC explores every interleaving of a remote runtime's lease, takeover by a newer connection, and a link reconnecting and stopping; the listening runtime may stop and start again (an app hosting its plugins). Issues #210 and the items merged from #615; quality standard Q11.2, Q6.13.3; status §7.2 (risks E4, E5). Two models:

- `RemoteLease.tla`: a listening remote runtime and the two controller links that dial it (the scope of the issue);
- `AccessPoint.tla`: far ends dialing a `rutis-host/listen` row: handshakes side by side, one session per id, a newer connection of the same id replacing the older, and the dialers' reconnection rules.

**Reconciliation (2026-11-09).** The model now covers four items of [#615](https://github.com/arcships/rutis/issues/615): authentication as a step of the takeover order (`Auth = "step"`), the cleanup deadline (`Cleanup = "deadline"`), app-host stop-then-start (`MaxRounds`, property `RestartIsFresh`), and the static listen link after revocation (`access-static-revoke`). The correspondence table now lists Swift / Kotlin / app hosting. Still not in the model from #615: liveness after faults (the `MaxFaults` configuration), the Cordis node link (`bridge/link.mjs`) backoff and takeover, the Kotlin heartbeat's starting point on takeover, Node / Bun killing at `RUTIS_SHUTDOWN_TIMEOUT` instead of 5 s, and the `RutisApp.kt:142-144` thread interleaving under start / stop (#630's Fray). The revocation hook was removed in 0.19 (#273); the static listen link configurations follow the tracking table's rules (DIA-5-7, DIA-5-10, DIA-5-11, DIA-5-13) and have no implementation to check against.

## Design and code it corresponds to

| Part of the model | Design | Implementation |
| --- | --- | --- |
| Controller (a `rutis-host/dial` row): dial, back off and reconnect, wait by failure category, stop | [remote plugins](../../docs/design-remote-plugins-2026-10-03.en.md) §3.2; [dial-out](../../docs/design-remote-runtime-dial-2026-10-14.en.md) §6; [remote session](../../docs/design-remote-session-2026-10-28.en.md) §4.1 | `crates/rutis-host/src/connect/dial.rs`: dial, handshake, mount the session, wait for it to unload, back off |
| The runtime's lease and takeover order (authenticate → close the old session → clean the old lease → answer the new hello); the cleanup's deadline | remote plugins §4.4; [remote session](../../docs/design-remote-session-2026-10-28.en.md) §4.3 | `python/rutis/rutis/__main__.py` `serve`; `go/rutis/internal/runtime/listen.go` `listen`; `node/rutis-runtime/src/serve.mjs` `serve` (Bun's `src/remote.ts` alike); Swift `Remote/Listen.swift`, `Remote/App.swift`; Kotlin `internal/remote/Listen.kt` |
| App hosting: stop then start, a start during the stop taking effect once it is done; dials and rounds counted afresh | [Swift SDK](../../docs/design-swift-sdk-2026-10-12.en.md) §7; [Android SDK](../../docs/design-android-runtime-2026-10-12.en.md) §5 | `RutisApp.start` / `stop`: Kotlin `RutisApp.kt`, Swift `Remote/App.swift` |
| listen: handshakes side by side, replacement by the same id (old session fully unloaded → old channel closed with 4002 → new session installed) | [remote session](../../docs/design-remote-session-2026-10-28.en.md) §4.2 | `crates/rutis-host/src/connect/listen.rs`: `Listening::run`, `Replaceable` |
| The static listen link after its id is revoked: registration kept, `Waiting { AuthRejected }`, later connections of the id closed with 4003 after their hello | [dial-out](../../docs/design-remote-runtime-dial-2026-10-14.en.md) §5, §6; [tracking table](../../docs/formal-trace/rules/protocol-remote.md) DIA-5-7 / 5-10 / 5-11 / 5-13 | the revocation hook was removed in 0.19 (#273); no implementation to check against; existing connections are dropped by stopping the row |
| The dialer's reconnection rules | dial-out §6 | `python/rutis/rutis/dialing.py` `keep_dialing`; `go/rutis/internal/runtime/dial.go`; `node/rutis-runtime/src/dialing.mjs`; `bun/rutis-bun/src/dialing.ts`; Swift `Remote/Dial.swift`; Kotlin `internal/remote/Dialing.kt` (the Cordis node link is not in the table; #615) |

## The models

Common to both: each direction of a connection is delivered in order, with any delay (more general than the issue's "delayed by at most 2 steps"); a side that closed a connection reads nothing more on it, and frames to it are dropped (kept only where a mutation reads them). The environment's actions (stop, timeout, lost connection, revocation) and the runtime's stop and start may happen at any moment; the configuration says which are on.

### `RemoteLease.tla`

- **Participants**: controllers `c1`, `c2` (rutis `rutis-host/dial` rows) and one listening remote runtime. Connection `<<c, n>>` is `c`'s n-th dial.
- **Controller** (`connect/dial.rs`): once a dial succeeds the upgrade is done and the runtime's listener queues the connection; the row waits for the runtime's hello (the handshake), then mounts the session and is `Ready`; the loader then loads rows through the session (`load` frames). A session that ends (replaced, 4002, or ended by the runtime) is retryable: back off, reconnect. Stopping (dispose) aborts the task: the session closes, the retry is cancelled. Environment: failed dials, handshake timeout or lost connection (`Lose`), stop.
- **Runtime** (Python `serve`, the Swift `RutisApp` listening loop, Kotlin `Listening` and the others): one controller at a time. Taking the next connection while a session runs is the §4.4 order: ① authenticate (`Auth = "step"`: the connection queues first, the runtime checks it before it may enter the takeover queue; refusing closes only that connection and touches no session); ② close the old session's channel (4002) and read none of its frames; ③ clean the old lease (every row unloaded); ④ start the new session and answer its hello. When a session's far end closes, its lease is cleaned.
- **Cleanup deadline** (`Cleanup = "deadline"`): ③ is split into "cleanup starts" and "finishes, or is killed when the deadline passes". A kill is the old process disappearing: the old session's rows go with it, and only then is the new hello answered; at most one kill per run. `Cleanup = "instant"` keeps the old model's one-step cleanup.
- **Stop then start** (`MaxRounds = 2`, app-hosted `RutisApp.listen`): a stop closes the current session and the connections accepted but not served, cleans the lease and accepts no dial; a start opens a new round. A start asked for while the stop cleans up runs once it is done; the old round's sessions and queued connections are never served in the new one (`RestartIsFresh`). At most one stop per run, and only while another round remains.
- **Reconnects**: each controller dials at most 4 times (3 reconnects); the new configurations use 2–4, see "Results".

### `AccessPoint.tla`

- **Participants**: far ends `d1`, `d2` dialing out, greeting with the same id (`SameId`) or ids of their own; on the rutis side one `rutis-host/listen` row.
- **Dialer** (`dialing.py`): a dial delivers the connection to listen and starts its handshake. rutis' hello means "connected"; a closed connection (4002, lost, a failed handshake) is waited out, then dialed again. A stopped dialer dials no more.
- **listen** (`Listening::run`):
  - Handshakes run side by side in a `JoinSet`: `HandshakeDone` (answers hello; none of the far end's plugins is registered yet) or `HandshakeFails` (closes that connection and touches nothing else).
  - `Join`: the loop takes a connection whose handshake is done. No session of its id: install it. Otherwise mark the old connection's closer replaced, take it out of `live`, start unloading the old session; the loop then sits on `unload().await` and does nothing else.
  - `Replace`: once the old session is unloaded (`Unloaded`: every far plugin unloaded, the channel closed last, as 4002 since it is marked replaced), install the new session.
  - `Register`: an installed session registers the far end's plugins.
  - `EndSelf`, `Unloaded`, `ReadEnding`: a session ending on its own (far end closed, or lost) unloads; when the loop reads its ending, it forgets it only if `live` still holds it (same serial).
- **Revocation** (`Faults = {"revoke"}`): the environment revokes an id. An installed session unloads and its channel closes with 4003 (the model's `"rejected"` frame); the dialer waits as an authentication failure (`outcome = "authRejected"`). The listen row keeps its registration (`registered`), so later connections of the id still complete their handshake, receive rutis' hello, and are then closed with 4003 in `Join` without an installed session or registered plugins. Revocation happens once a session is installed, once the dialer has received rutis' hello (`ready`), or while it is not mid-handshake; a revocation during a handshake is not in the model.
- **Reconnects**: each dialer dials at most 4 times (3 reconnects); 3 in the configuration with lost connections (`lose`), 2 in the one with every fault, 2 with a static revocation, see "Results".

## Simplifications

- No time: backoff, the 30 s wait, the 10 s handshake limit and "a session lasting 60 s resets the backoff" are all just "some later moment". A timeout is a `Lose` that may happen at any moment, more general than real timers. The cleanup's deadline is the same: once cleaning starts, "finished" and "killed at the deadline" are both possible actions, with no clock; at most one kill per run (`ghost.killed`). A kill is the old process disappearing, the old session's rows going with it (`killLeavesRows` is the counterexample); the race between the kill and the cleanup thread is not modelled.
- App-host stop and start: `MaxRounds = 2`, at most one stop per run (`ghost.stopped`) and only while another round remains; the runtime stops only while its loop is idle, a stop during a takeover is not modelled. The stop's cleanup is one step (a real `shutdownTimeout` would kill at the deadline, see above); "a start asked for while the stop cleans up" is `pending`, and the next round opens after the cleanup. The thread interleaving of `RutisApp.kt:142-144` belongs to #630 and is not in the model.
- The bounded number of reconnects is the model's bound, not the implementation's behaviour. Liveness is checked only in runs without faults; there every behaviour is finite (each dial uses up budget), so `WF_vars(Next)` makes every enabled step happen. Two controllers configured for the same runtime (or two dialers with the same id) keep replacing each other (written in dial-out §6); in the model the last one to connect holds the lease once the budget is spent, in the implementation backoff slows the alternation to about once every 30 s each. `lease-app-restart` checks no liveness: failed dials while the runtime is stopped also use up the budget, and fairness cannot rule out "the budget is spent before the start", which is the model's bound.
- `RemoteLease`: the controller's hello is left out (the runtime reads it when it starts the session; it has no other effect).
- `AccessPoint`: token routing (`Router`) is left out: a dial reaches the row; `registered` is a Boolean saying whether the row's registration is there. The far plugins are modelled only as "all registered" and "all unloaded", the reverse-order unload one step; calls over the session are left out. `old.closer.close(Replaced)` comes after `Unloaded`, with the channel already closed, so it is no step of its own. There are no close codes: `"close"` is 4002 or a normal close, `"rejected"` is 4003. The listen row has no status variable of its own: `registered` says its registration is still there, and `Waiting { AuthRejected }` is the dialer's `outcome = "authRejected"`.
- Each action is atomic.

## Invariants and liveness

| Name | Model | Meaning |
| --- | --- | --- |
| `OneLease` | RemoteLease | At most one active lease on the runtime: the session it serves and the rows not cleaned yet belong to one connection |
| `OldCommandsStayOld` | RemoteLease | A session's commands (`load`) act only on that session's lease; late commands of an old session never act on a new lease |
| `StoppedStaysDown` | both | A stopped link / dialer never dials again and keeps no connection open |
| `ReadyIsCurrent` | RemoteLease | A controller is `Ready` only on its current, open connection: a late hello of a handshake it gave up on never revives it |
| `CleanBeforeHello` | RemoteLease | When the runtime is about to answer the new hello, the old lease's rows are all gone (a kill included) |
| `RuntimeStopped` | RemoteLease | A stopped runtime serves no session, holds no lease, and keeps none of the connections it accepted before the stop |
| `RestartIsFresh` | RemoteLease | The runtime serves, and is about to serve, only connections of its current round; a controller still `Ready` on an old connection must have had it closed by the runtime (it has not read the close yet) |
| `OnePluginSetPerId` | AccessPoint | Never two sessions of one id with their far plugins registered at once: the new session registers only after the old one fully unloaded |
| `StaysRegistered` | AccessPoint | Revoking an id does not drop the listen row's registration: later connections still reach it |
| `RevokedStaysOut` | AccessPoint | A revoked id never has an installed session again: later connections of it are closed with 4003 after their hello |
| `EventuallyHeld` | both, no faults | Eventually a controller holds the lease (`RemoteLease`: a controller is `Ready` and the runtime serves its connection; `AccessPoint`: each id has an installed session with its far plugins registered, and its dialer is connected) |
| `EndsHeld` | both, no faults | `[]<>Held`: in a finite run, "someone holds the lease at the end"; rules out "they replaced each other and nobody got it" |
| `EventuallyRevokedWaits` | AccessPoint, revocation | After an id is revoked, its dialer eventually waits as an authentication failure (`Waiting { AuthRejected }`) |

## Results

TLC 2.19 (tla2tools v1.7.4), Java 21, 32 cores (`-workers auto`). "ok" means the exploration completed and every listed property holds; the state counts of these rows are deterministic, the times vary with the machine's load.

| Configuration | What | Result | States | Time |
| --- | --- | --- | --- | --- |
| `lease-healthy` | 2 controllers, 4 dials each, no faults; liveness included | ok | 791 | 1 s |
| `lease-faults` | As above; stop, failed dials, timeouts / lost connections at any moment | ok | 1,050,999 | 7 s |
| `lease-takeover` | 1 controller, up to 3 dials (2 takeovers); authentication a step before the old session is closed | ok | 864 | 1 s |
| `lease-cleanup-deadline` | 1 controller, 2 dials (1 takeover); the cleanup has a deadline and the old session is killed | ok | 158 | 1 s |
| `lease-app-restart` | 1 controller; the runtime stops once and opens the next round; a start can be asked for while the stop cleans up | ok | 191 | 1 s |
| `access-healthy-same` | 2 dialers with the same id, 4 dials each, no faults; liveness included | ok | 16,020 | 3 s |
| `access-healthy-distinct` | Ids of their own; liveness included | ok | 64 | 2 s |
| `access-lose` | Same id, 3 dials each; failed dials and handshakes, lost connections, sessions ending | ok | 6,350,873 | 36 s |
| `access-all` | Same id, 2 dials each; every fault (lost, stopped) | ok | 129,131 | 3 s |
| `access-static-revoke` | 1 dialer, 2 dials; its id is revoked after the installed session; liveness included | ok | 38 | 1 s |

All 21 configurations (with the mutations below) take about 1 minute.

Configurations with lost connections are limited in dials: 3 each with lost connections only, 2 with lost connections and stops together (3 gives 10.44 million states).

## Counterexamples in the implementation (fixed)

TLC found two places where revoking an identity during a handshake still set a session up (a link providing the peer, an access point accepting an id during a takeover); they were fixed (#210). Identity revocation (`Revocations` / `Revocable`, #273) was removed in 0.19, and with it the configurations `lease-before-fix`, `access-before-fix`, `access-revoke-same`, `access-revoke-distinct`, `access-mut-revoke-cuts-all` and the property `RevocationScoped`. `RevokedStaysOut` came back with #615 from the tracking table's rules (DIA-5-7, DIA-5-10, DIA-5-11, DIA-5-13) (`access-static-revoke`); it checks the rules, and the current implementation has no revocation hook.

0.19 removes the access point (`access_point.rs`) and registration generations (`registration.rs`); `AccessPoint.tla` was rebuilt from the code of `rutis-host/listen`, and the configurations `access-registration`, `access-mut-route-by-id`, `access-mut-no-gen-check` and the properties `OneInstancePerId`, `OldCallsStayOld`, `NoStaleHandOver` went with them.

TLC found no counterexample to the model otherwise: the §4.4 takeover order (the authentication step included), listen's replacement order, cancelling the retry on stop, a cleanup killed at the deadline leaving no old lease, and the new round after an app-host stop never serving old connections all agree with the model. The new configurations were not run against the implementation; the Swift / Kotlin / app-hosting order was read from the code (see the table above).

## Mutations: the model is not vacuous

Each main invariant and liveness property has a deliberately introduced bug that TLC must catch (`violated:<name>` in `expected.txt`; `violated:liveness` for liveness); `CleanBeforeHello` is covered by `lease-mut-kill-leaves-rows`, and `RuntimeStopped` and `EventuallyRevokedWaits` have no mutation of their own:

| Configuration | Mutation | Property violated |
| --- | --- | --- |
| `lease-mut-start-before-clean` | The runtime answers the new hello before cleaning the old lease (§4.4 ③ and ④ swapped) | `OneLease` |
| `lease-mut-read-old` | The runtime keeps reading a replaced session and applies its commands to the current lease (one shared row table) | `OldCommandsStayOld` |
| `lease-mut-stop-keeps-retry` | Stopping a link that waits to reconnect does not cancel the retry | `StoppedStaysDown` |
| `lease-mut-late-hello` | A late hello of a handshake given up on makes the link `Ready` | `ReadyIsCurrent` |
| `lease-mut-wait-for-old` | A takeover stops reading the old session but does not close it, waiting for the old controller to leave (no faults) | liveness (`EndsHeld`) |
| `lease-mut-kill-leaves-rows` | The cleanup's deadline passes and the old session is killed, but its lease's rows are left behind | `OneLease` |
| `lease-mut-stale-round` | Stopping the runtime keeps the connections it accepted but had not served; the new round serves them (the superseded run serves the latest round) | `RestartIsFresh` |
| `access-mut-accept-before-release` | A replacement installs the new session before unloading the old one (§4.2 step 3 moved before step 1) | `OnePluginSetPerId` |
| `access-mut-stop-keeps-retry` | Stopping a dialer that waits to dial again does not cancel it | `StoppedStaysDown` |
| `access-mut-revoke-installs` | A connection of a revoked id is installed when its handshake completes | `RevokedStaysOut` |
| `access-mut-revoke-unregisters` | Revoking an id also drops the listen row's registration, so later dials are refused at the upgrade and never reach the row | `StaysRegistered` |

Each mutation's counterexample is found within 2 seconds (tens to some thousands of states).

## Running it

Needs Java 11+ and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/remote-lease/run.sh                     # every configuration
TLA2TOOLS=/tmp/tla2tools.jar specs/remote-lease/run.sh lease-faults        # one
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/remote-lease/run.sh       # limit TLC's threads
```

`run.sh` picks the model by name (`lease-*` → `RemoteLease.tla`, `access-*` → `AccessPoint.tla`), compares each configuration's outcome with `expected.txt`, and exits 0 when all match; logs and counterexamples go to `TLC_OUT` (a temporary directory by default). CI's `specs` job runs it on pull requests that change `specs/**` and in the nightly full run ([CI](../../docs/ci.en.md) §4).

## Maintenance

When remote plugins §3.2, §4.4, dial-out §5, §6 or remote session §4.2, §4.3 change, or `connect/dial.rs`, `listen.rs` (`Listening::run`), a language's listening takeover order and cleanup deadline, `RutisApp.start/stop`, or the dialers' reconnection rules change, update this model and `expected.txt` with them (standard Q11.2.3). The static listen link's revocation has no implementation; change `access-static-revoke` when the tracking table's DIA-5-7 / 5-10 / 5-11 / 5-13 change.
