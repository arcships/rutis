# Per-row unload budget and diagnostics for Node, Bun, and Python

[中文](design-row-unload-budget-2026-11-20.md)

Status: draft, PoC and TLA+ model implemented (branch `design/1323-unload-budget`, not merged). Date: 2026-11-20 (same-day revision: refined to Route B per design principles §7 — explicit taint refusing silent in-place reload in the same runtime process until restart — with semantic-state analysis and the `specs/row-unload-budget/` TLA+ model + TLC results). Base: `main` `5c2e143e` (after 0.23.0).
Issue: [#1323](https://github.com/arcships/rutis/issues/1323). Related: [design principles](design-philosophy.en.md) (§7), [unload order](design-unload-order-2026-11-01.en.md) (D7, D8, §3, §6), [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) (§7, §8), [diagnostics design](design-diagnostics-2026-10-26.en.md) (§4.5, §6), [events protocol](design-events-protocol-2026-10-18.en.md) (§7.1), [TLA+ model](../specs/row-unload-budget/README.en.md).

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

### 1.2 The semantic state introduced by timeout: `stale_cleanup_running`

Inside a shared runtime process (multiple rows sharing one Node, Bun, or Python process), **returning after a cleanup timeout is NOT the same semantic state as finishing an unload cleanly**:

- **Clean unload**: every user cleanup of the old generation has genuinely terminated; zero code from generation $g$ remains running in the process. Loading generation $g+1$ next naturally preserves **at most one active generation of a row per shared process**.
- **Timed-out unload (`stale_cleanup_running`)**: JavaScript cannot forcibly kill an unsettled Promise, and Python cannot forcibly kill a coroutine that traps `asyncio.CancelledError` or blocks inside external calls:
  - On a **leaf unload stack** (Node leaf, Bun, Python), the runtime stops waiting on the hanging cleanup and skips later user cleanups, yet **the timed-out cleanup itself remains alive in the background**;
  - On a **Node native Cordis fiber**, Cordis 4.0.4's `_unload()` starts all disposables concurrently via `Promise.all` (which cannot be interrupted), so **multiple disposables from that fiber may still be running in the background**.

Pretending the row unloaded cleanly and silently allowing an in-place reload in the same runtime process (**Route A**) silently degrades the single-generation invariant into a hidden race: generation $g$'s lingering cleanup and generation $g+1$'s `apply` / service methods execute concurrently in the same process, colliding over listening ports, file locks, module-level singletons, globals, and connection pools. That directly violates two principles of [design principles](design-philosophy.en.md) §7: **"write down what cannot cross a boundary as a boundary rule; never silently degrade"** and **"explicit over implicit"**.

We therefore compare both routes and adopt **Route B**:

| Dimension | Route A (silent in-place reload in same runtime — dropped) | Route B (explicit taint refusing in-place reload until runtime restart — adopted) |
| --- | --- | --- |
| Framework withdrawals (D8) | Complete service/listener/slot withdrawals | Complete service/listener/slot withdrawals |
| Unrelated healthy rows | Continue running | Continue running, completely unaffected |
| Subsequent `rows.load` of the same row in the same runtime | Starts generation $g+1$ concurrently with lingering cleanup (TLC `budget-route-a` violates `NoConcurrentSameRowInSharedProcess`) | **Fails explicitly** with a clear error saying the previous cleanup timed out and may still be running, advising runtime restart or row isolation (TLC `budget-route-b` & `budget-route-b-cordis` pass all invariants) |
| Recovery path | Hope old & new generations do not collide on external state | Restart the runtime process (or isolate the row in its own runtime) and load cleanly |
| Step 2 host policy integration | Host cannot tell clean unload from degraded unload | Runtime enforces no-silent-degradation immediately; Step 2 adds host-observable timeout status and optional policy (`taint` vs auto `restart-runtime`) |

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

This design delivers the per-row unload budget, Route B taint semantics, and TLA+ model for Node, Bun, and Python first; Go, Swift, and Kotlin are scheduled in §6 using the same environment variable and semantics.

## 2. Decisions

| # | Decision | Why; what was dropped |
| --- | --- | --- |
| D1 | **Opt-in budget; unlimited by default**: add environment variable `RUTIS_ROW_UNLOAD_TIMEOUT` in **seconds** (positive finite floating-point number, parsed identically to `RUTIS_SHUTDOWN_TIMEOUT`). Unset, empty, `<= 0`, `NaN`, `Infinity`, or unparseable values disable the budget (unlimited). Also expose an explicit runtime option for tests and embedders: Node `run({ rowUnloadTimeout })` (ms) and `toCordis(..., unloadOptions)`, Bun `new Runtime(project, { rowUnloadTimeout })` (ms), Python `Runtime(row_unload_timeout=...)` (seconds) | Leaving the budget off by default preserves native Cordis and existing rutis behavior so legitimate slow teardowns are never cut short unexpectedly. Opt-in lets dev hot-reload, operators, and tests bound single-row unloads explicitly, and `RUTIS_ROW_UNLOAD_TIMEOUT=0` overrides an inherited variable cleanly. Dropped: enabling a default 10 s per-row budget out of the box (breaks slow legitimate cleanups); reusing `RUTIS_SHUTDOWN_TIMEOUT` directly (host shutdown deadline and single-row reload budget serve distinct operational needs) |
| D2 | **Budget bounds user cleanups only, never framework withdrawals** (extends [unload order](design-unload-order-2026-11-01.en.md) D8): the budget is one shared total budget across **all user cleanup steps** of a single row (`ctx.effect` registrations and the cleanup returned by `apply`). Framework withdrawals and teardown — `provide` withdrawals on the unload stack, listener withdrawal (`bus.withdraw`), exporter disposal (`exporter.dispose`), handle retirement and slot deletion (`gone` / `slots.delete`), row-table removal, and `provideWaitingLater` — are **never constrained by the budget and always run**. Step 4 (waiting for running listeners) keeps its existing `RUTIS_SHUTDOWN_TIMEOUT` cap ([events protocol](design-events-protocol-2026-10-18.en.md) §7.1); the single-row cleanup budget starts when draining the unload stack begins | Framework withdrawals are fast in-memory table updates and notifications that cannot hang; skipping them leaks stale service registrations (caught by TLC `budget-mut-skip-withdraw` violating `NoOrphanWithdrawal`) and blocks healthy sibling rows or waiting host services from taking the name. Dropped: per-step budget resets (multiplies hang time by N); capping withdrawals (violates D8) |
| D3 | **On timeout: stop waiting on the stuck cleanup, skip all remaining user cleanups, still run every remaining framework withdrawal**: when a user cleanup exceeds the remaining budget (or remaining budget is `0` before the next user cleanup starts): (1) stop awaiting it (in Python, use `asyncio.wait` + `task.cancel()`, never `asyncio.wait_for`, which hangs if a coroutine traps `CancelledError`; in both JS and Python, attach a rejection/done handler so late failures never become unhandled rejections); (2) do **not** run any subsequent user cleanups remaining on the stack; (3) **do** run every remaining framework withdrawal on the stack and all `finally` teardown | You cannot claim both "unstick a hanging cleanup" and "preserve strict serial LIFO order" while the timed-out cleanup is still executing in the background (caught by TLC `budget-mut-continue-serial` violating `SerialCleanupsWithinGen`): starting earlier-registered user cleanups races with the still-running outer cleanup. Framework withdrawals, by contrast, must run to keep tables consistent. Dropped: continuing remaining user cleanups; dropping the whole stack |
| D4 | **Adopt Route B: mark the timed-out row tainted in this runtime process, refusing silent in-place reload until runtime restart; complete `rows.unload` normally with one verbatim stderr diagnostic line**: on timeout: (1) record the row's id and key in this runtime's `tainted` map, complete all remaining framework withdrawals and `finally` teardown, and return normally from `rows.unload` (preserving any pre-timeout cleanup exception as before); (2) write one verbatim unprefixed stderr diagnostic line (§3 & §4, zero internal words); (3) any subsequent `rows.load` (or Bun/Python `rows.update`) for that row in the **same runtime process** — whether reusing the old key or a fresh session key — **fails explicitly** with a plain developer-readable error until the runtime restarts or the row is isolated in its own runtime; (4) unrelated healthy rows in the same runtime continue completely unaffected; (5) Step 2 adds host-observable timeout policy (`taint` in `rutis-host status` or opt-in auto `restart-runtime`) | Directly honours [design principles](design-philosophy.en.md) §7 ("explicit over implicit; no silent degradation"), and TLC proves `NoConcurrentSameRowInSharedProcess` holds under Route B and fails under Route A. Completing `rows.unload` cleanly unblocks host teardown and healthy siblings while explicit taint prevents silent dual-generation execution. Dropped: Route A (allows silent concurrent old cleanup + new generation) |
| D5 | **Isolate late actions from lingering cleanups and document synchronous-blocking limits honestly**: neither JS nor Python can forcibly kill a stuck Promise, a `CancelledError`-trapping coroutine, or synchronous blocking code, but the runtime guarantees a lingering cleanup can never corrupt another healthy row or future state (TLC `NoCrossGenCorruption`): (1) late `ctx.provide` is refused (Node `leaf.mjs` adds an explicit `if (draining)` guard; Bun and Python check active row membership); (2) late `ctx.effect`, `ctx.on`, and `ctx.emit` are refused; (3) an old `withdraw()` handle checks registration identity so it can never remove a sibling row's service of the same name; (4) late rejections/exceptions from the abandoned cleanup are absorbed rather than crashing the runtime or blaming another row. Synchronous infinite loops holding the single event-loop thread cannot be preempted by async timers and remain covered by the 5 s blocking watch ([diagnostics design](design-diagnostics-2026-10-26.en.md) §6.4) and `RUTIS_SHUTDOWN_TIMEOUT` watchdog | Honest about language limits while enforcing strict cross-row/cross-generation isolation backed by TLA+ and tests. Dropped: `SIGKILL`ing the entire shared runtime process on a single-row timeout (would kill unrelated healthy rows) |
| D6 | **Native Cordis fibers in Node without patching `_unload`**: when the opt-in budget is enabled, `runner.mjs` checks `LEAF_FIBERS.has(row.inner)` to separate leaf plugins from native Cordis plugins. Leaf plugins enforce the budget inside `leaf.mjs`'s `drain()`, invoke `onTimeout` so `runner.mjs` taints the row, and run all remaining withdrawals before `fiber.dispose()` settles (with no competing outer timer). Native Cordis fibers (`!LEAF_FIBERS.has(row.inner)`) race `row.fiber.dispose()` against the budget in both `unloadRow` and `dispose`; on timeout, `runner.mjs` stops waiting, taints the row, attaches a no-op rejection handler to the pending `dispose()` promise, prints the native diagnostic line, and runs `finally` (`row.unloaded = true`, `bus.withdraw(key)`, `bus.forgetRow(key)`) plus `provideWaitingLater()`. Cordis 4.0.4's internal `Promise.all` cannot be interrupted, but Cordis removes the fiber from `ctx.registry` before `_unload()` and `ctx.provide`'s disposer deletes the service from `ctx.reflect.store` on its first microtask — so services, exporters, listeners, and `waitingHosts` are cleanly withdrawn | Preserves [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §7 ("no monkey-patching `_unload`") while giving native Cordis rows the same Route B taint guarantee (verified by TLC `budget-route-b-cordis`). Native Cordis `rows.update` combines unload and `apply` inside `fiber.update`, so racing `fiber.await()` would wrongly cap startup `apply`; only `rows.unload` and `dispose` are bounded for native Cordis fibers |
| D7 | **Schedule Go, Swift, and Kotlin as follow-up steps**: this design, PoC, and TLA+ model land Node, Bun, and Python first — the three runtimes that currently have zero cleanup budget even on `dispose`. Go, Swift, and Kotlin also lack single-row `rows.unload` budgets today and are scheduled in §6 using the same `RUTIS_ROW_UNLOAD_TIMEOUT` variable, Route B taint semantics, and diagnostic wording | Keeps the PoC focused on the three runtimes with zero cleanup budgets today while converging all six SDKs on one design |

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
| Taint registration (`tainted` map records row id and key) | **No** | **Yes** (written on timeout) |
| Step 4 waiting for running listeners | **No** (keeps `RUTIS_SHUTDOWN_TIMEOUT`) | Keeps §7.1 rule |

### 3.3 Timeout state machine, taint rules, and verbatim outputs

1. **Budget disabled (default)**: runs the exact existing path — no timers, no wrapper tasks, no taint entries.
2. **Budget enabled on leaf unload stacks (Node leaf, Bun, Python)**:
   - Take the row's unload stack and iterate in reverse; each entry is tagged `withdrawal: true | false`.
   - When `withdrawal === true`: run unconditionally regardless of `timedOut` or remaining time.
   - When `withdrawal === false`:
     - If `timedOut` is already true: skip without invoking the cleanup.
     - Compute `remaining = max(0, endAt - now)`. If `remaining === 0`: set `timedOut = true`, taint the row, emit the diagnostic line once, and skip this and all subsequent user cleanups.
     - Otherwise invoke the cleanup and wait at most `remaining`:
       - **Node leaf / Bun**: `Promise.race` between the cleanup Promise and `setTimeout(remaining)`; `clearTimeout` immediately when the cleanup settles first; if the timer wins, set `timedOut = true`, taint the row, emit the diagnostic line once, and keep a no-op rejection handler on the stuck Promise.
       - **Python**: wrap the step in `asyncio.create_task` and await `asyncio.wait({task}, timeout=remaining)` (**never** `asyncio.wait_for`); if pending, call `task.cancel()`, attach `_consume_task_result`, set `timedOut = true`, taint the row, and emit the diagnostic line once. Wrap the loop in `try / finally` so even external cancellation runs every remaining `withdrawal`.
3. **Node native Cordis fiber**:
   - In `unloadRow` and `dispose`, `runner.mjs` checks `LEAF_FIBERS.has(row.inner)`: when true, `leaf.mjs`'s `drain()` enforces the budget and calls `onTimeout` to taint the row; when false, `runner.mjs` races `row.fiber.dispose()` against `setTimeout(budgetMs)`, and on timeout clears the timer, absorbs late rejections, taints the row, prints the native diagnostic line once, and executes `finally` plus `provideWaitingLater()`.
4. **Route B taint keys and load refusal**:
   - On timeout, the runtime records **both** the row's display name (`place.id` / `id`) and its session key (`key`) in the process's `tainted` map (storing the display row name and budget).
   - Every subsequent `rows.load` (and Bun/Python `rows.update`) checks `tainted` first: if either the row name or key matches, it throws immediately without mutating `rows`, `slots`, or services, and without importing or executing the plugin.
   - Unrelated healthy rows load, take over withdrawn service names, run, and unload completely unaffected.
   - Restarting the runtime process starts with an empty `tainted` map, allowing the previously tainted row to load cleanly.
5. **Lingering-cleanup isolation**:
   - Node `leaf.mjs` `ctx.provide` checks `if (draining) throw new Error(...)`; Bun and Python refuse `provide`, `on`, and `effect` for inactive rows; all three refuse `emit` et al. once unloaded; all three `withdraw()` handles verify registration identity before removing an entry.
6. **Formatting `<s>`**: no trailing `.0` — `String(Number((budgetMs / 1000).toPrecision(12)))` in JS/TS and `f"{budget:g}"` in Python (e.g. `5`, `0.25`, `0.05`).
7. **Verbatim outputs** (strictly compliant with [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5 — zero internal words):
   - **Timeout diagnostic lines** (written via `say(...)` to stderr without `[<row>] ` prefix):
     - Leaf rows in a runtime (Node leaf, Bun, Python):
       ```text
       runtime <runtime>: cleanup of row <row> did not finish within <s> s; skipped remaining cleanups and withdrew its services; reloading row <row> in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)
       ```
     - Node native Cordis rows:
       ```text
       runtime <runtime>: cleanup of row <row> did not finish within <s> s; stopped waiting and finished unloading the row; reloading row <row> in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)
       ```
     - Standalone Node `toCordis` (no runner / no row table):
       ```text
       runtime <runtime>: cleanup of plugin <plugin> did not finish within <s> s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)
       ```
   - **Explicit refusal error when reloading a tainted row in the same runtime (Node, Bun, Python)**:
     ```text
     row <row> cannot be loaded in runtime <runtime>: its previous cleanup did not finish within <s> s and may still be running; restart the runtime (or isolate this row in its own runtime) before loading it again
     ```

### 3.4 Cross-language and formal verification matrix

| Runtime | Single-row `rows.unload` (`RUTIS_ROW_UNLOAD_TIMEOUT` set) | In-place reload after timeout (Route B) | `dispose` | Status |
| --- | --- | --- | --- | --- |
| Node (leaf) | `drain()` caps user cleanups to one total budget; skips remaining cleanups; withdrawals always run | Explicitly refused until runtime restart; healthy siblings unaffected | Applies same per-row budget (plus `RUTIS_SHUTDOWN_TIMEOUT` on disconnect) | PoC + TLA+ verified |
| Node (native Cordis) | Races `row.fiber.dispose()`; prints native diagnostic and runs `finally` teardown | Same as above | Same as above | PoC + TLA+ verified |
| Bun | `unload` caps user cleanups to one total budget; skips remaining cleanups; withdrawals always run | Same as above | `dispose` calls `unload`, automatically covered | PoC + TLA+ verified |
| Python | `unload` uses `asyncio.wait` + `cancel()`; skips remaining cleanups; withdrawals always run | Same as above | `dispose` calls `unload`, automatically covered | PoC + TLA+ verified |
| Kotlin | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unloadRow` (reusing existing per-entry timing + D8 withdrawals) | Same as above | Already has `dispose(shutdownMillis)` | §6 follow-up |
| Go | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unload` with D8 withdrawal preservation | Same as above | Already has `dispose(ctx)` | §6 follow-up |
| Swift | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into `unload` with D8 withdrawal preservation | Same as above | Already has `dispose(within:)` | §6 follow-up |

**TLA+ formal verification results** ([`specs/row-unload-budget/`](../specs/row-unload-budget/README.en.md), TLC 2.19):
- `budget-route-b` (Route B leaf mode): **pass** (1,185 distinct states, 1 s) — all 5 invariants (`TypeOK`, `NoOrphanWithdrawal`, `SerialCleanupsWithinGen`, `NoCrossGenCorruption`, `NoConcurrentSameRowInSharedProcess`) hold;
- `budget-route-b-cordis` (Route B Node native Cordis mode): **pass** (702 distinct states, <1 s) — all 5 invariants hold;
- `budget-route-a` (Route A allow in-place reload): **violates `NoConcurrentSameRowInSharedProcess`** (198 distinct states to counterexample, 1 s);
- All 3 deliberate mutations (`budget-mut-skip-withdraw`, `budget-mut-continue-serial`, `budget-mut-corrupt-new-gen`) catch `NoOrphanWithdrawal`, `SerialCleanupsWithinGen`, and `NoCrossGenCorruption` respectively, matching `expected.txt`.

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

Output on unload: **no extra output at all**. Both cleanups run in reverse order one at a time, `forecast` is withdrawn at its stack position, the row is not tainted, and reloading works normally. Python (`define_plugin` + `async def` cleanups) and native Cordis plugins (`export function apply(ctx)`) differ only in plugin API syntax and likewise emit nothing extra when cleanups finish on time.

#### When it fails / times out

| Trigger | Verbatim output (stderr or error message) | Host error code / recorded on | How to fix |
| --- | --- | --- | --- |
| A Node leaf row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime node: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` returns normally (no error code), row leaves `Unloading` and services are withdrawn; diagnostic written on runtime stderr for `row weather` | Find the never-settling `await` (or circular wait between two `ctx.effect`s), honour `ctx.signal`, or add an internal timeout; restart the runtime before loading again |
| Standalone Node `toCordis` (no row name, plugin `weather`) times out at 50 ms | `runtime node: cleanup of plugin weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | No host; recorded on stderr for `plugin weather` | Same as above |
| A Bun row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime bun: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` returns normally (no error code); recorded on runtime stderr for `row weather` | Same as above |
| A Python row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` (including a coroutine trapping `CancelledError`) | `runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` returns normally (no error code); recorded on runtime stderr for `row weather`; the task was sent `cancel()` | Do not swallow `asyncio.CancelledError` and wait indefinitely; bound async closes with `asyncio.wait_for`; restart the runtime before loading again |
| A Node native Cordis plugin row `weather` has a cleanup that does not return within `RUTIS_ROW_UNLOAD_TIMEOUT=5` | `runtime node: cleanup of row weather did not finish within 5 s; stopped waiting and finished unloading the row; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` returns normally (no error code); recorded on runtime stderr for `row weather`; services, listeners, and exporters are withdrawn | Fix the hanging `ctx.effect` disposer in the native plugin; restart the runtime before loading again |
| Attempting to load tainted row `weather` again in the same runtime process (same key or fresh key; Node / Bun / Python) | `row weather cannot be loaded in runtime py: its previous cleanup did not finish within 5 s and may still be running; restart the runtime (or isolate this row in its own runtime) before loading it again` (`runtime node` / `runtime bun` respectively) | `rows.load` fails (`Error` / `RuntimeError`, recorded on `row weather`); unrelated healthy sibling rows continue unaffected | Fix the plugin's cleanup, then restart that runtime process (or isolate the row in its own runtime) and load again |
| An earlier cleanup throws (`flush failed`) before a later cleanup times out | Both the verbatim timeout line above and the earlier cleanup's error (propagated in Bun/Python, logged via `context.logger.error` in Node leaf), and the row is also tainted | Host gets the earlier cleanup's error (`site.doing = "cleanup"`, recorded on `row weather`); the timeout does not mask it | Fix both the throwing cleanup and the hanging cleanup, then restart the runtime |
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
   runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)
   ```
2. `weather` leaves `Unloading` immediately and unrelated sibling rows keep serving;
3. If `dev` immediately attempts to load the updated `weather` in the same runtime process, loading fails explicitly with:
   ```text
   row weather cannot be loaded in runtime py: its previous cleanup did not finish within 5 s and may still be running; restart the runtime (or isolate this row in its own runtime) before loading it again
   ```
   rather than silently running the updated plugin alongside its stuck old cleanup. Fix the cleanup and restart `rutis-host` (or that runtime process) to reload cleanly.

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

- `process.unload_row(&key)` resolves with `Ok(())` promptly once the budget expires, keeping host reconciliation and healthy sibling rows unblocked;
- Any subsequent `rows.load` for that row in the same runtime process fails explicitly with the verbatim refusal error above, preventing silent dual-generation execution;
- Step 2 host integration will surface timeout state explicitly to the host with configurable policy (default `taint` displayed in `rutis-host status`, or opt-in `restart-runtime` to recycle the tainted runtime process automatically).

### 4.4 Deployers

#### When it works

Configure `RUTIS_ROW_UNLOAD_TIMEOUT` alongside `RUTIS_SHUTDOWN_TIMEOUT` in the container, systemd unit, or remote runtime environment:

```bash
RUTIS_SHUTDOWN_TIMEOUT=15
RUTIS_ROW_UNLOAD_TIMEOUT=5
```

Keep the per-row budget smaller than the shutdown budget (e.g. 5 s vs 15 s) so a single hanging row during shutdown still leaves time for other rows to clean up. Set `RUTIS_ROW_UNLOAD_TIMEOUT=0` or unset it to disable an inherited budget.

#### When it fails / times out

Alert on log lines matching `cleanup of row ` + ` did not finish within ` and `cannot be loaded in runtime `. Healthy sibling rows remain online after a row becomes tainted; schedule a restart of that runtime process (or isolate the offending plugin in its own dedicated runtime) to reclaim the lingering background work and reload the row cleanly.

### 4.5 Concepts to learn (against the #663 concept budget)

Against the per-layer concept budget recorded in [#663](https://github.com/arcships/rutis/issues/663) (7 for plugin authors / ~9 while debugging, 6 for `rutis-host` users, 10 for host owners, up to +3 for debugging/ops):

- **Plugin authors**: **0 new concepts**. They already know "cleanup" and "unload stack" ([unload order](design-unload-order-2026-11-01.en.md)); the budget is opt-in and invisible unless a cleanup hangs, in which case the two self-explanatory messages (timeout line + reload refusal error) state the cause and fix in plain words with no internal terminology.
- **`rutis-host` users, host owners, deployers**: **+1 optional environment variable** (`RUTIS_ROW_UNLOAD_TIMEOUT`), mirroring `RUTIS_SHUTDOWN_TIMEOUT` in unit and parsing rules. It fits inside the debugging/ops slot with no new conceptual layer and stays within budget.

## 5. Compatibility and migration

- **100% backwards compatible by default**: with `RUTIS_ROW_UNLOAD_TIMEOUT` unset, Node (leaf and native Cordis), Bun, and Python behave identically to before. No breaking change; no migration-guide breaking entry is needed.
- **No protocol change in this step**: wire frames, protocol version, control methods, and `rutis/*` events are unchanged.
  - **New runtime with old host**: works seamlessly; if `RUTIS_ROW_UNLOAD_TIMEOUT` is set in the runtime environment, the runtime enforces the budget locally, completes withdrawals, and refuses silent in-place reload of the tainted row.
  - **Old runtime with new host**: works seamlessly; the old runtime ignores `RUTIS_ROW_UNLOAD_TIMEOUT` and keeps waiting indefinitely.
- **Behavior when opted in**: hanging cleanups no longer block `rows.unload` forever; subsequent user cleanups on that row are skipped, all framework withdrawals still run, one verbatim diagnostic line is written to stderr, and reloading that row in the same runtime process is explicitly refused until the runtime restarts.

## 6. Implementation

| Step | Scope & changes | Done criteria |
| --- | --- | --- |
| 1. This design, Route B PoC & TLA+ model ([#1323](https://github.com/arcships/rutis/issues/1323), current branch) | Bilingual design docs `docs/design-row-unload-budget-2026-11-20.{md,en.md}`; TLA+ model `specs/row-unload-budget/` (registered in `specs/README.md` & `.github/workflows/ci.yml`); Route B PoC + tests in `node/rutis-runtime/src/{leaf,runner}.mjs`, `bun/rutis-bun/src/runner.ts`, `python/rutis/rutis/runner.py` | All 6 TLC configs match `expected.txt` (Route B leaf & Cordis pass; Route A and 3 mutations catch expected violations); all three unit test suites verify default behavior, verbatim timeout & taint refusal outputs, skipped later cleanups, D8 withdrawals, unaffected healthy siblings, lingering-action isolation, and fresh-runtime recovery; link/fault/guarantee/formal-trace/`check-model-pr` checks pass |
| 2. Host-observable timeout policy & guide integration (follow-up issue) | Surface timeout status explicitly to the host with configurable policy (default `taint` shown in `rutis-host status`; optional `restart-runtime` automatic runtime recycle); update `docs/guide/rutis-host.{md,en.md}`, `docs/guide/cordis-boundaries.{md,en.md}` (§7–§8), and `docs/design-events-protocol-2026-10-18.{md,en.md}` (§7.1); optional `--row-unload-timeout` flag in `rutis-host`; end-to-end `crates/rutis-loader` tests; register in `docs/guarantees.md` once promoted past PoC | E2E tests lock verbatim timeout/refusal outputs and runtime-restart recovery across all three runtimes; bilingual guides updated |
| 3. Go / Swift / Kotlin alignment (follow-up issue) | Wire `RUTIS_ROW_UNLOAD_TIMEOUT` into single-row `rows.unload` in Go (`go/rutis/internal/runtime/runtime.go`), Swift (`swift/rutis/Sources/Rutis/API/Runtime.swift`), and Kotlin (`kotlin/rutis/.../internal/Runtime.kt`), preserving D8 withdrawals, Route B tainting, and identical verbatim wording | All six SDKs share the same env var, D8 withdrawal invariant, Route B tainting, and verbatim diagnostic/refusal wording |
