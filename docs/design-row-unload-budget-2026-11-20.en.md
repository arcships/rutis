# Per-row unload budget and diagnostics for Node, Bun, and Python

[中文](design-row-unload-budget-2026-11-20.md)

Status: draft, PoC implemented (branch `design/1323-unload-budget`, not merged). Date: 2026-11-20 (same-day revision: aligned with the existing "plugins are trusted; no sandbox" trust model — after a timeout the runtime only observes and reports, and no longer refuses same-process reloads). Base: `main` `5c2e143e` (after 0.23.0).
Issue: [#1323](https://github.com/arcships/rutis/issues/1323). Related: [design principles](design-philosophy.en.md) (§7), [unload order](design-unload-order-2026-11-01.en.md) (D7, D8, §3, §6), [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) (§7, §8), [diagnostics design](design-diagnostics-2026-10-26.en.md) (§2, §4.5, §6), [remote plugins](design-remote-plugins-2026-10-03.en.md) (§7), [multilang runtimes](design-multilang-runtimes-2026-10-03.en.md) (§2), [events protocol](design-events-protocol-2026-10-18.en.md) (§7.1).

## 1. Where things stand, semantic state difference, and what to solve

### 1.1 Current problem

When the host asks a Node, Bun, or Python runtime to unload a single row (`rows.unload` — triggered by removing a row, reloading it, changing its config, losing a dependency, or rolling back a failed load), the runtime waits indefinitely for the row's user cleanups with **no per-row time budget**:

- Node leaf plugins (`drain` in `node/rutis-runtime/src/leaf.mjs`) `await` each unload-stack step without a deadline; native Cordis plugins (`unloadRow` in `node/rutis-runtime/src/runner.mjs`) `await row.fiber.dispose()` directly.
- Bun (`unload` in `bun/rutis-bun/src/runner.ts`) `await`s each `this.within(key, step)` without a deadline.
- Python (`unload` in `python/rutis/rutis/runner.py`) `await`s each `_settle(step())` without a deadline.

If a single user cleanup never settles (an unclosed connection pool, a deadlocked queue, a never-resolving Promise/coroutine, or two cleanups waiting on each other after [unload order](design-unload-order-2026-11-01.en.md) switched Node leaf plugins from concurrent to serial LIFO):

1. `rows.unload` never replies, leaving the row stuck in `Unloading` on the host forever so it cannot finish unloading or reload;
2. Any framework withdrawals sitting deeper on the unload stack (such as `provide` withdrawals) never run, keeping the row's stale service registrations lodged in the runtime;
3. `--shutdown-timeout` / `RUTIS_SHUTDOWN_TIMEOUT` (default 10 s) only kicks in when the **entire host stops or the session disconnects**, and in Node, Bun, and Python it terminates the whole runtime process (exit status 1) rather than unsticking the single hanging row;
4. No diagnostic line is printed when a single-row unload hangs ([unload order](design-unload-order-2026-11-01.en.md) §3 and §6 note this gap, and [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §8 distinguishes call timeouts from the host shutdown deadline).

### 1.2 The semantic state introduced by timeout

Inside a shared runtime process (multiple rows sharing one Node, Bun, or Python process), **returning after a cleanup timeout is NOT the same semantic state as finishing an unload cleanly**:

- **Clean unload**: every user cleanup of the old generation has genuinely terminated; zero code from generation $g$ remains running in the process. Loading generation $g+1$ next naturally preserves **at most one active generation of a row per shared process**.
- **Timed-out return**: JavaScript cannot forcibly kill an unsettled Promise, and Python cannot forcibly kill a coroutine that traps `asyncio.CancelledError` or blocks inside external calls:
  - On a **leaf unload stack** (Node leaf, Bun, Python), the runtime stops waiting on the hanging cleanup and skips later user cleanups, yet **the timed-out cleanup itself remains alive in the background**;
  - On a **Node native Cordis fiber**, Cordis 4.0.4's `_unload()` starts all disposables concurrently via `Promise.all` (which cannot be interrupted), so **multiple disposables from that fiber may still be running in the background**.

Neither kind of timed-out return is a "clean unload": the stuck cleanup has not been killed, only stopped being waited on.

**This design does not refuse same-process reloads after a timeout**, for three reasons:

1. It contradicts the existing trust model. The trust model in [remote plugins](design-remote-plugins-2026-10-03.en.md) §7 is "plugins are trusted; networks and remote machines are not fully trusted. Out-of-process execution is not a sandbox" — rutis does not defend against plugins inside a process in the first place, so there is no reason to introduce a "which rows may not reload" table solely for an opt-in cleanup budget.
2. The platform should not decide for the business whether a row may reload. The principle in [diagnostics design](design-diagnostics-2026-10-26.en.md) §2 is "the platform only shows, it does not decide for the business". If a row is reloaded while its old cleanup is still running, the risk of two concurrent generations (colliding over ports, connection pools, etc.) belongs to the plugin author; refusing by row key in the runtime decides "you may not reload now" on behalf of plugin authors, hosts, and operators.
3. A hard isolation path already exists when it is needed: put the row in its own runtime instance ([multilang runtimes](design-multilang-runtimes-2026-10-03.en.md) §2 item 6, "Add isolation instances only when needed"). It is the only truly reliable way to guarantee old and new generations never overlap; a refusal table inside the runtime cannot deliver that.

After a timeout the runtime therefore does exactly four things: stop waiting when the budget expires, skip the remaining user cleanups, finish the framework withdrawals (D8), and write one diagnostic line to stderr; **report faithfully, never block a reload**. Whether to reload that row afterwards is decided by the plugin author, the host, and operations.

Current unload-budget coverage across all six languages before this design:

| Runtime | Per-row `rows.unload` cleanup budget (before this design) | Session-end / `dispose` cleanup budget (before this design) |
| --- | --- | --- |
| Node (leaf plugin) | None: waits until the stack finishes | No per-row budget; process exits at `RUTIS_SHUTDOWN_TIMEOUT` after disconnect (status 1) |
| Node (native Cordis) | None: waits until `row.fiber.dispose()` settles | Same as above |
| Bun | None: waits until the stack finishes | Same as above |
| Python | None: waits until the stack finishes | Same as above |
| Kotlin | None: waits until the stack finishes | `dispose(shutdownMillis)` times each entry with remaining budget; withdrawals never skipped (D8) |
| Go | None: waits until the stack finishes | `dispose(ctx)` bounded by `RUTIS_SHUTDOWN_TIMEOUT`; gives up the current row and those after it |
| Swift | None: waits until the stack finishes | `dispose(within:)` bounded by `RUTIS_SHUTDOWN_TIMEOUT`; gives up the current row and those after it |

This design delivers the per-row unload budget for Node, Bun, and Python first; Go, Swift, and Kotlin are scheduled in §6 using the same environment variable and semantics.

## 2. Decisions

| # | Decision | Why; what was dropped |
| --- | --- | --- |
| D1 | **Opt-in budget; unlimited by default**: add environment variable `RUTIS_ROW_UNLOAD_TIMEOUT` in **seconds** (positive finite floating-point number, parsed identically to `RUTIS_SHUTDOWN_TIMEOUT`). Unset, empty, `<= 0`, `NaN`, `Infinity`, or unparseable values disable the budget (unlimited). Also expose an explicit runtime option for tests and embedders: Node `run({ rowUnloadTimeout })` (ms) and `toCordis(..., unloadOptions)`, Bun `new Runtime(project, { rowUnloadTimeout })` (ms), Python `Runtime(row_unload_timeout=...)` (seconds) | Leaving the budget off by default preserves native Cordis and existing rutis behavior so legitimate slow teardowns are never cut short unexpectedly. Opt-in lets dev hot-reload, operators, and tests bound single-row unloads explicitly, and `RUTIS_ROW_UNLOAD_TIMEOUT=0` overrides an inherited variable cleanly. Dropped: enabling a default 10 s per-row budget out of the box (breaks slow legitimate cleanups); reusing `RUTIS_SHUTDOWN_TIMEOUT` directly (host shutdown deadline and single-row reload budget serve distinct operational needs) |
| D2 | **Budget bounds user cleanups only, never framework withdrawals** (extends [unload order](design-unload-order-2026-11-01.en.md) D8): the budget is one shared total budget across **all user cleanup steps** of a single row (`ctx.effect` registrations and the cleanup returned by `apply`). Framework withdrawals and teardown — `provide` withdrawals on the unload stack, listener withdrawal (`bus.withdraw`), exporter disposal (`exporter.dispose`), handle retirement and slot deletion (`gone` / `slots.delete`), row-table removal, and `provideWaitingLater` — are **never constrained by the budget and always run**. Step 4 (waiting for running listeners) keeps its existing `RUTIS_SHUTDOWN_TIMEOUT` cap ([events protocol](design-events-protocol-2026-10-18.en.md) §7.1); the single-row cleanup budget starts when draining the unload stack begins | Framework withdrawals are fast in-memory table updates and notifications that cannot hang; skipping them leaks stale service registrations and blocks healthy sibling rows or waiting host services from taking the name. Dropped: per-step budget resets (multiplies hang time by N); capping withdrawals (violates D8) |
| D3 | **On timeout: stop waiting on the stuck cleanup, skip all remaining user cleanups, still run every remaining framework withdrawal**: when a user cleanup exceeds the remaining budget (or remaining budget is `0` before the next user cleanup starts): (1) stop awaiting it (in Python, use `asyncio.wait` + `task.cancel()`, never `asyncio.wait_for`, which hangs if a coroutine traps `CancelledError`; in both JS and Python, attach a rejection/done handler so late failures never become unhandled rejections); (2) do **not** run any subsequent user cleanups remaining on the stack; (3) **do** run every remaining framework withdrawal on the stack and all `finally` teardown | You cannot claim both "unstick a hanging cleanup" and "preserve strict serial LIFO order" while the timed-out cleanup is still executing in the background: starting earlier-registered user cleanups races with the still-running outer cleanup. Framework withdrawals, by contrast, must run to keep tables consistent. Dropped: continuing remaining user cleanups; dropping the whole stack |
| D4 | **After a timeout, observe and report only; never block a reload**: after a timeout (1) finish the remaining framework withdrawals and `finally`; (2) write one verbatim diagnostic line (no internal words) to stderr; (3) `rows.unload` returns `null`; (4) subsequent `rows.load` / `rows.update` in the same process proceed as usual; the risk that the old cleanup is still running in the background belongs to the plugin author, and isolation means a dedicated runtime instance | Aligns with "plugins are trusted; no sandbox" ([remote plugins](design-remote-plugins-2026-10-03.en.md) §7) and "the platform only shows, it does not decide for the business" ([diagnostics design](design-diagnostics-2026-10-26.en.md) §2), with no state machine. Dropped: refusing reloads (a timed-out-marker state machine, scope keys, and an entry check in `rows.load`), because it makes the business decision "may this row reload" inside the runtime on the plugin author's behalf, and its cost (two registration tables, an entry check, plus a formal model) is disproportionate for an opt-in feature |
| D5 | **Isolate late actions from lingering cleanups and document synchronous-blocking limits honestly**: neither JS nor Python can forcibly kill a stuck Promise, a `CancelledError`-trapping coroutine, or synchronous blocking code, but the runtime guarantees a lingering cleanup's late operations on shared tables can never corrupt another healthy row or future state: (1) late `ctx.provide` is refused by the active/draining guard (Node `leaf.mjs` adds an explicit `if (draining)` guard; Bun and Python check active row membership); (2) late `ctx.effect`, `ctx.on`, and `ctx.emit` are refused; (3) an old `withdraw()` handle checks registration identity so it can never remove a sibling row's service of the same name; (4) late rejections/exceptions from the abandoned cleanup are absorbed and never mis-attributed to a live row. All four are pinned by unit tests. Timeout timers call `timer?.unref?.()` (and `clearTimeout` on normal completion) so they never pin event-loop exit. Synchronous infinite loops holding the single event-loop thread cannot be preempted by async timers and remain covered by the 5 s blocking watch ([diagnostics design](design-diagnostics-2026-10-26.en.md) §6.4) and `RUTIS_SHUTDOWN_TIMEOUT` watchdog | Honest about language limits while enforcing strict cross-row/cross-generation isolation with hard guards. Dropped: `SIGKILL`ing the entire shared runtime process on a single-row timeout (would kill unrelated healthy rows) |
| D6 | **Native Cordis fibers in Node without patching `_unload`**: when the opt-in budget is enabled, `runner.mjs` checks `LEAF_FIBERS.has(row.inner)` to separate leaf plugins from native Cordis plugins. Leaf plugins enforce the budget inside `leaf.mjs`'s `drain()` itself: on timeout it writes the diagnostic line to stderr directly through `unloadOptions.report` (default `say()`) and runs all remaining withdrawals before `fiber.dispose()` settles (`runner.mjs` merely `await`s `row.fiber.dispose()` for leaf rows — it neither times nor gets notified — and hangs no competing outer timer, so the outer one cannot fire first). Native Cordis fibers (`!LEAF_FIBERS.has(row.inner)`) race `row.fiber.dispose()` against the budget in both `unloadRow` and `dispose`; on timeout, `runner.mjs` stops waiting, attaches a no-op rejection handler to the pending `dispose()` promise, prints the native diagnostic line, and runs `finally` (`row.unloaded = true`, `bus.withdraw(key)`, `bus.forgetRow(key)`) plus `provideWaitingLater()`. Cordis 4.0.4's internal `Promise.all` cannot be interrupted, but Cordis removes the fiber from `ctx.registry` before `_unload()` and `ctx.provide`'s disposer deletes the service from `ctx.reflect.store` on its first microtask — so services, exporters, listeners, and `waitingHosts` are cleanly withdrawn | Preserves [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §7 ("no monkey-patching `_unload`"). Native Cordis `rows.update` combines unload and `apply` inside `fiber.update`, so racing `fiber.await()` would wrongly cap startup `apply`; only `rows.unload` and `dispose` are bounded for native Cordis fibers |
| D7 | **Schedule Go, Swift, and Kotlin as follow-up steps**: this design and PoC land Node, Bun, and Python first — the three runtimes that currently have zero cleanup budget even on `dispose`. Go, Swift, and Kotlin also lack single-row `rows.unload` budgets today and are scheduled in §6 using the same `RUTIS_ROW_UNLOAD_TIMEOUT` variable, timeout semantics, and diagnostic wording | Keeps the PoC focused on the three runtimes with zero cleanup budgets today while converging all six SDKs on one design |

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
   - Node: `run({ ..., rowUnloadTimeout })` takes milliseconds; `undefined` reads `env.RUTIS_ROW_UNLOAD_TIMEOUT`; `null`, `<= 0`, or non-finite explicitly disables. Standalone `toCordis(spec, rowInject, providerOf, unloadOptions)` accepts either an object `{ rowUnloadTimeout, runtimeName, rowName, report }`, where `rowUnloadTimeout` follows the same rule (omitting `unloadOptions` reads `process.env.RUTIS_ROW_UNLOAD_TIMEOUT`), or a function `unloadOptions(context)` returning `{ budgetMs, runtime, target, report }` per row (milliseconds; an invalid or missing `budgetMs` means unlimited and does not read the environment variable; `runtime` / `target` / `report` default to `node` / `plugin <name>` / `say()`). The `leafUnloadOptions` that `runner.mjs` passes is this function form: the runtime builds `target` from the row name and uses its own `say`.
   - Bun: `new Runtime(project, { rowUnloadTimeout })` takes milliseconds; omitting `rowUnloadTimeout` reads `process.env.RUTIS_ROW_UNLOAD_TIMEOUT`; `null`, `<= 0`, or non-finite explicitly disables.
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

### 3.3 Timeout state machine and verbatim outputs

1. **Budget disabled (default)**: runs the exact existing path — no timers, no wrapper tasks.
2. **Budget enabled on leaf unload stacks (Node leaf, Bun, Python)**:
   - Take the row's unload stack and iterate in reverse; each entry is tagged `withdrawal: true | false`.
   - When `withdrawal === true`: run unconditionally regardless of `timedOut` or remaining time.
   - When `withdrawal === false`:
     - If `timedOut` is already true: skip without invoking the cleanup.
     - Compute `remaining = max(0, endAt - now)`. If `remaining === 0`: set `timedOut = true`, emit the diagnostic line once, and skip this and all subsequent user cleanups.
     - Otherwise invoke the cleanup and wait at most `remaining`:
       - **Node leaf / Bun**: `Promise.race` between the cleanup Promise and `setTimeout(remaining)`; `clearTimeout` immediately when the cleanup settles first; if the timer wins, set `timedOut = true`, emit the diagnostic line once, and keep a no-op rejection handler on the stuck Promise.
       - **Python**: wrap the step in `asyncio.create_task` and await `asyncio.wait({task}, timeout=remaining)` (**never** `asyncio.wait_for`); if pending, call `task.cancel()`, attach `_consume_task_result`, set `timedOut = true`, and emit the diagnostic line once. Wrap the loop in `try / finally` so even external cancellation runs every remaining `withdrawal`.
3. **Node native Cordis fiber**:
   - In `unloadRow` and `dispose`, `runner.mjs` checks `LEAF_FIBERS.has(row.inner)`: when true it simply `await`s `row.fiber.dispose()` — `leaf.mjs`'s `drain()` enforces the budget itself, and on timeout writes the leaf diagnostic line through `unloadOptions.report` (default `say()`) and runs all remaining withdrawals, with `runner.mjs` neither timing nor being notified; when false, `runner.mjs` races `row.fiber.dispose()` against `setTimeout(budgetMs)`, and on timeout clears the timer, absorbs late rejections, prints the native diagnostic line once, and executes `finally` plus `provideWaitingLater()`.
4. **No state is recorded after a timeout**: the runtime keeps no table of "which rows may not reload", and `rows.load` / `rows.update` perform no entry check; loading the same row again in the same process proceeds as usual. A lingering cleanup's late operations on shared tables are still refused by the guards (see below).
5. **Lingering-cleanup isolation**:
   - Late `ctx.provide`: all three runtimes throw `row <row> is not active` (Node leaf says `plugin <plugin>`; Node `leaf.mjs` checks `if (draining)`, Bun and Python check that the row is still in `rows` and not unloading).
   - Late `ctx.on`: Bun and Python throw `row <row> is not active`; the Node leaf has no guard of its own and Cordis throws `cannot create effect on inactive context` when an effect is registered on a disposed Context (all three refuse and register nothing).
   - Late `ctx.effect`: none of the three throws; each writes one stderr line `rutis: row <row> is not active: cleanup is not registered` (Node leaf says `plugin`) and returns silently, leaving the cleanup unregistered (a registered one would never run anyway).
   - Late `ctx.emit`: bus dispatch treats the row as unloaded when it is no longer in `rows` (Node also keeps an `unloaded` set) and throws `BindingError` (`row <row> is not loaded in this runtime`).
   - All three `withdraw()` handles verify registration identity so a sibling row's same-named service is never removed.
6. **Formatting `<s>`**: no trailing `.0` — `String(Number((budgetMs / 1000).toPrecision(12)))` in JS/TS and `f"{budget:g}"` in Python (e.g. `5`, `0.25`, `0.05`).
7. **Verbatim outputs** (strictly compliant with [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5 — zero internal words):
   - **Timeout diagnostic lines** (written via `say(...)` to stderr without `[<row>] ` prefix):
     - Leaf rows in a runtime (Node leaf, Bun, Python):
       ```text
       runtime <runtime>: cleanup of row <row> did not finish within <s> s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)
       ```
     - Node native Cordis rows:
       ```text
       runtime <runtime>: cleanup of row <row> did not finish within <s> s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)
       ```
     - Standalone Node `toCordis` (no runner / no row table):
       ```text
       runtime <runtime>: cleanup of plugin <plugin> did not finish within <s> s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)
       ```

### 3.4 Cross-language matrix

| Runtime | Single-row `rows.unload` (`RUTIS_ROW_UNLOAD_TIMEOUT` set) | `dispose` | Status |
| --- | --- | --- | --- |
| Node (leaf) | `drain()` caps user cleanups to one total budget; skips remaining cleanups; withdrawals always run | Applies same per-row budget (plus `RUTIS_SHUTDOWN_TIMEOUT` on disconnect) | PoC implemented |
| Node (native Cordis) | Races `row.fiber.dispose()`; prints native diagnostic and runs `finally` teardown | Same as above | PoC implemented |
| Bun | `unload` caps user cleanups to one total budget; skips remaining cleanups; withdrawals always run | `dispose` calls `unload`, automatically covered | PoC implemented |
| Python | `unload` uses `asyncio.wait` + `cancel()`; skips remaining cleanups; withdrawals always run | `dispose` calls `unload`, automatically covered | PoC implemented |
| Kotlin | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unloadRow` (reusing existing per-entry timing + D8 withdrawals) | Already has `dispose(shutdownMillis)` | §6 follow-up |
| Go | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unload` with D8 withdrawal preservation | Already has `dispose(ctx)` | §6 follow-up |
| Swift | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unload` with D8 withdrawal preservation | Already has `dispose(within:)` | §6 follow-up |

The three runtimes behave identically on timeout: stop waiting when the budget expires, skip the remaining user cleanups, finish the framework withdrawals (D8), write one diagnostic line, and allow same-process reloads afterwards as usual.

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

Output on unload: **no extra output at all**. Both cleanups run in reverse order one at a time, `forecast` is withdrawn at its stack position, no timeout diagnostic is printed, and reloading works normally. Python (`define_plugin` + `async def` cleanups) and native Cordis plugins (`export function apply(ctx)`) differ only in plugin API syntax and likewise emit nothing extra when cleanups finish on time.

#### When it fails / times out

| Trigger | Verbatim output (stderr or error message) | Host error code / recorded on | How to fix |
| --- | --- | --- | --- |
| A Node leaf row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime node: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)` | `rows.unload` returns normally (no error code), row leaves `Unloading` and services are withdrawn; diagnostic written on runtime stderr for `row weather` | Find the never-settling `await` (or circular wait between two `ctx.effect`s), honour `ctx.signal`, or add an internal timeout |
| Standalone Node `toCordis` (no row name, plugin `weather`) times out at 50 ms | `runtime node: cleanup of plugin weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)` | No host; recorded on stderr for `plugin weather` | Same as above |
| A Bun row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime bun: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)` | `rows.unload` returns normally (no error code); recorded on runtime stderr for `row weather` | Same as above |
| A Python row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` (including a coroutine trapping `CancelledError`) | `runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)` | `rows.unload` returns normally (no error code); recorded on runtime stderr for `row weather`; the task was sent `cancel()` | Do not swallow `asyncio.CancelledError` and wait indefinitely; bound async closes with `asyncio.wait_for` |
| A Node native Cordis plugin row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime node: cleanup of row weather did not finish within 5 s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)` | `rows.unload` returns normally (no error code); recorded on runtime stderr for `row weather`; services, listeners, and exporters are withdrawn | Fix the hanging `ctx.effect` disposer in the native plugin |
| Loading row `weather` again after a timeout (same runtime process; Node / Bun / Python) | No extra output; loads as usual | `rows.load` returns normally; the old cleanup may still be running, and if it conflicts with the new generation (ports, connection pools, etc.) that shows up as the plugin's own error | If the two generations conflict, fix the cleanup or put the row in its own runtime instance |
| An earlier cleanup throws (`flush failed`) before a later cleanup times out | Both the verbatim timeout line above and the earlier cleanup's error (propagated in Bun/Python, logged via `context.logger.error` in Node leaf) | Host gets the earlier cleanup's error (`site.doing = "cleanup"`, recorded on `row weather`); the timeout does not mask it | Fix both the throwing cleanup and the hanging cleanup |
| A cleanup runs a synchronous infinite loop or blocking call (`while (true) {}`, `time.sleep(60)`) | Async timer cannot preempt the single event-loop thread, so no per-row timeout line fires while blocked; after 5 s the existing blocking watch fires, and on host stop the `RUTIS_SHUTDOWN_TIMEOUT` watchdog terminates the process | Reported by the blocking watch / runtime exit | Make the cleanup asynchronous (`await` async I/O, or `asyncio.to_thread` in Python) |

### 4.2 Users assembling plugins with `rutis-host`

#### When it works

- `RUTIS_ROW_UNLOAD_TIMEOUT` unset: behavior is identical to 0.23.0.
- Opting in for dev or run:

```bash
RUTIS_ROW_UNLOAD_TIMEOUT=5 rutis-host dev
```

When cleanups finish within 5 s, terminal output is completely unchanged.

#### When it fails / times out

On a hot reload or row removal in `rutis.json` where `weather`'s cleanup hangs for 5 s:

1. Stderr prints one unprefixed diagnostic line (no `[weather]` prefix, as it is the runtime's own diagnostic):
   ```text
   runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it, or isolate this row in its own runtime)
   ```
2. `weather` leaves `Unloading` immediately and unrelated sibling rows keep serving;
3. If `dev` immediately reloads the updated `weather` in the same runtime process, it loads as usual and does not fail. The old cleanup may still be running at that point; if the two generations conflict (ports, connection pools, etc.), that shows up as the plugin's own error. Fix the cleanup and reload; to avoid the overlap entirely, put the row in its own runtime instance.

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

- `process.unload_row(&key)` resolves with `Ok(())` promptly once the budget expires (in Step 1 PoC, `rows.unload` still returns `null`), keeping host reconciliation and healthy sibling rows unblocked; note that if a row is permanently removed without reloading, Step 1 surfaces the timeout only on runtime stderr until Step 2 wires host-observable timeout reporting;
- Any subsequent `rows.load` (or Bun/Python `rows.update`) for the same scoped row in the same runtime process loads as usual and the host receives no refusal; the old cleanup may still be running in the background, and whether the two generations conflict is up to the plugin itself;
- Step 2 host integration will surface per-row unload timeouts explicitly to the host (including remove-without-reload visibility); whether to reload, and when to restart that runtime, is decided by the host and operations under their own policy.

### 4.4 Deployers

#### When it works

Configure `RUTIS_ROW_UNLOAD_TIMEOUT` alongside `RUTIS_SHUTDOWN_TIMEOUT` in the container, systemd unit, or remote runtime environment:

```bash
RUTIS_SHUTDOWN_TIMEOUT=15
RUTIS_ROW_UNLOAD_TIMEOUT=5
```

Keep the per-row budget smaller than the shutdown budget (e.g. 5 s vs 15 s) so a single hanging row during shutdown still leaves time for other rows to clean up. Set `RUTIS_ROW_UNLOAD_TIMEOUT=0` or unset it to disable an inherited budget.

#### When it fails / times out

Alert on log lines matching `cleanup of row ` + ` did not finish within `. After a row times out, the remaining healthy rows carry on as usual; to reclaim the leftover work for good, restart that runtime in a maintenance window or split the row into its own runtime.

### 4.5 Concepts to learn (against the #663 concept budget)

Against the per-layer concept budget recorded in [#663](https://github.com/arcships/rutis/issues/663) (7 for plugin authors / ~9 while debugging, 6 for `rutis-host` users, 10 for host owners, up to +3 for debugging/ops):

- **Plugin authors**: **0 new concepts**. They already know "cleanup" and "unload stack" ([unload order](design-unload-order-2026-11-01.en.md)); the budget is opt-in and invisible unless a cleanup hangs, in which case the one self-explanatory message (timeout line) states the cause and fix in plain words with no internal terminology.
- **`rutis-host` users, host owners, deployers**: **+1 optional environment variable** (`RUTIS_ROW_UNLOAD_TIMEOUT`), mirroring `RUTIS_SHUTDOWN_TIMEOUT` in unit and parsing rules. It fits inside the debugging/ops slot with no new conceptual layer and stays within budget.

## 5. Compatibility and migration

- **100% backwards compatible by default**: with `RUTIS_ROW_UNLOAD_TIMEOUT` unset, Node (leaf and native Cordis), Bun, and Python behave identically to before. No breaking change; no migration-guide breaking entry is needed.
- **No protocol change in this step**: wire frames, protocol version, control methods, and `rutis/*` events are unchanged.
  - **New runtime with old host**: works seamlessly; if `RUTIS_ROW_UNLOAD_TIMEOUT` is set in the runtime environment, the runtime enforces the budget locally and completes withdrawals.
  - **Old runtime with new host**: works seamlessly; the old runtime ignores `RUTIS_ROW_UNLOAD_TIMEOUT` and keeps waiting indefinitely.
- **Behavior when opted in**: hanging cleanups no longer block `rows.unload` forever; subsequent user cleanups on that row are skipped, all framework withdrawals still run, and one verbatim diagnostic line is written to stderr.

## 6. Implementation

| Step | Scope & changes | Done criteria |
| --- | --- | --- |
| 1. This design & PoC ([#1323](https://github.com/arcships/rutis/issues/1323), current branch) | Bilingual design docs `docs/design-row-unload-budget-2026-11-20.{md,en.md}`; per-row unload budget PoC + tests in `node/rutis-runtime/src/{leaf,runner}.mjs`, `bun/rutis-bun/src/runner.ts`, `python/rutis/rutis/runner.py` | All three unit test suites verify default behavior, verbatim timeout output, skipped later cleanups, D8 withdrawals, multi-instance same-`id` different-`scope` isolation, unaffected healthy siblings, lingering-action isolation, and reload allowed after timeout; link/constraint checks pass |
| 2. Host-observable timeout & guide integration (follow-up issue) | Surface per-row unload timeout results explicitly to the host (including remove-without-reload visibility); whether to reload, and when to restart that runtime, is decided by the host and operations under their own policy; update `docs/guide/rutis-host.{md,en.md}`, `docs/guide/cordis-boundaries.{md,en.md}` (§7–§8), and `docs/design-events-protocol-2026-10-18.{md,en.md}` (§7.1); optional `--row-unload-timeout` flag in `rutis-host`; end-to-end `crates/rutis-loader` tests; register in `docs/guarantees.md` once promoted past PoC | E2E tests lock verbatim timeout output and reload-after-timeout across all three runtimes; bilingual guides updated |
| 3. Go / Swift / Kotlin alignment (follow-up issue) | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into single-row `rows.unload` in Go (`go/rutis/internal/runtime/runtime.go`), Swift (`swift/rutis/Sources/Rutis/API/Runtime.swift`), and Kotlin (`kotlin/rutis/.../internal/Runtime.kt`), preserving D8 withdrawals and the same timeout semantics, with identical verbatim wording | All six SDKs share the same env var, D8 withdrawal invariant, timeout semantics, and verbatim diagnostic wording |

## 7. Level, risk, and verification

1. **Level** (against §3): D2 "framework withdrawals must run" and D3 "skip the remaining user cleanups after a timeout" are **contract behavior** (Q3.2) — writing them into this design, pinning them with the three runtimes' tests, and registering them in the guarantee table (§9.4) are follow-up steps; D4 "after a timeout, observe and report only" is **contract behavior**; "late actions from lingering cleanups are isolated" (D5) is **contract behavior**; "synchronous blocking cannot be preempted by the budget and is covered by the blocking watch and `RUTIS_SHUTDOWN_TIMEOUT`" is **explicitly not guaranteed** (Q3.4), because it is a language boundary — synchronous code on a single-threaded event loop cannot be preempted by an async timer; "the stuck cleanup keeps running in the background after the budget expires" is likewise **explicitly not guaranteed**. Unlimited by default (the compatibility section, §5) is **contract behavior**.
2. **Risk assessment** (against §4, by impact — high / medium / low):
   - High: none. The D8 withdrawal invariant is pinned by the three runtimes' tests; a mutation that skips withdrawals was already caught by unit tests in the previous revision.
   - Medium: after a timeout the old cleanup and the new generation run concurrently in the same process, contending for ports / connection pools / file locks — discoverable (the plugin itself errors) and avoidable (isolate the row in its own runtime), which is why it is "medium" and not "high".
   - Low: the diagnostic line goes to stderr only, so `rutis-host status` on the host side cannot see it (closed in §6 step 2).
3. **Component type** (against §6): this design changes the unload path of the **language runtimes** (Q6.8) and the `ctx.effect` / `ctx.provide` guards of the **plugin SDK** (Q6.7). Q6.8's "runtime behavior is consistent across languages" is met by the three runtimes sharing one verbatim output and one test matrix; Q6.7's "SDK behavior has documentation and tests" is met by §4 and the three runtimes' unload-budget tests.
4. **Model checking**: per Q11.2.2 this design is sequential logic within a single process (reverse-order drain of the unload stack plus one wall-clock budget) and does not use design model checking; the order-sensitive part (withdrawals still run at their stack positions after a timeout) is pinned by the three runtimes' unit tests. The previous revision had a `specs/row-unload-budget/` TLA+ model; it verified the abandoned refusal state machine and was deleted together with that decision.
