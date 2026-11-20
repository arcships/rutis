# Per-row unload budget and diagnostics for Node, Bun, and Python

[中文](design-row-unload-budget-2026-11-20.md)

Status: draft, PoC implemented (branch `design/1323-unload-budget`, not merged). Date: 2026-11-20. Base: `main` `5c2e143e` (after 0.23.0).
Issue: [#1323](https://github.com/arcships/rutis/issues/1323). Related: [unload order](design-unload-order-2026-11-01.en.md) (D7, D8, §3, §6), [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) (§7, §8), [diagnostics design](design-diagnostics-2026-10-26.en.md) (§4.5, §6), [events protocol](design-events-protocol-2026-10-18.en.md) (§7.1).

## 1. Where things stand and what to solve

When the host asks a Node, Bun, or Python runtime to unload a single row (`rows.unload` — triggered by removing a row, reloading it, changing its config, losing a dependency, or rolling back a failed load), the runtime waits indefinitely for the row's user cleanups with **no per-row time budget**:

- Node leaf plugins (`drain` in `node/rutis-runtime/src/leaf.mjs`) `await` each unload-stack step without a deadline; native Cordis plugins (`unloadRow` in `node/rutis-runtime/src/runner.mjs`) `await row.fiber.dispose()` directly.
- Bun (`unload` in `bun/rutis-bun/src/runner.ts`) `await`s each `this.within(key, step)` without a deadline.
- Python (`unload` in `python/rutis/rutis/runner.py`) `await`s each `_settle(step())` without a deadline.

If a single user cleanup never settles (an unclosed connection pool, a deadlocked queue, a never-resolving Promise/coroutine, or two cleanups waiting on each other after [unload order](design-unload-order-2026-11-01.en.md) switched Node leaf plugins from concurrent to serial LIFO):

1. `rows.unload` never replies, leaving the row stuck in `Unloading` on the host forever so it cannot finish unloading or reload;
2. Any framework withdrawals sitting deeper on the unload stack (such as `provide` withdrawals) never run, keeping the row's service registrations lodged in the runtime;
3. `--shutdown-timeout` / `RUTIS_SHUTDOWN_TIMEOUT` (default 10 s) only kicks in when the **entire host stops or the session disconnects**, and in Node, Bun, and Python it terminates the whole runtime process (exit status 1) rather than unsticking the single hanging row;
4. No diagnostic line is printed when a single-row unload hangs ([unload order](design-unload-order-2026-11-01.en.md) §3 and §6 note this gap, and [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §8 distinguishes call timeouts from the host shutdown deadline).

Current unload-budget coverage across all six languages:

| Runtime | Per-row `rows.unload` cleanup budget (before this design) | Session-end / `dispose` cleanup budget (before this design) |
| --- | --- | --- |
| Node (leaf plugin) | None: waits until the stack finishes | No per-row budget; process exits at `RUTIS_SHUTDOWN_TIMEOUT` after disconnect (status 1) |
| Node (native Cordis) | None: waits until `row.fiber.dispose()` settles | Same as above |
| Bun | None: waits until the stack finishes | Same as above |
| Python | None: waits until the stack finishes | Same as above |
| Kotlin | None: waits until the stack finishes | `dispose(shutdownMillis)` times each entry with remaining budget; withdrawals never skipped (D8) |
| Go | None: waits until the stack finishes | `dispose(ctx)` bounded by `RUTIS_SHUTDOWN_TIMEOUT`; gives up the current row and those after it |
| Swift | None: waits until the stack finishes | `dispose(within:)` bounded by `RUTIS_SHUTDOWN_TIMEOUT`; gives up the current row and those after it |

This design first delivers the per-row unload budget and diagnostics for Node, Bun, and Python; Go, Swift, and Kotlin are tracked as follow-up steps in §6 using the same environment variable and semantics.

## 2. Decisions

| # | Decision | Why; what was dropped |
| --- | --- | --- |
| D1 | **Opt-in budget; unlimited by default**: add environment variable `RUTIS_ROW_UNLOAD_TIMEOUT` in **seconds** (positive finite floating-point number, parsed identically to `RUTIS_SHUTDOWN_TIMEOUT`). Unset, empty, `<= 0`, `NaN`, `Infinity`, or unparseable values disable the budget (unlimited). Also expose an explicit runtime option for tests and embedders: Node `run({ rowUnloadTimeout })` (ms) and `toCordis(..., unloadOptions)`, Bun `new Runtime(project, { rowUnloadTimeout })` (ms), Python `Runtime(row_unload_timeout=...)` (seconds) | Leaving the budget off by default preserves native Cordis and existing rutis behavior so legitimate slow teardowns (flushing buffers, closing models) are never cut off unexpectedly. Opt-in lets dev hot-reload, operators, and tests bound single-row unloads explicitly, and `RUTIS_ROW_UNLOAD_TIMEOUT=0` overrides an inherited variable cleanly. Dropped: enabling a default 10 s per-row budget out of the box (silently breaks slow legitimate cleanups and alters default Cordis semantics); reusing `RUTIS_SHUTDOWN_TIMEOUT` directly (whole-host shutdown deadlines and single-row hot-reload budgets serve different operational needs) |
| D2 | **Budget bounds user cleanups only, never framework withdrawals** (extends [unload order](design-unload-order-2026-11-01.en.md) D8): the budget is one shared total budget across **all user cleanup steps** of a single row (`ctx.effect` registrations and the cleanup returned by `apply`). Framework withdrawals and teardown — `provide` withdrawals on the unload stack, listener withdrawal (`bus.withdraw`), exporter disposal (`exporter.dispose`), handle retirement and slot deletion (`gone` / `slots.delete`), row-table removal, and `provideWaitingLater` — are **never constrained by the budget and always run**. Step 4 (waiting for running listeners) keeps its existing `RUTIS_SHUTDOWN_TIMEOUT` cap ([events protocol](design-events-protocol-2026-10-18.en.md) §7.1); the single-row cleanup budget starts when draining the unload stack begins | Framework withdrawals are fast in-memory table updates and notifications that cannot hang; skipping them leaks stale service registrations so reloading the same row or letting another row provide the name fails while no reader can use the stale entry. Dropped: resetting the budget per cleanup step (10 stuck cleanups would wait 10× the budget); capping the whole `rows.unload` including withdrawals (violates D8 and corrupts service tables) |
| D3 | **On timeout: stop waiting on the stuck cleanup, skip all remaining user cleanups, still run every remaining framework withdrawal**: when a user cleanup exceeds the remaining budget (or remaining budget is `0` before the next user cleanup starts): (1) stop awaiting it (in Python, use `asyncio.wait` + `task.cancel()`, never `asyncio.wait_for`, which hangs if a coroutine traps `CancelledError`; in both JS and Python, attach a rejection/done handler so late failures never become unhandled rejections); (2) do **not** run any subsequent user cleanups remaining on the stack; (3) **do** run every remaining framework withdrawal on the stack and all `finally` teardown | You cannot claim both "unstick a hanging cleanup" and "preserve strict serial LIFO order" while the timed-out cleanup is still executing in the background: starting an earlier-registered user cleanup (e.g. closing a DB pool) while a later-registered cleanup is still flushing into it violates LIFO ordering and creates races. Framework withdrawals, in contrast, must always run to keep service and listener tables consistent. Dropped: continuing to run remaining user cleanups after timeout (breaks LIFO guarantees and races with the stuck cleanup); dropping the entire stack on timeout (leaks provided services) |
| D4 | **Complete `rows.unload` normally and emit one verbatim diagnostic line on stderr (zero internal words)**: after running remaining withdrawals and `finally` teardown, `rows.unload` returns `null` / `None` (if a cleanup before the timeout threw an error, Bun and Python still propagate it as before and Node leaf logs it via `context.logger.error`; the timeout itself does not throw a protocol error). The runtime writes one verbatim line to unprefixed stderr via `say(...)` (§3 and §4), strictly following [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5 — no `rutis`, `Cordis`, `fiber`, `Promise.all`, `rows.unload`, or internal keys | Matches Step 4 listener settlement timeout (print diagnostic line, keep unloading) and Kotlin's `unloadRow` budget behavior; keeps Node leaf (where Cordis `_unload` swallows disposal throws), Node native, Bun, and Python consistent; allows `rows.update` and host reconciliation to proceed cleanly instead of aborting. Dropped: failing `rows.unload` with a timeout error (would abort `rows.update` before `load` in Bun/Python and diverge from Node Cordis disposal semantics); adding a new `rutis/*` event or protocol error code (adds wire/concept surface for an opt-in local safeguard) |
| D5 | **Be explicit about JS/Python runtime limits and reload consequences**: neither JavaScript (Node/Bun) nor Python can forcibly kill an arbitrary stuck Promise, a coroutine that suppresses `asyncio.CancelledError`, or synchronous blocking code holding the event-loop thread. Therefore after a timeout: (1) the stuck cleanup may continue running in the background; (2) because all framework tables (services, listeners, slots, rows) are cleanly withdrawn and old `ctx` mutations are refused, **reloading the same row succeeds at the framework level**; (3) however, if the stuck cleanup still holds external resources (a TCP port, file lock, DB connection, or shared global state), reloading in the same process may collide with them — the diagnostic explicitly warns `(the stuck cleanup may still be running; fix it or restart the runtime)`, and production deployments isolating flaky or untrusted plugins should restart the runtime process or isolate the plugin in its own runtime | Honest about what JS and Python can and cannot interrupt. Synchronous infinite loops or blocking calls (`while (true) {}`, `time.sleep(60)`) prevent async timers from firing until they yield; those remain covered by the existing 5 s blocking watch ([diagnostics design](design-diagnostics-2026-10-26.en.md) §6.4) and the `RUTIS_SHUTDOWN_TIMEOUT` watchdog. Dropped: `SIGKILL`ing the entire runtime process on a single-row timeout (would crash every healthy row sharing that runtime) |
| D6 | **Native Cordis fibers in Node without patching `_unload`**: when the opt-in budget is enabled, `runner.mjs` checks `LEAF_FIBERS.has(row.inner)` to tell leaf plugins from native Cordis plugins. Leaf plugins enforce the budget inside `leaf.mjs`'s `drain()` so remaining withdrawals run before `fiber.dispose()` settles (and `runner.mjs` does **not** start a competing outer timer that could fire before `drain()` finishes withdrawals). Native Cordis fibers (`!LEAF_FIBERS.has(row.inner)`) race `row.fiber.dispose()` against the budget in both `unloadRow` and `dispose`; on timeout, `runner.mjs` stops waiting, attaches a no-op rejection handler to the pending `dispose()` promise, prints the native diagnostic line, and still runs the `finally` teardown (`row.unloaded = true`, `bus.withdraw(key)`, `bus.forgetRow(key)`) plus `provideWaitingLater()`. Cordis 4.0.4's internal `Promise.all` cannot be interrupted, but Cordis removes the fiber from `ctx.registry` before `_unload()` and `ctx.provide`'s disposer deletes the service from `ctx.reflect.store` on its first microtask — so services, exporters, listeners, and `waitingHosts` are still cleanly withdrawn | Keeps [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §7 intact ("the runtime does not patch `_unload`") while preventing duplicate timers or missed withdrawals for leaf fibers. Native Cordis `rows.update` uses `fiber.update` (which combines unload and fresh `apply` in one `fiber.await()`), so racing `fiber.await()` would wrongly cap startup `apply`; native Cordis `rows.update` therefore stays unchanged while `rows.unload` and `dispose` enforce the budget (Node leaf, Bun, and Python enforce it on `rows.update` too) |
| D7 | **Document Go, Swift, and Kotlin as follow-up steps**: this design and PoC land Node, Bun, and Python first — the three runtimes that currently have no cleanup budget even during `dispose`. Go, Swift, and Kotlin also lack single-row `rows.unload` budgets today and are scheduled in §6 using the same `RUTIS_ROW_UNLOAD_TIMEOUT` env var and diagnostic wording | Keeps the PoC focused on the three runtimes with zero cleanup budgets today while ensuring all six converge on one convention |

## 3. Rules

### 3.1 Budget parsing and precedence

1. **Environment variable**: `RUTIS_ROW_UNLOAD_TIMEOUT`, in **seconds** (positive finite floating-point number, matching `RUTIS_SHUTDOWN_TIMEOUT` identically):

| Value | Node / Bun (ms) | Python (seconds) | Effect |
| --- | --- | --- | --- |
| unset / `""` | `null` | `None` | Disabled (unlimited) |
| `"5"` | `5000` | `5.0` | Enabled, 5 s total user-cleanup budget per row |
| `"0.25"` | `250` | `0.25` | Enabled, 0.25 s total user-cleanup budget per row |
| `"0"` / `"-1"` / `"NaN"` / `"Infinity"` / `"abc"` | `null` | `None` | Disabled (unlimited; `0` explicitly turns off an inherited variable) |

2. **Explicit option precedence**:
   - Node: `run({ ..., rowUnloadTimeout })` takes milliseconds; `undefined` reads `env.RUTIS_ROW_UNLOAD_TIMEOUT`; `null`, `<= 0`, or non-finite explicitly disables. Standalone `toCordis(spec, rowInject, providerOf, unloadOptions)` applies the same rule to `unloadOptions.rowUnloadTimeout` (omitting `unloadOptions` reads `process.env.RUTIS_ROW_UNLOAD_TIMEOUT`).
   - Bun: `new Runtime(project, { rowUnloadTimeout })` takes milliseconds; omitting the `rowUnloadTimeout` key reads `process.env.RUTIS_ROW_UNLOAD_TIMEOUT`; `null`, `<= 0`, or non-finite explicitly disables.
   - Python: `Runtime(row_unload_timeout=...)` takes seconds; omitting the argument reads `os.environ.get("RUTIS_ROW_UNLOAD_TIMEOUT")`; `None`, `<= 0`, non-finite, or `bool` explicitly disables.

### 3.2 Timing scope and step classification (D8 invariant)

1. **Clock and start point**: monotonic clock (`performance.now()` in Node/Bun; `asyncio.get_running_loop().time()` in Python). The budget starts after Step 4 (waiting for running listeners, still capped by `RUTIS_SHUTDOWN_TIMEOUT`) when draining the unload stack (or native Cordis `row.fiber.dispose()`) begins. All user cleanups of that row share this single budget.
2. **Step classification**:

| Step | Subject to per-row budget? | Must still run after timeout? |
| --- | --- | --- |
| User cleanups (`ctx.effect(...)`, cleanup returned by `apply`) | **Yes** | No (stop awaiting stuck cleanup; skip later user cleanups) |
| `ctx.provide(...)` withdrawals on the unload stack | **No** | **Yes** (run in reverse stack order) |
| Listener withdrawal (`bus.withdraw` / `withdraw_row`), `row.unloaded = true` | **No** | **Yes** |
| Exporter disposal (`exporter.dispose`), handle retirement (`gone`), slot deletion (`slots.delete`) | **No** (already run before stack) | **Yes** |
| Row-table removal, `bus.forgetRow`, Node `provideWaitingLater()` | **No** | **Yes** |
| Step 4 waiting for running listeners | **No** (keeps `RUTIS_SHUTDOWN_TIMEOUT`) | Keeps §7.1 rule |

### 3.3 Timeout state machine and runtime behavior

1. **Budget disabled (default)**: runs the exact existing path — no timers, no wrapper tasks, zero behavior change.
2. **Budget enabled on leaf unload stacks (Node leaf, Bun, Python)**:
   - Take the row's unload stack and iterate in reverse; each entry is tagged `withdrawal: true | false`.
   - When `withdrawal === true`: run unconditionally regardless of `timedOut` or remaining time.
   - When `withdrawal === false`:
     - If `timedOut` is already true: skip without invoking the cleanup.
     - Compute `remaining = max(0, endAt - now)`. If `remaining === 0` (earlier cleanups finished but exhausted the total budget): set `timedOut = true`, emit the diagnostic line once, and skip this and all subsequent user cleanups.
     - Otherwise invoke the cleanup and wait at most `remaining`:
       - **Node leaf / Bun**: `Promise.race` between the cleanup Promise and `setTimeout(remaining)`; `clearTimeout` immediately when the cleanup settles first; if the timer wins, set `timedOut = true`, emit the diagnostic line once, and keep a no-op rejection handler on the stuck Promise so a late rejection never triggers an unhandled rejection.
       - **Python**: wrap the step in `asyncio.create_task` and await `asyncio.wait({task}, timeout=remaining)` (**never** `asyncio.wait_for`, which blocks forever if a coroutine suppresses `CancelledError`); if pending, call `task.cancel()`, attach a done callback to retrieve/discard any late `CancelledError` or exception (avoiding "Task exception was never retrieved"), set `timedOut = true`, and emit the diagnostic line once. Wrap the whole stack loop in `try / finally` so even if `unload` itself is externally cancelled, every remaining `withdrawal` still executes before `rows.pop` and `bus.withdraw_row`.
3. **Node native Cordis fiber**:
   - In `unloadRow` and `dispose`, `runner.mjs` checks `LEAF_FIBERS.has(row.inner)`: when true, `leaf.mjs`'s `drain()` enforces the budget alone (no competing outer timer); when false, `runner.mjs` races `row.fiber.dispose()` against `setTimeout(budgetMs)`.
   - On timeout, clear the timer, attach a no-op rejection handler to the pending `dispose()` promise, emit the native diagnostic line once, and run `finally` (`row.unloaded = true`, `bus.withdraw(key)`, `bus.forgetRow(key)`) plus `provideWaitingLater()`.
4. **Errors thrown before timeout**: if an earlier cleanup throws before a later cleanup times out, Bun and Python still record and rethrow the first cleanup error (Node leaf logs it to `context.logger.error`); the timeout itself does not add or replace an exception.
5. **Formatting `<s>`**: no trailing `.0` — `String(Number((budgetMs / 1000).toPrecision(12)))` in JS/TS and `f"{budget:g}"` in Python (e.g. `5`, `0.25`, `0.05`).
6. **Verbatim diagnostic lines** (written via `say(...)` to stderr without `[<row>] ` prefix):
   - Leaf plugins (Node leaf, Bun, Python):
     ```text
     runtime <runtime>: cleanup of row <row> did not finish within <s> s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)
     ```
     When Node's `toCordis` runs standalone (no runner / row name), `row <row>` is written `plugin <plugin>` (or `plugin leaf` if unnamed).
   - Node native Cordis plugins:
     ```text
     runtime <runtime>: cleanup of row <row> did not finish within <s> s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it or restart the runtime)
     ```
7. **Synchronous busy-loops / blocking calls**: JS and Python event loops are single-threaded, so a synchronous blocking cleanup holds the thread and prevents async timers from firing until it returns; once it returns, if the budget is exhausted, remaining user cleanups are skipped. Long synchronous blocks remain handled by the 5 s blocking watch and the `RUTIS_SHUTDOWN_TIMEOUT` watchdog.

### 3.4 Cross-language matrix (target state)

| Runtime | Single-row `rows.unload` (`RUTIS_ROW_UNLOAD_TIMEOUT` set) | Single-row `rows.unload` (default unset) | `dispose` | Status |
| --- | --- | --- | --- | --- |
| Node (leaf) | `drain()` caps user cleanups to one total budget; skips remaining cleanups on timeout; withdrawals always run | Unlimited | Applies the same per-row budget (with `RUTIS_SHUTDOWN_TIMEOUT` hard cap on disconnect) | Implemented in this PoC |
| Node (native Cordis) | Races `row.fiber.dispose()`; prints native diagnostic and runs `finally` teardown (`Promise.all` cannot be interrupted) | Unlimited | Same as above | Implemented in this PoC |
| Bun | `unload` caps user cleanups to one total budget; skips remaining cleanups; withdrawals always run | Unlimited | `dispose` calls `unload`, automatically covered | Implemented in this PoC |
| Python | `unload` uses `asyncio.wait` + `cancel()`; skips remaining cleanups; withdrawals always run | Unlimited | `dispose` calls `unload`, automatically covered | Implemented in this PoC |
| Kotlin | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unloadRow` (reusing existing per-entry timing + D8 `NonCancellable` withdrawals) | Unlimited | Already has `dispose(shutdownMillis)` | §6 follow-up |
| Go | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unload` with D8 withdrawal preservation | Unlimited | Already has `dispose(ctx)` | §6 follow-up |
| Swift | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unload` with D8 withdrawal preservation | Unlimited | Already has `dispose(within:)` | §6 follow-up |

## 4. What developers see

### 4.1 Plugin authors

#### When it works

TypeScript example (Node leaf / Bun): a plugin provides a service and registers two cleanups, running with `RUTIS_ROW_UNLOAD_TIMEOUT=5`, and both cleanups finish within the budget:

```ts
import { definePlugin } from '@arcships/rutis'

export default definePlugin({
  name: 'weather',
  apply(ctx) {
    ctx.effect(async () => {
      await closeCache()
    })
    ctx.provide('forecast', { today: () => 'sunny' })
    ctx.effect(async () => {
      await flushMetrics(ctx.use('forecast'))
    })
  },
})
```

Output on unload: **no extra output at all**. Both cleanups run in reverse order one at a time, and `forecast` is withdrawn at its stack position, identically to when the budget is unset. Python (`define_plugin` + `async def` cleanups) and native Cordis plugins (`export function apply(ctx)`) differ only in plugin API syntax and likewise emit nothing extra when cleanups finish on time.

#### When it fails / times out

| Trigger | Verbatim output (stderr) | Host error code / recorded on | How to fix |
| --- | --- | --- | --- |
| A Node leaf row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime node: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | Host gets a normal reply (no error code); recorded on runtime stderr for `row weather`; row leaves `Unloading`, services withdrawn, remaining user cleanups skipped | Find the never-settling `await` (or circular wait between two `ctx.effect`s), honour `ctx.signal`, or add an internal timeout; restart the runtime if the stuck cleanup holds external resources |
| Standalone Node `toCordis` (no row name, plugin `weather`) times out at 50 ms | `runtime node: cleanup of plugin weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | No host; recorded on stderr for `plugin weather` | Same as above |
| A Bun row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime bun: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | Host gets a normal reply (no error code); recorded on runtime stderr for `row weather` | Same as above |
| A Python row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` (including a coroutine that traps `CancelledError`) | `runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | Host gets a normal reply (no error code); recorded on runtime stderr for `row weather`; the task was sent `cancel()` | Do not swallow `asyncio.CancelledError` and block again; bound async closes with `asyncio.wait_for`; restart the runtime if external resources remain held |
| A Node native Cordis plugin row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime node: cleanup of row weather did not finish within 5 s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it or restart the runtime)` | Host gets a normal reply (no error code); recorded on runtime stderr for `row weather`; services, listeners, and exporters are withdrawn while the stuck disposal remains in background | Fix the hanging `ctx.effect` disposer in the native plugin; do not have two `ctx.effect` disposers wait on each other; restart the runtime if external resources remain held |
| An earlier cleanup throws (e.g. `Error: flush failed`) before a later cleanup times out | Both the verbatim timeout line above and the earlier cleanup's error (propagated in Bun/Python, logged via `context.logger.error` in Node leaf) | Host gets the earlier cleanup's error (`site.doing = "cleanup"`, recorded on `row weather`); the timeout does not mask it | Fix both the throwing cleanup and the hanging cleanup |
| A cleanup runs a synchronous infinite loop or blocking call (`while (true) {}`, `time.sleep(60)`) | Async timer cannot preempt the single event-loop thread, so no per-row timeout line fires while blocked; after 5 s the existing blocking watch fires, and on host stop the `RUTIS_SHUTDOWN_TIMEOUT` watchdog terminates the process | Reported by the blocking watch / runtime exit | Make the cleanup asynchronous (`await` async I/O, or `asyncio.to_thread` in Python) |

**What happens on reload after a timeout**: because framework tables (services, listeners, slots, rows) were cleanly withdrawn, reloading `weather` succeeds without `forecast is already provided by weather`, and late calls through the old `ctx` (`ctx.provide`, `ctx.effect`, `ctx.emit`) are still refused. However, if the stuck background cleanup still holds an OS resource (listening socket, file lock, shared handle), the reloaded row may hit an OS-level conflict (such as `EADDRINUSE`); fix the cleanup and restart the runtime process.

### 4.2 Users assembling plugins with `rutis-host`

#### When it works

- `RUTIS_ROW_UNLOAD_TIMEOUT` unset: behavior is identical to 0.23.0.
- Opting in for dev or run:

```bash
RUTIS_ROW_UNLOAD_TIMEOUT=5 rutis-host dev
```

When cleanups finish within 5 s, terminal output is completely unchanged.

#### When it fails / times out

On a hot reload or row removal in `rutis.json` where `weather`'s cleanup hangs for 5 s, stderr prints one unprefixed diagnostic line (no `[weather]` prefix, as it is the runtime's own diagnostic):

```text
runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)
```

`weather` leaves `Unloading` immediately, `rutis-host status` no longer shows it stuck, and hot reload continues loading the updated code. If port/file conflicts or memory growth follow, restart `rutis-host` (or the remote runtime) to reclaim the leaked background work.

### 4.3 Host owners

#### When it works

Rust embedders pass `RUTIS_ROW_UNLOAD_TIMEOUT` in the runtime launcher's environment; tests or JS/TS/Python embedders instantiating runtimes directly can also pass the explicit option:

```ts
// Bun embedder / test
const runtime = new Runtime(projectDir, { rowUnloadTimeout: 5_000 })
```

```js
// Node factory / test
await run({ argv, env: { ...env, RUTIS_ROW_UNLOAD_TIMEOUT: '5' }, connect, exit })
// or explicit milliseconds option:
await run({ argv, env, rowUnloadTimeout: 5_000, connect, exit })
```

```python
# Python embedder / test
runtime = Runtime(row_unload_timeout=5.0)
```

#### When it fails / times out

- `process.unload_row(&key)` resolves with `Ok(())` promptly once the budget expires (no protocol error code), preventing host reconciliation from hanging in `Unloading`;
- The runtime's stderr carries the verbatim diagnostic line (free of internal words, safe to surface directly or alert on);
- Leases, service projections, and subsequent loads of the same row key proceed without stale registration clashes; if strict isolation from leaked background Promises/coroutines is required, restart the runtime process or isolate untrusted plugins in dedicated runtimes.

### 4.4 Deployers

#### When it works

Configure `RUTIS_ROW_UNLOAD_TIMEOUT` alongside `RUTIS_SHUTDOWN_TIMEOUT` in the container, systemd unit, or remote runtime environment:

```bash
RUTIS_SHUTDOWN_TIMEOUT=15
RUTIS_ROW_UNLOAD_TIMEOUT=5
```

Keep the per-row budget smaller than the shutdown budget (e.g. 5 s vs 15 s) so a single hanging row during shutdown still leaves time for other rows to clean up. Set `RUTIS_ROW_UNLOAD_TIMEOUT=0` or unset it to disable an inherited budget.

#### When it fails / times out

Alert on log lines matching `cleanup of row ` + ` did not finish within `. Repeated timeouts on a runtime indicate leaked background Promises/coroutines or open handles may be accumulating in that process; schedule a rolling restart of that runtime process or isolate the offending plugin in its own runtime.

### 4.5 Concepts to learn (against the #663 concept budget)

Against the per-layer concept budget recorded in [#663](https://github.com/arcships/rutis/issues/663) (7 for plugin authors / ~9 while debugging, 6 for `rutis-host` users, 10 for host owners, up to +3 for debugging/ops):

- **Plugin authors**: **0 new concepts**. They already know "cleanup" and "unload stack" ([unload order](design-unload-order-2026-11-01.en.md)); the budget is opt-in and invisible unless a cleanup hangs, in which case the single diagnostic line explains everything in plain words.
- **`rutis-host` users, host owners, deployers**: **+1 optional environment variable** (`RUTIS_ROW_UNLOAD_TIMEOUT`), mirroring the existing `RUTIS_SHUTDOWN_TIMEOUT` in unit and parsing rules. It fits inside the debugging/ops slot with no new conceptual layer and stays within budget.

## 5. Compatibility and migration

- **100% backwards compatible by default**: with `RUTIS_ROW_UNLOAD_TIMEOUT` unset, Node (leaf and native Cordis), Bun, and Python behave identically to before. No breaking change; no migration-guide breaking entry is needed.
- **No protocol change**: wire frames, protocol version, control methods, and `rutis/*` events are unchanged.
  - **New runtime with old host**: works seamlessly; if `RUTIS_ROW_UNLOAD_TIMEOUT` is present in the runtime environment, the runtime enforces it locally and replies `null` to `rows.unload`.
  - **Old runtime with new host**: works seamlessly; the old runtime ignores `RUTIS_ROW_UNLOAD_TIMEOUT` and keeps waiting indefinitely.
- **Behavior when opted in**: hanging cleanups no longer block `rows.unload` forever; subsequent user cleanups on that row are skipped, all framework withdrawals still run, and one verbatim diagnostic line is written to stderr.

## 6. Implementation

| Step | Scope & changes | Done criteria |
| --- | --- | --- |
| 1. This design + PoC ([#1323](https://github.com/arcships/rutis/issues/1323), current branch) | Bilingual design docs `docs/design-row-unload-budget-2026-11-20.{md,en.md}`; minimal runnable PoC in `node/rutis-runtime/src/leaf.mjs`, `node/rutis-runtime/src/runner.mjs`, `bun/rutis-bun/src/runner.ts`, `python/rutis/rutis/runner.py` + focused unit tests across all three | Default unset behavior preserved; opt-in timeout returns promptly, prints the exact §4 verbatim diagnostic, skips later user cleanups, still withdraws provided services cleanly; Node native Cordis fiber timeout covered; all three test suites + link/constraint checks pass |
| 2. Host & guide integration (follow-up issue) | Document `RUTIS_ROW_UNLOAD_TIMEOUT` in `docs/guide/rutis-host.{md,en.md}`, `docs/guide/cordis-boundaries.{md,en.md}` (§7–§8), and `docs/design-events-protocol-2026-10-18.{md,en.md}` (§7.1); optional `--row-unload-timeout` flag in `rutis-host`; end-to-end `crates/rutis-loader` test proving a timed-out row leaves `Unloading` and reloads cleanly; register in `docs/guarantees.md` once promoted past PoC | E2E tests lock verbatim output and reload recovery across all three runtimes; bilingual guides updated |
| 3. Go / Swift / Kotlin alignment (follow-up issue) | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into single-row `rows.unload` in Go (`go/rutis/internal/runtime/runtime.go`), Swift (`swift/rutis/Sources/Rutis/API/Runtime.swift`), and Kotlin (`kotlin/rutis/.../internal/Runtime.kt`), preserving D8 withdrawals and unifying the diagnostic wording | All six SDKs share the same env var, D8 withdrawal invariant, and diagnostic format for single-row unload budgets |
