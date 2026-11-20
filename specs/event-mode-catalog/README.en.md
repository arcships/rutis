# Optional event mode catalog and runtime checking (TLA+)

[中文](README.md)

Exhaustive TLC check of host event mode catalog updates (valid swaps, clears, and invalid updates), concurrent dispatchers (`emit`, `serial`, `bail`, unlisted events, and `rutis/*` framework events), per-epoch `Warn` deduplication, and two-phase listener close admission and drain. Issue [#1324](https://github.com/arcships/rutis/issues/1324); quality standard Q11.2; design: [optional event mode catalog and checking](../../docs/design-event-mode-catalog-2026-11-20.en.md) §2, §3.

## Design and code

| Model part | Design | Implementation |
| --- | --- | --- |
| Host catalog validation and atomic swap: validate (`""`, `rutis/*`, `internal/*`) before atomically incrementing `epoch`, replacing the catalog, resetting `warned` to the new epoch under `catalog -> warned` lock order, and updating `has_catalog` while still holding `catalog.write()` | Design §2 D1, D4, D6; §3.1 | `crates/rutis/src/events/catalog.rs` `EventModeCatalog::validate`; `crates/rutis/src/bus.rs` `EventBus::set_event_mode_catalog`, `EventBus::event_mode_catalog`; `crates/rutis/src/ctx.rs` `CtxHost::set_event_mode_catalog` |
| Preflight check before listener selection: `snapshot_with` runs `check_mode`; checks the `has_catalog` fast path, reads `(epoch, expected, policy)` under `catalog.read()`, and drops the read lock; `opts.framework` bypasses user catalog; `Error` rejects with 0 listeners invoked; `Warn` verifies `warned.epoch == epoch` before deduplicating per `(emitter, event, actual, expected)` and preserves delivery | Design §2 D3–D5; §3.2 | `crates/rutis/src/bus.rs` `EventBus::check_mode`, `EventBus::snapshot_with`, `WarnedMismatches`; `crates/rutis/src/ctx.rs` `Ctx::emitter_label`; `crates/rutis/src/error.rs` `ErrorKind::EventModeMismatch` |
| Two-phase close admission and in-flight call drain: catalog checks and swaps never cross the `lineage_closed` boundary or leak an in-flight call | Design §2 D5; §3.2; existing close admission | `crates/rutis/src/bus.rs` `EventBus::start`, `EventBus::start_alone`; `crates/rutis/src/bus/table.rs` `Entry::live` |

The design is in draft + PoC stage (2026-11-20): the `crates/rutis` kernel catalog and runtime check are implemented, and this model tracks the current PoC code.

## Model

- **Actors**: host catalog updates (`SetValidCatalog`, `SetInvalidCatalog`, plus the unlocked multi-step swap actions under mutation `nonAtomicSwap`), two concurrent dispatchers `d1` and `d2` (`BeginDispatch`, `CheckCatalog`, `RecordWarn`, `AdmitListener`, `FinishDelivery`, `CompleteCall`, `ResetDispatcher`), and two-phase close admission on closable listener `l2` (`CloseMark`, `ClosePublish`, `CloseCleanup`).
- **Events and dispatch shapes**: `Jobs` covers five representative dispatches: mismatched user `("msg", "emit", FALSE)` and `("msg", "serial", FALSE)`, matching user `("msg", "bail", FALSE)`, unlisted user `("other", "emit", FALSE)`, and framework `("rutis/row", "emit", TRUE)`.
- **Catalog, fast-path flag, and epochs**: `CatalogConfigs` switches between empty `("none", "warn")`, warning `("bail", "warn")`, and strict `("bail", "error")` (up to `MaxEpoch` times). Each healthy swap (`SetValidCatalog`) atomically increments `epoch`, installs the catalog, binds `warnedEpoch` to the new `epoch` while clearing `warned` / `epochWarnCount`, and updates `hasCatalog = (catalogMode # "none")` while holding `catalog.write()`. `SetInvalidCatalog` models attempting `""`, `rutis/*`, or `internal/*`: the valid protocol fails validation without touching the active catalog, `hasCatalog`, or `epoch`.
- **Dispatch pipeline**:
  - `BeginDispatch` starts a dispatch (up to `MaxDispatches` per dispatcher, exercising both same-epoch deduplication and cross-epoch reset);
  - `CheckCatalog` checks the `hasCatalog` fast path first, then reads the active catalog and `epoch`: framework dispatches bypass user rules; `Error` mismatches transition straight to `"rejected"`; `Warn` mismatches capture `dSnapEpoch[d] = epoch`, drop `catalog.read()`, and transition to `"recordingWarn"` so concurrent swaps may interleave before `warned.lock()`; unlisted and matching dispatches continue to `"delivering"`;
  - `RecordWarn` checks `warnedEpoch = dSnapEpoch[d]` under `warned.lock()`: stale pre-swap checks are ignored without polluting the newer epoch's deduplication table, while same-epoch mismatches record `<<d, actualMode>>` once and continue to `"delivering"`;
  - `AdmitListener` checks the two-phase close flags (`marked` / `closing` / `cleaned`) per listener, admitting live listeners into `inFlight` and `dInvoked`, and recording closed ones in `dSkippedClosed`;
  - `CompleteCall` completes an in-flight call; `CloseCleanup` runs only after `l2` published `closing` and has no in-flight calls.

## Simplifications

- Two dispatchers (`d1`, `d2`) and two listeners (always-live `l1`, closable `l2`); `"msg"` represents catalogued events, `"other"` unlisted user events, and `"rutis/row"` framework events.
- Omits mode-specific executors (`parallel` / `waterfall` internals), filter callbacks, dynamic listener registration/withdrawal, cross-node `invoke_entry`, and the 4096-key FIFO eviction bound (same-epoch deduplication, cross-epoch reset, and `CheckCatalog`/`RecordWarn` swap interleavings are exhaustively checked with `MaxEpoch = 2`, `MaxDispatches = 2`; bounded eviction is covered by code and unit/integration tests).
- Models two-phase close on a single closable listener `l2` (`marked -> closing -> cleaned`); full subtree postorder marking and concurrent spawning are verified separately in [`specs/event-close/`](../event-close/README.en.md).
- Assumes `ErrorSink` in `Warn` mode does not block or panic (in Rust it runs outside locks inside `catch_unwind`).
- Omits Rust atomic memory ordering (`Acquire`/`Release`), `rutis-bridge` wire forwarding, and `rutis.json` parsing.

## Invariants and liveness

| Name | Meaning |
| --- | --- |
| `TypeOK` | Catalog state, `hasCatalog`, `epoch`, `warnedEpoch`, deduplication sets, in-flight calls, and close sets stay within their valid domains |
| `NoPartialExecOnError` | Under `Error` policy, a rejected dispatch invokes 0 listeners (`dInvoked[d] = {}`) and leaves 0 in-flight calls |
| `AtomicCatalogValidation` | Invalid catalog updates (`""`, `rutis/*`, `internal/*`) never overwrite or clear the prior valid catalog or `epoch` |
| `CatalogFlagInSync` | Fast-path `hasCatalog` stays synchronized with whether a catalog is installed (`(catalogMode # "none") <=> hasCatalog`), `warnedEpoch = epoch`, and every key satisfies `k ∈ warned <=> epochWarnCount[k] = 1` (no lost or duplicate epoch warning) |
| `WarnPreservesDelivery` | Under `Warn` policy, mismatched dispatches never drop delivery — every eligible listener not skipped by close admission is invoked identically to an empty catalog — and each `(emitter, event, actual, expected)` warns at most once per catalog epoch |
| `FrameworkNeverBlocked` | Framework dispatches (`rutis/*`, `opts.framework = TRUE`) are never rejected by user catalog entries |
| `CloseDrainPreserved` | Catalog checks and swaps never admit a listener after close commit (`closeCommitted`), never allow cleanup with an in-flight call, and preserve `cleaned ⊆ closing ⊆ marked` |
| `AllInFlightDrain` | Liveness (`catalog-drain`): every admitted in-flight call eventually completes under concurrent catalog swaps and close admission |

## Results

TLC 2.19 (tla2tools v1.7.4), OpenJDK 17.0.18, 16 cores, `-workers auto`. `ok` means exhaustive exploration finished with all properties satisfied; mutation configs stop at the first counterexample, so state counts depend on search order. Full directory runs in ~36 seconds.

| Config | Scope | Outcome | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `catalog-healthy` | `MaxEpoch = 2`, `MaxDispatches = 2`; concurrent catalog swaps, invalid updates, `CheckCatalog`/`RecordWarn` lock-gap interleavings, all five dispatch shapes, and close admission; all safety invariants (incl. `CatalogFlagInSync`) | ok | 3,780,320 | 24 s |
| `catalog-drain` | `MaxEpoch = 1`, `MaxDispatches = 1`; liveness `AllInFlightDrain` | ok | 227,788 | 8 s |
| `mut-check-after-start` | Mutation: start listener `l1` before `CheckCatalog` | violated `NoPartialExecOnError` | 805 (stops on counterexample) | 1 s |
| `mut-clear-on-invalid` | Mutation: clear prior catalog before invalid validation fails | violated `AtomicCatalogValidation` | 128 (stops on counterexample) | <1 s |
| `mut-block-framework` | Mutation: apply user catalog check to `rutis/*` framework dispatch | violated `FrameworkNeverBlocked` | 833 (stops on counterexample) | 1 s |
| `mut-warn-drops-delivery` | Mutation: drop listener delivery after recording a `Warn` | violated `WarnPreservesDelivery` | 580 (stops on counterexample) | 1 s |
| `mut-nonatomic-swap` | Mutation: release `catalog.write()` between updating `catalogMode`, storing `hasCatalog`, and clearing `warned` (allowing a second swap or `CheckCatalog` to interleave) | violated `CatalogFlagInSync` | 2 (stops on counterexample) | 1 s |

## Mutations and counterexamples

| Config | Mutation | Violated | Counterexample |
| --- | --- | --- | --- |
| `mut-check-after-start` | Allow `StartBeforeCheck` to invoke a listener before `CheckCatalog` runs | `NoPartialExecOnError` | `SetValidCatalog(<<"bail","error">>)` -> `BeginDispatch(d, <<"msg","emit",FALSE>>)` -> `StartBeforeCheck(d, l1)` records `l1` in `dInvoked[d]` and `inFlight` -> `CheckCatalog(d)` rejects, leaving `dState[d] = "rejected"` with `dInvoked[d] = {"l1"}` |
| `mut-clear-on-invalid` | `SetInvalidCatalog` clears `catalogMode` before validation fails | `AtomicCatalogValidation` | `SetValidCatalog(<<"bail","error">>)` followed by `SetInvalidCatalog`: `catalogMode` becomes `"none"` while `lastValidMode` is still `"bail"` |
| `mut-block-framework` | `CheckCatalog` ignores `dFw[d]` and applies a strict user catalog to framework emits | `FrameworkNeverBlocked` | `SetValidCatalog(<<"bail","error">>)` -> `BeginDispatch(d, <<"rutis/row","emit",TRUE>>)` -> `CheckCatalog(d)` transitions the framework dispatch to `"rejected"` |
| `mut-warn-drops-delivery` | `RecordWarn` ends immediately after warning under `Warn` instead of transitioning to `"delivering"` | `WarnPreservesDelivery` | `SetValidCatalog(<<"bail","warn">>)` -> `BeginDispatch(d, <<"msg","emit",FALSE>>)` -> `CheckCatalog(d)` -> `RecordWarn(d)` sets `dState[d] = "done"`, `dResult[d] = "dropped"` with 0 eligible listeners invoked |
| `mut-nonatomic-swap` | Split `set_event_mode_catalog` so `catalog.write()` is dropped after writing `catalogMode`, before updating `hasCatalog` and clearing `warned`, permitting concurrent swaps and `CheckCatalog`/`RecordWarn` to interleave | `CatalogFlagInSync` | After `NonAtomicWriteCatalog(<<"bail","warn">>)` releases the write lock, `catalogMode = "bail"` and `epoch = 1` while `hasCatalog = FALSE` and `warnedEpoch = 0` (and an interleaved swap or `RecordWarn` can leave `hasCatalog` inverted or wipe the new epoch's `warned` key, causing duplicate same-epoch warnings) |

## Conclusions

- The protocol holds across all 3,780,320 safety states and 227,788 liveness states: preflight mode checking runs before listener selection so `Error` produces zero partial execution; `validate()` runs before mutating state so invalid catalogs never disturb an active catalog; updating `epoch`, `warned`, and `has_catalog` under `catalog.write()` paired with checking `warnedEpoch = dSnapEpoch[d]` in `RecordWarn` keeps `has_catalog` in sync and prevents both lost and duplicate epoch warnings; framework dispatches bypass user rules; `Warn` preserves delivery identically to an empty catalog; and close admission/drain remains intact with no leaked flights.
- All five deliberate mutations are caught by TLC in <= 1 s with minimal counterexamples, confirming none of the core invariants are vacuous.
- Outside this model's scope: Rust atomic memory ordering, the 4096-entry FIFO eviction cap, `rutis-bridge` cross-session `BindingError` encoding, and `rutis.json` parsing (covered by concurrency unit tests in `crates/rutis/src/bus.rs`, `crates/rutis/tests/event_mode_catalog.rs`, and steps 2–3 integration tests).

## How to run

Requires Java 11+ and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/event-mode-catalog/run.sh                    # all configs
TLA2TOOLS=/tmp/tla2tools.jar specs/event-mode-catalog/run.sh catalog-healthy    # one config
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/event-mode-catalog/run.sh      # cap TLC threads
```

`run.sh` compares every outcome with `expected.txt` and exits 0 only when all match; logs and counterexamples go to `TLC_OUT` (a temporary directory by default). CI's `specs` matrix includes `event-mode-catalog` (see [CI](../../docs/ci.en.md) §4).

## Maintenance

Update this model and `expected.txt` whenever design §2 D1–D9 or §3.1–§3.2 changes, or when `crates/rutis/src/bus.rs` (`set_event_mode_catalog`, `check_mode`, `snapshot_with`), `crates/rutis/src/events/catalog.rs` (`validate`), or `crates/rutis/src/ctx.rs` (`emitter_label`) changes (standard Q11.2.3).
