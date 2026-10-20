# Quality control, step four (0.16): closing out and the release gate (plan)

[中文](design-quality-0.16-2026-10-20.md)

Status: draft, for review. Date: 2026-10-20. Base: `main` `4e23339` (0.15 in development; 0.13 released, 0.14 skipped).
Related: step four of [#183](https://github.com/arcships/rutis/issues/183). Based on: the [quality standard](quality-standard.en.md) (the "standard"), the [quality status](quality-status.en.md) (the "status") §1 and §6, and the [guarantee register](guarantees.en.md).

Scope assumption: 0.15 is Swift / Kotlin and app hosting; 0.16 is the quality release after 0.15, closing out existing promises and existing controls without adding features. There was no 0.16 schedule in the repository before this document; the schedule is this document's proposal, and the controls can be reviewed independently.

## 1. Conclusions

| # | Conclusion | Section |
| --- | --- | --- |
| C1 | 0.16 covers the "what remains" column of status §6 and the P0 / P1 rows of §1 still marked "partly" or "not met"; the long runs, fuzzing, shuttle, TLA+, netem, cross-version and unified events finished in 0.12 and 0.13 are not reopened | 2 |
| C2 | Start by closing the promise loop: complete the guarantee register for loader / host / the new SDKs, add an automated check, and close each of the four "partly verified" core promises or lock it as an explicit non-guarantee with a test | 3 |
| C3 | Reference and call limits are the only protocol-level new design in 0.16: hello carries limits, defaulting to 4096 live references and 1024 in-flight calls per session; exceeding a limit fails only that call and the session continues | 4.1 |
| C4 | Residue checks, background exceptions, test-tool comparison, install smoke and the Windows column extend to every declared language and channel; Swift / Kotlin no longer get a "new SDK" exception | 4, 5 |
| C5 | The release gate gains three hard checks: the register check, install smoke for the six published channels (plus the release binary), and cross-version (0.15 ↔ 0.16); actionlint, licences and semver move from "warning" to "check" in CI | 6 |
| C6 | The exit condition is a checklist that can be ticked item by item (§7); anything unfinished cannot enter the 0.16 release notes as a "known issue" | 7 |

## 2. Current state and scope

Only the gaps this plan touches are listed. Each row was checked on `main`; the completed controls (FZ, SH, TLA+, S8, S5 in containers, cross-version, unified events) are not reopened.

| Gap | Current state | What 0.16 does |
| --- | --- | --- |
| Four core promises are still "partly verified" | `core/D22`, `core/D28`, `boundary/5.8`, `loader/instance-edits-all-or-none` | Add independent verification; where impossible, downgrade to an explicit non-guarantee with a regression test |
| The register is not closed | 194 rows, 549 `guarantee:` references; `loader/` and `host/` still have unregistered items; Swift / Kotlin have no `sdk/*` rows | Complete them and add `tools/check-guarantees.mjs`, run on PRs and at the release gate |
| No limits on references or calls | Only frame size and the 64-level nesting limit (second half of #178) | Design and implement per-session limits (§4.1) |
| Residue checks look only at processes and files | E2E checks processes, sockets, ports, temporary directories and credentials; fds / threads only in the soak tests; tokio tasks only in two tests; runtime `inspect` only in Node / Python and only used by S7 | A shared baseline: fds, threads, tasks and registries for the host and each runtime |
| Background exceptions are uneven | Python has it; Rust and Node / Bun have no unified "a background exception fails the test" | One mechanism in Rust test support, one in the Node / Bun test frameworks |
| Test-tool comparison is uneven | Node, Python, Go, Swift and Kotlin compare against the weather expectations; Bun has only a fixture, not the whole file; there is no single matrix | One expectations file, six implementations, one matrix, every difference written down |
| Install smoke misses channels | npm, PyPI and release binaries are covered; crates.io, the Go module, SwiftPM and Kotlin / Maven are not | Every published channel installs, runs and uninstalls in a clean environment |
| Windows columns are incomplete | Value round-trip is Unix-only; Ctrl-C and console close have no E2E | Value round-trip over loopback; both signals |
| Output and credentials are not checked uniformly | Common host errors are locked word for word; SDK / bridge library errors are not; credentials are checked only in E2E output | Snapshots for errors and status output; a credential check for library logs |
| CI and the release gate have warning-only items | No actionlint or licence check; semver only warns; the 10-minute target for ordinary PRs is not met (median 15.6 minutes on 2026-10-12) | Warnings become checks; re-measure and publish an achievable time budget |
| Scale and performance are not declared | Q5.7.1 and Q5.7.2 are still "partly" / "not met" | Publish the scale envelope; mark performance numbers as "promise" or "reference" |
| The dylib preload list and multi-writer tests are not reconciled | Q5.3.6's five items are unchecked; Q5.5.2's multi-writer tests are not checked one by one | Reconcile each item and add the missing tests |

Out of scope: S1 (#185, which needs the product design for "plugin services as agent tools"), coverage and mutation thresholds, performance regression thresholds, Miri / ASan / TSan, disk-full / fd-limit / OOM injection, more platforms, and automatic extraction of guarantees from design documents. The reasons follow the standard's appendix: no corresponding P0 / P1 risk, or the maintenance cost exceeds the benefit.

## 3. Closing the promise loop

### 3.1 Complete the register and check it automatically

Add three groups of rows:

- the remaining `loader/` and `host/` rows, one by one from status §5 and the design documents;
- `sdk/swift-*` and `sdk/kotlin-*` for the new SDKs, covering at least the session, values, events and the testing tool;
- `bridge/resource-limit` and the six-implementation version of `sdk/testing-matches-runtime`.

Add `tools/check-guarantees.mjs`, run in the `checks` job and at the release gate:

1. every `guarantee: <id>` in the source must match a row, and no id may appear twice;
2. every row marked "verified" must have at least one test reference, and the reference must resolve to a file and a test name in the repository;
3. every row marked "partly verified" or "not verified" must carry an issue number or a plan section;
4. every core-promise row must fill the "second verification" column (see 3.2).

The script starts from the existing 194 rows as a baseline: new rows and references must pass; existing references that do not resolve go into `tools/guarantees-baseline.txt`, which must be empty before 0.16 ships. This avoids blocking every PR on the historical backlog.

### 3.2 Two independent verifications for core promises

Standard Q3.1 requires at least two independent verifications for a core promise. The register has only one "verification" column, so compliance is invisible. Add a "second verification" column, filled only for core promises, with one of:

- a black-box E2E scenario (`tests/e2e`);
- a model (`specs/` TLA+ or shuttle);
- an independent test in another implementation (for example the same session contract run in Node, Python, Go, Bun, Swift and Kotlin).

Reconcile all 41 core promises before 0.16. The four "partly verified" rows:

| Row | Gap | 0.16 treatment |
| --- | --- | --- |
| `core/D22` | Pending until dependencies are ready is verified; **cycles are not detected** and no test locks that | Add a regression test: mutually waiting rows stay Pending, neither start nor error; register it as an explicit non-guarantee |
| `core/D28` | An isolated `Ctx` keeps its original fiber's ownership; ownership has no independent test | Add a contract test: isolated cleanup runs exactly once when the parent unloads; add a black-box or loader-side verification |
| `boundary/5.8` | An uncaught error ending the process is verified; "no timeout is added" is not asserted | Add a test: a call does not fail because of a host or compatibility-layer timeout; only the caller's own cancellation takes effect |
| `loader/instance-edits-all-or-none` | The all-or-none behaviour exists, but the partial rollback path has no independent verification | Add two tests, a dry-run failure and a partial failure, asserting the old generation keeps running and the editable layer is not written |

### 3.3 Lock the explicit non-guarantees

Standard Q3.4: behaviour that could be mistaken for a guarantee needs a test locking "it is not provided". For 0.16, reconcile all six explicit non-guarantees: keep the ones with tests, add one minimal regression test to the ones without, or write in the register why no test is possible. `core/D22` and the value range limits are mandatory; the rest follow the same rule.

### 3.4 Level and risk in design documents

A new design document must state "level / risk / acceptance" at the top, using the values from standard §3 and §4. Historical documents are not backfilled; the PR template gains an item, and `checks` only inspects newly added `docs/design-*.md` files for those three fields. The rule starts at 0.16 without creating a one-off documentation debt.

## 4. Resource integrity

### 4.1 Reference and call limits (second half of #178)

**Problem.** Only frame size and value nesting are limited today. A plugin can grant references or start calls without bound, and the host's and the peer's memory, task tables and reference tables grow forever; no test has a boundary.

**Protocol.** hello gains an optional field:

```json
"limits": { "refs": 4096, "calls": 1024 }
```

Each side advertises the limit it is willing to carry. A missing field means "no limit", for peers from 0.15 and earlier; a 0.16 side still enforces its own limit and says in the error that the peer did not advertise one.

**Counting.** Per session, per direction:

- `refs`: object or function references this side has granted and that are not yet released. Reference lifetime follows the current rule (until the call ends or an explicit release); granting the same object again takes another slot.
- `calls`: calls this side has started and that have not finished, including incoming calls and outgoing calls waiting for a result. An incoming call is checked before it executes; over the limit it does not execute.

**Defaults.** 4096 live references and 1024 in-flight calls per session. The host overrides with `--max-refs` / `--max-calls` or `RUTIS_MAX_REFS` / `RUTIS_MAX_CALLS`; the SDKs' `RuntimeOptions` / `Serve` take the same options. The implementation PR re-checks the defaults against the existing soak and event-matrix data; they may only be raised, never lowered in a patch release.

**Over the limit.** The receiver's count is authoritative; the sender pre-checks against the advertised limit, and the receiver's error wins if the two counts disagree. Return a structured `ResourceLimit` error with `resource` (`"refs"` or `"calls"`), `limit` and `current`:

- a reference that cannot be sent: that call fails before sending, no reference is created, the session continues;
- an incoming call: refused before execution with the error, no executor thread is taken;
- a reference in event arguments or results: only that dispatch fails, following the unified-event rule that an unsendable value affects only that call;
- the session is **not** closed, nothing is silently dropped, nothing is retried.

**Acceptance.** Rust and each of the six language implementations have one test at `limit - 1`, `limit` and `limit + 1` asserting success / success / only this call fails; after the failure the session continues and the reference table and task count return to baseline; a 0.15 peer that advertises no limits behaves as today; the error name and fields agree across all seven implementations.

**Not done.** Byte or memory quotas, per-plugin quotas, admission control and queue lengths are out of 0.16, so one release does not introduce two resource models.

### 4.2 A shared residue check

Move the fd / thread sampling from `soak.rs` into a shared module. Every E2E scenario then also checks:

- the host's fds, threads and live tokio tasks return to the baseline taken at the start of the scenario;
- each runtime's fds, threads and tasks return to baseline (via the runtime `inspect` below);
- the kernel `diagnostics()` fiber, binding, listener and event-backlog counts return to baseline;
- the runtime's rows, proxies and export slots return to baseline.

Extend runtime `inspect` to Go, Bun, Swift and Kotlin with one JSON shape:

```json
{ "rows": 0, "refs": 0, "calls": 0, "listeners": 0, "tasks": 0 }
```

Node and Python already have the control method; they only need the fields. E2E calls it at the start and end of a scenario; when a runtime has exited, only the host side is checked. The tolerances and their reasons live in `tests/e2e/src/residue.rs`: fds +0, threads +0, tasks +0; anything that genuinely stays resident (for example Node's esbuild loader) is named and allowed separately.

### 4.3 Background exceptions

- **Rust**: `rutis-test-support` gains background-panic capture. A panic in a thread or task spawned inside a test scope is recorded and fails the test at the end of the scope; tests that expect a panic mark it explicitly.
- **Node / Bun**: the test framework installs `uncaughtException` and `unhandledRejection` handlers, records them and fails at the end of the test; the runtime's own "uncaught error ends the process" behaviour is unchanged.
- **Swift / Kotlin**: the testing tool records unhandled Task / coroutine exceptions and fails at the end; runtime behaviour is unchanged.

Acceptance: a minimal test that throws in the background in each of the six implementations must fail; after the fix it must pass; ordinary background work is not reported.

### 4.4 Windows signals and residue

Add two Windows E2E cases, Ctrl-C and console close, for `rutis-host run` and `dev`, asserting exit codes, cleanup and the residue check. The existing Ctrl-Break and Job Object checks stay.

### 4.5 Dylib preload and multi-writer reconciliation

- Tick the five items in `design-dylib-sdk` §13 one by one, adding a test or stating why it does not apply;
- Reconcile Q5.5.2's multi-writer tests item by item: `save` is serial and in submission order, persistent conflicts return `Conflict`, and replayed edits that no longer apply leave the queue. Add the missing ones to `rutis-loader/tests/loader.rs`.

## 5. Cross-implementation consistency

### 5.1 Testing tools and real runtimes

One `weather.expected.json` and one `events.expected.json`; the testing tool of each of the six implementations runs both:

| Implementation | Weather comparison | Event comparison | What 0.16 adds |
| --- | --- | --- | --- |
| Node | yes | yes | join the unified matrix |
| Python | yes | yes | join the unified matrix |
| Go | yes | yes | join the unified matrix |
| Bun | fixture only | yes | wire the whole weather file; join the matrix |
| Swift | yes | yes | join the matrix; write down `tool: false` cases |
| Kotlin | yes | yes | join the matrix; write down `tool: false` cases |

Allowed differences live in one place, `crates/rutis-bridge/src/runtime/testing/allowed.json`, each with a reason and a test; no SDK keeps its own private exception list. `rutis-bridge/tests/session_matrix.rs` gains a "testing tool" column: each implementation runs the same file as a real runtime and as a testing tool, and the results must agree (except for `allowed`).

### 5.2 Install smoke for every channel

In a clean directory, for every published channel:

| Channel | Command | Assertion |
| --- | --- | --- |
| npm | `npx @arcships/rutis-host` | `--version`, `new`, `dev` runs the template |
| PyPI | `uvx rutis-host` | the same |
| crates.io | `cargo install rutis-host --version <release>` | the same |
| Go module | temporary Go module requiring `github.com/arcships/rutis/go/rutis@<release>` | `go build` a `rutis.Serve` plugin and run it under `rutis-host` |
| SwiftPM | temporary package `.package(url: "...", from: "<release>")` | build and run `RutisSessionFixture` |
| Kotlin / Maven | temporary Gradle project on `io.github.arcships:rutis:<release>` | compile and run the testing tool |
| release binary | the existing matrix | keep |

A channel that is not published is not pretended to pass: mark it explicitly as "not published" in `install.yml` with the reason; once published, the next release gate must cover it. After release, keep running the documentation examples against the registry packages.

### 5.3 Value round-trip on Windows

Change `value_roundtrip.rs` from `cfg(all(unix, ...))` to both channels: Unix socket and Windows loopback. Bun stays skipped on that platform with a written reason; the other five implementations run the property test on Windows and print the seed on failure. The Go column keeps testing the session layer.

### 5.4 Errors, status and credentials

- Add snapshots for SDK and bridge library errors: name, fields and message template, in the same style as the host's word-for-word locks, without addresses or times.
- Add one snapshot for `run` / `dev` status output covering starting, waiting, failed and stopped rows.
- Add a credential-scanning test for library logs and diagnostics: build a configuration and connection with a token, and assert the token never appears in logs, traces or errors; the existing E2E check stays.

### 5.5 Cross-version for Swift and Kotlin

Add Swift and Kotlin to `tests/e2e/tests/cross_version.rs`:

- a 0.15 Swift / Kotlin runtime with a 0.16 host: works, or the handshake says which end to upgrade;
- a 0.16 runtime with a 0.15 host: the same;
- a different event-protocol version is refused under the unified-event rules, never hanging.

The previous SwiftPM and Maven packages come from the published tag / repository; if they cannot be fetched, the test fails instead of skipping silently.

## 6. CI and the release gate

### 6.1 Static checks

- Add `actionlint` to `checks` for every workflow;
- add licence checks to `deps.yml`: `cargo deny check licenses`, plus licence inventories for npm / PyPI; a new dependency must state its reason in the PR description, and the PR template gains an item;
- write the licence policy into `docs/ci.md`; the allow list is established on the first run and does not default to allow-all.

### 6.2 Semver becomes a check

`semver-rutis` only warns today. From 0.16, a breaking public-API change must either bump the next train's minor version in the same PR or be explicitly approved as an allowed break; otherwise the job fails. The 0.x rule goes into `docs/ci.md` and shares one configuration with the release script's version check.

### 6.3 The 10-minute target for ordinary PRs

Re-measure with one week of data after 0.15 (`tools/ci-stats.mjs`), then decide:

- median ≤ 10 minutes: keep the target and add a regression check (open an issue when p90 exceeds the threshold for a week);
- median > 10 minutes: change the target to "median ≤ 10 minutes, p90 ≤ 20 minutes", write in `docs/ci.md` how queueing relates to the number of runners, and move long jobs such as `multiprocess` and `soak` off PRs if they are still there.

Neither missing the target nor silently changing it is allowed.

### 6.4 Release gate

`release.md`'s gate gains:

1. `tools/check-guarantees.mjs` passes and the baseline file is empty;
2. install smoke is green for the six published channels and the release binary (unpublished channels explicitly marked);
3. 0.15 ↔ 0.16 cross-version passes;
4. persisted samples include 0.15 and 0.16;
5. the scale envelope and performance reference values are updated;
6. the 0.16 release notes list every explicit non-guarantee.

### 6.5 Scale and performance declarations

- The published scale envelope follows the upper bounds in status §2.2: 500 rows, 10 local runtimes and 20 nodes per host; plus 4096 references and 1024 in-flight calls per session, 64 levels of value nesting and 16 MiB frames. Continuous running is verified for S8's one hour without growth; longer runs are not promised.
- Behaviour beyond the envelope is not promised; the new reference and call limits must fail explicitly, and everything else is recorded as an explicit non-guarantee.
- Mark every performance number "promise" or "reference". The ~30 µs synchronous call is a reference value with the machine and date; no CI regression threshold, to avoid noise.
- Longer runtimes and larger scales are left to later releases; 0.16 does not promise them.

## 7. Exit conditions (the 0.16 release gate)

Tick every item before 0.16 ships; anything unfinished cannot enter the release notes as a "known issue".

- [ ] 41 core promises: zero "partly verified"; the four current gaps are closed or downgraded to explicit non-guarantees with regression tests;
- [ ] 117 contract behaviours: zero "not verified"; every "partly verified" row has an issue and an owner, and none is new;
- [ ] all six explicit non-guarantees have a test or a written reason why not;
- [ ] the register covers loader / host / Swift / Kotlin; `check-guarantees` passes on PRs and at the release gate, with an empty baseline;
- [ ] reference and call limits pass boundary tests in Rust and all six language implementations; behaviour against a 0.15 peer is correct;
- [ ] E2E residue checks cover fds, threads, tasks, and kernel and runtime registries on all three platforms;
- [ ] background exceptions fail tests in Rust, Node, Bun, Swift and Kotlin;
- [ ] the six-implementation testing-tool matrix passes, with a reason for every `allowed.json` entry;
- [ ] install smoke passes for every published channel; unpublished channels are explicitly marked;
- [ ] Windows value round-trip, Ctrl-C and console close pass;
- [ ] SDK / bridge error and status snapshots pass; the library-level credential scan passes;
- [ ] actionlint, licence and semver checks are in CI; the 10-minute target is met or has been changed with data and written down;
- [ ] the scale envelope and performance reference values are documented;
- [ ] the full 0.16 release gate passes once.

## 8. Order and PR slicing

| Wave | PR | Content | Depends on |
| --- | --- | --- | --- |
| 0 | 1 | Complete the register + `check-guarantees.mjs` + baseline | — |
| 0 | 2 | Tests for the four core promises and the explicit non-guarantee locks | 1 |
| 0 | 3 | Level / risk fields in design docs and the PR template | — |
| 1 | 4 | Limits: protocol field, Rust side, error and tests | 1 |
| 1 | 5 | Limits: Node, Python, Go, Bun | 4 |
| 1 | 6 | Limits: Swift, Kotlin; cross-version | 4, 5 |
| 1 | 7 | Shared E2E residue + `inspect` in six runtimes | — |
| 1 | 8 | Background exceptions in Rust / Node / Bun; Swift / Kotlin test tools | — |
| 2 | 9 | Testing-tool matrix + `allowed.json` | 8 |
| 2 | 10 | Install smoke: crates.io, Go, SwiftPM, Kotlin | — |
| 2 | 11 | Windows value round-trip, Ctrl-C, console close | 7 |
| 2 | 12 | Swift / Kotlin cross-version | 6, 10 |
| 3 | 13 | actionlint, licences, semver checks | — |
| 3 | 14 | Error / status snapshots, library-level credential scan | — |
| 3 | 15 | Scale envelope and performance reference values | 4 |
| 3 | 16 | Release gate and CI time budget | 1, 10, 12, 13 |

Each PR does one thing; tests and documentation land with the implementation. The PR description states which gap it closes, which guarantee id it maps to, and on which platforms it ran.

## 9. Not in this plan

- S1 (#185): isolation and permissions for agent-written plugins need product design and are not mixed into the quality close-out;
- coverage thresholds, mutation thresholds, Miri / ASan / TSan, performance regression thresholds;
- disk-full, fd-limit and OOM injection;
- more platforms and device matrices (iOS / Android devices, a Linux distribution matrix);
- automatic extraction of guarantees from design documents; the register stays hand-maintained, with only references checked automatically;
- byte, per-plugin or per-tenant resource quotas;
- bulk backfilling of historical design documents.

## 10. Risks and treatment

| Risk | Treatment |
| --- | --- |
| Limit defaults are too low and hit normal plugins | Re-check against the existing event matrix and soak data first; defaults may only rise; options are available |
| Error names or fields differ across the six language implementations | Use the Rust error names; one same-named test in Rust and each of the six implementations; the cross-implementation conformance suite covers them |
| A protocol field in a minor release breaks 0.15 peers | `limits` is optional and absent means unlimited; the 0.15 ↔ 0.16 test is in the release gate |
| `check-guarantees` falsely reports historical rows | Baseline file + strict new rows; empty before 0.16 |
| Install smoke depends on an unpublished channel | Mark it "not published"; the first run after publishing must cover it |
| The 10-minute target is defeated by runner queueing | Decide with one week of data; keep it if met, otherwise change it and write down why, never silently |
| The scope grows into "fix every partly verified row" | Only P0 / P1 and core promises; the rest become follow-up issues and do not block 0.16 |
