# Managed Timers and Bare-Resource Boundaries

[中文](design-managed-timers-2026-11-20.md)

Status: draft (minimal PoC implemented for Rust / Node leaf / Bun / Python; Go / Swift / Kotlin pending). Date: 2026-11-20.
Issue: [#1325](https://github.com/arcships/rutis/issues/1325). Implementing issues are listed in §6.

## 1. Where Things Stand / What to Solve

Plugins routinely need a one-shot delayed action (debounce, retry, post-startup report) or a periodic background loop (heartbeat, cache refresh, polling an external API). Today `ctx` exposes only the low-level `ctx.effect` primitive, with no managed timer helper:

1. **Bare timers leak silently**: When a plugin calls bare `setTimeout` / `setInterval` (JS/TS), `asyncio.create_task` (Python), `tokio::spawn` (Rust), or `time.NewTicker` (Go) without pairing it with `ctx.effect`, unloading the row or hot-reloading under `rutis-host dev` produces no error while the old timer keeps firing inside the shared runtime process — operating on stale state/config and stacking duplicate loops after every reload.
2. **Hand-rolling safe timers is error-prone boilerplate**: Even when an author remembers `ctx.effect`, combining idempotent early cancellation, zero callbacks starting after unload, non-overlapping async intervals, contained row-attributed error reporting that never crashes a shared process, and refusal of registration during unload takes ~30 lines of subtle glue code per plugin.
3. **Unclear boundary for bare external resources**: Beyond timers, plugins create raw sockets, WebSockets, database pools, and OS threads. Developers need an explicit contract for what rutis manages vs. what remains the plugin author's responsibility via `ctx.effect` / cancellation signals, and why `rutis-host check` does not statically lint bare timers or sockets across six languages.

## 2. Decisions

| # | Decision | Why; rejected alternatives |
| --- | --- | --- |
| D1 | Managed timers are purely additive helpers on `Ctx` / each SDK's `ctx`, **built strictly on the existing `effect` (unload stack)** — no new kernel primitive, no wire protocol change, no `PLUGIN_API` bump | Follows [design principles](design-philosophy.en.md) §7 principles 1 & 2 ("can it live outside the kernel?"). Timers are process-local; building on the unload stack automatically inherits LIFO unload ordering, early cancellation, and inactive-registration refusal (`InactiveEffect` / `sdk/unload-order` D5). Rejected: adding a kernel timer scheduler or runtime protocol frame (pulls a local mechanism across process boundaries for no gain) |
| D2 | Unified names `timeout(delay, callback)` and `interval(period, callback)` across all 7 languages (exported as `Timeout` / `Interval` in Go), with duration types and units idiomatic to each language | Consistent vocabulary across all 7 languages without awkward unit conversions: `std::time::Duration` in Rust, milliseconds (`number`) in Node/Bun, seconds (`float`) in Python, `time.Duration` in Go, `Duration` in Swift, `kotlin.time.Duration` in Kotlin. Rejected: `setTimeout`/`setInterval` (visually collides with JS globals and is unidiomatic outside JS); forcing integer milliseconds in typed-duration languages or Python |
| D3 | Return an idempotent cancel handle; cancel automatically via the unload stack on unload; self-remove completed one-shot `timeout` entries from the unload stack | Authors can cancel early (debounce reset, stop on condition) safely multiple times or from inside the callback itself. Unload guarantees no new callback starts once unload begins, and cancels/awaits in-flight ticks before unload completes. Self-removing finished `timeout` entries prevents unbounded unload-stack growth in long-lived plugins. Rejected: returning void (no early cancel); keeping completed `timeout` closures until unload (memory growth) |
| D4 | **Non-overlapping fixed-delay `interval`**: the next wait of `period` starts AFTER the previous async callback finishes (whether it succeeds or throws); the first tick fires after `period` (not immediately at `t = 0`). The effective cadence intentionally drifts by callback duration (`callback_duration + period`) rather than catching up to fixed wall-clock multiples | Plugin background loops (polling external APIs, cache refreshes, heartbeats) routinely perform async I/O whose latency can exceed `period`. Fixed-rate scheduling (like bare `setInterval`) piles up overlapping invocations or fires catch-up bursts, exhausting pools and corrupting state; fixed-delay guarantees at most one execution is in flight at any time and naturally applies backpressure when slow or failing. Rejected: fixed-rate or a `mode` flag (adds conceptual surface and a sharp footgun; rare wall-clock-aligned loops can still be built directly on `ctx.effect` + a monotonic clock) |
| D5 | **Error containment**: errors, rejected Promises, `Err` results, or panics inside a timer callback are caught, attributed to the owning plugin/row, and reported without crashing the shared runtime process; `interval` continues scheduling subsequent ticks after a failed tick | Multiple rows share one Node/Bun/Python process, and multiple plugins share one Rust host. Uncaught timer errors would otherwise appear as "an unhandled error from an unknown row" (Bun, Node `run`) or trigger restart storms. Routing to `ErrorSink` (Rust) or row-attributed stderr lines (SDKs) matches `emit` listener containment. Continuing `interval` lets transient network or dependency blips self-heal on the next cycle (authors who want fail-stop invoke their cancel handle). Rejected: crashing the process, restarting the row on one failed tick, or silently stopping the interval |
| D6 | Registration during unload or after dispose follows [unload order](design-unload-order-2026-11-01.en.md) D5: refused, and **no timer is started** | Once unload takes the unload stack, a newly added timer could never be cleaned up by that unload and would leak. Rust returns `Err(ErrorKind::InactiveEffect)` (`Err(ErrorKind::Closed)` on root shutdown); SDKs write the verbatim line `rutis: row <row> is not active: timer is not registered` (Node leaf: `rutis: plugin <plugin> is not active: timer is not registered`) and return a no-op cancel handle. Rejected: silent drop or starting an unmanaged timer |
| D7 | Synchronous argument validation before active-state check: `timeout` requires finite `delay >= 0`, `interval` requires finite `period > 0`; JS/TS additionally rejects values `> 2147483647` ms (`2^31 - 1`) | `delay = 0` is a valid next-turn schedule; `period = 0` would busy-spin and starve the event loop (and panics `tokio::time::interval`). Node/Bun's underlying `setTimeout` silently clamps delays above `2^31 - 1` ms (~24.8 days) to `1` ms, so failing fast prevents silent 24-day misbehaviour. Rejected: browser-style silent clamping of negative/`NaN` values to `0` or `1` ms |
| D8 | Cordis compatibility in Node: `ctx.timeout` / `ctx.interval` are added **ONLY on the rutis leaf `ctx`** (`node/rutis-runtime/src/leaf.mjs`), never on Cordis `Context` | The Node runtime hosts both leaf plugins (`definePlugin`) and native Cordis plugins; many Cordis plugins rely on `@cordisjs/plugin-timer` or attach timer services to Cordis `Context`. Adding the helpers solely to the leaf adapter guarantees Cordis plugins see an unmodified Cordis `ctx` and `@cordisjs/plugin-timer` is never shadowed. Rejected: patching `Context.prototype` |
| D9 | **Bare resources boundary**: raw timers, sockets, DB pools, and threads created outside `ctx.effect` remain the plugin author's responsibility; **`rutis-host check` does NOT perform multi-language static AST linting** | (1) **High false-positive rate**: short-lived local utility timeouts (`AbortSignal.timeout`, local retry backoff `new Promise(r => setTimeout(r, 50))`, request-scoped connections closed in `finally`) are legitimate and do not leak across unloads; flagging them causes alert fatigue. (2) **6-language parser maintenance cost**: `rutis-host` is a single Rust binary; maintaining AST parsers and escape heuristics for JS/TS, Python, Go, Swift, Kotlin, and Rust inside it is disproportionate. (3) Monkey-patching global `setTimeout` breaks third-party libraries (HTTP keep-alive, retry helpers). Instead, document canonical `ctx.effect` / cancellation patterns and opt-in ecosystem lint rules (§3.6) |

## 3. Rules

### 3.1 Signatures Across All 7 Languages

| Language | `timeout` signature | `interval` signature | Duration unit | Cancel handle |
| --- | --- | --- | --- | --- |
| **Rust** (`rutis::Ctx`) | `pub fn timeout<F, Fut>(&self, delay: Duration, f: F) -> Result<Disposer, Error>` (`F: FnOnce() -> Fut + Send + 'static`, `Fut: Future<Output = Result<(), Error>> + Send + 'static`) | `pub fn interval<F, Fut>(&self, period: Duration, f: F) -> Result<Disposer, Error>` (`F: FnMut() -> Fut + Send + 'static`, `Fut: Future<Output = Result<(), Error>> + Send + 'static`) | `std::time::Duration` | `Disposer` (`.dispose().await` cancels and waits for exit, idempotent) |
| **Node leaf** (`@arcships/rutis`) | `ctx.timeout(delayMs: number, callback: () => unknown): () => void` | `ctx.interval(periodMs: number, callback: () => unknown): () => void` | milliseconds (`ms`) | `() => void` (synchronous cancel, idempotent) |
| **Bun** (`@arcships/rutis`) | `ctx.timeout(delayMs: number, callback: () => unknown): () => void` | `ctx.interval(periodMs: number, callback: () => unknown): () => void` | milliseconds (`ms`) | `() => void` (synchronous cancel, idempotent) |
| **Python** (`rutis`) | `ctx.timeout(delay: float, callback: Callable[[], Any]) -> Callable[[], None]` | `ctx.interval(period: float, callback: Callable[[], Any]) -> Callable[[], None]` | seconds (`s`) | `Callable[[], None]` (synchronous cancel, idempotent) |
| **Go** (`rutis`) | `func (c *Ctx) Timeout(delay time.Duration, fn func(context.Context) error) func()` | `func (c *Ctx) Interval(period time.Duration, fn func(context.Context) error) func()` | `time.Duration` | `func()` (synchronous cancel, idempotent) |
| **Swift** (`Rutis`) | `public func timeout(_ delay: Duration, _ body: @escaping @Sendable () async throws -> Void) throws -> () -> Void` | `public func interval(_ period: Duration, _ body: @escaping @Sendable () async throws -> Void) throws -> () -> Void` | `Duration` | `() -> Void` (synchronous cancel, idempotent) |
| **Kotlin** (`arcships.rutis`) | `fun timeout(delay: Duration, block: suspend () -> Unit): () -> Unit` | `fun interval(period: Duration, block: suspend () -> Unit): () -> Unit` | `kotlin.time.Duration` | `() -> Unit` (synchronous cancel, idempotent) |

### 3.2 Scheduling and Lifecycle Rules (Uniform Across All 7 Languages)

1. **`timeout(delay, callback)`**:
   - Validates arguments first, then checks whether the row/fiber is active. If inactive, refuses per §3.4 without starting a timer.
   - When active, registers its cancellation cleanup on the unload stack (via `effect`), waits `delay`, and if not cancelled and still active, invokes `callback` once (sync or async).
   - Once the callback finishes, the cleanup entry removes itself from the unload stack.
2. **`interval(period, callback)`**:
   - Performs the same validation, active check, and unload-stack registration.
   - The first tick runs after waiting `period` (never immediately at `t = 0`).
   - **Strictly non-overlapping (fixed-delay, not fixed-rate)**: after each tick's callback (including any returned Promise / coroutine / Future) settles — whether it succeeds or its error is caught and reported — the next `period` wait begins only if still active and not cancelled. The effective interval is therefore `callback_duration + period` (intentionally drifting by callback runtime instead of bursting to catch up), trading wall-clock alignment for guaranteed non-overlap and automatic backpressure.
   - After a tick fails and is reported, the loop still schedules the next tick after `period` unless cancelled or unloading.
3. **Early cancellation (including synchronous self-cancel)**:
   - Calling the returned handle aborts any pending wait immediately so no further callback starts.
   - Idempotent: calling it multiple times or after natural completion is a no-op.
   - Safe to call from inside the callback itself — including synchronously before an async callback's first `await`: the current tick finishes normally and no further tick is scheduled, **and the timer's cleanup entry MUST NOT be removed from the unload stack until that in-flight tick finishes**, ensuring that an unload racing with a self-cancelling async callback still awaits it before closing earlier LIFO resources (such as a DB pool).
     - Language note (Rust): `Disposer::dispose(self)` consumes the handle and awaits task exit, identical to `ctx.on`'s `Disposer`, so a callback task must not `.await` its own `Disposer` from inside itself (would self-deadlock); stop from outside via `Disposer`, or check an `AtomicBool` inside.
4. **Unload behaviour, LIFO ordering, and cross-language cancellation state matrix**:
   - From the moment unload begins (Rust `pre_cancel`, Node `signal` abort & `drain`, Bun row removal, Python `row.unloading = True`, Go/Swift/Kotlin context/task/job cancellation), no new timer callback starts.
   - Because timers sit on the same unload stack via `effect`, a timer registered after `provide` or a resource cleanup (such as `pool.close()`) is stopped and drained **before** that earlier entry runs, and one registered before stops after.
   - Runtimes differ fundamentally in whether an in-flight async callback can be forcibly aborted; the exact state semantics are pinned below and exhaustively checked by [`specs/managed-timers/`](../specs/managed-timers/README.en.md):

| Timer state | Abortable runtimes (Rust / Python / Go / Swift / Kotlin) | Non-abortable Promise runtimes (Node leaf / Bun) |
| --- | --- | --- |
| `scheduled` (sleeping) | Manual cancel or unload aborts the wait immediately with zero fires and detaches the stack entry | Manual cancel or unload calls `clearTimeout` immediately with zero fires and detaches the stack entry |
| `invoking_sync` (synchronous prologue before returning Future/Promise) | Rust: unload cancels child token and awaits task exit; Python/Go/Swift/Kotlin: stack entry stays registered while running and self-cancel only sets `cancelled = true` without aborting the current tick | Keeps `invoking = true` so even a synchronous self-cancel inside `callback()` **never** removes `cleanup` from `steps`; if `callback()` returns a Promise, `running` is assigned before `invoking` clears, so unload always awaits it |
| `in_flight_async` (suspended at `await`) | **Abortable**: Rust `tokio::select!` drops the Future at `.await` (running `Drop`) and unload awaits `TimerExitSignal`; Python unload calls `task.cancel()` (raising `CancelledError`) and awaits `task` (including `finally`); Go/Swift/Kotlin cancel `context.Context` / `Task` / `Job` and await exit — all before running earlier LIFO cleanups (e.g. `pool.close()`) | **Non-abortable**: JS Promises cannot be killed externally; unload sets `cancelled = true` to prevent further ticks and awaits `running` inside the unload stack before advancing to earlier LIFO cleanups (e.g. `pool.close()`), preventing use-after-pool-close. **Coupling with [#1323](https://github.com/arcships/rutis/issues/1323)**: if a JS timer callback returns a Promise that never settles, unload waits on that cleanup until the runtime/host unload budget (`RUTIS_SHUTDOWN_TIMEOUT` / #1323 per-cleanup budget) gives up |
| `idle` / `done` (between ticks or finished) | Unload or cancel enters `done` with nothing left on the stack; repeated cancel is idempotent | Same; completed one-shot `timeout` entries self-detach on settlement |
5. **Output prefixing and row attribution**:
   - Timer callbacks run inside the owning row's async context (Node `AsyncLocalStorage` propagation, Bun `within(row.key, ...)`, Python `contextvars` carrying `current_row`, Go/Swift/Kotlin/Rust row binding). Under `RUTIS_PREFIX_OUTPUT=1`, logs printed inside callbacks carry `[<row>] `, while the runtime's own `rutis: timer of …` report lines are written unprefixed.

### 3.3 Argument Validation Rules

Validation runs synchronously **before** the active-state check:

| Language | Invalid callback | Invalid `timeout` `delay` | Invalid `interval` `period` |
| --- | --- | --- | --- |
| Rust | Compile-time type check | `Duration` is non-negative and finite by type | `period.is_zero()` returns `Err(ErrorKind::Validation { issues: ["interval period must be greater than zero"] })` |
| Node leaf / Bun | Throws `TypeError: timeout needs a callback function` / `interval needs a callback function` | Non-number, non-finite, or `< 0` throws `RangeError: timeout delay must be a non-negative finite number of milliseconds, got <value>`; `> 2147483647` throws `RangeError: timeout delay must be at most 2147483647 ms, got <value>` | Non-number, non-finite, or `<= 0` throws `RangeError: interval period must be a positive finite number of milliseconds, got <value>`; `> 2147483647` throws `RangeError: interval period must be at most 2147483647 ms, got <value>` |
| Python | Raises `TypeError: timeout needs a callable callback` / `interval needs a callable callback` | `bool`, non-`int`/`float`, non-finite, or `< 0` raises `ValueError: timeout delay must be a non-negative finite number of seconds, got <repr>` | `bool`, non-`int`/`float`, non-finite, or `<= 0` raises `ValueError: interval period must be a positive finite number of seconds, got <repr>` |
| Go | `fn == nil` panics `"rutis: Timeout needs a callback"` / `"rutis: Interval needs a callback"` | `delay < 0` panics `"rutis: timeout delay must be non-negative"` | `period <= 0` panics `"rutis: interval period must be greater than zero"` |
| Swift | Compile-time type check | `delay < .zero` throws `RutisError(name: "RangeError", message: "timeout delay must be non-negative")` | `period <= .zero` throws `RutisError(name: "RangeError", message: "interval period must be greater than zero")` |
| Kotlin | Compile-time type check | `delay.isNegative()` throws `IllegalArgumentException("timeout delay must be non-negative")` | `!period.isPositive()` throws `IllegalArgumentException("interval period must be greater than zero")` |

### 3.4 Registration During Unload or After Dispose

| Language | Behaviour & verbatim output | Return value |
| --- | --- | --- |
| Rust | No task spawned; returns `Err(ErrorKind::InactiveEffect)` (`Err(ErrorKind::Closed)` during root shutdown; `Err(ErrorKind::StaleGeneration)` on stale generation) | `Err(...)` |
| Node leaf | No timer started; writes to stderr: `rutis: plugin <plugin> is not active: timer is not registered` (`<plugin>` is `leaf` when unnamed) | No-op `() => {}` |
| Bun / Python / Go / Swift / Kotlin | No timer started; writes to stderr: `rutis: row <row> is not active: timer is not registered` | No-op cancel handle |

### 3.5 Error Containment and Verbatim Reporting Format

| Language | Reporting channel | Verbatim format |
| --- | --- | --- |
| Rust | `ctx.error_sink()` (default sink prints to stderr), wrapped as `ErrorKind::PluginFailed`, never crashes threads | `<owner>: timer failed: <error>` (row id if row, plugin name if Rust plugin, `the host` if root) |
| Node leaf | stderr (`say`, unprefixed), never triggers `uncaughtException` / `unhandledRejection` | `rutis: timer of plugin <plugin> failed: <ErrorName>: <message>` |
| Bun | stderr (`say`), never triggers `uncaughtException` / `unhandledRejection` | `rutis: timer of row <row> failed: <ErrorName>: <message>` |
| Python | stderr (`say`), never triggers loop unhandled-exception handler | `rutis: timer of row <row> failed: <ExceptionType>: <message>` |
| Go | stderr, panics recovered via `peer.Protect` | `rutis: timer of row <row> failed: <err>` |
| Swift | runtime log / stderr | `rutis: timer of row <row> failed: <err>` |
| Kotlin | `Report.line` (stderr) | `rutis: timer of row <row> failed: <ExceptionType>: <message>` |

Note: in JS/TS, when a non-`Error` value is thrown (e.g. `throw 'boom'`), `<ErrorName>: <message>` falls back to `String(error)` (`boom`).

### 3.6 Bare-Resource Boundary and Recommended Patterns

The following resources are **not** automatically managed by rutis and remain the plugin author's responsibility to release on unload:

- Bare language primitives: raw `setTimeout` / `setInterval`, `asyncio.create_task`, `threading.Thread`, `tokio::spawn`, `go func()`;
- Raw network listeners and connections (`net.createServer`, `WebSocket`, gRPC channels, persistent HTTP clients);
- Database connection pools, file descriptors, child processes, message queue consumers.

Two canonical patterns (documented in [Plugin API](guide/plugin-api.en.md)):

1. **Pair acquisition immediately with `ctx.effect`** so teardown runs in LIFO order on unload:
   ```ts
   const pool = createPool(config.dsn)
   ctx.effect(() => pool.end())
   ```
2. **Thread the lifecycle cancellation signal into blocking I/O**:
   - Node / Bun: `fetch(url, { signal: ctx.signal })`, `ctx.signal.addEventListener('abort', ...)`;
   - Rust: `tokio::select! { _ = ctx.cancelled() => ..., ... }` paired with `ctx.effect` joining the task;
   - Go: pass `ctx` (a `context.Context`) into DB/HTTP calls and run background goroutines via `ctx.Go(...)`;
   - Python / Swift / Kotlin: run periodic work in managed timers/tasks and close pools in `ctx.effect`.

Teams that want static enforcement against bare `setTimeout` / `setInterval` in their own plugin repositories can opt into standard ecosystem linters rather than adding six language parsers to `rutis-host check`:

- JS/TS (ESLint): `no-restricted-globals: ["error", "setTimeout", "setInterval"]`;
- Python (Ruff): `flake8-tidy-imports.banned-api` (forbidding raw `asyncio.create_task` in plugin modules);
- Rust (Clippy): `disallowed-methods` in `clippy.toml` (forbidding bare `tokio::spawn` in plugin crates).

## 4. What Developers See

### 4.1 Plugin Authors

#### When It Works

TypeScript example (identical in Node leaf and Bun):

```ts
import { definePlugin } from '@arcships/rutis'

export default definePlugin({
  name: 'heartbeat',
  inject: ['store'],
  config: {
    type: 'object',
    properties: {
      periodMs: { type: 'number', default: 5000 },
      dsn: { type: 'string' },
    },
    required: ['dsn'],
  },
  apply(ctx, config) {
    const store = ctx.use('store')

    // 1. One-shot delayed task: returns a cancel handle; self-cleans after firing
    const cancelWarmup = ctx.timeout(500, async () => {
      await store.ping()
    })

    // 2. Periodic task: waits periodMs AFTER each tick finishes (non-overlapping); stops on unload
    const stopLoop = ctx.interval(config.periodMs, async () => {
      await store.flush()
    })

    // 3. Bare resource (DB pool): managed explicitly via ctx.effect
    const pool = openPool(config.dsn)
    ctx.effect(() => pool.close())

    ctx.provide('heartbeat', {
      pause() {
        cancelWarmup()
        stopLoop()
      },
    })
  },
})
```

Normal runs emit no extra output. When editing the file under `rutis-host dev`, reloading stops the old instance's `timeout` and `interval` immediately so duplicate loops never accumulate. If one `store.flush()` call takes 8 s while `periodMs` is 5000, the next tick starts 5 s after that `flush()` completes — never concurrently.

Differences in other languages are only method naming and duration units:

- **Rust**: `let stop = ctx.interval(Duration::from_secs(5), || async move { ... Ok(()) })?;` (cancel early with `stop.dispose().await?`);
- **Python**: `stop = ctx.interval(5.0, flush)` (seconds; `flush` may be `def` or `async def`);
- **Go**: `stop := ctx.Interval(5*time.Second, func(ctx context.Context) error { ... })`;
- **Swift**: `let stop = try ctx.interval(.seconds(5)) { try await flush() }`;
- **Kotlin**: `val stop = ctx.interval(5.seconds) { flush() }`.
- **Native Cordis plugins in Node**: still receive Cordis's unmodified `ctx` without `ctx.timeout` / `ctx.interval`; `@cordisjs/plugin-timer` works unchanged.

#### When It Fails

| Trigger | Language | Verbatim output (and error code) | Recorded on row | How to fix |
| --- | --- | --- | --- | --- |
| Timer callback throws or rejects / returns `Err` / panics | Node leaf | `rutis: timer of plugin heartbeat failed: Error: connection refused` | `heartbeat` (plugin name) | Fix callback logic or handle expected transient errors; runtime stays up and `interval` still runs the next tick |
| Timer callback throws or rejects | Bun | `rutis: timer of row heartbeat failed: Error: connection refused` | `heartbeat` | Same as above |
| Timer callback raises an exception | Python | `rutis: timer of row heartbeat failed: RuntimeError: connection refused` | `heartbeat` | Same as above |
| Timer callback returns `Err` or panics | Rust | `heartbeat: timer failed: connection refused` (sent to `ErrorSink`, `ErrorKind::PluginFailed`) | `heartbeat` (row or plugin name; `the host` for root) | Same as above |
| Timer callback returns error or panics | Go / Swift / Kotlin | `rutis: timer of row heartbeat failed: connection refused` (Kotlin includes class name: `IllegalStateException: connection refused`) | `heartbeat` | Same as above |
| Negative / `NaN` / `Infinity` / non-number `delay` in `timeout` | Node leaf / Bun | `RangeError: timeout delay must be a non-negative finite number of milliseconds, got -5` (in `apply`, fails the row: `heartbeat: failed in apply: RangeError: …`) | `heartbeat` | Pass a finite number `>= 0` in milliseconds |
| `delay > 2147483647` ms in `timeout` | Node leaf / Bun | `RangeError: timeout delay must be at most 2147483647 ms, got 3000000000` | `heartbeat` | Use a shorter interval or `<= 2147483647` ms |
| Negative / `NaN` / `bool` `delay` in `timeout` | Python | `ValueError: timeout delay must be a non-negative finite number of seconds, got -1` | `heartbeat` | Pass a finite `int`/`float` `>= 0` in seconds |
| `0` or negative `period` in `interval` | Node leaf / Bun | `RangeError: interval period must be a positive finite number of milliseconds, got 0` | `heartbeat` | Pass a finite number `> 0` in milliseconds |
| `0` or negative `period` in `interval` | Python | `ValueError: interval period must be a positive finite number of seconds, got 0` | `heartbeat` | Pass a finite `int`/`float` `> 0` in seconds |
| `Duration::ZERO` in `interval` | Rust | `config validation failed: ["interval period must be greater than zero"]` (`ErrorKind::Validation`) | caller (row/plugin if returned from `apply`) | Pass a non-zero `Duration` |
| Non-function / non-callable callback | Node leaf / Bun / Python | `TypeError: timeout needs a callback function` / `TypeError: interval needs a callback function` (Python: `TypeError: timeout needs a callable callback` / `TypeError: interval needs a callable callback`) | `heartbeat` | Pass a function / callable |
| Calling `ctx.timeout` / `ctx.interval` during unload or after dispose | Node leaf | `rutis: plugin heartbeat is not active: timer is not registered` (no timer started; returns no-op handle) | `heartbeat` | Do not schedule new timers during cleanup; finish teardown inside the cleanup itself |
| Same | Bun / Python / Go / Swift / Kotlin | `rutis: row heartbeat is not active: timer is not registered` (no timer started; returns no-op handle) | `heartbeat` | Same as above |
| Same | Rust | `Err(ErrorKind::InactiveEffect)` (Display: `fiber disposed`; during root shutdown: `Err(ErrorKind::Closed)`, Display: `root has been shut down`) | caller | Same as above |
| Bare `setInterval` or raw socket outside `ctx.effect` | All languages | **No error** on unload; old callback or connection leaks silently | — | Replace bare timers with `ctx.timeout` / `ctx.interval`; wrap sockets/pools in `ctx.effect` and observe cancellation signals |

#### Concepts to Learn (Against #663 Concept Budget)

Against [#663](https://github.com/arcships/rutis/issues/663)'s plugin-author budget of 7 concepts (plugin, service, `inject`, config, events, testing tool, `dev` & rows; plus `ctx.effect` when holding external resources): **+0 new concepts**. `ctx.timeout` and `ctx.interval` are direct conveniences on the existing `ctx.effect` lifetime model and reduce the hand-written lifecycle boilerplate authors otherwise have to learn.

### 4.2 `rutis-host` Users

- **When it works**: No `rutis.json` changes and no new CLI flags. Hot-reloading plugins under `rutis-host dev` cleanly cancels previous timers with zero duplicate loops.
- **When it fails**: A failing timer callback prints one row-attributed line to stderr (see §4.1) without crashing the runtime or disturbing sibling rows; an invalid delay/period inside `apply` fails that row with the standard `apply` failure output.
- **Concepts to learn**: **+0 new concepts**.

### 4.3 Host Owners (Applications Embedding `rutis`)

- **When it works**: Call `ctx.timeout` / `ctx.interval` on any `Ctx` (plugin or root). Custom `ErrorSink` implementations receive `Arc<ErrorKind>` (`ErrorKind::PluginFailed` with `to_string()` equal to `<owner>: timer failed: <error>`). If a timer callback blocks synchronously past `shutdown_with_timeout`, `Stuck` reports the registration site via `#[track_caller]` (`still running the cleanup registered at src/lib.rs:42:9`).
- **When it fails**: `ctx.interval(Duration::ZERO, ...)` returns `Err(ErrorKind::Validation)`; registering on an unloading or disposed fiber returns `Err(ErrorKind::InactiveEffect)`; registering during root shutdown returns `Err(ErrorKind::Closed)`.
- **Concepts to learn**: **+0 new concepts** (reuses `Disposer`, `ErrorSink`, `InactiveEffect`, and `Validation`).

## 5. Compatibility and Migration

- **Purely additive, zero breaking changes**: Adds only `timeout` and `interval` on `Ctx` and each language's leaf `ctx`. No existing signatures change, the wire protocol is unchanged, and `PLUGIN_API` stays `3`.
- **Node Cordis compatibility**: Native Cordis plugins bypass `leaf.mjs`, so `@cordisjs/plugin-timer` and all other Cordis plugins/services are unaffected.
- **Mixed versions**:
  - New runtime + old plugin: 100% compatible; existing plugins using `ctx.effect` or bare timers behave identically.
  - Old runtime + new plugin calling `ctx.timeout` / `ctx.interval`: fails immediately in `apply` with a standard missing-method error (JS `TypeError: ctx.timeout is not a function`, Python `AttributeError: 'Context' object has no attribute 'timeout'`). Plugins using managed timers specify the release that ships them as their minimum runtime/SDK version.
- **`docs/guarantees.md`**: This document is currently a draft with a minimal PoC. Once all 7 runtimes implement the feature, `sdk/managed-timers` will be registered in both `docs/guarantees.md` and `docs/guarantees.en.md` linked to tests in all 7 languages.

## 6. Implementation

| # | Scope | Work | Done criteria |
| --- | --- | --- | --- |
| T-1 (this PoC) | Design docs + TLA+ model + Rust / Node leaf / Bun / Python | Bilingual design docs; `specs/managed-timers/` TLA+ formal model (abortable & non-abortable modes + 5 mutation counterexamples); implement `timeout` / `interval` in `crates/rutis/src/ctx.rs` (with `crates/rutis/public-api.txt` refreshed), `node/rutis-runtime/src/leaf.mjs`, `node/rutis/src/index.d.ts`, `bun/rutis-bun/src/runner.ts`, `python/rutis/rutis/{plugin,runner}.py` (and `public-api.txt`) | TLC verifies all configs matching `expected.txt`; `tools/public-api.sh` passes; unit tests in all 4 runtimes covering: one-shot fire / unload-stack self-detach / early cancel / unload before delay; interval multi-fire / zero fires after unload / non-overlapping slow async / self-cancel; sync self-cancel before await still awaited before earlier LIFO cleanups; Python cross-thread registration & cancel safety; argument validation; verbatim refusal during unload & after dispose; callback error & panic containment with verbatim output; Node Cordis non-shadowing |
| T-2 | Go SDK | Implement `Ctx.Timeout` / `Ctx.Interval` in `go/rutis/internal/runtime`; update `go/rutis/public-api.txt` | Go unit tests covering all §3 rules, §4 verbatim outputs, and `goleak` zero-goroutine-leak check |
| T-3 | Swift SDK | Implement `timeout` / `interval` in `swift/rutis/Sources/Rutis/API/Ctx.swift`; update `swift/rutis/public-api.txt` | `SelfTest` cases covering all §3 rules and §4 verbatim outputs |
| T-4 | Kotlin SDK | Implement `timeout` / `interval` in `kotlin/rutis/.../Ctx.kt`; update `api/*.api` | `jvmTest` cases covering all §3 rules and §4 verbatim outputs |
| T-5 | Guides & guarantee register | Update `docs/guide/plugin-api.md` / `.en.md` (document `ctx.timeout`/`ctx.interval` and bare-resource `ctx.effect` patterns) and register `sdk/managed-timers` in `docs/guarantees.md` / `.en.md` (`crates/rutis/public-api.txt` already refreshed in T-1) | `check-md-links.mjs`, `check-guarantees.mjs`, `formal-trace.mjs --check`, and `public-api.sh` all pass |
