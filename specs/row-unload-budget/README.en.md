# Per-row unload budget and taint state: Route B formal model (TLA+)

[中文](README.md)

TLC exhaustively explores all interleavings of multi-scope row instances (including copies sharing the same bare row name across different `instances` scopes), multi-generation unload timeouts, lingering background cleanups acting on real shared runtime tables, framework withdrawals, and runtime restarts inside one shared runtime process (Node, Bun, or Python). It proves Route B (scoped taint-on-timeout, refusing silent in-place reload in the same runtime process until restart) safe and produces concrete counterexamples for Route A (allow silent in-place reload) and 6 deliberate guard mutations. Issue #1323; quality standard Q11.2; design: [per-row unload budget and diagnostics](../../docs/design-row-unload-budget-2026-11-20.en.md) §2, §3, aligned with [design principles](../../docs/design-philosophy.en.md) §7 and [unload order](../../docs/design-unload-order-2026-11-01.en.md) D8.

## Design and code it corresponds to

| Part of the model | Design | Implementation |
| --- | --- | --- |
| Leaf unload stack: outer cleanup `c2` → framework service withdrawal `wd` → inner cleanup `c1`; on timeout skip remaining cleanups and still run all withdrawals | design §2 D2, D3; §3.2, §3.3 | `node/rutis-runtime/src/leaf.mjs` `toCordis`, `drain`; `bun/rutis-bun/src/runner.ts` `Runtime.unload`; `python/rutis/rutis/runner.py` `Runtime.unload` |
| Node native Cordis fiber: `Promise.all` concurrent disposal, service withdrawal runs on first microtask, stop waiting on timeout and finish `finally` teardown | design §2 D6; §3.3 | `node/rutis-runtime/src/runner.mjs` `disposeRowFiber`, `unloadRow`, `dispose` |
| Route B scoped taint table: key `taintedSlots` by `(rowName, scope)` (plus exact session key `key`), refuse in-place reload of that scoped instance explicitly until restart; copies with the same bare name in other scopes and healthy siblings stay unaffected | design §2 D4; §3.3, §4 | `node/rutis-runtime/src/runner.mjs` `taintScopeKey`, `markTainted`, `findTaint`, `loadRow`; `bun/rutis-bun/src/runner.ts` `taintScopeKey`, `Runtime.load`, `Runtime.unload`, `Runtime.update`; `python/rutis/rutis/runner.py` `_taint_scope_key`, `Runtime.load`, `Runtime.unload`, `Runtime.update` |
| Shared-table guards against lingering cleanups: `LateProvide` (active-row + generation guard), `LateWithdraw` (registration identity `<<epoch, slot, gen, token>>` check), `LateUnhandledError` (rejection absorption + active-origin guard) | design §2 D5; §3.3 | `node/rutis-runtime/src/leaf.mjs` `ctx.provide`, `drain`; `node/rutis-runtime/src/runner.mjs` `disposeRowFiber`, `charge`; `bun/rutis-bun/src/runner.ts` `Runtime.provide`, `Runtime.unload`; `python/rutis/rutis/runner.py` `Context.provide`, `Runtime.provide`, `_consume_task_result`, `Runtime.charge` |

The design document's status is "draft, PoC and TLA+ model implemented" (2026-11-20): Node, Bun, and Python all implement scoped Route B tainting with unit tests, and this model tracks both the design and the PoC.

## The model

- **Participants and real shared tables**: the Host (`Load`, `LoadRefused`, `StartUnload`, `RestartRuntime`), one shared runtime process (`epoch ∈ 1..MaxEpoch`), scoped row slots `Slots = RowNames × Scopes` (default `RowNames = {"w"}`, `Scopes = {"s1", "s2"}`, directly modeling two instances of bare row name `"w"` in different scopes), and three shared runtime tables:
  - `svcTable[svc]`: shared service table mapping each service to `NoOwner` or owner identity `<<epoch, slot, gen, token>>` (`token = 1` for legitimate load registrations, `token = 2` for unguarded late `provide`);
  - `rowStatus[s]`: runtime row table state (`"none" | "active" | "draining" | "failed"`);
  - `taintedSlots`: the process's Route B taint set; plus `lingering` tracking stuck background cleanups with their origin identity and service name.
