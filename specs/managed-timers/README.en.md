# Managed Timers and the Unload Stack (TLA+)

[中文](README.md)

TLC exhaustively explores all interleavings between a plugin row's managed timer (`ctx.timeout` / `ctx.interval`), synchronous self-cancellation, external cancellation, and LIFO unload-stack draining across both abortable runtimes (Rust / Python) and non-abortable Promise runtimes (Node leaf / Bun). Issue #1325; quality standard Q11.2; design: [Managed Timers and Bare-Resource Boundaries](../../docs/design-managed-timers-2026-11-20.en.md) §2, §3.

## Design and code it corresponds to

| Part of the model | Design | Implementation |
| --- | --- | --- |
| Rust abortable runtime: registers on the unload stack via `effect` (`register_effect_named`), child `CancellationToken`, `tokio::select!` cancellation + `TimerExitSignal` wait, and self-detach after one-shot `timeout` completion | design §2 D1-D6; §3.2, §3.4 | `crates/rutis/src/ctx.rs` `Ctx::timeout`, `Ctx::interval`, `TimerExitSignal` |
| Node leaf non-abortable Promise runtime: tracks both `invoking` and `running` inside `toCordis` so synchronous self-cancel before `await` keeps cleanup on `steps`, and unload awaits `running` before earlier LIFO cleanups | design §2 D1-D8; §3.2 | `node/rutis-runtime/src/leaf.mjs` `toCordis` (`ctx.timeout`, `ctx.interval`) |
| Bun non-abortable Promise runtime: same `invoking` / `running` guard on `row.steps` LIFO stack; removing the row on unload immediately blocks new ticks | design §2 D1-D7; §3.2 | `bun/rutis-bun/src/runner.ts` `Runtime.load` (`ctx.timeout`, `ctx.interval`) |
| Python abortable runtime: `asyncio.Task` registered via `self.effect(cleanup)` on `row.steps`; self-cancel while task is running leaves cleanup registered; unload runs `task.cancel()` + `await task` and detaches via `done_callback` | design §2 D1-D7; §3.2 | `python/rutis/rutis/runner.py` `Context.timeout`, `Context.interval`, `Context._timer` |

The design document status is "draft (minimal PoC implemented for Rust / Node leaf / Bun / Python)" (2026-11-20). This model follows the design and the four-runtime PoC; `mut-self-cancel-drops-step` reproduces and pins the real Node leaf / Bun race where a synchronous self-cancel before `await` prematurely removed the cleanup from `steps`.

## The model

- **Resource and unload stack**: models an earlier resource `pool` (`"open"` / `"closed"`) registered before the timer, so LIFO unload drains `timerCleanup` (`unloadStage = "timer"`) before `pool.close()` (`unloadStage = "pool"`).
- **Timer states** (`timerState`):
  - `"scheduled"`: sleeping for delay/period;
  - `"invoking_sync"`: running the callback's synchronous prologue (before returning a Promise/Future), where `SyncSelfCancel` may invoke the cancel handle (repeatable);
  - `"in_flight_async"`: callback returned a Promise/Future and suspended at `await`;
  - `"idle"`: `interval`-only boundary between a completed tick and scheduling the next wait;
  - `"done"`: terminal state (one-shot `timeout` finished, or cancelled/unloaded with all work exited).
- **Two runtime modes** (`Mode`):
  - `"abortable"` (Rust / Python): `StartUnload` may occur in any state (including concurrently with `invoking_sync` in multi-threaded Rust); once `in_flight_async`, if `cancelled \/ rowState /= "active"`, either `AsyncAbort` aborts at `await` (running `Drop`/`finally`) or `AsyncComplete` finishes normally.
  - `"nonAbortable"` (Node leaf / Bun): single-threaded JS cannot preempt `invoking_sync` with `StartUnload`; once `in_flight_async`, a JS Promise cannot be killed externally and must settle via `AsyncComplete` (touching `pool`), while `UnloadTimerStep` waits before advancing to `UnloadPoolStep`.
- **Manual cancel** (`ManualCancel`) and **synchronous self-cancel** (`SyncSelfCancel`): allowed up to twice to check idempotence; immediately moves `"scheduled"` / `"idle"` to `"done"` and detaches `onStack`, while leaving `onStack = TRUE` during `"invoking_sync"` / `"in_flight_async"` until execution settles.
- **Fixed-delay bound** (`MaxTicks`): `MaxTicks = 1` for `timeout` and `MaxTicks = 2` for `interval`, covering tick 1 completion → rescheduling → tick 2 self-cancel / unload races.

## Simplifications

