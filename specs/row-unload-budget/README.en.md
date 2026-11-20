# Per-row unload budget and taint state: Route B formal model (TLA+)

[中文](README.md)

TLC exhaustively explores all interleavings of multi-row, multi-generation unload timeouts, lingering background cleanups, framework withdrawals, and runtime restarts inside one shared runtime process (Node, Bun, or Python). It proves Route B (taint-on-timeout, refusing silent in-place reload in the same runtime process until restart) safe and produces a concrete counterexample showing Route A (allow silent in-place reload) violates single-generation isolation in a shared process. Issue #1323; quality standard Q11.2; design: [per-row unload budget and diagnostics](../../docs/design-row-unload-budget-2026-11-20.en.md) §2, §3, aligned with [design principles](../../docs/design-philosophy.en.md) §7 and [unload order](../../docs/design-unload-order-2026-11-01.en.md) D8.

## Design and code it corresponds to

| Part of the model | Design | Implementation |
| --- | --- | --- |
| Leaf unload stack: outer cleanup `c2` → framework service withdrawal `wd` → inner cleanup `c1`; on timeout skip remaining cleanups, still run all withdrawals, refuse late `provide` | design §2 D2, D3, D5; §3.2, §3.3 | `node/rutis-runtime/src/leaf.mjs` `toCordis`, `drain`, `ctx.provide`; `bun/rutis-bun/src/runner.ts` `Runtime.unload`, `Runtime.provide`; `python/rutis/rutis/runner.py` `Runtime.unload`, `Context.provide` |
| Node native Cordis fiber: `Promise.all` concurrent disposal, service withdrawal runs on first microtask, stop waiting on timeout and finish `finally` teardown | design §2 D6; §3.3 | `node/rutis-runtime/src/runner.mjs` `disposeRowFiber`, `unloadRow`, `dispose` |
| Route B taint table: mark timed-out row `tainted`, refuse in-place `rows.load` explicitly until runtime restart; unrelated healthy rows stay unaffected | design §2 D4, D5; §3.3, §4 | `node/rutis-runtime/src/runner.mjs` `markTainted`, `loadRow`; `bun/rutis-bun/src/runner.ts` `Runtime.load`, `Runtime.unload`; `python/rutis/rutis/runner.py` `Runtime.load`, `Runtime.unload` |
| Isolation of late actions and late unhandled rejections from lingering cleanups | design §2 D3, D5; §3.3 | `node/rutis-runtime/src/leaf.mjs` `drain`, `ctx.provide`; `node/rutis-runtime/src/runner.mjs` `disposeRowFiber`; `bun/rutis-bun/src/runner.ts` `Runtime.unload`; `python/rutis/rutis/runner.py` `_consume_task_result`, `Runtime.unload` |

The design document's status is "draft, PoC implemented" (2026-11-20): Node, Bun, and Python all implement Route B tainting with unit tests, and this model tracks both the design and the PoC.

## The model

- **Participants**: the Host (`Load`, `LoadRefused`, `StartUnload`, `RestartRuntime`), one shared runtime process (`epoch ∈ 1..MaxEpoch`), multiple rows (`Rows = {"w", "m"}`) across generations (`gen ∈ 1..MaxGen`), and the set `lingering` of stuck cleanups still running in the background of the current process after a timeout.
- **Leaf mode (`Mode = "leaf"`)**: unloading drains in strict reverse order `c2` (outer user cleanup) → `wd` (framework service withdrawal) → `c1` (inner user cleanup) → `finish` (`finally` teardown and reply):
  - `StartUnload(r)` withdraws listeners immediately (§7.1);
  - `RunC2Ok(r)` completes `c2` within budget; `TimeoutC2(r)` expires the budget, places `<<epoch, r, gen[r]>>` into `lingering`, and adds `r` to `tainted` under `Policy = "taint"`;
  - `RunWithdraw(r)` withdraws the generation's service (D8: never skipped by timeout); after a timeout it jumps directly to `finish`, skipping `c1`;
  - `FinishUnload(r)` finishes teardown and replies to the host (`hostState[r]` becomes `"tainted"` under Route B, or `"idle"` under Route A).
