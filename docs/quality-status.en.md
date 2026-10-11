# rutis quality status

[中文](quality-status.md) · [Quality standard](quality-standard.en.md)

Status: kept up to date with the code. Last updated: 2026-10-10 (corrected after the #202 review), baseline `main` `a8733d4` (after 0.8.0).
Companion to: [Quality standard](quality-standard.en.md). This document maps the standard, clause by clause, to the current implementation: which clauses are met, which partly, what is missing and how it will be added. The standard describes rules that stay the same; the file names, test names, issue numbers and versions here change with the implementation.

---

## 1. Status by clause

Status: **met**, **partly**, **not met**, **n/a**. The gaps column refers to risk numbers in §4 and to issues.

### 1.1 Cross-cutting requirements

| Clause | Content | Status | Current state and gaps |
| --- | --- | --- | --- |
| Q3.1 | Core guarantees verified at the highest level | Partly | Lifecycle rules are verified thoroughly in the core; "nothing left behind" and "explicit failure on interruption" are not verified as a black box or on Windows (B2, B3, C10, E1) |
| Q3.1, Q8.3 | Core guarantees verified in two independent ways | Partly | Session and runtime conformance is driven from Rust; the Node / Python SDKs' own session tests are incomplete (Python lacks release counting, grant rollback and more, #178) |
| Q3.4 | Behavior explicitly not guaranteed is pinned | Not met | D29 (events not filtered by isolate) and D22 (no dependency cycle detection) have no regression tests (#177) |
| Q4.5 | Risk assessment recorded | Partly | §4 of this document is the first pass |
| Q5.1.2 | Verification checks results | Partly | No ErrorSink assertions in `typed_plugin.rs`; `contract.rs` sorts before asserting D24, hiding delivery order (#177) |
| Q5.2 | Each listed fault verified by injection | Partly | Runtime crashes yes; a link dropping mid-call, slow peers, concurrent starts, the host receiving signals no (#178, #184, #174). The `fault` decorator exists but is only used in the memory channel's own tests |
| Q5.3.2 | Encryption across machines, TLS required off loopback | Partly | Positive cases verified; among negative cases only an untrusted CA is (`websocket.rs` asserts `AuthRejected`), not an expired certificate or a hostname mismatch; no TLS tests on the Python side (#178) |
| Q5.3.3 | Credentials never in output | Partly | Required by design, no common automated check |
| Q5.3.4 | Limits on external input, no crash on malformed input | Partly | WebSocket has a 16 MiB limit with tests; local line framing has no limit (#173); no fuzzing at all (#179) |
| Q5.3.6 | Unsafe code concentrated, checks before loading | Partly | `unsafe` is concentrated in dylib loading and process spawning; dylib checks are verified by scripts; 5 items in `design-dylib-sdk` §13 are unchecked and need reconciling |
| Q5.3.7 | Dependency vulnerability and license checks | Not met | No `cargo deny`, `npm audit` or `pip-audit` in CI |
| Q5.4.2 | Residue checked at the end of every scenario | Partly | Only the two soak tests sample fds and threads; `runtime_rows.rs` has one process assertion (Unix only) |
| Q5.4.4 | No orphans when the host is forcibly terminated | Not met | Not verified on any platform |
| Q5.5.2 | Conflict detection for several writers | Partly | The loader implements CAS and replay; the multi-writer tests listed in its design §17 need checking one by one |
| Q5.5.3 | Persisted formats readable by later versions | Not met | No samples from earlier releases are kept (J3) |
| Q5.5.4 | Cross-language value round trips verified | Partly | The conformance suites cover only a few fixed values (P7) |
| Q5.6.1, Q5.6.3 | Error content and pinned output | Not met | No output snapshots; no tests for `rutis-host` error output |
| Q5.6.2 | No branching on error text | Met | Connection errors are classified by structured category, with tests across implementations |
| Q5.7.1 | Performance figures marked guarantee or reference | Partly | "About 30 µs per synchronous call" in the design philosophy is not marked; benchmarks have no fixed environment |
| Q5.7.2 | Declared scale | Not met | Nothing declared publicly; the scale in §2.2 is an assumption to confirm |

### 1.2 Component types

| Clause | Component | Status | Current state and gaps |
| --- | --- | --- | --- |
| Q6.1 | Core `rutis` | Partly | 65 contract tests, 57 Cordis parity tests, interleaving tests; missing: model checking with controlled scheduling (K2), single- and multi-threaded execution not both covered, task counts and table capacity on a long-lived root (K5), the items in #177 |
| Q6.2 | Session protocol, runtime control operations, node operations | Partly | Conformance suites complete; missing: malformed / unknown frames (P3), SDK version mismatch (P8), multi-hop cancellation (P6), fault injection (Q5.2). The cross-runtime synchronous call policy is not model-checked (P10) |
| Q6.3 | Local sockets, inherited fd, loopback, WebSocket, memory channel | Partly | All Rust channels run the channel contract; the Node and Python channel implementations do not; the local channel has no size limit (#173) |
| Q6.4 | Frame decoding, framing, handshakes, configuration, manifests, patch merging | Partly | Patch merging has differential verification (against cordis-plugin-include); the rest has no boundary-value tests or fuzzing |
| Q6.5 | Parts that start processes or hold connections | Partly | Concurrent creation has a race (#184); residue checks incomplete |
| Q6.6 | `rutis-host`, `rutis.json`, project templates | Not met | No black-box tests (`main.rs` at 0% coverage); signal handling missing (#174); templates only checked for generated file content |
| Q6.7 | Node / Python SDKs and test tools | Partly | Both SDKs have test tools with strict mode; test tools are not compared with real runtimes (A1) |
| Q6.8 | Node and Python runtimes; Bun (planned) | Partly | Both runtimes pass conformance. On Windows the loader's row kinds run (loopback handover, including Node and Python cold-starting together and using each other's services); but session conformance (`runtime_conformance.rs`) and the runtime conformance matrix (`session_matrix.rs`, including its WebSocket columns) do not, and 5 tests in `runtime_rows.rs` are Unix only. Reentrancy rules written, cross-runtime policy undecided |
| Q6.9 | Service contracts across implementations | Partly | Method shapes can be declared; every language can name its errors; value passing rules are written, but cross-language round trips cover only a few fixed values (P7) and test tools are not compared with real runtimes (A1); replacement itself has no end-to-end verification (S4 #188). Whether implementations return the same data is out of scope (item 4, decided 2026-10-11) |
| Q6.10 | Loader editable layer, dsh profile files | Partly | Atomic writes and conflict replay implemented; no samples from earlier releases |
| Q6.11 | Cordis binding generation | Partly | Generator unit tests; real baseline plugins compiled and called; no periodic verification against new external versions (F4) |
| Q6.12 | dylib loading | Partly | Runs on every merge on Linux and macOS; Windows only when dylib code changes |
| Q6.13 | Node links, remote runtimes, leases | Partly | Library-level tests complete; no verification between two independent host processes (E11); no real network conditions (E12); the backoff sequence (from 0.5 s, 30 s cap, ±20%, reset) has unit tests, but the link's actual retry timing and the reset after 60 s stable do not (E3); leases and reconnection not model-checked (E4, E5) |
| Q6.14 | Cordis mounts and Cordis nodes | Partly | Differential verification against native Cordis; boundary rules 3, 4, 5, 7 not verified (#178) |
| Q6.15 | npm, PyPI, binaries, crates.io | Not met | Packaging only; never installed and run in a clean environment (#193) |
| Q6.16 | `rutis-dev` dev channel | Partly | Local socket and `0600` implemented; "absent from production builds" not verified |

### 1.3 Test rules, process, matrix, release

| Clause | Content | Status | Current state and gaps |
| --- | --- | --- | --- |
| Q7.1 | Determinism | Partly | 7 places depend on the real clock (#182) |
| Q7.2 | Reproducibility | Not met | No seeds in `interleave.rs` or `stress.yml` |
| Q7.3 | No masking of failures | Partly | CI does not retry; no process for recording intermittent failures |
| Q7.4 | Background exceptions fail tests | Not met | The exception in #197 was only printed in a background thread |
| Q7.6 | Black box and white box separate | Not met | No black-box layer |
| Q7.7 | References can be regenerated | Partly | The patch reference file has generation instructions; CI does not check it |
| Q7.8 | Skips explained | Not met | Whole-file `cfg(unix)` makes Windows silently empty |
| Q8.1 | Three layers | Partly | Rules layer thorough; composition only at library level; environment only builds |
| Q9.1.1 | Designs state levels, risks, acceptance | Partly | Most designs have acceptance sections; none state levels or risks |
| Q9.3.2 | Incompatible changes detected; versions meeting across releases | Partly | `cargo semver-checks` only warns; the previous release meeting the current one is not verified |
| Q9.4 | Guarantee register | Not met | No register (#181) |
| Q10.2 | Minimum and latest versions in the support matrix | Partly | Node verified on 24 (Linux) and 26 (macOS), 22 to add once the minimum drops; Python on 3.12 only; the websockets lower bound never verified (#197); MSRV 1.85 never verified; on Windows loader rows are verified, the bridge's session and runtime conformance suites are not |
| Q11.3.2 | No coverage gate | Met | — |
| Q12.1 | Verification at levels | Partly | Before merge and daily; the main line runs exactly what PRs run, with no extra full matrix or installation checks; packaging jobs run on every PR (§8.2) |
| Q12.3 | Selection by change | Not met | Every PR runs every job (§8.2) |
| Q12.4 | Cancel superseded runs | Not met | No `concurrency` |
| Q12.5 | One merge condition | Not met | No aggregate check |
| Q12.6 | Time as developers wait | Not met | 22–46 min from push to result; dylib on the critical path; 6–8 macOS jobs per run |
| Q12.7 | No duplication | Not met | Build reproducibility checked in three places |
| Q12.8 | Main line failures first | Partly | Done by habit, no written process |
| Q12.9 | Measure CI | Not met | §8.1 is the first measurement |
| Q12.10 | CI configuration is code | Partly | Reviewed; PRs that change CI already run everything |
| Q13 | Release gate | Partly | Version consistency check, packaging, two-machine smoke test; missing clean installation, register check, persisted samples, post-release verification |

---

## 2. The project today

### 2.1 Parts

| Part | Content | Used directly by |
| --- | --- | --- |
| Core `rutis` | Plugins, fiber lifecycle, dependency gating, services, events | Rust embedders; every other part |
| `rutis-loader` | Manages a set of plugins from data (rows): layered configuration, imperative edits, persistence, instances | Rust embedders; rutis-host |
| `rutis-bridge` | Channels, the session protocol, language runtimes, node links, Cordis mounts | Rust embedders; rutis-host |
| `rutis-host` | A host without writing Rust: `run` / `dev` / `check` / `new` + `rutis.json` | Plugin authors; operators |
| Node packages | `@arcships/rutis` (plugin SDK and test tool), `@arcships/rutis-runtime` (runtime, Cordis node bridge), `@arcships/rutis-host` (distribution) | Plugin authors; Cordis applications |
| Python packages | `rutis` (plugin SDK, test tool, runtime), `rutis-host` (distribution) | Plugin authors |
| dylib toolchain | `rutis-sdk`, `rutis-dylib*`, `rutis-dev` (dev channel) | Rust plugin authors |
| Applications | `rutis-dsh`, `rutis-agent` | Used in this repository; also the framework's first users |

### 2.2 Operating conditions and scale assumptions

| Condition | Value | Source |
| --- | --- | --- |
| Operating systems | Linux, macOS, Windows x64 (MSVC) | guide/README |
| Language environments | Node ≥ 22 (after #195), Python ≥ 3.10 (after #196), Rust ≥ 1.85, Bun (#194, planned) | guides, design decisions |
| Optional dependencies | `websockets` ≥ 13 (Python networking) | pyproject |
| Processes | The host is one process; each language runtime is at least one; plugins may start their own children | multilanguage design |
| Network | Local: Unix socket / inherited fd / loopback TCP; across machines: WebSocket + TLS, possibly behind a reverse proxy, possibly disconnected, half-open, delayed | network stack design |
| Trust | Plugins trusted; networks and remote machines not fully trusted; running out of process is not a sandbox | remote design §7 |
| Versions | 0.x, a release every few days, minors may break; host, runtimes and SDKs upgrade independently | release.md |

**Scale assumptions** (to be confirmed; they affect the ratings of performance and soak risks in §4):

| Item | Assumed typical | Assumed upper bound |
| --- | --- | --- |
| Rows in one host | 10–50 | 500 |
| Language runtime instances | 1–3 | 10 |
| Nodes | 1–3 | 20 |
| Instances (tenants, sessions) in one embedding application | tens | thousands, created and closed repeatedly |
| Continuous running time | weeks | months |
| Hot reloads during development | hundreds per session | — |
| Cross-process call rate | tens per second | thousands per second (about 30 µs per synchronous call) |

### 2.3 Existing tests

| Part | Tests | Test functions | Line coverage |
| --- | --- | ---: | ---: |
| Core `rutis` | 20 files including contract `contract.rs`, Cordis parity `parity.rs`, interleaving `interleave.rs`; 8 doctests, 4 `compile_fail` | 287 | 92.8% |
| `rutis-bridge` | 34 test files; four conformance suites (channel / session / runtime / node); `fault` decorator; two soak tests | 143 | 87.8% |
| `rutis-loader` | 20 test files; patch merging against cordis-plugin-include | 110 | 89.8% |
| `rutis-host` | Unit tests inside src only | 8 | 69.8% (`main.rs` 0%) |
| `rutis-dev` | dev channel | 5 | — |
| dylib toolchain | `tools/test-dylib*.sh`, `tools/test-sdk-bundle.sh`; SDK hashes compared across two runners | — | — |
| Node packages | `node/*/test/` | 46 | 86% |
| Python packages | `python/rutis/tests/` | 25 | 81% |

Coverage figures come from the local measurement in #183 and include Node / Python child processes started by Rust tests.

CI: `ci.yml` (full Linux run, macOS networking and runtimes, Windows runtimes, packaging, semver, dylib, SDK reproducibility); `stress.yml` (daily: core tests repeated 20 rounds, two 600 s soaks); `dylib-windows.yml` (only when dylib code changes). CI has no fmt, clippy, dependency checks, MSRV, minimum language versions, clean installation or black-box tests.

## 3. Control codes

The risk tables in §4 refer to controls by these codes; how to choose among them is in standard §11.

| Code | Method | Finds | Does not find | Cost |
| --- | --- | --- | --- | --- |
| **U** | Unit tests | Logic errors in a single function or module | Problems between components or processes | Low |
| **K** | Contract and conformance suites (the existing channel / session / runtime / node suites, core contract / parity) | Deviations of different implementations and channels from one specification | Errors in the specification; behavior the suite does not cover | Medium (written once, reused by every implementation) |
| **X** | Differential tests against a reference implementation (native Cordis, cordis-plugin-include, dsh) | Deviations from the reference, including ones nobody thought to test | Errors in the reference; parts without a reference | Medium |
| **E** | Black-box end to end: start a real `rutis-host` or installed packages, drive with configuration files and probe plugins | Assembly, binary behavior, signals, exit codes, multi-process interaction | Hard-to-construct timings; internal state | High (needs a harness) |
| **R** | Residue check: processes, sockets, ports, fds, threads, tasks, registries at the end of a scenario | Resource leaks and orphan processes | Why something leaked | Low (written once, used everywhere) |
| **FI** | Fault injection: the `fault` channel decorator, killing processes, breaking configuration, occupying ports | Whether behavior under faults matches the guarantees | Faults not on the injection list | Medium |
| **FZ** | Fuzzing (cargo-fuzz) | Crashes, unbounded memory, endless loops when parsing external input | Logic errors (wrong results without a crash) | Medium (target written once, runs nightly) |
| **SH** | Concurrency model checking (shuttle: takes over scheduling, systematically tries execution orders, reports the exact order on failure) | Races, lost wakeups, deadlocks in small pieces of code | Wide interactions; code must be extracted into small models | Medium |
| **TLA** | TLA+ design model checking | Deadlocks and invariant violations in a protocol design | Implementation diverging from design | High (learning; specs maintained with the design) |
| **PT** | Property tests (random input, checked properties) | Logic with clear properties: value round trips, counting invariants, matching rules | Behavior without clear properties | Low to medium |
| **SK** | Soak: long repeated operation with sampled resource curves | Slowly growing leaks | Errors that show up fast | Medium (CI time) |
| **MX** | Version and platform matrix: every declared OS, language version, dependency lower bound | Problems specific to a version or platform | — | Medium (CI time) |
| **IN** | Installation smoke test: install this build's packages in a clean directory and run the minimal path | Packaging, distribution, platform package selection | Functional logic | Medium |
| **G** | Output snapshots: error messages, status output, `check` output compared with stored expected text | Worse error messages, changed output formats | Whether the message is actually useful (needs review) | Low |
| **ST** | Static checks: fmt, clippy, `cargo deny`, feature combinations | Common coding errors, vulnerable dependencies, conditional compilation gaps | Logic errors | Low |
| **SV** | API compatibility: `cargo semver-checks`; migration guide code compiled and run | Undeclared breaking changes; outdated migration guides | Behavioral breakage | Low |
| **DOC** | Running documentation examples | Documentation out of step with reality | — | Medium |
| **DR** | Design review and checklists: acceptance sections, PR template items | Design gaps; changes without tests or docs | Depends on people | Low |
| **BM** | Benchmarks | Performance changes | Small changes under CI noise | Medium |

---

## 4. Risk analysis

Rating as in standard §4 (impact, likelihood, P0 / P1 / P2).

### 4.1 Base layer: core and protocols

Every scenario depends on this layer, so it is analyzed on its own.

#### Core `rutis`

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| K1 | Lifecycle rules do not hold: a cleanup runs zero or two times, a plugin starts before dependencies are ready, wrong eviction order | Correctness | High | Medium | P0 | K (65 contract, 57 parity), U | PR | Yes |
| K2 | Races under concurrent interleaving: lost wakeups, driver exit races, a forked tail chain | Correctness | High | Medium (happened 3 times) | P0 | SH (only the points that failed before); multi-threaded repetition with seeds | PR (small SH), nightly | Partly: no seed in `interleave.rs`; `stress.yml` does nothing for single-threaded tests (#182) |
| K3 | Event ordering guarantees only half hold: per-emitter order with several emitters, delivery after a panic | Correctness | Medium | Medium | P1 | K | PR | Partly (D24, D31, #177) |
| K4 | Behavior written as "not guaranteed" changes silently (D29 events not filtered by isolate, D22 no cycle detection) | Correctness | Medium | Low | P2 | U (regression tests pin current behavior) | PR | No (#177) |
| K5 | Repeated creation and destruction on a long-lived root grows internal tables and tasks | Residue | High | Medium | P0 | U + R (task counts and table sizes back to baseline); SK | PR, nightly | Partly: `transient_release.rs` checks entry counts and Weak only (#177) |
| K6 | A user callback panic crosses the boundary and kills a driver task | Faults | High | Low | P1 | U | PR | Yes; panic propagation in async waterfall missing (#177) |
| K7 | Wrong behavior without a tokio runtime or on a current_thread runtime | Compatibility | Medium | Medium | P1 | U (D8); contract tests in both flavors | PR | Partly: D8 untested |
| K8 | Dropping a waiting future interrupts a cleanup already started | Correctness | High | Low | P1 | U | PR | Partly: dropped restart / update futures untested (#177) |
| K9 | `diagnostics()` disagrees with the actual state | Diagnosability | Medium | Medium | P1 | U | PR | Yes (`lifecycle_diagnostics.rs`) |
| K10 | Tests depend on machine speed: false alarms under CI load, or testing nothing on fast machines | — | Medium | High | P0 | Determinism rules (standard Q7.1) | PR | Partly: 7 places depend on the real clock (#182) |

#### Channels and the session protocol

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| P1 | A channel breaks its contract: lost or reordered messages, backpressure failing, close not waking | Correctness | High | Low | P1 | K (channel contract, 6 checks) | PR | Rust channels yes; Node and Python channels do not run the contract |
| P2 | Malformed frames, oversized lines, writes without newlines cause a crash or unbounded memory | Faults, security | High | Medium | P0 | U (boundary values); FZ (frame decoding, line framing) | PR, nightly | WebSocket has a limit and tests; the local channel does not (#173) |
| P3 | Unknown op, wrong field types, nonexistent reference ids make the session hang or are silently ignored | Faults | Medium | Medium | P1 | U (each language) | PR | No (#173) |
| P4 | Reference counting errors: released too early (the far end's calls fail) or never (leak) | Correctness, residue | High | Medium | P0 | K; PT (after any grant / release sequence, table size = number not released) | PR | Rust and Node yes; Python partly (#178) |
| P5 | Call chain rewriting is wrong when forwarding across sessions: callbacks reach the wrong runtime or deadlock | Correctness | High | Medium | P0 | K (`rpc_callbacks.rs`, same call ids on both sides) | PR | Yes |
| P6 | Cancellation does not reach the far end, or is lost across hops | Correctness | Medium | Medium | P1 | K (`abortable`); one new multi-hop cancellation test | PR | Single hop yes; multi-hop no (#178) |
| P7 | Values convert differently across the three languages: `undefined` / `null` / absent, large integers, dataclasses, nesting depth | Correctness | High | High | P0 | PT (the same random values round-tripped Rust↔Node and Rust↔Python, compared); K | PR | Partly: conformance covers a few fixed values |
| P8 | On a protocol version or capability mismatch, things hang or fail only after loading | Compatibility, diagnosability | Medium | High (runtimes and hosts upgrade independently) | P1 | U (each language); MX (previous release's runtime vs current host) | PR, merge | Rust yes; neither SDK (#178) |
| P9 | During a synchronous call the far end calls back: Node should return `SyncWaitCycle`, Python should execute; behavior differs from the docs | Correctness | Medium | Medium | P1 | K (`reenter`) | PR | Yes |
| P10 | The cross-runtime synchronous call policy itself has holes (two non-reentrant runtimes calling each other) | Correctness | High | Medium | P0 | TLA (design time); then K with counterexample tests | Design | No; design undecided (multilanguage design §9) |
| P11 | Exceptions in background threads or workers are swallowed and tests pass anyway | — | High | High (#197 happened) | P0 | Test rule: background exceptions fail tests (standard Q7.4) | PR | No |

---

### 4.2 Scenarios

Each scenario first says what it is, which parts it goes through and under what conditions, then lists risks.

#### Scenario A: the plugin author's development loop

**What**: `rutis-host new` creates a project → unit tests with the SDK test tool → `rutis-host dev` runs it in a local host and reloads on change → `check` → publish to npm / PyPI → used by a host. Both TS and Python.

**Through**: `rutis-host` (new / dev / check), templates, SDKs and test tools, runtimes, loader reloads.

**Conditions**: three platforms; minimum and latest Node and Python; hundreds of reloads in a session; code often in a broken state.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A1 | Test tool and real runtime disagree: a plugin passing in `load()` fails in a host (or the other way) | Correctness | High | Medium | P0 | X: run the runtime conformance fixture (`conformance-weather`) both through the test tool and in real runtimes and compare | PR | No |
| A2 | A generated project does not install, test and run as is | Compatibility, docs | Medium | High (dependencies and toolchains move fast) | P1 | E (S2 steps 1–2); MX | Merge | Only generated file content checked |
| A3 | After a reload the old code still runs, or the old instance is not unloaded | Correctness, residue | High | Medium | P0 | E + R | PR | Library level in the loader; not at the binary level |
| A4 | A syntax error crashes `dev`, or the old version stops serving | Faults | Medium | High | P1 | E (break a file, assert the old version keeps serving and `cannot reload` is printed) | PR | No |
| A5 | Resource use grows after hundreds of reloads | Residue | Medium | Medium | P1 | E + R, looped N times | Nightly | No |
| A6 | `check` misjudges: errors on something that runs, passes something that does not | Diagnosability | Medium | Medium | P1 | G (a set of good and bad projects, output and exit codes compared) | PR | No |
| A7 | Error messages do not say which file, line or service | Diagnosability | Medium | High | P1 | G (snapshots of common errors); DR | PR | No |
| A8 | On Windows, paths, venv (`Scripts\python.exe`), line endings parsed wrong | Compatibility | Medium | Medium | P1 | E on Windows | PR | No |
| A9 | On a plugin API version mismatch, the error does not say what to upgrade | Compatibility, diagnosability | Medium | Low | P2 | U (both SDKs) | PR | Yes |
| A10 | A declared method shape disagrees with the implementation (declared sync but returns a Promise) and the test tool misses it | Correctness | Medium | Medium | P1 | U (test tool strict mode) | PR | Yes |
| A11 | Python re-imports only the entry module (known limitation); users expect changes to other modules to apply | Docs | Low | High | P2 | Documented; `dev` hints when other modules change | — | Documented |
| A12 | The publishing workflow in templates does not work | Docs | Low | Medium | P2 | ST (actionlint on workflow syntax) | PR | No |

#### Scenario B: operators deploying with rutis-host

**What**: install from npm / PyPI / binary / crates.io → write `rutis.json` → `rutis-host run` under a process manager such as systemd → edit configuration while running → stop, restart, upgrade.

**Through**: distribution packages, `rutis-host` configuration parsing and assembly, loader, runtimes, nodes.

**Conditions**: three platforms; may be ended by SIGTERM or SIGKILL; configuration may be wrong; runs for weeks.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| B1 | A channel's package is broken: platform package not selected, wheel missing files, binary does not run on a clean machine | Compatibility | High | Medium | P0 | IN (three platforms × npm / PyPI / binary, this build's packages); again with registry packages after release | Merge, after release | Packaging only (#193) |
| B2 | On SIGINT / SIGTERM (Windows: Ctrl-C, Ctrl-Break, console close) cleanup does not run; exit codes differ from the docs | Residue | High | High (#174 happened) | P0 | E + R (each signal in turn) | PR | No |
| B3 | After the host is SIGKILLed, runtime children are orphaned | Residue | High | Medium | P0 | E + R (Unix through channel close, Windows through a Job Object) | PR | No |
| B4 | After a configuration is broken, old rows are unloaded and new ones do not start | Faults | High | Medium | P0 | E (write a broken configuration, assert the old version keeps serving); loader K exists | PR | Loader yes; host level no. Check first whether `run` watches the configuration file |
| B5 | `rutis.json` parsing: unknown fields silently ignored, wrong base for relative paths, wrong precedence of credential environment variables | Correctness, diagnosability | Medium | Medium | P1 | U (each field); FZ (configuration, low priority) | PR | Partly |
| B6 | Missing runtime package, interpreter not found, port in use, bad certificate file give insufficient errors | Diagnosability | Medium | High | P1 | G (a snapshot per error) | PR | Partly |
| B7 | Credentials appear in logs, errors, status output, traces | Security | High | Low | P1 | Common E assertion: token values absent from all captured output | PR | Partly |
| B8 | A listener without TLS binds to a non-loopback address | Security | High | Low | P1 | U | PR | To check |
| B9 | Status output format changes break operators' scripts | Compatibility | Low | Medium | P2 | G | PR | No |
| B10 | When the process manager restarts the host, the previous socket file or port is not released and the new process fails to start | Residue | Medium | Medium | P1 | E (restart right after kill -9) | PR | No |

#### Scenario C: composing languages

**What**: in one host, Rust, TS and Python plugins provide and use each other's services. Design philosophy §2: "multilanguage plugins are not an add-on; they are the reason it exists".

**Through**: core gating, loader rows, two-phase readiness (`RuntimeRows`), runtime processes, session forwarding, service projection.

**Conditions**: several runtimes cold-starting together; runtime processes may crash; synchronous and asynchronous calls interleave; three platforms (Windows over loopback TCP).

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| C1 | Runtimes wait for each other on a cold start | Correctness | High | Low | P1 | K (the M1/M2 boundary conformance test) | PR | Yes |
| C2 | A row starts before its declared dependencies are complete (two-phase readiness fails) | Correctness | High | Medium | P0 | K (`runtime_rows.rs`, resolve before start) | PR | Yes |
| C3 | One runtime crashing affects others, or unrelated rows stop | Faults | High | Medium | P0 | K (M2 §3.4); E (kill -9, assert other runtimes' pids unchanged) | PR | Library level yes; black box no |
| C4 | When a runtime crashes, calls in flight hang instead of failing explicitly | Faults | High | Medium | P0 | FI (the conformance fixture's `crash()` during a call) | PR | Partly |
| C5 | Values convert inconsistently across languages (see P7) | Correctness | High | High | P0 | PT | PR | Partly |
| C6 | Two Node runtimes deadlock on crossing synchronous calls (known limitation); the limitation grows with a third language | Correctness | High | Medium | P0 | TLA (choose a policy); docs; K counterexamples once chosen | Design | Documented only |
| C7 | The Python runtime is reentrant, so a plugin's service is called in the middle of its own synchronous call and its state is changed concurrently | Correctness | Medium | Medium | P1 | Docs; optionally simulate reentry in the test tool's strict mode | — | Documented |
| C8 | Wrong withdrawal order: providers stop before consumers | Correctness | High | Low | P1 | K | PR | Yes |
| C9 | Direct objects within one runtime vs proxies across runtimes differ beyond what the docs say | Correctness | Medium | Medium | P1 | K (`multilang.rs`) | PR | Yes |
| C10 | Any of the above fails on Windows (loopback TCP + token handover) | Compatibility | High | Medium | P0 | MX: the loopback and WebSocket columns of the conformance suites run on Windows | PR | Partly: the loader's row kinds run on Windows over loopback (including the cross-language cold start); 5 bridge test files, among them session conformance and the runtime conformance matrix, are whole-file `cfg(unix)` and empty on Windows (#176) |
| C11 | Starting several runtimes concurrently in one process fails intermittently | Faults | Medium | High (#184 about 50% locally) | P0 | FI: N threads starting together, 100 rounds | PR | No |
| C12 | A runtime writing heavily to stdout / stderr fills the pipe and blocks | Faults | Medium | Low | P2 | FI (fixture writing continuously) | Nightly | No |

#### Scenario D: embedding in a Rust application, multi-tenant instances

**What**: a Rust application assembles the host from crates and provides its own Rust services; creates instance subtrees per business object (tenant, session, document), with one loader managing global and instance rows; configuration is persisted and restored after restart.

**Through**: core instance subtrees, loader instances, persistence, `Persist` implementations, instance scopes inside runtimes, instances in nodes.

**Conditions**: long-lived process; tens to thousands of instances created and closed repeatedly; embedders configure tokio differently; embedders upgrade crate versions.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| D1 | Service visibility leaks between instances: tenant A reads tenant B's service | Security, correctness | High | Low | P1 | K (`instances.rs`, `instance_services.rs`, `instance_runtimes.rs`) | PR | Yes |
| D2 | Residue after closing an instance: core registrations, registrations inside runtimes, export slots, proxies | Residue | High | Medium | P0 | U + R (registrations inside runtimes too, not only diagnostics) | PR | Partly |
| D3 | Thousands of create/close cycles grow tasks, memory, table capacity | Residue | High | Medium | P0 | SK (1000 rounds, task counts and table capacity back to baseline) | Nightly | Partly (instance-subtrees §5 not fully implemented, #177) |
| D4 | One edit affects several instances, some succeed and some fail | Correctness | High | Low | P1 | K | PR | Yes |
| D5 | After persisting and restarting, state differs from before | Correctness | High | Medium | P0 | K (restart consistency in loader §17); E (S7) | PR | §17 needs checking item by item |
| D6 | Several processes write the same storage and changes are lost | Correctness | High | Medium (dsh's CLI and web UI open together) | P0 | K (loader multi-writer CAS, replay drops); PT (random edits with simulated conflicts, final storage = all edits) | PR | To check |
| D7 | Undeclared breaking changes in public APIs | Compatibility | Medium | High (0.x) | P1 | SV (semver-checks exists); migration guide code compiled | PR | Partly |
| D8 | Embedders calling a synchronous method from a current_thread runtime or a blocked thread deadlock | Faults | Medium | Medium | P1 | K (the network stack's requirement that the channel progresses while the caller's current_thread is blocked) | PR | To check |
| D9 | Builds fail with only some features enabled | Compatibility | Medium | Medium | P1 | ST (the existing 7 combinations) | PR | Yes |
| D10 | `shutdown` does not finish by its deadline and embedders cannot tell the state | Diagnosability | Medium | Low | P2 | U | PR | Yes |
| D11 | Latency grows abnormally at the upper bound of instances | Performance | Medium | Low | P2 | BM (create, close, read services with 1000 instances) | Weekly | No |

#### Scenario E: nodes across machines and remote runtimes

**What**: nodes connect over WebSocket + TLS, import and export services, run plugins for each other, forward events; a GPU machine runs only a Python runtime, managed by the main node.

**Through**: WebSocket transport, identity, link, session, node feature plugins, remote runtime leases, `peer:` rows in the loader.

**Conditions**: real networks (delay, loss, disconnection, half-open, NAT, reverse proxies); the two ends may run different versions; credentials may be revoked; remote machines are not fully trusted.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| E1 | When the network drops, calls in flight report success, or are retried automatically so side effects run twice | Faults | High | Medium | P0 | FI (`fault::drop_and_close` after frame N, the far end counts side effects) | PR | No: `fault` only used in the memory channel's own tests (#178) |
| E2 | A half-open connection is not found within 30 s | Faults | Medium | Medium | P1 | FI (`half_open` with a paused clock) | PR | Yes |
| E3 | Reconnection backoff differs from the docs (from 0.5 s, capped at 30 s, ±20%, reset after 60 s stable; 30 s after rejected credentials; stop on incompatibility) | Correctness | Medium | Medium | P1 | U (paused clock, exact intervals asserted) | PR | Partly: a unit test in `link.rs` covers the backoff sequence (from 0.5 s, 30 s cap, ±20%, reset); the link's actual retry timing, the reset after 60 s stable and the 30 s after rejected credentials are untested |
| E4 | During takeover of a remote runtime, old and new leases coexist; late instructions of the old session act on the new lease | Correctness | High | Low | P1 | TLA (design); K (`leases.rs`) | Design, PR | Partly |
| E5 | Connections still possible after credentials are revoked; a registration revoked mid-handshake still lets the old handshake hand over | Security | High | Low | P1 | K (`link.rs`); TLA | PR | Partly |
| E6 | TLS check holes: still connects with an expired certificate, wrong CA, hostname mismatch | Security | High | Medium | P0 | U (three negative cases, Rust and Python each) | PR | Partly: untrusted CA on the Rust side yes; expired certificate, hostname mismatch and the Python side no (#178) |
| E7 | A malformed WebSocket upgrade request crashes the listener | Security, faults | High | Medium | P0 | FZ | Nightly | No |
| E8 | Rejection categories differ between implementations (Python↔Rust) | Compatibility | Medium | Medium | P1 | K (a Python column in `websocket_cross.rs`) | PR | Node↔Rust only |
| E9 | Cancellation and release not passed hop by hop | Correctness, residue | Medium | Medium | P1 | K (new multi-hop cancellation and release tests) | PR | No |
| E10 | Service announcements arrive out of order and an old one restores a withdrawn service | Correctness | Medium | Low | P2 | U (constructed reordering) | PR | To check |
| E11 | Behavior between two independent host processes (not within one test process) differs from library-level tests | Correctness | Medium | Medium | P1 | E (S5: two `rutis-host` processes over loopback) | PR | No |
| E12 | Under real conditions (200 ms delay, 5% loss), timeouts and frequent reconnects | Faults, performance | Medium | Medium | P1 | E + containers + netem | Nightly | No |
| E13 | When the two ends run different versions, the incompatibility message does not tell which end to upgrade | Compatibility, diagnosability | Medium | High | P1 | MX (previous release's node vs current); G | Merge | Partly |
| E14 | A remote runtime accepts `file:` or absolute plugin paths | Security | High | Low | P1 | U | PR | To check |
| E15 | Resource use grows as one link drops and reconnects repeatedly | Residue | Medium | Medium | P1 | SK (`websocket_soak.rs` exists) | Nightly | Yes |

#### Scenario F: existing Cordis plugins and Cordis applications

**What**: three ways — Cordis plugins as rutis-loader rows; Cordis plugins mounted in a Rust application with Rust types generated at build time; existing Cordis applications joined as nodes with `@arcships/rutis-runtime/bridge`.

**Through**: Node runtime, Cordis 4.0.4, the generator (`generate.mjs`), service projection, event forwarding, boundary rules.

**Conditions**: plugins are third-party published npm packages whose versions users pin and which keep changing; Cordis itself changes too.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| F1 | Mounted behavior differs from native Cordis | Correctness | High | Medium | P0 | X (`dsh_baseline.rs` compares the same scenarios with native Cordis) | PR | Yes |
| F2 | Generator type mapping errors (optional vs nullable, newtypes, unions of live objects) | Correctness | High | Medium | P0 | U (15 in `generate.test.mjs`); generate from baseline plugins, compile and call | PR | Yes |
| F3 | Violations of boundary rules 3, 4, 5, 7 not detected, silently degraded | Correctness | Medium | Medium | P1 | K (one cross-process test per rule, compared with native) | PR | No (#178) |
| F4 | A new Cordis or plugin release breaks mounting | Compatibility | Medium | High | P1 | X: nightly baseline with the latest published versions (lock files unchanged), report only | Nightly | No |
| F5 | After the Node process exits on an uncaught plugin exception, services are not withdrawn or dependents do not stop | Faults | High | Medium | P0 | K | PR | Yes |
| F6 | After moving to another directory (`RUTIS_CORDIS_ROOT`), the runtime or plugins cannot be found | Compatibility | Medium | Medium | P1 | E (build, copy elsewhere, run) | Merge | No |
| F7 | Members that cannot be bound are not reported at build time | Diagnosability | Low | Medium | P2 | G (snapshot of build warnings) | PR | To check |
| F8 | Mounting through `rutis.json` (not at library level) behaves differently | Correctness | Medium | Medium | P1 | E (S6) | Nightly | No |

#### Scenario G: moving an implementation (try → prove → solidify)

**What**: design philosophy §4: `planner` depends on the service name `calendar`; the implementation moves from a Python plugin to a plugin on an intranet node, then to Rust; through all three stages `planner` changes no line and the host does not restart.

**Through**: loader edits, the service name catalog, core eviction and reload, node imports, runtimes.

**Conditions**: implementations of the same service written by different people in different languages; the switch happens while running.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| G1 | Implementations of the same service in different languages return differently shaped data (field names, optional fields, error types); consumers break after the switch | Correctness | — | — | Out of scope | The service's authors are responsible (standard Q6.9.4, decided 2026-10-11). The part rutis owns: the same value converts the same way between languages (P7), and test tools behave like real runtimes (A1) | — | — |
| G2 | Calls hang during the switch | Faults | Medium | Medium | P1 | E (S4: continuous calls during the switch, each succeeds or fails explicitly) | PR | No |
| G3 | The old implementation leaves residue after the switch (processes, links) | Residue | Medium | Medium | P1 | E + R | PR | No |
| G4 | Consumers do not restart and keep the old implementation | Correctness | High | Low | P1 | K (core eviction) | PR | Yes |
| G5 | The host needs a restart during the switch (breaks the guarantee) | Correctness | High | Low | P1 | E (host pid unchanged) | PR | No |

#### Scenario H: an agent writes a plugin, connects it, iterates

**What**: the main line of design philosophy §1: whatever the host needs to reach, an agent writes a plugin for, tries, changes, replaces.

**Today**: a product capability is missing. `design-session-persist-and-self-tools` §3 says "no dynamic loading of new code"; the agent's tools come from `ToolRegistry` and are not connected to rutis services (#185). Most controls for this scenario wait for that design. Risks known now:

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| H1 | A plugin written by an agent loops forever, blocks the event loop or crashes the runtime, taking other plugins in the same runtime down | Faults | High | High | P0 (when the capability ships) | Design: agent-written plugins go into a separate runtime instance by default; E | Design | No design |
| H2 | Agent-written code runs with the host's privileges (not a sandbox) | Security | High | High | P0 (when the capability ships) | Design decision (design philosophy §8 "permissions"); DR | Design | Open question |
| H3 | Why loading failed is not returned in a form the agent can use | Diagnosability | Medium | High | P1 | G (snapshots of structured error output) | PR | No |
| H4 | Resource use grows after hundreds of generate/load/unload cycles | Residue | Medium | Medium | P1 | SK + R | Nightly | No |
| H5 | Tests with real models are not repeatable | — | Medium | High | P1 | Drive with a scripted model backend (`ScriptedLlm`); real models only in `#[ignore]` manual tests | — | Scripted backend exists |

#### Scenario I: dylib plugins and the dev channel

**What**: Rust plugins compiled as dynamic libraries, loaded after the host checks their identity; the `rutis-dev` channel swaps them during development.

**Through**: `rutis-sdk`, `rutis-dylib`, the launcher, `rutis-dev`, `xtask`.

**Conditions**: toolchain versions, build paths and platforms vary; loading the wrong library can cause memory errors.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| I1 | A library with mismatched identity (SDK, toolchain, artifact hash) is loaded and its initializers run | Security, faults | High | Medium | P0 | E (`tools/test-dylib*.sh`, `test-sdk-bundle.sh`) | PR (Linux, macOS) | Yes; 5 items in `design-dylib-sdk` §13 unchecked, need reconciling |
| I2 | After a failed swap the old version stops serving | Faults | High | Low | P1 | E | PR | Yes |
| I3 | SDK builds are not reproducible | Compatibility | Medium | Medium | P1 | Two runners build independently and compare hashes | PR | Yes |
| I4 | dylib problems on Windows | Compatibility | Medium | Medium | P1 | E (dylib-windows) | Only on dylib changes | Partly: suggest running on every merge |
| I5 | A toolchain mismatch is not caught by `xtask dev` before compiling | Diagnosability | Medium | Medium | P1 | E | PR | No (#193 item 5) |
| I6 | The dev channel is present in production builds | Security | High | Low | P1 | ST (production builds contain no dev listener symbols or feature) | Merge | No |
| I7 | dev channel commands behave unclearly with missing arguments, missing targets, or a closing host | Diagnosability | Low | Medium | P2 | U (commands × errors) | PR | Partly (5 tests) |

#### Scenario J: upgrading rutis

**What**: users move from 0.x to 0.(x+1): Rust crates, npm packages, PyPI packages, `rutis-host`. Host, runtimes and plugin SDKs may not be upgraded together.

**Through**: the release train, protocol versions, plugin API versions, persisted formats, migration guides.

**Conditions**: 0.x allows breaking changes; plugin authors depend on SDK version ranges, not in step with hosts.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| J1 | Code in a migration guide is outdated and following it does not compile | Docs | Medium | Medium | P1 | SV ("after" code from migration guides compiled and run; one exists in `migration_example.rs`) | PR | Partly |
| J2 | A host upgraded without its runtime (or the reverse) behaves unclearly: hangs, or fails after loading | Compatibility | Medium | High | P1 | MX: the previous release's runtime package with the current host must either work or fail clearly at start | Merge | No |
| J3 | Persisted configuration written by an older version (loader layers, dsh profiles) cannot be read by the new one, or changes meaning | Compatibility | High | Medium | P0 | U: each release stores a persisted sample as a fixture; new versions must read it | PR | No |
| J4 | Release train versions are inconsistent and users get two copies of the core | Compatibility | High | Low | P1 | ST (`scripts/train.mjs`) | PR | Yes |
| J5 | A new plugin SDK marks a higher plugin API and old hosts give an unclear error | Compatibility, diagnosability | Medium | Medium | P1 | U | PR | Yes |

#### Scenario K: long runs

**What**: a host runs for weeks or months, with configuration edits, reloads, runtime crashes and network reconnects along the way.

| # | What can go wrong | Dimension | Impact | Likelihood | Pri | Control | Stage | Current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| L1 | The host's fds, threads, tasks, memory grow over time | Residue, performance | High | Medium | P0 | SK: host-process soak with random operations (edits, reloads, killing runtimes, dropping links), resource curves sampled, no sustained growth asserted | Nightly (1 h), weekly (longer) | Partly: only the two bridge-level soaks |
| L2 | A slow listener makes the event backlog grow without bound | Performance | Medium | Medium | P1 | U (backlog diagnostics); docs | PR | Diagnostics yes, no limit (by design) |
| L3 | After runtime processes are killed repeatedly, host state or counters go wrong | Faults | Medium | Medium | P1 | SK (random operations include kills) | Nightly | No |
| L4 | Failures in random operation sequences cannot be reproduced | — | Medium | High | P1 | Sequences generated from seeds; seed and sequence saved on failure | Nightly | — |

---

## 5. Residue checks: proposed approach

How standard Q5.4.2 is to be checked on the current implementation; not implemented yet. Today only the two soak tests sample fds and threads (tolerance +16) and check that every child process is reaped. To apply to every black-box scenario, every soak and every repeated create/destroy test:

| Item | How |
| --- | --- |
| Every process the host started has exited | Linux: the test process sets `PR_SET_CHILD_SUBREAPER` and enumerates by process group; macOS: `ps` by process group; Windows: the host runs in a Job Object whose process list must be empty |
| Socket files removed | No `*.sock` under the scenario's temporary directory |
| Ports released | `bind` on recorded ports succeeds again |
| fds and threads of the test process | Back to baseline, tolerance to be decided (the existing soak uses +16); the sampling code in `local_soak.rs` is to be moved into a shared test module |
| Live tokio tasks | `Handle::metrics().num_alive_tasks()` back to baseline |
| Core registries | Fibers, bindings, listeners, event backlog in `diagnostics()` back to baseline |
| Registrations inside runtimes | Runtimes provide a test query returning how many rows, proxies and export slots they hold |
| Credentials absent from output | No token value in any captured output |

## 6. Controls to build

The controls used by the P0 and P1 risks of §4, merged into what needs building or extending.

| Control | Risks served | Current | Work |
| --- | --- | --- | --- |
| **Determinism rules** | K10, P11, and the trustworthiness of every test | Partly | Standard Q7.1–Q7.5; #182 |
| **E + R: black-box harness and residue checks** | A3–A5, A8, B2–B4, B10, C3, E11, F6, G2, G3, G5, H4 | No | `tests/e2e/`: the harness starts real binaries or installed packages, probe plugins (TS, Python), residue checks; scenarios written in Rust (#175) |
| **Scenarios** | As above | No | S2 development loop (A), S3 languages and crashes (C), S9 installation (B1) first; then S4 moving implementations (G), S5 nodes in two processes (E); S6 Cordis (F8), S7 embedding and instances (D5), S8 long runs (L1, L3) nightly |
| **FI: fault injection on sessions and links** | C4, C11, E1, E2 | `fault` exists, self-tested only | Wire into session conformance and link tests; concurrent start test (#178, #184) |
| **MX: version and platform matrix** | C10, A8, E13, J2, P8 | Node 24 / 26, Python 3.12; Windows skips the bridge's session and runtime conformance suites | Node 22 (once the minimum drops), minimum and latest Python, the websockets lower bound; Windows loopback and WebSocket columns; previous release's runtime vs current host |
| **IN: installation smoke test** | B1 | No | Three platforms × three channels after merge; again with registry packages after release (#193) |
| **PT: cross-language value round trips** | P7, C5, P4 | Partly | The same random values round-tripped Rust↔Node and Rust↔Python and compared; release counting invariant |
| **X: test tools vs real runtimes** | A1 | No | The conformance fixture run both in test tools and in real runtimes |
| **X: nightly baseline with the latest Cordis plugins** | F4 | No | Nightly install of the latest versions, baseline run, report only |
| **FZ** | P2, E7, B5 | No | Session frames, local line framing, WebSocket upgrade requests; configuration files at low priority (#179) |
| **SH** | K2 | No | Only the 3 places that failed before: lost wakeup in TransitionTask, the `post_join` race, exactly-once cleanup under concurrent dispose (#179) |
| **TLA** | P10, C6, E4, E5 | No | §7 |
| **SK: host-level soak** | L1, L3, D3, A5, H4 | Bridge level only | Host-process soak with seeded random operations |
| **G: output snapshots** | A6, A7, B6, B9, E13, F7, H3 | No | Common errors, `check` output, status output |
| **J3: persisted samples** | J3 | No | A sample per release; new versions must read it |
| **ST** | All | No fmt or clippy in CI | fmt, clippy, `cargo deny`, MSRV, actionlint |
| **Unit and contract tests to add** | K3–K8, P3, P6, P8, E3, E6, E8, E9, F3 and more | Partly | #177, #178, #173 |

---

## 7. Current scope of design models

The two places selected by standard §11.2.

### 7.1 Spec 1: cross-runtime synchronous calls and the deadlock policy (P10, C6)

**Why first**: multilanguage design §9 states this is unresolved. The Node runtime is not reentrant, the Python one is, and two Node runtimes making crossing synchronous calls deadlock. A policy must be chosen before a third language (Swift, Go, or Bun #194). Deadlocks appear only in particular interleavings, which tests hit unreliably.

**Model**

- Participants: the Rust host (relay), 2–3 runtimes, each reentrant or not.
- Actions: a runtime makes a synchronous call (possibly to a service of another runtime); the host forwards it and rewrites the call chain `path`; while waiting synchronously, the callee receives incoming calls and handles them by its own rule (non-reentrant: only calls on its own chain, others deferred; reentrant: all); return; return `SyncWaitCycle`.
- Invariants: no deadlock (while calls are unfinished, some participant can proceed); a callback always returns to the runtime that made it; `SyncWaitCycle` is returned only when the wait really forms a cycle.

**The two candidate policies compared** (from multilanguage design §9):

| Policy | Content | Question to answer |
| --- | --- | --- |
| A | All leaf runtimes reentrant, only the Cordis runtime not; Rust refuses synchronous calls between two non-reentrant runtimes | Can it be decided when the call is made; does it refuse legitimate calls |
| B | Rust tracks waits across sessions and returns `SyncWaitCycle` on a cycle | Does the host see enough to detect cycles; does it miss or misreport |

**Scale**: 3 runtimes, call chains of depth ≤ 3, ≤ 2 concurrent calls.

**Output**: the chosen policy written back into multilanguage design §9; counterexamples turned into tests in `rpc_callbacks.rs`.

### 7.2 Spec 2: remote lease takeover, link reconnection and registration revocation (E4, E5)

**Why**: remote design §4.4 defines the takeover order (authenticate → close the old session → clean up the old lease → answer the new hello); the network stack design defines registration generations, revocation during handshakes, and that late results are not handed over. The existing `leases.rs` and `link.rs` cover only the timings they can construct.

**Model**

- Participants: 2 controller links, 1 remote runtime (listening), Identity (revocable).
- Actions: dial, authenticate, hello, disconnect, reconnect with backoff, takeover by a new connection, lease cleanup, identity revocation, late handshake results, late instructions from an old session.
- Invariants: at any time a runtime instance has at most one active lease; instructions of an old session never act on a new lease; after a registration is revoked, an old handshake never completes handover; a stopped link does not reconnect.
- Liveness: with both controllers healthy, one eventually holds the lease.

**Scale**: 2 controllers, up to 3 reconnects each, messages delayed by up to 2 steps.

### 7.3 Practice

- Specs live in `specs/<name>/`: `.tla`, `.cfg` and a README (which design and section, what is simplified, which invariants are checked).
- TLC runs small configurations nightly; larger ones by hand when the design changes.
- When a design change touches these two protocols, the PR template requires updating the spec.
- No trace validation (checking implementation logs against the spec); too costly for now.
- Timing: Spec 1 before Bun (#194) or a third language; Spec 2 before the next major change to lease code.

---

## 8. Continuous integration: current state and changes

Corresponds to standard §12.

### 8.1 Measurements (2026-10-10, last 30 runs of `ci.yml`)

From push to result: 22–25 minutes without queueing, 35–46 minutes when busy, once over 2 hours.

Execution time of each job in a run without queueing (#202, run `38024950688`):

| Job | Execution | Breakdown |
| --- | ---: | --- |
| dylib-macos | 25.1 min | build reproducibility 9.8, external plugin build 7.6, launcher checks 5.3 |
| dylib-linux | 20.9 min | build reproducibility 9.1, external plugin build 6.1, launcher checks 4.4 |
| test (full Linux) | 7.7 min | `cargo test --workspace` 3.3 |
| sdk-repro ×2, sdk-repro-macos ×2 | 3.6–4.9 min each | — |
| runtimes-windows | 4.2 min | — |
| network-macos | 2.2 min | — |
| The other 8 | < 3 min each | — |

With repeated pushes to one branch (`feat/bun-runtime` pushed 6 times between 02:10 and 02:52, 6 runs at once), macOS jobs queued (run `38016607150`): sdk-repro-macos 32 minutes, dylib-macos 23.5, static-platforms 14.7, network-macos 10.7.

### 8.2 Problems

| Clause | Current state |
| --- | --- |
| Q12.4 Cancel superseded runs | No `concurrency` setting; older runs of the same PR run to the end and occupy runners |
| Q12.3 Selection by change | `ci.yml` selects nothing by path: a docs-only PR runs all 17 jobs and waits 20–25 minutes for dylib |
| Q12.6.1 Critical path | dylib-macos / dylib-linux are the critical path, three times longer than any other job |
| Q12.6.2 Scarce resources | Each run takes 6 macOS jobs (8 on the Bun branch); macOS runners are few, and queueing for them is the main wait when busy |
| Q12.6.3, Q12.7 No duplication; uncacheable checks | Build reproducibility is checked in three places: `sdk-repro` (hashes from two Linux runners), `sdk-repro-macos` (two macOS runners), and `test-dylib-repro.sh` inside both dylib jobs; all before merging, none can use caches |
| Q12.1 Levels | Packaging-only jobs (`release-windows`, `release-wheel-aarch64`, `release-dry-run`) run on every PR, though they can only fail when packaging files change |
| Q12.5 Aggregate check | None; once jobs are selected by change, an aggregate check is needed as the required check in branch protection |
| Q12.9 Measurement | None; this section is the first measurement |

### 8.3 Mapping from change to verification

Before merging, a first job determines what changed and later jobs run accordingly; on the main line everything runs (Q12.3).

| Change | Runs before merging |
| --- | --- |
| Documentation only (`docs/**`, `*.md`) | Link check |
| Core, bridge, loader, host, Node / Python packages | Full Linux tests; macOS networking and runtimes (with the static build check); Windows runtimes; static checks |
| dylib: `rutis-dylib*`, `rutis-sdk`, `rutis-dev`, `rutis-xtask`, `tools/*dylib*`, `tools/test-sdk-bundle.sh`, `tests/dylib-fixtures/**`; and core changes that change the SDK's content | Also dylib-linux, dylib-macos |
| Packaging: `release*.yml`, `scripts/train.mjs`, each `pyproject.toml` / `package.json`, `node/rutis-host/scripts/**` | Also the three packaging jobs |
| `Cargo.lock`, root `Cargo.toml`, `rust-toolchain.toml`, `.github/workflows/**` | Everything (Q12.3.2, Q12.10) |

### 8.4 Target layout

| When | Budget (push to result, including queueing) | Content |
| --- | --- | --- |
| **PR** | Docs only ≤ 2 min; ordinary changes ≤ 15 min; dylib changes ≤ 25 min | Selected per §8.3; `concurrency` cancels older runs; aggregate check `ci-ok`. At most 1 macOS job per run (2 when dylib changes). Added as tests are built: ST, U, K, X (except the daily baseline), the conformance matrix, E (S2 on three platforms; S3, S4, S5 loopback), small SH, PT, minimum and latest Node / Python / websockets |
| **Merge to main** | ≤ 60 min | Every job: build reproducibility compared across two runners, packaging, dylib-windows, IN on three platforms, MX previous release's runtime vs current host, MSRV, `cargo deny`, DOC, F6 |
| **Nightly** | ≤ 4 h | Core tests repeated with varying thread counts (seeded); SK (bridge level 600 s, host level 1 h); FZ 20 min per target; large SH; TLC; E: S5 with containers + netem, S6, S7, S8; X: latest Cordis plugin baseline |
| **Weekly** | ≤ 8 h | Long host soaks; BM (fixed machine); CI measurement summary (Q12.9) |

Tests run with `cargo nextest`: each test in its own process, with timeouts, `retries = 0`.

### 8.5 Expected effect

| PR | Now | After |
| --- | --- | --- |
| Docs only | 22–46 min | About 1 min |
| Ordinary code change | 22–46 min | About 8 min (critical path: full Linux tests) |
| dylib change | 22–46 min | About 22 min; about 10 min on Linux after splitting, macOS unchanged (reproducibility cannot be cached) |
| Repeated pushes to one branch | Every run completes | Only the last completes |

### 8.6 Steps

1. Add `concurrency` and cancel older runs on PRs (Q12.4). A few lines, merged on their own, effective at once.
2. Select jobs per §8.3; add the aggregate check `ci-ok` and update branch protection; merge `static-platforms (macos)` into `network-macos`; keep only the single-machine reproducibility check inside the dylib jobs, and move the two-runner `sdk-repro*` and the three packaging jobs to the main line (Q12.3, Q12.5, Q12.6, Q12.7).
3. On Linux, split the dylib job into three parallel jobs: launcher, build reproducibility, external plugin build (Q12.6.1). macOS is not split.
4. Weekly CI measurement: push to result, queueing, job times, intermittent failures, main line failures (Q12.9).

## 9. Plan

Ordered by risk level, P0 first; within a level, infrastructure others depend on first. Aligned with the current release plan.

### 9.1 Step one (0.8.1 → 0.9): infrastructure and P0s that already happened

| Item | Risks covered | Issue |
| --- | --- | --- |
| Fix the four known bugs with regression tests | P2, B2, C11, P11 | #173, #174, #184, #197 |
| Determinism rules; nextest; seeds in `interleave.rs` | K10, P11 | #182 |
| CI changes §8.6 steps 1–2: cancel superseded runs, select by change, aggregate check, merge macOS jobs, remove duplicates | Feedback speed for every risk | #203 |
| ST (fmt, clippy, deny, MSRV); minimum version matrix | All; MX | #204 |
| E2E harness + residue checks + S2 | A3–A5, A8, B2, B3, B10 | #175, #186 |
| S9 installation smoke test | B1; acceptance of #195, #196 | #193 |
| Unit tests for rutis-host configuration and assembly; snapshots of common errors | B5, B6, A6, A7 | #205 |

### 9.2 Step two (0.10): P0s across processes and platforms

| Item | Risks covered | Issue |
| --- | --- | --- |
| S3, S4, S5 loopback | C3, G2, G3, G5, E11 | #187, #188, #189 |
| `fault` wired into sessions and links; negative TLS; missing protocol cases; SDK parity | C4, E1, E6, E8, E9, P3, P6, P8, F3 | #178 |
| Windows columns; missing cells | C10 | #176 |
| Cross-language value round-trip property tests | P7, C5, P4 | part of #178 |
| Test tools vs real runtimes | A1 | #206 |
| Persisted samples | J3 | #207 |
| Core guarantees completed | K3–K8 | #177 |
| Guarantee register | All | #181 |
| TLA+ Spec 1 | P10, C6 | #208 |

### 9.3 Step three (0.11): long runs, fuzzing, concurrency models, undecided designs

| Item | Risks covered | Issue |
| --- | --- | --- |
| CI changes §8.6 steps 3–4: split dylib jobs on Linux; CI measurement | Critical path; Q12.9 | #203 |
| Three FZ targets; three SH models | P2, E7, K2 | #179 |
| Host-level soak (S8); S6, S7 | L1, L3, D3, F8, D5 | #192, #190, #191 |
| Previous release's runtime vs current host | J2, E13 | #209 |
| Nightly baseline with the latest Cordis plugins | F4 | part of #190 |
| TLA+ Spec 2 | E4, E5 | #210 |
| Bun columns | Bun variant of C10 | #194 (after Spec 1) |

### 9.4 Waiting for design

| Item | Prerequisite |
| --- | --- |
| All controls of scenario H (S1 #185) | The design for plugin services becoming agent tools; decisions on isolating agent-written plugins and their permissions |
| Resource limits on references and calls | A design first (raised in #178) |

---

## 10. Assumptions to confirm and open questions

1. **Scale assumptions** (§2.2): affect the ratings of D11, L1, L2 and the length and size of soaks.
2. **Whether `rutis-host run` watches its configuration file**: affects how B4 is verified.
3. ~~Service contract test tool (G1)~~: decided (2026-10-11). Whether implementations of one service return the same data is up to the service's authors; rutis provides no contract tool (standard Q6.9.4). rutis only guarantees that the rules for converting values across languages hold in every runtime and test tool (P7, A1).
4. **Isolation and permissions for agent-written plugins (H1, H2)**: design philosophy §8 "permissions" lists this as open.
5. **Adopting TLA+**: the first spec, including learning, takes about one to two weeks; staffing to confirm.

---

## Appendix: specific methods evaluated and not adopted for now

Not adopted after evaluation under standard Q11.3: no matching P0 / P1 risk in §4, or duplicates a chosen method.

| Method | Why not | When to reconsider |
| --- | --- | --- |
| Coverage gates | Coverage shows code ran, not that results were checked; a gate invites tests written for the number | Not reconsidered |
| Mutation testing as a gate | Runs take hours; a gate needs someone to keep handling surviving mutants | Run once by hand as a review aid after major core changes |
| proptest state machine for the core | Overlaps heavily with contract tests, parity and `interleave.rs`; the reference model can itself be wrong | When core semantics change substantially again |
| Exhaustive loom | shuttle already covers the three chosen places; loom needs larger code changes | When shuttle finds a problem it cannot reproduce reliably |
| SDKs running the channel contract themselves; cross-language frame reference samples | Overlap with the Rust-driven conformance suites; the malformed-frame part is in P3 | When a fourth language SDK arrives |
| `cargo public-api` | Duplicates `cargo semver-checks` | — |
| Miri, ASan, TSan | `unsafe` is concentrated in dylib loading and process spawning (FFI and system calls), which Miri cannot run; tokio code does not run under Miri | When dylib has memory problems |
| Performance regression thresholds in CI | CI noise exceeds 15% and would alarm repeatedly; performance is not a guarantee yet | With a fixed benchmark machine |
| Full disk, fd limit, OOM injection | The matching risks in §4 are all low likelihood | When users report such problems |
| Extracting the register from docs automatically, enforced in CI | Regex extraction produces many false matches; maintaining the script costs more than it saves | When the register exceeds 300 entries |
| A TOML DSL for end-to-end scenarios | Few scenarios; Rust is simpler | Over 15 scenarios, or when non-Rust contributors need to write them |
| TLA+ for the core state machine | Single process; past problems were all in the implementation | — |
| TLA+ trace validation | Too costly | When spec and implementation diverge repeatedly |
