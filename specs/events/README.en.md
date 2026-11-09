# The event protocol: host rules, listener table, emit and node links (TLA+)

[中文](README.md)

These models are issue #613's `specs/events/`: the [events protocol spec](../../docs/design-events-protocol-2026-10-18.en.md) §4 (registration), §5 (dispatch), §6 (filters), §7 (lifecycle) and §9 (node links) written as finite models whose interleavings TLC explores. Background: [unified events](../../docs/design-events-2026-10-18.en.md) and [formal verification redesign](../../docs/design-formal-verification-2026-11-08.en.md) §3.2.

| Module | Content | Participants and size | Configurations |
| --- | --- | --- | --- |
| `EventsHost.tla` | Definitions only, no state: name / prefix matching, registration order, elsewhere, filters, when each of the five modes starts the next call, `export` / `import` and narrowing, the epoch of an announced id | none | `host-defs` runs `EventsHostCheck.tla`, which evaluates the spec's examples |
| `EventsTable.tla` | Registration, withdrawal, elsewhere, the fast path and the via-host path, the `rows.load` reply, a row's unload, `rows.ended`, a session ending and starting again | host + 2 runtimes; the healthy configuration has 1 row per runtime (`table-order` has 1 runtime with 2 rows); at most 1–2 dispatches | 10 |
| `EventsEmit.tla` | `emit` order at the host and the runtime: a held call, the reply read in place, a filter that answers late or never, a row unloading | host + `R1` (two row identities) + `R2`; two emits | 5 |
| `EventsLink.tla` | `link.offers` snapshots, only the highest version, `<epoch>.<n>` announcements, an announcement before its offer, the three accept steps, EVP-9-17 | host + one peer; two entries, two families, two offers | 5 |

`EventsHost.tla` is not an executable spec: the three stateful modules `EXTENDS` it and each configuration uses a part of it. `host-defs` checks by `ASSUME`: the examples of §4, §5, §6 and §9 must evaluate to what the spec writes, and then the model stays in its one state. It does not check that the definitions hold for every input, only that they agree with the examples.

## What the models correspond to

| Model part | Implementation |
| --- | --- |
| The host table, positions, `prepend`, the liveness check | `crates/rutis/src/bus/table.rs` (`select`, `insert`, `Entry::start`) |
| The snapshot, filters, the five modes, the emit tail and backlog | `crates/rutis/src/bus.rs` (`emit_erased`, `snapshot`, `start`), `crates/rutis/src/bus/run.rs` |
| A session's `events.on` / `events.off` / `events.dispatch`, row withdrawal, the emit queue | `crates/rutis-bridge/src/events/hub.rs` (`SessionEvents::on`, `off`, `dispatch`, `run_emit`, `next_emit`, `row_unloading`, `row_ended`, `let_go_of_its_emits`) |
| Frame order (`events.*`, `rows.ended`) | `crates/rutis-bridge/src/session/rpc.rs` |
| The node link, accepting and withdrawing | `crates/rutis-bridge/src/events/link.rs` (`accept`, `announced_under`, `peer_offer`, `offer_now`, `older_than`) |
| The version and `since` of `link.offers` | `crates/rutis-bridge/src/peer.rs` (`Offers`, `next_offers`, `send_if_modified`) |

## Simplified

Common to all modules:

- One name (`EventsTable`, `EventsEmit`) or two entries (`EventsLink`); no prefix entries in the configurations, though `EventsHost.tla` defines them.
- No scope (a descriptor only), values, error contents, `AggregateError` aggregation, cancellation, timeouts, or the rule that `next` is called once.
- Messages are delivered in order in each direction, delayed arbitrarily; some modules narrow "in order" to one FIFO queue per side.
- Time is a ghost set of "what happened", not a clock: reaching the same state one step later is the same state.

`EventsTable.tla`:

- One listener per runtime (one entry per row). The issue's "2 runtimes, 2 rows, 2 entries each" in one configuration exceeds a minute, so the healthy configuration runs 2 runtimes × 1 row and `table-order` checks `OneOrder` with 1 runtime × 2 rows.
- No cross-process call (the causality of §4.4 step 3 is not modelled as such); the `events.on` reply is not modelled, and the `rows.load` reply gate only counts registrations issued and accepted.
- `rows.update`, `once` and generations are not modelled; a session may only end and start again when no snapshot is still starting, so a call of the old session never starts in the new one.
- No filter in any configuration.