- **Leaf mode (`Mode = "leaf"`)**: unloading drains in strict reverse order `c2` (outer user cleanup) → `wd` (framework service withdrawal) → `c1` (inner user cleanup) → `finish` (`finally` teardown and reply):
  - `StartUnload(s)` withdraws listeners immediately (§7.1) and transitions `rowStatus[s]` to `"draining"`;
  - `RunC2Ok(s)` completes `c2` within budget; `TimeoutC2(s)` expires the budget, puts `<<epoch, s, gen[s], 1, rowSvc[s]>>` into `lingering`, and adds `TaintFor(s)` to `taintedSlots`;
  - `RunWithdraw(s)` withdraws this generation's registration identity (D8: never skipped by timeout); after a timeout it jumps directly to `finish`, skipping `c1`;
  - `FinishUnload(s)` finishes teardown and replies to the host (`hostState[s]` becomes `"tainted"` under Route B, or `"idle"` under Route A).
- **Native Cordis mode (`Mode = "cordis"`)**: `CordisWithdraw(s)` models `ctx.provide`'s disposer deleting the service from Cordis's store on its first microtask; `CordisFinishOk(s)` / `CordisTimeout(s)` model `Promise.all` settling within budget or timing out and proceeding to `FinishUnload(s)`.
- **Real lingering-cleanup actions on shared tables**: any `item ∈ lingering` in the active `epoch` may settle (`LingeringFinish(item)`) or attempt three real operations against `svcTable` and `rowStatus`:
  - `LateProvide(item, svc)`: checks `rowStatus[s] = "active" ∧ gen[s] = g` before mutating `svcTable`; mutation `"missingActiveGuard"` omits the guard and writes `svcTable`;
  - `LateWithdraw(item)`: checks exact registration identity `svcTable[svc] = <<ep, s, g, tok>>` before deleting; mutation `"withdrawByNameOnly"` deletes by service name alone;
  - `LateUnhandledError(item, target)`: checks rejection-absorption / active-origin attribution before failing a row; mutation `"errorCorruptsLive"` marks an active `target` `"failed"` in `rowStatus`.
- **Runtime restart (`RestartRuntime`)**: when `taintedSlots ≠ {}` and `epoch < MaxEpoch`, restarting the runtime increments `epoch`, clears `lingering`, `taintedSlots`, and shared tables, and resets all slots to `"idle"`.

## Simplifications

- Each slot claims at most one shared service (`Services = {"forecast"}`; claimed when free, otherwise loaded concurrently and able to take over after withdrawal), one listener set, and two user cleanups (`c2` registered after `provide`, `c1` before `provide`); any prefix of cleanups hanging reduces to the first hung cleanup followed by at least one remaining user cleanup.
- Step 4 listener settlement (capped by `RUTIS_SHUTDOWN_TIMEOUT`) is omitted; the model begins at unload-stack draining.
- Synchronous infinite loops blocking the single JS/Python event-loop thread (handled by the 5 s blocking watch and shutdown watchdog) are out of scope.
- Time is abstracted as nondeterministic within-budget completion vs budget expiration; exact millisecond values, error text formatting, wire framing, and remote node links are omitted.

## Invariants