- **Native Cordis mode (`Mode = "cordis"`)**: `CordisWithdraw(r)` models `ctx.provide`'s disposer deleting the service from Cordis's store on its first microtask; `CordisFinishOk(r)` / `CordisTimeout(r)` model `Promise.all` settling within budget or timing out and proceeding to `FinishUnload(r)`.
- **Lingering old cleanups**: any `item ∈ lingering` in the active `epoch` may finish later (`LingeringFinish(item)`) or attempt a late `provide` / `emit` / `withdraw` / error against a live sibling row or newer generation (`LingeringLateAction(item, target)`).
- **Runtime restart (`RestartRuntime`)**: when `tainted ≠ {}` and `epoch < MaxEpoch`, restarting the runtime increments `epoch`, terminates all old `lingering` tasks, clears `tainted`, and resets all rows to `"idle"`.

## Simplifications

- Each row is represented by one service, one listener set, and two user cleanups (`c2` registered after `provide`, `c1` before `provide`); any prefix of cleanups hanging reduces to the first hung cleanup followed by at least one remaining user cleanup.
- Step 4 listener settlement (capped by `RUTIS_SHUTDOWN_TIMEOUT`) is omitted; the model begins at unload-stack draining.
- Synchronous infinite loops blocking the single JS/Python event-loop thread (handled by the 5 s blocking watch and shutdown watchdog) are out of scope.
- Time is abstracted as nondeterministic within-budget completion vs budget expiration; exact millisecond values, error text formatting, wire framing, and remote node links are omitted.

## Invariants

| Name | Meaning |
| --- | --- |
| `TypeOK` | All state variables stay within their declared domains |
| `NoOrphanWithdrawal` | D8 invariant: after any normal or timed-out unload finishes (`hostState ∈ {"idle", "tainted"}`), no service or listener of that row remains registered |
| `SerialCleanupsWithinGen` | D3 invariant: within a generation, inner user cleanup `c1` never starts while outer user cleanup `c2` has timed out and is still `lingering` |
| `NoCrossGenCorruption` | D5 / isolation invariant: a lingering old cleanup never fails, withdraws, or corrupts another healthy row or a newer generation |
| `NoConcurrentSameRowInSharedProcess` | Core Route B property: within one shared runtime process (`epoch`), a row `r` is never `"running"` a new generation concurrently with its own lingering old cleanup |

## Results

TLC 2.19 (tla2tools v1.7.4), OpenJDK 17.0.18, 16 cores, `-workers auto`. "Pass" means exhaustive exploration completed with all listed invariants holding; violated configurations stop at the first counterexample. The whole directory completes in ~5 s.

| Configuration | Content | Outcome | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `budget-route-b` | Route B (`Policy = "taint"`), leaf mode, 2 rows / 2 gens / 2 epochs, all 5 invariants | pass | 1,185 | 1 s |
| `budget-route-b-cordis` | Route B (`Policy = "taint"`), Node native Cordis mode, 2 rows / 2 gens / 2 epochs, all 5 invariants | pass | 702 | <1 s |
| `budget-route-a` | Route A (`Policy = "allowReload"`), leaf mode, checking `NoConcurrentSameRowInSharedProcess` | violates `NoConcurrentSameRowInSharedProcess` | 198 (stops at counterexample) | 1 s |
| `budget-mut-skip-withdraw` | Mutation: skip framework withdrawal `wd` on timeout | violates `NoOrphanWithdrawal` | 9 (stops at counterexample) | 1 s |
| `budget-mut-continue-serial` | Mutation: keep running inner cleanup `c1` after `c2` timed out | violates `SerialCleanupsWithinGen` | 14 (stops at counterexample) | 1 s |
| `budget-mut-corrupt-new-gen` | Mutation: lingering cleanup lacks isolation guards and corrupts a live row | violates `NoCrossGenCorruption` | 70 (stops at counterexample) | 1 s |

