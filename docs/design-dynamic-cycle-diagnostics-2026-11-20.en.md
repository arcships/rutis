# Dynamic diagnostics for long cycles and unknown providers

[中文](design-dynamic-cycle-diagnostics-2026-11-20.md)

Status: draft, with PoC. Date: 2026-11-20 (same-day revision: clarified semantic boundaries and non-atomic snapshot transients, and recorded the property-based graph verification choice per the formal verification design). Base: `main` `5c2e143e` (0.23.0).
Issue: [#1326](https://github.com/arcships/rutis/issues/1326). Implemented by: [#1326](https://github.com/arcships/rutis/issues/1326) (split in §6).
Based on: [design philosophy](design-philosophy.en.md) §7, §8 (dependency cycle diagnostics); [design document conventions](design-docs.en.md); [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5, §4.6, §5.1; [error messages](design-error-messages-2026-11-03.en.md) §3.3, §3.5, §4.4 (#663 concept budget); [formal verification redesign](design-formal-verification-2026-11-08.en.md) §2.3, §3.5.

Out of scope:

- Kernel dependency resolution and scheduling: the `rutis` kernel does not promise cycle detection ([design philosophy](design-philosophy.en.md) §8); this design stays entirely inside `rutis-loader`'s diagnostics snapshot.
- Synchronous reentrant call cycles (`SyncWaitCycle`): those are execution-time deadlocks reported by the bridge and runtimes ([diagnostics design](design-diagnostics-2026-10-26.en.md) §4.3), outside startup `inject` wait diagnostics.
- Static cycle detection in `rutis-host check`: `check` already detects arbitrary-length declared cycles (`cycles()` in `crates/rutis-loader/src/loader/check.rs`); this design brings runtime `Loader::waits` / `rutis-host status` / `run` / `dev` in line with it and adds dynamic unknown-provider chain reporting.
- Session protocol, plugin API, and `rutis.json` format: no new fields and no version bumps.

## 1. Where things stand / what to solve

In a running host, wait reasons come from `Loader::waits` / `Loader::waits_of` (`crates/rutis-loader/src/loader/waits.rs`) and are formatted by `describe_waits` (`crates/rutis-loader/src/loader/status.rs`) for `rutis-host run`, `dev`, `status`, and embedding hosts.

Today `provider()` checks only **1 hop** (`waits.rs:366`: `services.iter().find(|service| own.contains(service))`), leaving two blind spots:

1. **Three-or-more-row dynamic cycles report generic waiting**: when `a -> bsvc -> b -> csvc -> c -> asvc -> a` forms a 3-row cycle (for example after hot reload, dynamic `rutis/loader` row edits, or inside an instance without running `check` first), `a` only prints:
   ```text
   a: waiting for bsvc (row b waits too, for csvc)
   ```
   Developers cannot tell that `a`, `b`, and `c` form a closed deadlock or which rows' `inject` to edit without manually stitching every row in `rutis-host status` together.
2. **Chains stalled on undeclared / unknown providers give no clue**: when a row (or a chain of waiting rows `c -> a -> b`) stalls on a service `storage` that no row declares, while another unstarted row `u` in the same scope may announce services only once it runs (`announces: true`, or declarations not yet known), today the rows print only:
   ```text
   b: waiting for storage (nothing provides it yet)
   c: waiting for asvc (row a waits too, for bsvc)
   ```
   In particular, when `b` (`announces: true`) waits for `asvc` from `a` while `a` waits for `bsvc` (which `b` only calls `ctx.provide` for inside `apply`), the two rows are dynamically deadlocked, yet `a` only says `nothing provides it yet` and `b` only says `row a waits too, for bsvc` — neither points out that unstarted row `b` may provide services only once it runs. Furthermore, the current lookup does not compare per-service `isolate` labels, risking cross-isolate mismatches.

## 2. Decisions

| # | Decision | Why; what is dropped |
| --- | --- | --- |
| D1 | Traverse the **same-scope Pending wait graph** with BFS inside `Loader::waits_of`, matching both the instance copy `scope` and each service's effective `isolate` label; add a graph edge only for a **sole live provider** (unique `Active`/`Loading`/`Pending` declarer in scope, or the binding provider for `ProviderInactive`), iterating missing services sorted by name for determinism. | Matches `Loader::check`'s single-provider rule so alternative starting/running providers or isolated scopes never trigger false cycles; a `visited` set bounds traversal strictly to $O(\|V\| + \|E\|)$ with zero risk of infinite loops. Dropped: cycle detection inside the `rutis` kernel — the kernel does not know declared `provides`, and Design Principle §7 #2 keeps non-core work outside the kernel. |
| D2 | Keep 2-row cycles ($N = 2$) returning the exact existing `WaitReason::Cycle { row, service }` and exact existing wording (`row b waits for asvc too: they inject each other`); for $N \ge 3$, add a `#[non_exhaustive]`-compatible variant `WaitReason::CycleChain { links: Vec<CycleLink> }` reusing `CycleLink` (adding `CycleLink::new`) and formatted with the same vocabulary as `rutis-host check` (who waits for whom, for which service, how to break it). | Preserves `loader/wait-words`, `host/row-state-lines`, and all existing tests and callers matching `WaitReason::Cycle`; `CycleLink` is already exported in `rutis_loader::report`. Dropped: folding $N = 2$ into `CycleChain` (would break existing guarantees and pattern matches) and introducing a duplicate link struct. |
| D3 | A **lead-in row** outside a cycle that waits on a cycle member (`d -> a -> b -> c -> a`, or `c -> a <-> b`) stays `WaitReason::ProviderWaiting { row: "a", services: ["bsvc"] }` and is never accused of being in the cycle. | Matches `rutis-host check` (which includes only cycle members in `cycles`); avoids repeating a long cycle chain on 20 downstream rows or misleading developers into editing `d`'s `inject`; preserves `rows_that_inject_each_other_say_so` identically across 2-row and 3+-row downstream cycles. Dropped: adding a `ProviderInCycle` variant on lead-in rows (noisy on fan-out and inconsistent with the existing 2-row lead-in contract). |
| D4 | Add `WaitReason::UnknownProvider { chain: Vec<CycleLink>, missing: String, candidates: Vec<String> }`. It triggers when a direct wait or a Pending wait chain reaches a missing service `T` with no declared provider in scope and no close spelling match (`closest == None`), while at least one unstarted row in the same scope (`Pending`/`Loading`/`Unresolved` with `announces == true` or unavailable declarations) has not run yet. Output gives the bounded wait chain, names the unstarted candidate rows (prioritizing rows that can reach the terminal waiter in the Pending graph, capped at 3 names plus `and N more`), and states what to check/fix. | Keeps a clean line between proved declared cycles and unprovable dynamic stalls: surfaces hidden dynamic deadlocks without ever misreporting an unknown provider as a cycle. Misspelt names (`closest == Some(_)`) still take precedence (`did you mean`, matching #937 D5). Dropped: listing already-`Active` rows with `announces: true` — most language SDKs set `announces: true` by default, so including running rows would pollute ordinary missing-service waits. |
| D5 | Bound every formatted output and guarantee formatting never fails: display at most 8 hops for cycle and unknown-provider chains (`and N more steps` beyond 8), at most 3 candidate rows (`and N more` beyond 3, matching `some_of`), and fall back safely on degenerate empty vectors without panicking. | Satisfies [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.6 (bounded, panic-free formatting) and prevents huge graphs from overflowing a single status line. Dropped: unbounded formatting. |

## 3. Rules

### 3.1 Scope and wait graph

For each snapshot call to `Loader::waits_of`, build the per-scope `facts` table (in tree order) for the queried row's instance `scope: Option<u64>`:

1. **Effective isolate labels**: walk each row's `parent` chain to collect `raw_scope.isolate`, innermost winning (matching `isolated_in` in `check.rs`). Two rows `u` and `v` share scope for service `S` iff both are in `scope` (including root rows) and `isolate_of(u, S) == isolate_of(v, S)`.
2. **Row facts**: alongside `status`, `plugin`, `provides`, and `unknown_off`, record:
   - `announces: bool`: `Declared::announces`;
   - `unknown_decl: bool`: the row is wanted (`wanted == true`) but has no `Ok(Declared)` (unresolved, `declares == None`, or declaration callback errored).
3. **Sole live provider edges**: for a `Pending` row `u` **not waiting on its own runtime**, and each of its missing non-`OwnRuntime` dependencies `(key, status)` (named `S`, iterated sorted by service name):
   - `status == DependencyStatus::ProviderInactive(_)`: resolve the binding's provider row `v` through `diagnostics.bindings` and `row_of`; if `v != u` and `v` is in `facts`, emit edge `u --S--> v`.
   - `status == DependencyStatus::Missing` and `unexported(&root, S) == None`: collect rows `v != u` in `facts` with `v.provides.contains(S)` and the same isolate label as `u` for `S`. Emit edge `u --S--> v` **only when exactly one** of them is live (`Active`, `Loading`, or `Pending`). Multiple live declarers produce no cycle/chain edge so an alternative starting/running provider never triggers a false cycle.

### 3.2 Arbitrary-length cycle detection (`N >= 2`) and lead-in rows

When `start` waits on `first_service`, whose same-scope provider `first_provider` (`!= start`) is `Pending` and not waiting on its own runtime, run cycle BFS first:

1. Seed the queue with `first_provider`, parent link `first_provider <- (start, first_service)`, and `visited = {first_provider}` (`start` is intentionally excluded from `visited` so returning to `start` closes the cycle).
2. Pop each `Pending` row `curr` (skip outgoing edges if `curr` waits on its own runtime) and iterate its sole-live-provider edges `curr --svc--> next` sorted by `svc`:
   - If `next == start`: reconstruct the shortest simple cycle `links: Vec<CycleLink>` from `start` through `first_provider` … `curr` back to `start`:
     - `links.len() == 2` → `WaitReason::Cycle { row: first_provider, service: links[1].service }`;
     - `links.len() >= 3` → `WaitReason::CycleChain { links }`.
   - Else if `next` is `Pending` and not in `visited`: insert `next` into `visited`, record `next <- (curr, svc)`, and enqueue `next`.
3. If the queue empties without reaching `start` (including when `first_provider` belongs to a downstream cycle not containing `start`, e.g. `d -> a -> b -> c -> a`), cycle detection returns `None` and proceeds to §3.3; if §3.3 also finds nothing, it falls back to `WaitReason::ProviderWaiting { row: first_provider, services }`.

### 3.3 Unknown / unannounced provider chain diagnostics

For a missing service `T` waited on directly by row `W` (`DependencyStatus::Missing`, `unexported == None`, no row in `W`'s scope declares `T`, and `closest_for(W, T) == None` excluding services provided solely by `W` itself so a row is never suggested its own service), define the **unstarted candidate set** `candidates(W, T)`:

1. **Eligibility**: every row `R` in `facts` such that:
   - `R != W`;
   - `isolate_of(W, T) == isolate_of(R, T)`;
   - `R.announces || R.unknown_decl`;
   - `R.status` is `Running(Pending)`, `Running(Loading)`, or `Unresolved(_)`.
   `Active`, `Failed`, `Disabled`, `Inactive`, and `Stopped` rows are excluded.
2. **Ordering** (deduplicated):
   - Tier 1: candidates that can reach `W` along sole-live-provider `Pending` edges (potential dynamic-cycle participants, such as `b` in `b -> asvc -> a -> bsvc`), ordered by shortest distance to `W`, ties broken by tree order;
   - Tier 2: remaining eligible candidates in tree order.
3. **Trigger paths**:
   - **Direct wait**: `start` waits on `S` with no declared provider in scope, `closest(S, &known) == None`, and `!candidates(start, S).is_empty()` → `WaitReason::UnknownProvider { chain: vec![], missing: S, candidates }`. If `candidates` is empty, keep existing rules (`NoEnabledProvider` when another row has `unknown_off`, otherwise `NoProvider`). Whenever `closest(S, &known)` is `Some(_)`, always keep existing typo suggestions (`NoEnabledProvider` / `NoProvider`).
   - **Chained wait**: after §3.2 finds no cycle back to `start`, run BFS from `first_provider` (never revisiting `start` or any visited row). When a popped `Pending` row `curr` (not waiting on its own runtime) has a missing service `T` with no declared provider in `curr`'s scope, `unexported == None`, `closest(T, &known) == None`, and `!candidates(curr, T).is_empty()`, reconstruct `chain: Vec<CycleLink>` (`start -> first_provider -> ... -> curr`) and return `WaitReason::UnknownProvider { chain, missing: T, candidates }`. Otherwise continue along sole-live-provider `Pending` edges.
   - **Zero false positives**:
     - Ordinary startup waits (a declared provider is `Loading`): hits `ProviderStarting` / `ProviderWaiting`, never `CycleChain` or `UnknownProvider`;
     - Ordinary missing service (no other unstarted `announces`/`unknown_decl` row, or `announces: true` rows are already `Active`): `candidates` is empty → `NoProvider` / `ProviderWaiting`;
     - Misspelt service names (`closest != None`): keeps `did you mean`, never `UnknownProvider`.

### 3.4 Formatting and bounds

`why()` in `crates/rutis-loader/src/loader/status.rs` formats each variant with the exact templates below, using only developer-written row names, service names, `inject`, and `provides`, with zero internal words ([diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5):

- `WaitReason::Cycle { row, service }` (unchanged):
  `row {row} waits for {service} too: they inject each other`
- `WaitReason::CycleChain { links }` (`MAX_LINKS = 8`):
  - `links.is_empty()`: `they inject each other; break the cycle in their inject`
  - `1..=8` links:
    `rows {r0}, {r1}, ... wait for each other: {r0} waits for {s1} from {r1}, ..., {rn-1} waits for {s0} from {r0}; break the cycle in their inject`
  - `> 8` links (`rest = links.len() - 8`):
    `rows {r0}, ..., {r7} and {rest} more wait for each other: {r0} waits for {s1} from {r1}, ..., {r7} waits for {s8} from {r8}, and {rest} more steps back to {r0}; break the cycle in their inject`
- `WaitReason::UnknownProvider { chain, missing, candidates }` (`MAX_LINKS = 8`, `MAX_CANDIDATES = 3`):
  - Candidate suffix `tail`:
    - `candidates.is_empty()`: empty string
    - `[one]`: `; unstarted row {one} may provide services once it runs; declare provides or check why {one} has not started`
    - `2..=3` rows: `; unstarted rows {list} may provide services once they run; declare provides or check why they have not started`
    - `> 3` rows (`rest = candidates.len() - 3`): `; unstarted rows {c0}, {c1}, {c2} and {rest} more may provide services once they run; declare provides or check why they have not started`
  - `chain.is_empty()` (direct wait):
    `nothing declares it yet{tail}`
  - `1..=8` hops:
    `{r0} waits for {s1} from {r1}, ..., {last.provider} waits for {missing}; nothing declares {missing} yet{tail}`
  - `> 8` hops (first 7 hops + `and {rest} more steps` + terminal `{last.provider} waits for {missing}`, where `rest = chain.len() - 7`):
    `{r0} waits for {s1} from {r1}, ..., {r6} waits for {s7} from {r7}, and {rest} more steps, {last.provider} waits for {missing}; nothing declares {missing} yet{tail}`

Cross-language behaviour: all analysis and formatting happen inside `rutis-loader` in Rust, so Node, Bun, Python, Go, Swift, and Kotlin plugins see identical output in `rutis-host status`, `run`, `dev`, and `rutis/loader`'s `status()` JSON.

### 3.5 Semantic boundaries: proved cycles, unknown providers, and non-atomic snapshot transients

This design maintains a strict three-way semantic boundary:

1. **Proved declared cycles (`Cycle` / `CycleChain`)**: emitted only when sole-live-provider edges in the same scope form a simple closed loop returning to the queried row itself. Every edge rests on declared `provides` and observed `Pending` waits in the snapshot, with zero guessing.
2. **Unknown / unannounced providers (`UnknownProvider`)**: states only that no row in scope currently declares the missing service while unstarted rows exist that may provide services once they run. It is a **diagnostic clue**, not a deadlock proof — a candidate row may never provide that service once started, or it may genuinely form a dynamic deadlock with a row in the chain — so its wording never claims a cycle and never overrides typo suggestions (`did you mean`).
3. **Non-atomic snapshot transients during fast startup or reload**: like `Loader::status()` ([diagnostics design](design-diagnostics-2026-10-26.en.md) §5.1), `Loader::waits_of` reads row states and declarations under a short loader lock and then calls `root.diagnostics()` outside the lock without introducing a global stop-the-world barrier. Within a millisecond window during rapid startup, hot reload, or right as a row completes `apply`, a single snapshot may transiently observe row `a` still `Pending` while row `b` has not yet transitioned to `Active`:
   - A declared provider in `Loading` falls into `ProviderStarting` / `ProviderWaiting` and never produces a false cycle;
   - An unannounced (`announces: true`) provider transiently observed in `Loading` as an `UnknownProvider` candidate automatically resolves on the next look (`follow` poll or subsequent `status`) once it enters `Active`, becoming either `running` (if it provided the service) or `nothing provides it yet` (if it did not).

In addition, the following cases **cannot** be proved to be cycles statically or from a `Loader::waits` snapshot, and rutis explicitly refuses to guess a cycle for them:

1. **Conditional or delayed `ctx.provide` inside an `Active` row**: a row whose `apply` already finished (`Active`) may only call `ctx.provide("x", ...)` later on a timer, request, or event. A snapshot cannot know whether it will ever do so; waiting rows see `nothing provides it yet`.
2. **Unannounced providers (`announces: true` without `provides`)**: before an unstarted row runs `apply`, rutis does not know which service names it will pass to `ctx.provide(...)`. Thus `a --bsvc--> b (undeclared) --asvc--> a` is reported as `UnknownProvider` ("may provide services once it runs"), never as `Cycle` or `CycleChain`. Declaring `provides` turns it into a proved cycle.
3. **Child plugins mounted dynamically inside `apply` (`ctx.plugin(...)`)**: dependencies or services of dynamically mounted children are invisible before the parent runs `apply`; once mounted, their fibers appear in `diagnostics()` and any inactive bindings participate through `ProviderInactive`.
4. **Private dependencies inside a foreign runtime**: Cordis dependencies resolved privately inside one Node runtime (not shared through `ServiceCatalog`) do not gate the row's rutis fiber and are not visible to `Loader::waits`.
5. **Undeclared remote / peer services**: services on a disconnected peer or announced across a node link without local declarations cannot be linked to a remote Pending graph from the local snapshot alone (locally they appear as waiting on the connection row or `nothing provides it yet`).
6. **Execution-time synchronous call cycles**: synchronous calls between already-running rows that call back into each other are reported by the bridge/runtime as `SyncWaitCycle`, not as startup `inject` cycles.

### 3.6 Verification choice: property-based graph testing rather than a protocol TLA+ model

Following [formal verification redesign](design-formal-verification-2026-11-08.en.md) §2.3 (pick the method that actually answers the question) and §3.5 (Layer D: property-based testing over large input spaces), and confirmed by `node tools/check-model-pr.mjs` (none of the files changed by this design are covered by any concurrent protocol model in `specs/`):

- `#1326` is a pure read-only, side-effect-free snapshot graph traversal that adds **no concurrent protocol state transitions, message interleavings, lock orders, or shutdown races**; writing a protocol TLA+ model for it would answer the wrong question.
- Its real risks lie in termination, zero false positives, soundness/completeness, and bounded output across arbitrary directed graph topologies — an input-space property. We therefore verify it with **deterministic randomized / property-based graph tests** (`proptest` in `crates/rutis-loader/src/loader/waits_prop_tests.rs`, covering thousands of generated graphs across 2..=12-node cycles, lead-in chains, multiple `isolate` scopes, multiple live providers, `announces: true` unknown rows, disabled/failed/loading/unresolved rows, and random DAGs) checked against an independent reachability oracle:
  1. Every graph terminates cleanly with no panics in traversal or formatting, bounded output, and zero §4.5 forbidden internal words;
  2. Every DAG produces zero `Cycle` / `CycleChain` false positives;
  3. Every 2-node sole-provider cycle produces `WaitReason::Cycle` (backward compatible with `loader/wait-words`);
  4. Every $N \ge 3$ sole-provider cycle produces `WaitReason::CycleChain` with a valid closed loop starting at the queried row;
  5. Every lead-in row outside a cycle stays `WaitReason::ProviderWaiting`;
  6. Every unknown-provider chain never claims a cycle and never overrides typo suggestions (`did you mean`).

## 4. What developers see

### 4.1 People assembling plugins with `rutis-host`

Normal startup and non-cycle waits keep the exact same configuration and output:

```json
{
  "rows": [
    { "id": "c", "name": "./c.mjs" },
    { "id": "b", "name": "./b.mjs" },
    { "id": "a", "name": "./a.mjs" }
  ]
}
```

While `c` is starting (`Loading`), `b` waits for `csvc`, and `a` waits for `bsvc`, `rutis-host status` is unchanged and never raises a false cycle:

```text
rows:
  a: waiting for bsvc (row b waits too, for csvc)
  b: waiting for csvc (row c is starting)
  c: starting
```

**When stalled / failing** (verbatim across `rutis-host status`, `run`, and `dev`; wait states have no error code):

| # | How it is caused | Output (word for word) | Code | Recorded on | How to fix |
| --- | --- | --- | --- | --- | --- |
| 1 | 2 rows inject each other (unchanged) | `a: waiting for bsvc (row b waits for asvc too: they inject each other)` | none (state) | both rows (`a`, `b`) | break `inject` in `a` or `b` |
| 2 | 3-row cycle (`a -> bsvc -> b -> csvc -> c -> asvc -> a`) | `a: waiting for bsvc (rows a, b, c wait for each other: a waits for bsvc from b, b waits for csvc from c, c waits for asvc from a; break the cycle in their inject)` | none (state) | every row in the cycle (`a`, `b`, `c`, each starting from itself) | break `inject` in one of `a`, `b`, `c` |
| 3 | Long cycle (e.g. 10 rows `r0`..`r9`, over the 8-hop cap) | `r0: waiting for s1 (rows r0, r1, r2, r3, r4, r5, r6, r7 and 2 more wait for each other: r0 waits for s1 from r1, r1 waits for s2 from r2, r2 waits for s3 from r3, r3 waits for s4 from r4, r4 waits for s5 from r5, r5 waits for s6 from r6, r6 waits for s7 from r7, r7 waits for s8 from r8, and 2 more steps back to r0; break the cycle in their inject)` | none (state) | every row in the cycle | break `inject` in one row of the cycle |
| 4 | Lead-in row waiting on a cycle member (`d -> asvc -> a`, while `a -> b -> c -> a` cycles) | `d: waiting for asvc (row a waits too, for bsvc)` | none (state) | lead-in row `d` (cycle details are printed on `a`, `b`, `c`) | inspect `a`'s line and break the cycle among `a`, `b`, `c`; leave `d` alone |
| 5 | Direct wait on undeclared `bsvc` while unstarted row `b` (`announces: true`) waits for `asvc` | `a: waiting for bsvc (nothing declares it yet; unstarted row b may provide services once it runs; declare provides or check why b has not started)` | none (state) | `a` | declare `provides` on `b`; check why `b` has not started (e.g. whether it waits on `a`) |
| 6 | Multi-hop chain hitting undeclared service (`c -> asvc -> a -> bsvc -> b -> storage`, unstarted row `u` has `announces: true`) | `c: waiting for asvc (c waits for asvc from a, a waits for bsvc from b, b waits for storage; nothing declares storage yet; unstarted row u may provide services once it runs; declare provides or check why u has not started)` | none (state) | `c` (`a` and `b` each show the chain starting from themselves) | declare `provides` on `u`, or check why `u` has not started / add a provider for `storage` |
| 7 | Many unstarted unknown providers (e.g. 5 rows `b`..`f`) | `a: waiting for bsvc (nothing declares it yet; unstarted rows b, c, d and 2 more may provide services once they run; declare provides or check why they have not started)` | none (state) | `a` | declare `provides` on the provider row, or check why those rows have not started |
| 8 | Misspelt service name (even with another unstarted `announces: true` row) | `weather: waiting for lmm (nothing provides it yet; did you mean llm?)` | none (state) | `weather` | change `lmm` to `llm` |
| 9 | Ordinary missing service (all `announces: true` rows are already `running`, or all rows only use declared services) | `store: waiting for blob (nothing provides it yet)` | none (state) | `store` | add a row that provides `blob` |
| 10 | Cross-`isolate` same-name service (`a` isolates `bsvc: true`, `b` provides `bsvc` outside and injects `asvc`) | `a: waiting for bsvc (nothing provides it yet)`; `b: waiting for asvc (row a waits too, for bsvc)` | none (state) | `a`, `b` | share an `isolate` label or provide `bsvc` inside `a`'s isolate |

### 4.2 Plugin authors

Normal plugin code does not change. Declaring `provides` on plugins that provide services remains recommended (TypeScript shown; Python, Go, Swift, Bun, and Kotlin behave identically):

```ts
import { definePlugin } from '@arcships/rutis'

export default definePlugin({
  inject: ['asvc'],
  provides: { bsvc: { get: 'sync' } },
  apply(ctx) {
    ctx.provide('bsvc', { get: () => 'ok' })
  },
})
```

**When stalled / failing**:

- If a plugin omits `provides`, calls `ctx.provide('bsvc', ...)` only inside `apply`, and ends up mutually waiting with `a` (which provides `asvc` and injects `bsvc`), `rutis-host dev` prints clear clues on both rows:
  ```text
  a: waiting for bsvc (nothing declares it yet; unstarted row b may provide services once it runs; declare provides or check why b has not started)
  b: waiting for asvc (b waits for asvc from a, a waits for bsvc; nothing declares bsvc yet; unstarted row b may provide services once it runs; declare provides or check why b has not started)
  ```
- Adding `provides: { bsvc: ... }` to `b` immediately turns the reload into a proved cycle diagnostic:
  ```text
  a: waiting for bsvc (row b waits for asvc too: they inject each other)
  b: waiting for asvc (row a waits for bsvc too: they inject each other)
  ```
- For 3-or-more plugin cycles (`a`, `b`, `c`), `dev` prints the exact 3-row cycle line from §4.1 row 2 in every language with no language differences.
- The `rutis/loader` `status()` JSON shape is unchanged; each row's `reason` string carries the exact line above.

### 4.3 Host owners (applications embedding rutis)

Reading waits and snapshots works as before:

```rust
let status = loader.status();
eprintln!("{status}");

for entry in loader.entries() {
    for wait in loader.waits(&entry) {
        match &wait.reason {
            WaitReason::Cycle { row, service } => { /* 2-row cycle (unchanged) */ }
            WaitReason::CycleChain { links } => { /* 3+-row cycle, Vec<CycleLink> */ }
            WaitReason::UnknownProvider { chain, missing, candidates } => { /* unknown/unannounced provider chain */ }
            _ => {}
        }
    }
}
```

- `WaitReason` is already `#[non_exhaustive]`, so adding `CycleChain` and `UnknownProvider` is source-compatible with existing `match` arms.
- `CycleLink::new(row, service, provider)` is added so embedding hosts and tests can construct `CycleLink` values without internal access.
- `rutis_loader::status::describe_waits(&waits, &runtime)` produces the exact verbatim lines in §4.1; no new kernel error codes are introduced.

### 4.4 Concepts to learn

**New concepts added: 0.**

- Plugin authors (#663 budget: 7): rows, services, `inject`, `provides`, and `dev` are already known.
- `rutis-host` users (#663 budget: 6): rows, services, `inject`, `provides`, `status`, and `check` are already known; 3+-row cycles reuse `check`'s exact vocabulary.
- Host owners (#663 budget: 10): two new variants on the existing `WaitReason` enum reusing the existing `CycleLink` type, adding no new abstraction or concept.

## 5. Compatibility and migration

- **Rust API compatibility**: `WaitReason` is already `#[non_exhaustive]`; adding `CycleChain { links }` and `UnknownProvider { chain, missing, candidates }` and `CycleLink::new` is purely additive. `WaitReason::Cycle { row, service }` and its semantics are 100% unchanged.
- **Output compatibility**:
  - 2-row cycles keep the exact existing text (`row b waits for asvc too: they inject each other`);
  - Lead-in rows keep the exact existing text (`row a waits too, for bsvc`);
  - Misspelt names, ordinary missing services (when no other unstarted `announces`/unknown-declaration row exists), unresolved disabled rows, and starting providers keep the exact existing text;
  - Only previously under-diagnosed cases change: 3+-row dynamic cycles upgrade from `row b waits too, for csvc` to the full `CycleChain` sentence, chains hitting unstarted `announces`/unknown-declaration rows upgrade to `UnknownProvider`, and cross-`isolate` waits no longer misattribute providers across isolated scopes.
- **Protocol / configuration / plugin compatibility**: session protocol (4), `rutis.json` format (1), and plugin API (3) are unchanged; old runtimes with a new host and new runtimes with an old host need no changes.
- **Release notes & guides**: when finalized, update the "What `run` prints" wait-reason table in [rutis-host guide](guide/rutis-host.en.md) (and [Chinese](guide/rutis-host.md)) and the target release's migration guide.

## 6. Implementation

| # | Scope (with paths) | Done when |
| --- | --- | --- |
| D-1 Wait graph & new variants | Add `CycleLink::new` in `crates/rutis-loader/src/loader/check.rs`; add `WaitReason::CycleChain` and `WaitReason::UnknownProvider` in `crates/rutis-loader/src/loader/waits.rs`, extend `Fact` (`announces`, `unknown_decl`, `isolates`), and implement scope-aware BFS cycle detection (preserving 2-row `Cycle` and lead-in `ProviderWaiting`) and unknown-provider chain detection; add property-based graph tests in `crates/rutis-loader/src/loader/waits_prop_tests.rs` (§3.6) | `cargo test -p rutis-loader --all-features` and `--no-default-features --lib` pass; tests cover 2-row cycle, 3-row cycle, lead-in into cycle, direct and chained unknown providers, normal startup wait, ordinary missing service, cross-`isolate` non-cycle, and thousands of randomized property-based graph cases checked against an independent oracle |
| D-2 Formatting & verbatim acceptance tests | Implement bounded, panic-free formatting for `CycleChain` and `UnknownProvider` in `crates/rutis-loader/src/loader/status.rs` (8-hop / 3-candidate caps); add verbatim acceptance tests in `crates/rutis-loader/src/loader/status.rs` and `crates/rutis-loader/tests/waits.rs` matching §4 word for word | `cargo test -p rutis-loader -p rutis-host` passes, verbatim assertions match §4, and no forbidden words from [diagnostics design](design-diagnostics-2026-10-26.en.md) §4.5 appear |
| D-3 Guides & guarantees register (follow-up PR) | Update the wait-reason table in `docs/guide/rutis-host.md` / `.en.md`; register long-cycle and unknown-provider diagnostics in the guarantees register and formal trace if promoted to contract guarantees | Link, guarantee, and formal-trace checks pass |