| Name | Meaning |
| --- | --- |
| `TypeOK` | All state variables stay within their declared domains |
| `NoOrphanWithdrawal` | D8 invariant: after any normal or timed-out unload finishes (`hostState ∈ {"idle", "tainted"}`), no service in `svcTable` or listener remains owned by that slot |
| `SerialCleanupsWithinGen` | D3 invariant: within a generation, inner user cleanup `c1` never starts while outer user cleanup `c2` has timed out and is still `lingering` |
| `ServiceOwnerValid` | Shared service-table validity: every non-`NoOwner` entry in `svcTable` belongs to the current `epoch`, an `"active"` or `"draining"` slot at its current `gen`, with legitimate load token `1` and matching `rowSvc` |
| `ActiveServiceIntact` | Live service integrity: every `"active"` slot that registered a service still owns its exact `<<epoch, s, gen[s], 1>>` entry in `svcTable` (never deleted by a late `withdraw` or overwritten by a late `provide`) |
| `NoSpuriousRowFailure` | Row-table isolation: no live slot is ever marked `rowStatus = "failed"` by a lingering old cleanup's late unhandled error |
| `NoCrossGenCorruption` | D5 cross-slot / cross-generation isolation conjunction: `ServiceOwnerValid ∧ ActiveServiceIntact ∧ NoSpuriousRowFailure` |
| `HealthyInstanceNotBlocked` | Multi-instance scoped taint isolation: `taintedSlots ⊆ timedOutInEpoch` — a healthy instance (even sharing the same bare row name in another scope) is never falsely tainted or blocked |
| `NoConcurrentSameRowInSharedProcess` | Core Route B property: within one shared runtime process (`epoch`), a scoped slot `s` is never `"active"` in a new generation concurrently with its own lingering old cleanup |

## Results

TLC 2.19 (tla2tools v1.7.4), OpenJDK 17.0.18, 16 cores, `-workers auto`. "Pass" means exhaustive exploration completed with all listed invariants holding; violated configurations stop at the first counterexample. The whole directory completes in ~6 s.

| Configuration | Content | Outcome | Distinct states | Time |
| --- | --- | --- | --- | --- |
| `budget-route-b` | Route B (`Policy = "taint"`), leaf mode, two same-name scoped instances `{<<"w","s1">>, <<"w","s2">>}` / 2 gens / 2 epochs, all 9 invariants | pass | 3,271 | 1 s |
| `budget-route-b-cordis` | Route B (`Policy = "taint"`), Node native Cordis mode, two same-name scoped instances / 2 gens / 2 epochs, all 9 invariants | pass | 1,369 | 1 s |
| `budget-route-a` | Route A (`Policy = "allowReload"`), leaf mode, checking `NoConcurrentSameRowInSharedProcess` | violates `NoConcurrentSameRowInSharedProcess` | 187 (stops at counterexample) | 1 s |
| `budget-mut-skip-withdraw` | Mutation: skip framework withdrawal `wd` on timeout | violates `NoOrphanWithdrawal` | 9 (stops at counterexample) | 1 s |
| `budget-mut-continue-serial` | Mutation: keep running inner cleanup `c1` after `c2` timed out | violates `SerialCleanupsWithinGen` | 16 (stops at counterexample) | <1 s |
| `budget-mut-missing-active-guard` | Mutation: `LateProvide` omits active/draining guard and writes `svcTable` | violates `ServiceOwnerValid` | 119 (stops at counterexample) | <1 s |
| `budget-mut-withdraw-by-name` | Mutation: `LateWithdraw` deletes from `svcTable` by name without identity check | violates `ActiveServiceIntact` | 458 (stops at counterexample) | <1 s |
| `budget-mut-error-corrupts-live` | Mutation: `LateUnhandledError` omits absorption/origin guard and fails a live sibling | violates `NoSpuriousRowFailure` | 195 (stops at counterexample) | 1 s |
| `budget-mut-taint-by-bare-name` | Mutation: tainting keys by bare `rowName`, ignoring `scope` | violates `HealthyInstanceNotBlocked` | 72 (stops at counterexample) | 1 s |

## Mutations and counterexamples

Every core property, real shared-table guard, and the Route A vs Route B choice has a deliberate failing configuration TLC must catch (`violated:<name>` in `expected.txt`):

