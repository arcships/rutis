# Optional event mode catalog and checking

[中文](design-event-mode-catalog-2026-11-20.md)

Status: draft (core bus PoC implemented in `crates/rutis`; `rutis-bridge`, `rutis.json`, and `rutis-host check` not yet implemented). Date: 2026-11-20.
Issue: [#1324](https://github.com/arcships/rutis/issues/1324). Implemented by steps 1–4 in §6.

## 1. Where things stand and what to solve

In rutis's unified event model ([unified events](design-events-2026-10-18.en.md), [Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §1), **one listener table** serves **five dispatch modes** (`emit`, `parallel`, `serial`, `waterfall`, `bail`):

- A listener registers once (`ctx.on("message", fn)`, `ctx.on::<Message>(...)`) without declaring a mode.
- The emitter chooses the dispatch mode at each call site (`ctx.emit`, `ctx.serial`, `ctx.bail`, etc.).
- Rust's `Event` trait declares only `NAME` and `Value`, and the other six languages use plain event name strings; neither declares which `Mode` must be used.

This leaves room for **silent contract mismatches**:

- **The blacklist interceptor scenario**: A host or message pipeline defines `"message"` so interceptors can short-circuit delivery by returning `true` (requiring `bail` or `serial`). A blacklist plugin registers `ctx.on("message", msg => isBlocked(msg.user))` and returns `true`, while a dispatcher plugin (such as a newly mounted Cordis plugin or a plugin written in another language) calls `ctx.emit("message", msg)` by mistake.
- **What happens today**: `emit` starts the blacklist listener, the listener computes and returns `true`, and the bus drops the return value by definition ([Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §1). **There is no warning and no error**; the blocked message passes straight through.
- Similar mismatches surface late or silently:
  - Dispatching a `waterfall` event with `emit` / `parallel` / `serial` fails only when a listener invokes `next()` (`WaterfallNext`, which under `emit` goes only to the error sink and never back to the emitter).
  - Dispatching an async `serial` event with synchronous `bail` appears to work while only synchronous listeners happen to be mounted, then fails with `AsyncListener` the moment an async listener is added ([Cordis compatibility boundaries](guide/cordis-boundaries.en.md) §2).

### Why static source scanning in `rutis-host check` alone cannot solve this

It is tempting to scan plugin source files inside `rutis-host check` for `emit` / `serial` / `bail` call sites and check them against a table. Across rutis, **pure static scanning is fundamentally incomplete**:

1. **Six languages and compiled binaries**: A single `rutis-host` deployment mixes Node/TypeScript, Bun, Python, Go, Swift, Kotlin, Rust (`dylib` / builtins), and remote nodes. Go, Swift, Kotlin, and Rust plugins ship as compiled binaries — `rutis-host check` receives only a JSON manifest (`describe`), never source code. Embedding six language parsers and AST analyzers in `rutis-host` would also cross the platform boundary ([platform cleanup](design-platform-cleanup-2026-10-28.en.md) §1).
2. **Unmodified Cordis plugins and helper wrappers**: Published npm Cordis plugins call `ctx.emit`, `ctx.events.dispatch(...)`, or internal wrapper helpers and carry no rutis event declaration.
3. **Dynamic event names**: Rust event types with `NAME = ""` overriding `Event::name(&self)`, `CtxInternal::named`, and runtime string expressions in JS/Python/Go (`ctx.emit(`room/${id}`, ...)`) cannot be statically resolved without either missing real dispatches or generating noisy false positives.
4. **Remote nodes and late-connecting runtimes**: Emitters on linked nodes (`rutis-host/dial`, `rutis-host/listen`) may not even be online when `check` runs.

On the other hand, since [platform cleanup](design-platform-cleanup-2026-10-28.en.md) §7.5 removed the runtime-local fast path, **every non-`internal/*` event dispatch from all six language runtimes, unmodified Cordis plugins, and Rust plugins flows through the host's single Rust kernel `EventBus`**.

This design therefore enforces an **optional Event Mode Catalog** at **two complementary points**, with **runtime bus enforcement as the primary mechanism**:

1. **Primary mechanism (runtime kernel `EventBus`)**: `EventBus` (configured by hosts through `CtxHost`) holds an optional catalog mapping `event name -> expected Mode + policy`. Whenever a catalogued event is dispatched with a different `Mode`, `warn` (the default) reports the mismatch once through `ErrorSink` while letting the dispatch proceed unchanged (never breaking existing Cordis plugins), and opt-in `error` rejects the dispatch before running any listener. One mechanism in `crates/rutis` covers all six runtimes, Cordis plugins, compiled binaries, and dynamic names that resolve to a catalogued name at runtime — with zero Cordis API changes and no `PLUGIN_API` bump.
2. **Complementary mechanism (`rutis.json` + `rutis-host check`)**: `rutis.json` (`format: 1`) gains an optional top-level `"events"` object. `rutis-host check` validates the catalog entries (unknown modes with spelling suggestions, invalid policies, reserved prefixes) and cross-checks them against any plugin that optionally declares static event expectations, explicitly skipping undeclared plugins and dynamic event names with zero false positives. `rutis-host run` / `dev` installs the same `"events"` catalog into the root context at startup.

## 2. Decisions

**D1 Key the catalog by exact event name, and keep it opt-in.** The catalog maps exact event names (such as `"message"`, `"session/created"`) to an expected `Mode` (`emit | parallel | serial | waterfall | bail`) and an optional per-event policy (`warn | error`). Events not in the catalog (including unlisted dynamic names) are unchecked, producing zero false positives; when the catalog is empty, the dispatch hot path takes a single atomic load without locking. *Dropped*: adding a mandatory `const MODE: Mode` to `rutis::Event` — breaks every existing Rust `Event` implementation, cannot cover string-based languages or Cordis plugins, and prevents legitimate multi-mode events. *Dropped*: prefix/glob catalog keys — `"message/*"` would falsely catch `"message/deleted"` (`emit`) under `"message"` (`bail`) and create precedence ambiguity.

**D2 Enforce at two complementary points: kernel `EventBus` at runtime (primary) and `rutis-host check` before start (complementary).** Only the kernel `EventBus` sees compiled binaries, Cordis wrappers, and runtime-computed names across all six languages; `check` catches `rutis.json` typos and conflicts with plugins that opt into declaring static event modes before anything starts. *Dropped*: static-only source scanning in `rutis-host check` — blind to compiled binaries, Cordis wrappers, and dynamic names.

**D3 Default mismatch policy is `warn`, with opt-in `error` catalog-wide or per event.**
- `EventModePolicy::Warn` (default): dispatch proceeds **unchanged** (return-value semantics stay as they are, so turning on a catalog never breaks existing Cordis plugins), and `ErrorKind::EventModeMismatch` is reported to the host's `ErrorSink`. **`warn` is also the consistent cross-language default across all five modes**: under `warn`, Rust, all six language runtimes, and Cordis plugins behave identically at the call site and report the same diagnostic line through `ErrorSink`, whereas under `error`, `emit` has an inherent API difference between Rust (`Result<(), Error>` returned synchronously) and the other languages / Cordis (fire-and-forget `undefined` / `None` that cannot throw synchronously at the `ctx.emit()` call site; see §3.3 and §4.1).
- `EventModePolicy::Error`: dispatch is **rejected** before selecting or calling any listener (0 listeners execute), returning `ErrorKind::EventModeMismatch`.
- The catalog has a default policy (`Warn`) and each event entry may override it (`expect_with` / `on_mismatch`), letting hosts tighten strictness one event at a time. *Dropped*: defaulting to `error` — breaks existing unmigrated Cordis plugins the moment a host documents its event catalog, and exposes the Rust-vs-fire-and-forget `emit` call-site difference to hosts that never asked for strict rejection.

**D4 Deduplicate `warn` per `(emitter, event, actual, expected)` (bounded at 4096 keys); never deduplicate `error`.** High-frequency events (such as `"message"`) would otherwise flood stderr under `warn`, while deduplicating by event name alone would hide a second buggy emitter. Deduplicating by the 4-tuple reports each distinct offending `(emitter, event, actual, expected)` once until `set_event_mode_catalog` replaces the catalog. The remembered set uses FIFO eviction bounded at 4096 entries so dynamically named plugins cannot grow memory without bound. Under `error`, every mismatched dispatch is rejected and never deduplicated. *Dropped*: time-window throttling — makes tests non-deterministic and still repeats log noise.

**D5 Check once per dispatch at the dispatching host's `EventBus`, regardless of listener count.**
- The check runs in `EventBus::snapshot_with` (after `ensure_bus` and the `rutis/*` reserved check, before listener selection and filtering).
- Mismatches are caught even when **0 listeners** are currently mounted, so a wrong dispatch is still surfaced when the blacklist plugin is disabled or has not started yet.
- Cross-node single-entry calls (`EventBus::invoke_entry` / `invoke_entry_sync`) do not re-check the catalog, avoiding duplicate reports per listener; each node enforces its own catalog on dispatches it originates.
- Framework dispatches (`opts.framework == true`, i.e., `rutis/*` and `rutis::*`) bypass the catalog. *Dropped*: warning only when a listener actually returns a non-empty value — misses `waterfall` / `bail` / `serial` mismatches whenever no listener currently matches.

**D6 Reject empty names and `rutis/*` / `internal/*` prefixes when installing the catalog.** Empty strings, `rutis/*` (owned by the host and always dispatched as `emit`), and `internal/*` (runtime-local to Cordis, never crossing into `EventBus`) are rejected by `set_event_mode_catalog` and `rutis.json` parsing so dead rules cannot silently sit in config. A failed `set_event_mode_catalog` **leaves the previously installed catalog untouched**. *Dropped*: silently ignoring them — violates design principle §7 #10 (explicit over implicit).

**D7 Keep the Rust API in `rutis::host` (zero new plugin-author concepts) and reuse `BindingError` on the wire.**
- `EventModeCatalog`, `EventModeEntry`, and `EventModePolicy` live in `rutis::host`, and `set_event_mode_catalog` / `event_mode_catalog` live on `CtxHost` (delegating to `EventBus`). Nothing is added to the crate root `rutis::*`.
- `ErrorKind` (already `#[non_exhaustive]`) adds `EventModeMismatch { name, actual, expected, emitter }`.
- Across sessions (`rutis-bridge`, step 2), it crosses like `bad-config` ([diagnostics design](design-diagnostics-2026-10-26.en.md) §4.3): `BindingError` with `code: "event-mode-mismatch"` and `data: {"event", "actual", "expected"}`. No language SDK needs a new error class and `PLUGIN_API` stays unchanged. *Dropped*: adding a dedicated `EventModeMismatchError` class to all six SDKs — expands SDK surface and plugin-author concepts for no gain.

**D8 Add an optional `"events"` object to `rutis.json` (`format: 1`) with one uniform object shape; keep plugin declarations optional.**
- Top-level `"events"` defaults to empty. Every entry has the uniform object form `{"mode": "...", "on_mismatch"?: "warn" | "error"}` (`#[serde(deny_unknown_fields)]`). *Dropped*: also allowing a string shorthand `"message": "bail"` — two equivalent shapes complicate validation and documentation and diverge from `runtimes` / `rows`.
- In step 3, `rutis-loader`'s `Declared` gains an optional `events: BTreeMap<String, Mode>` (empty by default). Plugins that declare nothing (all existing plugins and Cordis plugins) and dynamic event names are silently skipped in `check` with zero false positives. When a plugin does declare static expectations that conflict with `rutis.json` or another enabled row, `check` reports the conflict (failing with exit 1 under `error` policy or `--strict`).

**D9 Count synchronous waterfall as `Mode::Waterfall`.** Both Rust's `ctx.waterfall(e).sync(...)` and Cordis's synchronous `ctx.waterfall(...)` dispatch with `Mode::Waterfall`, matching an expected mode of `waterfall`.

## 3. Rules

### 3.1 Data model and catalog validation (`crates/rutis`)

- `EventModePolicy`: `Warn` (default), `Error`; serde `kebab-case` (`"warn"` / `"error"`).
- `EventModeEntry { mode: Mode, policy: Option<EventModePolicy> }`.
- `EventModeCatalog`: holds `default_policy: EventModePolicy` (default `Warn`) and `entries: HashMap<Arc<str>, EventModeEntry>`.
  - `EventModeCatalog::new()`: empty catalog with default policy `Warn`.
  - `.with_policy(policy)`: sets the catalog's default policy.
  - `.expect(name, mode)`: registers an exact event name using the catalog's default policy.
  - `.expect_with(name, mode, policy)`: registers an exact event name overriding the policy for that event.
  - `.get(name) -> Option<(Mode, EventModePolicy)>`: exact lookup returning `(expected_mode, effective_policy)`.
- `CtxHost::set_event_mode_catalog(&self, catalog: EventModeCatalog) -> Result<(), Error>`:
  1. Validates all keys in sorted order:
     - `""` -> `"events: event name cannot be empty"`
     - prefix `"rutis/"` -> `"events.<name>: rutis/* events are owned by the host and cannot be configured here"`
     - prefix `"internal/"` -> `"events.<name>: internal/* events stay inside their runtime and cannot be configured here"`
     If any issue is found, returns `Err(ErrorKind::Validation { issues })` **without modifying the existing catalog or deduplication state**.
  2. Otherwise, while holding the `catalog` write lock, increments a monotonic `epoch`, replaces the catalog, clears the `Warn` deduplication set and binds it to the new `epoch` (fixed lock order `catalog -> warned`), updates the `has_catalog` atomic flag (`!catalog.is_empty()`), and drops the write lock. Installing an empty `EventModeCatalog::new()` disables checking.

### 3.2 Runtime bus check order (`EventBus`)

On every non-framework dispatch (`emit_erased`, `parallel_erased`, `serial_erased`, `waterfall_erased`, `bail_erased`, `waterfall_sync_erased` — covering both typed `Ctx::*` methods and raw `EventBus::dispatch` / `dispatch_sync`):

1. Existing preflight checks run first: `bail` and synchronous `waterfall` run `ctx.registration_preflight()`; then `ensure_bus(ctx)`; then `!opts.framework && name.starts_with("rutis/")` returns `ReservedEvent`.
2. If `opts.framework` is true or `has_catalog` is false, skip the catalog check.
3. Look up `catalog.get(name)` together with the current `epoch` under a short read lock and drop the lock immediately. If absent or `expected == actual`, proceed normally.
4. Otherwise resolve `emitter`:
   - `Dispatch.emitter` if set (`rutis-bridge` passes the emitting row id, or runtime name when there is no row);
   - else `ctx.row()` if `Some(row)`;
   - else `"the host"` when `ctx` belongs to the root fiber;
   - else `ctx.plugin_name()` (never emitting `<root>`, internal ids, or internal plugin names, per [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5).
5. Build `ErrorKind::EventModeMismatch { name, actual, expected, emitter }`, whose `Display` text is fixed verbatim as:
   ```text
   {emitter}: event {name} dispatched with {actual}, expected {expected}: use {expected} instead
   ```
6. Apply the effective policy:
   - `EventModePolicy::Error`: return `Err(report.into())` immediately; no listener is selected, no filter runs, no listener executes.
   - `EventModePolicy::Warn`: under the deduplication lock, check `warned.epoch == epoch` (ignoring any stale pre-swap check so it can neither pollute the newer epoch's deduplication table nor trigger a duplicate warning) and record `(emitter, name, actual, expected)` in the bounded set (cap 4096, FIFO eviction); drop the lock; if newly inserted, call `ctx.error_sink()(Arc::new(report))` inside `catch_unwind`; then continue normal selection and dispatch.

### 3.3 Cross-language behaviour and the `emit` semantic difference under `Error`

| Case | Rust | JS / Node (incl. Cordis) / Bun / Python / Go / Swift / Kotlin |
| --- | --- | --- |
| Mismatch under `warn` (all 5 modes) | Dispatch proceeds unchanged; host `ErrorSink` receives `EventModeMismatch` once | **Identical**: dispatch proceeds unchanged (`emit` / `parallel` still ignore return values); host `ErrorSink` receives `EventModeMismatch` once naming the emitting row |
| Mismatch under `error` (`serial` / `bail` / `parallel` / `waterfall`) | Returns `Err(ErrorKind::EventModeMismatch)` at the call site; 0 listeners run | **Identical**: throws / returns `BindingError(code: "event-mode-mismatch", data: {event, actual, expected})` directly at the call site; 0 listeners run |
| Mismatch under `error` (`emit`) | `ctx.emit()` returns `Result<(), Error>`, so the caller receives `Err(ErrorKind::EventModeMismatch)` **synchronously** at the call site; 0 listeners run | **Call-site difference**: `ctx.emit()` is fire-and-forget (returns `undefined` in JS / Node / Bun / Cordis, `None` in Python, no return value in Go / Swift / Kotlin), so host rejection **cannot throw synchronously at the `ctx.emit()` call site**. `rutis-bridge` checks `EventBus::check_mode` in `SessionEvents::dispatch` **before** queuing into `run_emit` and refuses the request, guaranteeing **0 listeners run**; the rejection surfaces via the runtime/host refused-emit error reporting path while `ctx.emit()` returns `undefined` / `None` immediately |
| Unlisted event / unlisted dynamic name | Unchecked, zero warnings | **Identical**: unchecked, zero warnings |
| `internal/*` (Cordis) | N/A | Stays inside Node process, never reaches the bus, not subject to catalog |

This `emit` call-site difference under `Error` is why **`warn` is the consistent cross-language default**: under `warn`, all five modes keep identical call-site behaviour in every language and surface the same deduplicated diagnostic through the host's `ErrorSink`. Opting into `error` guarantees 0 listeners execute across all languages, throwing `BindingError(code: "event-mode-mismatch")` at the call site for `serial` / `bail` / `parallel` / `waterfall` (and returning `Err` synchronously for Rust `ctx.emit()`), while non-Rust `ctx.emit()` surfaces the refusal through the runtime/host refused-emit reporting path.

### 3.4 `rutis.json` and `rutis-host check` (step 3)

- Optional top-level `"events"` object in `rutis.json` (`format: 1`):
  ```json
  {
    "format": 1,
    "events": {
      "message": { "mode": "bail" },
      "http/request": { "mode": "waterfall", "on_mismatch": "error" }
    }
  }
  ```
- Validation rules (any failure causes `rutis-host check` / `run` / `dev` to refuse startup with exit code 1):
  - `mode` is required and must be one of `emit | parallel | serial | waterfall | bail`; typos get a suggestion via `rutis::config::closest`.
  - `on_mismatch` is optional (default `"warn"`) and must be `warn` or `error`.
  - Unknown fields are rejected (`#[serde(deny_unknown_fields)]`).
  - Keys cannot be empty or start with `rutis/` or `internal/`.
- `rutis-host check` comparison rules:
  - When a row's plugin optionally declares static event expectations via `Declared.events` and the catalog names a different `mode` for the same event:
    - If the catalog entry's `on_mismatch` is `"error"`: the row is marked unable to run (exit 1).
    - If `"warn"`: printed as a warning (exit 0), or promoted to an error (exit 1) under `--strict`.
  - When two enabled rows declare conflicting modes for the same static event name via `Declared.events` (even without `"events"` in `rutis.json`): warning by default, error (exit 1) under `--strict`.
  - Plugins without `Declared.events` (all existing plugins and Cordis plugins) and dynamic event names are silently skipped with zero warnings.
- `rutis-host run` / `dev` installs the `EventModeCatalog` in `Host::start_with` before mounting rows and updates it on `Host::reload`.

### 3.5 Explicit non-guarantees

- Does not scan plugin source code or guarantee catching unexecuted dispatch call sites during `check`.
- Does not check Cordis process-local `internal/*` events or host-emitted `rutis/*` framework events.
- Does not synchronize event catalogs automatically across WebSocket node links: each node enforces its own catalog on dispatches originating from it.
- `warn` policy does not alter dispatch semantics (`emit` and `parallel` continue to discard listener return values).

## 4. What developers see

All outputs below are locked word for word and name only terms the developer wrote (row names, plugin names, event names, mode names, and what to change) — no internal rutis terminology ([diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5).

### 4.1 Plugin authors

**When it works (TypeScript; Python, Go, Swift, Kotlin, Rust, and Cordis are unchanged):**

No code changes and no new imports. The blacklist plugin listens on `"message"` as before, and the dispatcher uses the expected mode:

```ts
// blacklist plugin
ctx.on('message', (msg) => msg.user === 'mallory')

// chat plugin
const blocked = ctx.bail('message', { user: 'mallory', text: 'spam' })
if (blocked) return
```

Output: none; the blacklist listener returns `true` and the message is blocked.

**When it fails:**

The default policy is `warn`, which is completely consistent across every language and all five modes (the dispatch proceeds unchanged and host stderr prints the diagnostic once). When a host enables `error` for an event, `serial` / `bail` / `parallel` / `waterfall` throw or return an error directly at the call site in every language. For `emit`, the language APIs differ: Rust's `ctx.emit()` returns `Result<(), Error>` synchronously at the call site, whereas JS / Node / Bun / Python / Go / Swift / Kotlin / Cordis `ctx.emit()` is fire-and-forget (returns `undefined` / `None` without throwing synchronously at the call site), so 0 listeners run and the refusal surfaces through the runtime/host refused-emit error reporting path:

| Trigger | Output word for word (and error code) | Recorded on | How to fix |
| --- | --- | --- | --- |
| Catalog policy is `warn` (default, identical across all languages); `chat` calls `ctx.emit('message', msg)` when catalog expects `bail` | Host stderr prints once: `chat: event message dispatched with emit, expected bail: use bail instead` (no error code; `emit` proceeds unchanged in every language) | `chat` (emitter row) | Change `ctx.emit('message', msg)` to `ctx.bail('message', msg)` |
| Catalog policy is `error`; `chat` calls `serial` / `bail` / `parallel` / `waterfall` (e.g., `await ctx.parallel('message', msg)` when catalog expects `bail`) | Throws / returns `BindingError` directly at the call site, `code: "event-mode-mismatch"`, `data: {"event":"message","actual":"parallel","expected":"bail"}`, message: `chat: event message dispatched with parallel, expected bail: use bail instead` (0 listeners run) | `chat` (emitter row) | Change `ctx.parallel('message', msg)` to `ctx.bail('message', msg)` |
| Catalog policy is `error`; Rust plugin `chat` calls `ctx.emit(Message { ... })` when catalog expects `bail` | `ctx.emit()` returns `Err(ErrorKind::EventModeMismatch)` **synchronously** at the call site, message: `chat: event message dispatched with emit, expected bail: use bail instead` (0 listeners run) | `chat` (emitter plugin) | Change `ctx.emit(...)` to `ctx.bail(...)` |
| Catalog policy is `error`; JS / Node / Bun / Python / Go / Swift / Kotlin / Cordis plugin `chat` calls `ctx.emit('message', msg)` when catalog expects `bail` | `ctx.emit()` returns `undefined` / `None` immediately (cannot throw synchronously at the call site); **0 listeners run**; the runtime/host refused-emit error reporting path prints: `chat: event message dispatched with emit, expected bail: use bail instead` (`BindingError`, `code: "event-mode-mismatch"`) | `chat` (emitter row) | Change `ctx.emit('message', msg)` to `ctx.bail('message', msg)` |

**Concepts to learn:** **+0**. Against the [#663](https://github.com/arcships/rutis/issues/663) concept budget of 7 concepts for plugin authors (plugin, services, `inject`, config, events, test kit, `dev` and rows), the total stays **7 / 7**. Even on error, the line mentions only the row name, event name, and mode names the author already wrote.

### 4.2 Those assembling plugins with `rutis-host`

**When it works:**

Add an optional `"events"` object to `rutis.json`:

```json
{
  "format": 1,
  "rows": [
    { "id": "blacklist", "name": "./blacklist.mjs" },
    { "id": "chat", "name": "./chat.mjs" }
  ],
  "events": {
    "message": { "mode": "bail" },
    "http/request": { "mode": "waterfall", "on_mismatch": "error" }
  }
}
```

Running `rutis-host check`:

```text
blacklist (file:///.../blacklist.mjs): ok
chat (file:///.../chat.mjs): ok
$ echo $?
0
```

(Cordis and leaf plugins without event declarations, as well as unlisted dynamic event names, produce no extra warnings.)

**When it fails:**

| Trigger | Output word for word (and exit code) | Recorded on | How to fix |
| --- | --- | --- | --- |
| Typo in `mode` (`"baill"`) in `rutis.json` | `rutis.json: events.message.mode: unknown mode "baill" (expected emit, parallel, serial, waterfall, or bail; did you mean "bail"?)` (exit `1`) | config `events.message.mode` | Change `"baill"` to `"bail"` |
| Invalid `on_mismatch` (`"ignore"`) in `rutis.json` | `rutis.json: events.message.on_mismatch: unknown policy "ignore" (expected warn or error)` (exit `1`) | config `events.message.on_mismatch` | Use `"warn"` or `"error"` |
| Empty event name (`""`) in `rutis.json` | `rutis.json: events: event name cannot be empty` (exit `1`) | config `events` | Name the event or remove the entry |
| Reserved prefix (`rutis/*` or `internal/*`) in `rutis.json` | `rutis.json: events.rutis/row: rutis/* events are owned by the host and cannot be configured here` / `rutis.json: events.internal/dispatch: internal/* events stay inside their runtime and cannot be configured here` (exit `1`) | config `events.<name>` | Remove the reserved entry |
| Unknown field in an event rule (`"strict": true`) | `rutis.json: events.message: unknown field "strict", expected "mode" or "on_mismatch"` (exit `1`) | config `events.message` | Replace with `"on_mismatch": "error"` |
| `rutis-host check` finds a row's optional event declaration disagrees with a `warn` catalog entry | `rutis-host: warning: chat (chat-plugin): event message declares emit, catalog expects bail` (exit `0`; with `--strict`: `rutis-host: error: ...`, exit `1`) | `chat` | Update the plugin's dispatch mode or `events.message.mode` in `rutis.json` |
| `rutis-host check` finds a row's optional event declaration disagrees with an `error` catalog entry | `chat (chat-plugin): cannot run: event message declares emit, catalog expects bail` (exit `1`) | `chat` | Update the plugin's dispatch mode or `events.message.mode` in `rutis.json` |
| Runtime mismatch during `rutis-host run` / `dev` | Stderr prints: `chat: event message dispatched with emit, expected bail: use bail instead` (once per `(emitter, event, actual, expected)` under `warn`; rejects dispatch under `error`) | `chat` | Fix the dispatch call in `chat` |

**Concepts to learn:** **+1 (optional)** — only assemblers who opt into `"events"` learn `events.<name>.mode` and `on_mismatch`; omitting `"events"` adds 0 concepts.

### 4.3 Host owners (Rust applications embedding `rutis`)

**When it works:**

```rust
use rutis::events::Mode;
use rutis::host::{CtxHost, ErrorKind, EventModeCatalog, EventModePolicy};

let catalog = EventModeCatalog::new()
    .expect("message", Mode::Bail)
    .expect_with("http/request", Mode::Waterfall, EventModePolicy::Error);
root.set_event_mode_catalog(catalog)?;
```

A custom `ErrorSink` or dispatch error handler can match `ErrorKind::EventModeMismatch { name, actual, expected, emitter }` directly. Dispatches matching the catalog produce no warnings and no output.

**When it fails:**

| Trigger | Output word for word (and error kind) | Recorded on | How to fix |
| --- | --- | --- | --- |
| Passing empty name, `rutis/*`, or `internal/*` to `set_event_mode_catalog` | `Err(ErrorKind::Validation { issues })` with exact issues: `events: event name cannot be empty`, `events.rutis/row: rutis/* events are owned by the host and cannot be configured here`, `events.internal/dispatch: internal/* events stay inside their runtime and cannot be configured here` (previous catalog remains active) | Host call site | Remove invalid keys and retry |
| Root `Ctx` dispatches `"message"` with wrong mode | Default `ErrorSink` / `Err` display: `the host: event message dispatched with emit, expected bail: use bail instead` (`ErrorKind::EventModeMismatch`) | `the host` | Use `root.bail(...)` |
| Child plugin `chat` dispatches `"message"` with wrong mode | Default `ErrorSink` / `Err` display: `chat: event message dispatched with emit, expected bail: use bail instead` (`ErrorKind::EventModeMismatch`) | `chat` | Use `ctx.bail(...)` |

**Concepts to learn:** **+1 (optional, `rutis::host` only)** — `EventModeCatalog` (with `EventModePolicy`) and `ErrorKind::EventModeMismatch`. The plugin-facing crate root `rutis::*` adds zero concepts.

## 5. Compatibility and migration

- **`PLUGIN_API` unchanged**: No version bump; existing compiled Go, Swift, Kotlin, and dylib plugins continue to run without rebuilding.
- **Cordis API and Cordis plugins unchanged**: Unmodified Cordis plugins already dispatch through the host `EventBus`; enabling a catalog with the default `warn` policy never breaks them and surfaces silently discarded return values immediately.
- **Session protocol unchanged**: No new frame `op` and no protocol version bump. Across sessions, `EventModeMismatch` reuses `BindingError` with additive `code: "event-mode-mismatch"` and `data`; older ends ignore unknown codes/data while still displaying the full message.
- **`rutis.json` (`format: 1`) additive field**: Existing `rutis.json` files without `"events"` behave identically. Note: an older `rutis-host` binary enforces `#[serde(deny_unknown_fields)]` on `HostConfig` and will reject a config that adds `"events"` as an unknown field until `rutis-host` is upgraded.
- **Rust public API 100% additive**: `ErrorKind` is already `#[non_exhaustive]`, `CtxHost` is sealed, and all new types live in `rutis::host`. No existing caller breaks.

## 6. Implementation

| Step | Scope | Done when |
| --- | --- | --- |
| 1 (this PR: design + PoC + formal model) | Bilingual design docs; `crates/rutis` implementation of `EventModePolicy`, `EventModeEntry`, `EventModeCatalog`, `ErrorKind::EventModeMismatch` (whose `wire_name()` already maps to `"BindingError"` in step 1), internal `pub(crate)` `EventBus` runtime check and epoch-synchronized bounded deduplication, `CtxHost::set_event_mode_catalog` / `event_mode_catalog`, and `Dispatch.emitter`; `specs/event-mode-catalog/` TLA+ model and mutation configs | `crates/rutis/tests/event_mode_catalog.rs` asserts verbatim output for the blacklist `warn`/`error` cases, all five matching modes, zero false positives on unlisted and dynamic events, zero-listener detection, per-event policy override, invalid catalog rejection, and concurrent catalog-swap consistency; `specs/event-mode-catalog/run.sh` matches `expected.txt`; `cargo test -p rutis`, `tools/public-api.sh`, `cargo fmt --check`, and doc/model checks pass |
| 2 (`rutis-bridge` wiring) | Populate `Dispatch.emitter` (row id / runtime name) in `SessionEvents::dispatch` and run the mode check before queuing `Mode::Emit`; build on step 1's `wire_name() -> "BindingError"` in `events/errors.rs` to wire `code: "event-mode-mismatch"` and `data` (`{"event", "actual", "expected"}`) on `EventModeMismatch`; add the code row to [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.3 | Real Node (including Cordis plugins) and Python integration tests verify every §4.1 output verbatim, including `warn` compatibility and `error` rejection on the blacklist scenario |
| 3 (`rutis-host` and `rutis-loader`) | Add optional top-level `"events"` to `HostConfig` with typo suggestions; install catalog in `Host::start_with` / `Host::reload`; add optional `Declared.events` in `rutis-loader` and report conflicts in `Loader::check` and `rutis-host check` / `--strict` | Acceptance tests match every §4.2 config error, conflict warning, `--strict` exit code, and runtime line verbatim, confirming zero false positives for Cordis and dynamic events |
| 4 (Guides and guarantees) | Update [rutis-host guide](guide/rutis-host.en.md), [Rust host guide](guide/rust-host.en.md), and [Cordis boundaries](guide/cordis-boundaries.en.md) (both languages); register guarantees in [guarantees](guarantees.en.md) | `node tools/check-md-links.mjs`, `check-guarantees.mjs`, and `formal-trace.mjs --check` all pass |