`EventsEmit.tla`:

- One scenario only: `R1`, inside a synchronous wait, holds the call of an emit that went through the host; after reading the reply, its wait chain emits the same name again. The host's per-row queue, several rows, host emits interleaved with runtime emits, and the other modes are not modelled.
- `R2a` / `R2b` are configuration constants, but in this model R2's calls start in frame order under both, so the two configurations have the same states: the difference (overlapping awaits, a queue per listener) is not modelled. The two configurations only confirm that `EmitOrder` does not depend on that rule.
- Deadlock checking is off (`CHECK_DEADLOCK FALSE`): the terminal state is "the run is over", not a stuck one; this model does not answer "does not get stuck".
- Only `E1` has a filter; `E2` has none.

`EventsLink.tla`:

- One peer, two entries, two families, two offers; no a—b—c multi-hop, and `NoEcho` checks one hop only.
- Carried messages: `events.on` / `events.off` in order, and `link.offers` on a separate, unordered channel (`peer.rs`'s `announce` sends on a task). That is where #593's timing comes from.
- The offer boundary itself withdraws: a new offer of `events` does not send an `events.off` per old entry; the epoch withdraws them (`link.rs`'s `offered()`).
- The three accept steps are split only under the `acceptUnlocked` mutation; the base model checks the epoch, registers and inserts in one step.

## Properties

| Name | Module | Content | Guarantee |
| --- | --- | --- | --- |
| `RegisteredThenSeen` | Table | A registration the dispatcher knew when it emitted, not withdrawn anywhere by the snapshot, is in the snapshot (§4.4 steps 1, 3) | `events/presence`, `events/hub-dispatch` |
| `WithdrawnStaysOut` | Table | An entry the dispatcher knew withdrawn when it emitted starts no call of that dispatch (§4.4 step 4, §7.1) | `events/hub-lifecycle`, `events/lifecycle` |
| `OneOrder` | Table | Two entries that two dispatches both start start in the same order (§4.3, §5.4) | `events/order`, `events/one-bus` |
| `OneCallPerEntry` | Table | At most one call of an entry per dispatch (one half of `SettlesOnce`) | `events/hub-lifecycle` |
| `RunningRowHeard` | Table | A row reported Running had every registration of its apply accepted (§4.4 step 2) | no tracker entry; to be added with the model |
| `NoDispatchBeforeElsewhere` | Table | A new session dispatches only after it applied the full elsewhere set (§4.5) | — |
| `SeesAll` | Table | Every dispatch sees every entry registered before it, not withdrawn then; the protocol does **not** promise this, and the configuration is expected violated | `events/one-bus` (the counterexample is below) |
| `EmitOrder` | Emit | For one emitter and one listener, `emit` calls start in emission order (§5.5) | `events/emit`, `events/emit-order` |
| `FastPathByFrame` | Emit | After the reply of a via-host emit is read, a later emit of the name is not left on the via-host path (§5.5, last item) | `events/one-bus` (`emit/order-across-fast-path`) |
| `UnloadedRowEmitsNothing` | Emit | An emit queued or waiting for its filter when its row unloaded starts no call (§5.5, §7.1) | no tracker entry; to be added with the model |
| `CurrentOffer` | Link | With nothing on the link and nothing left to send, the peer's entries in the host table are exactly those its current offer announced and still has (§9) | `events/node-links` |
| `NoEcho` | Link | The host never announces an entry it took from the peer back (one hop) | `events/no-multi-hop` |

## Results

TLC 2.19 (tla2tools v1.7.4), Temurin 21.0.12, 32 cores, `-workers 4`, 2026-11-09. "Pass" means the search completed and every listed property holds; a violated configuration stops at the first counterexample, so its state count depends on the search order. The whole directory takes about 35 seconds.

| Configuration | Content | Result | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `host-defs` | The definitions agree with the spec's examples | pass | 1 | <1 s |
| `table-healthy` | 2 runtimes × 1 row, the host's own listener, withdrawal, row unload, at most 1 dispatch | pass | 897,749 | 9 s |
| `table-sees-all` | As healthy, checking `SeesAll` | violated `SeesAll` | stops at the first counterexample | <1 s |
| `table-ended` | A runtime reports `rows.ended` | pass | 188,817 | 3 s |
| `table-restart` | A session ends; the new one applies the full elsewhere set first | pass | 645,101 | 9 s |
| `table-order` | 1 runtime × 2 rows, `prepend`, two dispatches (fast and via host) | pass | 20,047 | 2 s |
| `table-mut-local-order` | The runtime's local order ignores `prepend` | violated `OneOrder` | stops at the first counterexample | 1 s |
| `table-mut-out-of-order` | The host processes a session's frames out of order (before #587) | violated `WithdrawnStaysOut` | stops at the first counterexample | 2 s |
| `table-mut-stale-fast` | The fast path's snapshot and liveness check read the host's table, not its own registrations | violated `WithdrawnStaysOut` | stops at the first counterexample | <1 s |
| `table-mut-load-reply` | `rows.load` is replied before its registrations are accepted | violated `RunningRowHeard` | stops at the first counterexample | 1 s |
| `table-mut-fresh` | A new session dispatches without the elsewhere set | violated `NoDispatchBeforeElsewhere` | stops at the first counterexample | <1 s |
| `emit-r2a` | A held call, the reply read in place, a late filter; R2 rule a | pass | 459 | 1 s |
| `emit-r2b` | The same, R2 rule b | pass | 459 | <1 s |
| `emit-mut-held` | A fast-path emit of the name passes the held call (before #557) | violated `EmitOrder` | stops at the first counterexample | 1 s |
| `emit-mut-reply-async` | The runtime records the reply one step after reading it (before #513) | violated `FastPathByFrame` | stops at the first counterexample | <1 s |
| `emit-mut-unload` | A row's queued emits are not dropped when it unloads (before #536) | violated `UnloadedRowEmitsNothing` | stops at the first counterexample | 1 s |
| `link-healthy` | Two offers, an announcement before its offer, a snapshot caused by the other family | pass | 14,042 | 1 s |
| `link-mut-accept-unlocked` | The accept unlocks between its epoch check and its insert (as `link.rs` `accept` is) | violated `CurrentOffer` | stops at the first counterexample | 1 s |
| `link-mut-bare-id` | Announced ids carry no epoch and are replaced by id only (EVP-9-17) | violated `CurrentOffer` | stops at the first counterexample | 1 s |
| `link-mut-offer-race` | A snapshot without `events` withdraws everything (before #593) | violated `CurrentOffer` | stops at the first counterexample | <1 s |
| `link-mut-echo` | The host announces an imported entry back | violated `NoEcho` | stops at the first counterexample | 1 s |

## Counterexamples

Every checked property has a deliberate error (`violated:<name>` in `expected.txt`). Besides the mutations, `table-sees-all` violates what the protocol explicitly does not promise:

- **`table-sees-all`**: `r1` registered a listener and its `events.on` was accepted into the host's table; `r2`'s elsewhere is not updated yet, so it takes the fast path and sees only its own entry. "Every dispatch sees every entry registered before it" does not hold. This is the example at the start of issue #613: `SeesAll` is not a promise; `RegisteredThenSeen` and `WithdrawnStaysOut` are.
- **`table-mut-out-of-order`**: `r1` sends `events.on`, then `off`, then `events.dispatch`; the host takes the dispatch first, its snapshot holds the entry, and it starts a call for it before processing the `off`. `r1` knew the withdrawal when it emitted, so `WithdrawnStaysOut` does not hold. This is the frame-order problem before #587.
- **`table-mut-stale-fast`**: after `r1` sends `off`, before the host processes it, the fast path takes its snapshot from the host's accepted table and asks that table whether the entry is alive, so it starts a call for an entry it had already withdrawn.
- **`table-mut-local-order`**: the local order ignores `prepend`: the fast path starts calls in arrival order, a via-host dispatch in position order; the two dispatches start the same two entries in different orders, so `OneOrder` does not hold.
- **`table-mut-load-reply`**: `rows.load` is replied before the `events.on` is accepted; `RunningRowHeard` does not hold.
- **`table-mut-fresh`**: a new session dispatches before it applied the full elsewhere set; `NoDispatchBeforeElsewhere` does not hold.
- **`emit-mut-held`**: E1's call at `L1` is held by the wait; after R1 reads the reply, a fast-path E2 starts the call at `L1` at once; E1's call runs when the wait ends. For one emitter, E2 starts before E1 at `L1`, so `EmitOrder` does not hold (before #557).
- **`emit-mut-reply-async`**: R1 has read E1's reply frame but records it one step later; the wait chain emits E2 in between and the runtime still sends it to the host. `FastPathByFrame` does not hold (before #513).
- **`emit-mut-unload`**: a row's unload does not drop its queued emits; when the filter answers, E1 and E2 still start calls, so `UnloadedRowEmitsNothing` does not hold (before #536).
- **`link-mut-accept-unlocked`**: after the epoch check for `<<1,2>>` and before its insert, the host processes a higher-version snapshot with `events: 2` (which withdraws the old offer); the insert then puts `<<1,2>>` back into the table. When the link goes quiet, the table holds an entry the peer's current offer does not announce, so `CurrentOffer` does not hold. `link.rs`'s `accept()` is built this way now: the epoch check under the lock, the entry registered outside it, the insert under the lock again.
- **`link-mut-bare-id`**: announced ids carry no epoch and the host replaces by id only; after the peer offers `events` again, entries announced under the old offer that the new offer does not have stay in the table, so `CurrentOffer` does not hold (EVP-9-17).
- **`link-mut-offer-race`**: a snapshot without `events` withdraws everything; an announcement that arrived before its offer is taken away by that snapshot even though its offer was never shown (before #593).
- **`link-mut-echo`**: the host announces an entry it accepted from the peer back to it; `NoEcho` does not hold.

## Conclusion

- Under the current spec, `EventsHost.tla`'s definitions agree with the spec's examples, and `EventsTable.tla`'s four safety properties hold in the explored range: register then emit in one process on both paths, no call after a known withdrawal, the same order for the same pair of entries everywhere, registrations accepted before the `rows.load` reply, and the full elsewhere set before a new session dispatches.
- `EventsEmit.tla`'s two ordering properties and the unload property hold in its scenario, and it finds the three pre-fix behaviours: a held call passed by a later fast-path emit, the reply recorded one step late, and an unload that keeps queued emits.
- `EventsLink.tla`: keeping only the highest snapshot, withdrawing the old offer's entries by epoch, and keeping an announcement that arrived before its offer all hold under the current rules; an id without an epoch (EVP-9-17) and the unlock window of `accept()` each have a counterexample.
- What the models do not prove (issue #613's scope): 2 runtimes × 2 rows in one configuration, cross-process causality, prefix entries, scope, values, errors, cancellation, the host's per-row emit queue, R2a's overlapping execution, multi-hop nodes, and the `python-deferred` and `before-future-reply` configurations. They are either under "Simplified" above or not built; this directory is not the whole of #613's evidence.

## Running

Java 11+ and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/events/run.sh                 # all configurations
TLA2TOOLS=/tmp/tla2tools.jar specs/events/run.sh table-healthy    # one configuration
TLC_WORKERS=2 TLA2TOOLS=/tmp/tla2tools.jar specs/events/run.sh    # limit the workers
```

`run.sh` picks the module by the configuration's prefix (`host-defs` → `EventsHostCheck.tla`, `table-*` → `EventsTable.tla`, `emit-*` → `EventsEmit.tla`, `link-*` → `EventsLink.tla`), compares each outcome with `expected.txt`, and exits 0 when all match; logs and counterexamples go to `TLC_OUT` (a temporary directory by default). The CI `specs` job matrix needs `events` (the main thread does it; see [CI](../../docs/ci.en.md) §4).

## Maintenance

When protocol spec §4, §5, §6, §7 or §9 changes, or `bus/table.rs`'s `select` / `Entry::start`, `bus.rs`'s `emit_erased` / snapshot, `hub.rs`'s row withdrawal and emit queue, `link.rs`'s `accept` / announcement withdrawal, or `peer.rs`'s `Offers` changes, change these models and `expected.txt` with it (quality standard Q11.2.3). Re-run every configuration: the mutations must still find their counterexamples and the healthy ones must still pass.