| Configuration | Policy / Mutation | Violated | Counterexample |
| --- | --- | --- | --- |
| `budget-route-a` | `Policy = "allowReload"` (Route A: allow silent in-place reload in the same runtime) | `NoConcurrentSameRowInSharedProcess` | `Load(<<"w","s1">>)` (gen 1) → `StartUnload` → `TimeoutC2` (`<<1,<<"w","s1">>,1,1,"forecast">> ∈ lingering`) → `RunWithdraw` → `FinishUnload` (`"idle"`) → `Load(<<"w","s1">>)` (gen 2 becomes `"active"` while gen 1's cleanup is still lingering in `epoch = 1`) |
| `budget-mut-skip-withdraw` | `Mutation = "skipWithdraw"` (`TimeoutC2` jumps over `wd` to `finish`) | `NoOrphanWithdrawal` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2` → `FinishUnload`: `hostState = "tainted"`, yet `svcTable["forecast"] = <<1, <<"w","s1">>, 1, 1>>` remains registered |
| `budget-mut-continue-serial` | `Mutation = "continueSerial"` (`RunWithdraw` still advances to `c1` after timeout) | `SerialCleanupsWithinGen` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2` → `RunWithdraw` → `RunC1`: `c1` starts while `c2` is still in `lingering`, setting `c1StartedWhileHung[<<"w","s1">>] = TRUE` |
| `budget-mut-missing-active-guard` | `Mutation = "missingActiveGuard"` (`LateProvide` omits active-row guard) | `ServiceOwnerValid` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2` → `LateProvide`: writes untracked token-2 registration `<<1, <<"w","s1">>, 1, 2>>` into `svcTable["forecast"]` |
| `budget-mut-withdraw-by-name` | `Mutation = "withdrawByNameOnly"` (`LateWithdraw` omits registration-identity check) | `ActiveServiceIntact` | `Load(<<"w","s2">>)` owns `"forecast"` → `StartUnload` → `TimeoutC2` → `RunWithdraw` frees `"forecast"` → `Load(<<"w","s1">>)` takes over `"forecast"` → `<<"w","s2">>`'s `LateWithdraw` clears `svcTable["forecast"]`, deleting `<<"w","s1">>`'s live service |
| `budget-mut-error-corrupts-live` | `Mutation = "errorCorruptsLive"` (`LateUnhandledError` omits absorption/origin guard) | `NoSpuriousRowFailure` | `Load(<<"w","s1">>)` and `Load(<<"w","s2">>)` run concurrently → `StartUnload(<<"w","s1">>)` → `TimeoutC2` → `LateUnhandledError` marks healthy active `rowStatus[<<"w","s2">>]` as `"failed"` |
| `budget-mut-taint-by-bare-name` | `Mutation = "taintByBareName"` (taint keyed by bare `rowName`) | `HealthyInstanceNotBlocked` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2`: `timedOutInEpoch = {<<"w","s1">>}`, yet `taintedSlots = {<<"w","s1">>, <<"w","s2">>}`, falsely blocking the healthy `"w"` copy in scope `"s2"` |

## Conclusion

- Within the explored state space, **Route B** (complete framework withdrawals + skip remaining user cleanups + scoped `(rowName, scope)` tainting until restart + real shared-table guards) satisfies all 9 safety invariants for both leaf sequential stacks and Node native Cordis fibers; healthy same-name instances in other scopes and unrelated sibling rows continue unaffected, and restarting the runtime process restores tainted slots cleanly.
- The **Route A** counterexample proves directly that because JS and Python cannot forcibly kill a stuck cleanup, allowing silent in-place reload in the same process inevitably allows two generations of the same scoped row to execute concurrently, violating [design principles](../../docs/design-philosophy.en.md) §7 ("explicit over implicit; no silent degradation").
- The 6 mutation configs pin D8 withdrawals, D3 serial order, the active-row guard on late `provide`, registration-identity verification on late `withdraw`, row-table isolation against late errors, and multi-instance scoped taint isolation — none of the invariants is vacuous or tautological.

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