## Mutations and counterexamples

Every core property and the Route A vs Route B choice has a deliberate failing configuration TLC must catch (`violated:<name>` in `expected.txt`):

| Configuration | Policy / Mutation | Violated | Counterexample |
| --- | --- | --- | --- |
| `budget-route-a` | `Policy = "allowReload"` (Route A: allow silent in-place reload in the same runtime) | `NoConcurrentSameRowInSharedProcess` | `Load("w")` (gen 1) → `StartUnload("w")` → `TimeoutC2("w")` (`<<1,"w",1>> ∈ lingering`) → `RunWithdraw("w")` → `FinishUnload("w")` (`hostState["w"] = "idle"`) → `Load("w")` (gen 2 starts `"running"` while gen 1's cleanup is still lingering in `epoch = 1`) |
| `budget-mut-skip-withdraw` | `Mutation = "skipWithdraw"` (`TimeoutC2` jumps over `wd` to `finish`) | `NoOrphanWithdrawal` | `Load("w")` → `StartUnload("w")` → `TimeoutC2("w")` → `FinishUnload("w")`: `hostState["w"] = "tainted"`, yet `svcOwner["w"] = <<1, 1>>` remains registered |
| `budget-mut-continue-serial` | `Mutation = "continueSerial"` (`RunWithdraw` still advances to `c1` after timeout) | `SerialCleanupsWithinGen` | `Load("w")` → `StartUnload("w")` → `TimeoutC2("w")` → `RunWithdraw("w")` → `RunC1("w")`: `c1` starts while `c2` is still in `lingering`, setting `c1StartedWhileHung["w"] = TRUE` |
| `budget-mut-corrupt-new-gen` | `Mutation = "corruptNewGen"` (unisolated lingering cleanup mutates a running sibling row) | `NoCrossGenCorruption` | `Load("w")`, `Load("m")` → `StartUnload("w")` → `TimeoutC2("w")` → `LingeringLateAction(<<1,"w",1>>, "m")`: `corrupted["m"] = TRUE` |

## Conclusion

- Within the explored state space, **Route B** (run all framework withdrawals + skip remaining user cleanups + taint the row in this runtime process until restart) satisfies all four safety invariants for both leaf sequential stacks and Node native Cordis fibers; healthy sibling rows continue unaffected and restarting the runtime process restores the tainted row cleanly.
- The **Route A** counterexample proves directly that because JS and Python cannot forcibly kill a stuck cleanup, allowing silent in-place reload in the same process inevitably allows two generations of the same row to execute concurrently, violating [design principles](../../docs/design-philosophy.en.md) §7 ("explicit over implicit; no silent degradation").
- The three mutation configs pin the D8 withdrawal invariant, D3 skipping of subsequent user cleanups, and lingering-cleanup isolation — none of the invariants is vacuous.

## How to run

Needs Java 11+ and `tla2tools.jar` ([v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4), sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`). Do not commit the jar.

```sh
TLA2TOOLS=/tmp/tla2tools.jar specs/row-unload-budget/run.sh                    # every configuration
TLA2TOOLS=/tmp/tla2tools.jar specs/row-unload-budget/run.sh budget-route-b     # one configuration
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/row-unload-budget/run.sh      # limit TLC's workers
```

`run.sh` compares each configuration's outcome with `expected.txt` and exits 0 when all match; logs and counterexamples go to `TLC_OUT` (a temporary directory by default). This directory is registered in CI's `specs` job matrix ([CI](../../docs/ci.en.md) §4).

## Maintenance

Update this model and `expected.txt` together with [per-row unload budget and diagnostics](../../docs/design-row-unload-budget-2026-11-20.en.md) §2–§3 or the per-row unload budget / taint logic in Node (`leaf.mjs`, `runner.mjs`), Bun (`runner.ts`), or Python (`runner.py`) (standard Q11.2.3).