- One earlier resource (`pool`) and one managed timer; independent sibling timers drain one by one in LIFO order and do not interact in the unload stack.
- Wall-clock durations, argument validation (synchronous preflight), and `ErrorSink`/stderr formatting are omitted: a contained callback error and a normal return have identical lifecycle transitions (`SyncComplete` / `AsyncComplete`).
- Shutdown timeout budgets (`RUTIS_SHUTDOWN_TIMEOUT` / #1323) are not modelled: under `nonAbortable` mode, a JS Promise that never resolves preserves safety (`pool` is not closed early) while unload liveness depends on the Promise settling or being abandoned by the runtime/host unload budget (see design §3.2 cross-language matrix).
- Go / Swift / Kotlin implementations are follow-up issues; their cooperative cancellation semantics match `Mode = "abortable"`.

## Invariants

| Name | Meaning |
| --- | --- |
| `TypeOK` | All state variables remain within their declared domains |
| `NoUseAfterPoolClose` | `~usedPoolAfterClose /\ (pool = "closed" => inFlight = 0)`: an in-flight callback never executes or accesses `pool` after `pool` closes (including after synchronous self-cancel before `await`) |
| `NoFireAfterUnload` | `~firedAfterUnload`: once unload begins or finishes, no new timer callback starts |
| `NoOverlappingTicks` | `inFlight <= 1`: at most one callback execution is active at any instant (fixed-delay non-overlapping) |
| `CleanDetach` | `(timerState = "done" => ~onStack)`, `(rowState = "disposed" => ~onStack /\ timerState = "done")`, `cleanupRuns <= 1`: completed/cancelled timers detach cleanly, nothing remains after dispose, and timer cleanup runs at most once |

## Results

TLC 2.19 (tla2tools v1.7.4), OpenJDK 17.0.18, `-workers auto`. "Pass" means exhaustive exploration completed with all invariants holding; mutations stop at the first counterexample. Total runtime across all 8 configs is ~6 s.

| Configuration | Content | Outcome | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `timers-timeout-abortable` | One-shot `timeout`, abortable mode (Rust / Python) | pass | 55 | <1 s |
| `timers-interval-abortable` | Fixed-delay `interval` (`MaxTicks = 2`), abortable mode (Rust / Python) | pass | 100 | 1 s |
| `timers-timeout-nonabortable` | One-shot `timeout`, non-abortable Promise mode (Node leaf / Bun) | pass | 49 | 1 s |
| `timers-interval-nonabortable` | Fixed-delay `interval` (`MaxTicks = 2`), non-abortable Promise mode (Node leaf / Bun) | pass | 88 | 1 s |
| `mut-self-cancel-drops-step` | Mutation: synchronous self-cancel checks only `!running` and drops stack entry early | violates `NoUseAfterPoolClose` | 104 (stops at counterexample) | <1 s |
| `mut-no-await-inflight` | Mutation: unload timer cleanup advances to `pool.close()` without awaiting in-flight Promise | violates `NoUseAfterPoolClose` | 100 (stops at counterexample) | 1 s |
| `mut-overlap-tick` | Mutation: fixed-rate `interval` starts a second tick while tick 1 is still in flight | violates `NoOverlappingTicks` | 83 (stops at counterexample) | 1 s |
| `mut-fire-after-unload` | Mutation: firing checks `~cancelled` only, ignoring `rowState = "active"` after unload begins | violates `NoFireAfterUnload` | 44 (stops at counterexample) | 1 s |

## Mutations and counterexamples

Every core safety invariant has a deliberate mutation that TLC must catch (`violated:<name>` in `expected.txt`):

| Configuration | Mutation | Violated | Counterexample trace |
| --- | --- | --- | --- |
| `mut-self-cancel-drops-step` | `SyncSelfCancel` sets `onStack = FALSE` during `invoking_sync` (reproducing `if (!running) removeStep(cleanup)` before the `invoking` guard) | `NoUseAfterPoolClose` | `TimerFire` → `SyncSelfCancel` (`onStack=FALSE`) → `ReturnAsync` (`in_flight_async`, `inFlight=1`) → `StartUnload` → `UnloadTimerStep` (skips because `~onStack`) → `UnloadPoolStep` (`pool="closed"` while `inFlight=1`) |
| `mut-no-await-inflight` | `UnloadTimerStep` advances to `"pool"` while `timerState = "in_flight_async"` | `NoUseAfterPoolClose` | `TimerFire` → `ReturnAsync` → `StartUnload` → `UnloadTimerStep` (does not await) → `UnloadPoolStep` (`pool="closed"` while `inFlight=1`) |
| `mut-overlap-tick` | Enables `OverlapFire` while `timerState = "in_flight_async"` | `NoOverlappingTicks` | `TimerFire` → `ReturnAsync` → `OverlapFire` (`inFlight=2`) |
| `mut-fire-after-unload` | `TimerFire` ignores `rowState = "active"` while `~cancelled` | `NoFireAfterUnload` | `StartUnload` (`rowState="unloading"`, before `UnloadTimerStep`) → `TimerFire` (`firedAfterUnload=TRUE`) |

## Conclusion

- Under both abortable (Rust / Python) and non-abortable Promise (Node leaf / Bun) modes, the fixed state machine satisfies `NoUseAfterPoolClose`, `NoFireAfterUnload`, `NoOverlappingTicks`, and `CleanDetach` across all reachable states.
- The 7-step `mut-self-cancel-drops-step` counterexample proves why Node leaf and Bun need the `invoking` guard (never detaching cleanup from `steps` before an async callback settles); the other three mutations pin awaiting in-flight callbacks on unload, non-overlapping fixed-delay ticks, and checking row liveness on fire.
- Out of scope for this model: bounded timeout abandonment of hung JS Promises (#1323), OS/Tokio thread-level switch points, shared mutable state across multiple timers, and the not-yet-implemented Go / Swift / Kotlin codebases.

## How to run

Requires Java 11+ and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/managed-timers/run.sh                           # every configuration
TLA2TOOLS=/tmp/tla2tools.jar specs/managed-timers/run.sh timers-interval-abortable # one configuration
```

`run.sh` compares each configuration's outcome against `expected.txt` and exits 0 when all match.

## Maintenance

Update this model and `expected.txt` whenever design §2 D1-D8, §3.2-3.4, or timer cancellation/unload logic in `crates/rutis/src/ctx.rs`, `node/rutis-runtime/src/leaf.mjs`, `bun/rutis-bun/src/runner.ts`, or `python/rutis/rutis/runner.py` changes (standard Q11.2.3).
