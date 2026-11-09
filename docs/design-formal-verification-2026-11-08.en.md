# Redesigning formal verification: prove the new architecture's behaviour first

[中文](design-formal-verification-2026-11-08.md)

Status: draft, not implemented. Date: 2026-11-08.
Issue: [quality tracker #183](https://github.com/arcships/rutis/issues/183), step four; implementation issues [#635](https://github.com/arcships/rutis/issues/635), [#613](https://github.com/arcships/rutis/issues/613), [#622](https://github.com/arcships/rutis/issues/622), [#624](https://github.com/arcships/rutis/issues/624), [#630](https://github.com/arcships/rutis/issues/630), [#631](https://github.com/arcships/rutis/issues/631), [#634](https://github.com/arcships/rutis/issues/634), [#639](https://github.com/arcships/rutis/issues/639). Based on: the [quality standard](quality-standard.en.md) §§3, 8, 9.4 and 11; [quality status](quality-status.en.md).

This document resets the scope, order and completion criteria for formal verification. It does not mean translating all code into mathematics. It answers a more practical question: **what evidence proves each behaviour rutis promises, at the design level, in the implementation, in every language and in a real deployment?**

## 1. What needs fixing now

rutis has recently changed events, cross-runtime calls, remote leases, node hosting, services and lifecycles in succession. A model written for the old architecture cannot be treated as proof of the new one:

- rules for session frames, events, remote takeover and row lifecycles are scattered across old designs, some of which are already superseded;
- the existing TLA+ models cover cross-runtime synchronous calls and remote leases; the existing shuttle model covers only three historical races;
- step four of #183 has been reorganised into eight connected issues, rather than the old 0.11–0.14 release sequence;
- a rule may have been checked only by Rust tests, with no shared case for other runtimes; or a model may prove a design while nothing proves that the implementation follows it;
- timing, platform differences, user-visible text and operating-system behaviour are not what TLA+ is good at, but they still need verification.

The first task is therefore to rebuild the list of current rules, then choose the right evidence for each question. Every model from an old design must be compared with the current specification and marked **current, superseded or explicitly not guaranteed** before it is reused.

## 2. Decisions

### 2.1 Move formal verification before behaviour is final

For designs involving multiple participants, message interleavings, delays, takeover or close ordering, a small model and its key invariants are checked before the design is finalised and implemented. This is how the quality standard's Q9.1.2 is applied; formal verification is no longer a post-feature quality add-on.

The #1025 event-admission optimisation follows the same order: first state and check when the close marker takes effect for new events and how already admitted events drain; only then implement the marker optimisation. A performance benchmark cannot replace a semantic check.

### 2.2 Organise by observable behaviour, not by tools

Tools are means, not the plan. Developers care whether messages handshake correctly, whether a plugin starts before its dependencies, what happens after a disconnect and whether an error is understandable. Coverage is split into four layers:

| Layer | The plain-language question | Main work |
| --- | --- | --- |
| A. Protocol foundations | What exactly do the two ends exchange? What happens on an error, cancellation or an oversized message? | #635: one current session specification for frames, handshakes, references, cancellation, errors, limits and malformed input |
| B. Concurrency and lifecycle | Can simultaneous start, call and close deadlock, leak cleanup or run something twice? | #622, #630, #613: controlled scheduling in the core and runtimes, plus event and call models |
| C. Cross-process deployment | During a disconnect, takeover, reconnect or node replacement, can old state affect new state? | #613, #624: remote leases, hosted nodes, access points, trace validation and cross-implementation conformance |
| D. Inputs and long-term reliability | Does it remain correct for arbitrary values, bad frames, bad configuration and long runs? | #631, #639: property and fuzz testing, Kani/Miri, virtual clocks, conformance cases and ordinary tests |

#634 is the shared traceability and close-out work for these four layers. It is not a fifth feature and does not represent another model.

The following alternatives are explicitly rejected: scheduling separate tool projects for TLA+/shuttle/property tests, because the same rule would be scattered and completion would be unclear; one “universal model” for every implementation, because it would hide language-scheduler and implementation differences; a code-coverage gate, because executing code does not assert its behaviour; and handing every rule to formal models, because platforms, output, timing and long-run resource trends need other evidence.

### 2.3 Every rule must close the loop from rule to evidence

Process every core promise or contract behaviour as follows:

1. **Find one current source**: state it clearly in the protocol specification, design or user documentation; mark an old source if it is still referenced.
2. **Write the outcome in plain language**: cover success, failure, scope and recovery; “eventually consistent” or “no deadlock” alone is not enough.
3. **List counterexamples**: name the interleaving or input that would violate it, such as “a late command from the old session is applied to the new session” or “a new callback is admitted after the close marker was committed”.
4. **Choose a method that answers the question**: TLA+ for design questions, shuttle/controlled scheduling for implementation interleavings, conformance suites for implementation differences, property/fuzz testing for input spaces, virtual clocks for timing, and black-box tests for real assembly.
5. **Connect evidence back to code**: tests, models, traces and implementation files cite the guarantee number; a passing model is not a passing implementation.
6. **Deliberately introduce a mistake**: mutate a model, reference implementation or test and confirm that the verification catches it, rather than merely producing a successful run.
7. **Run it for every applicable implementation and platform**: list Rust, Node, Python, Go, Bun, Swift and Kotlin only where their capability and platform are actually supported; give a reason for every inapplicable cell.

Core promises must also meet the quality standard's Q8.2 and Q8.3: evidence at the rule, composition and environment levels, with at least two independent verification methods.

## 3. Concrete coverage

The following describes the questions coverage must answer, not a requirement that one tool handle every item.

### 3.1 Layer A: protocol foundations (#635)

First establish one current session protocol specification, replacing repeated and conflicting descriptions scattered through mount, network, value-tag and runtime documents. It must cover at least:

- handshake, protocol version, capability declarations and refusal on mismatch;
- row and service load/unload, state changes and unknown operations;
- calls, returns and errors (`code` and `data`), the synchronous-wait marker and forwarded `path`;
- object/value reference grant, release, reference counts and session close;
- cancellation, timeouts where promised, and hop-by-hop propagation;
- event emission, posting, import/export and cancellation boundaries;
- message size, nesting depth, field types, unknown fields, duplicate/out-of-order messages and nonexistent references;
- the failure scope of each error: this call only, the session, one row, or the runtime;
- wire-format and capability evolution rules.

The acceptance criterion is not merely having a document. The same protocol cases must produce the same category of result in every applicable implementation. #635 must also reproduce and resolve the known object-reference question: when an object is sent to a runtime that does not support `objects`, the call should fail according to the specification without accidentally ending the entire session; if the current design deliberately chooses the latter, that scope must be written into the specification and its error.

### 3.2 Layer B: core, events and concurrency (#622, #613)

Cover the complete lifecycle of a plugin from creation to end:

- do not start before dependencies are ready; stop when a dependency disappears; recover according to the written rule when it returns;
- run one cleanup at most once under concurrent close, repeated close and user-callback failure;
- close roots, subtrees, instances and rows in the specified order; define how closing objects reject new work;
- event-listener admission, snapshots, cancellation, ordering and delivery after a panic;
- the #1025 close protocol: under the admission lock, commit the close marker for the complete fixed subtree before publishing the existing `closing` states; after the commit no new callback may be admitted; an admitted callback may begin after the commit but must finish or leave under the existing cancellation rule; models and implementation tests must cover `FiberInner::lineage_closed` publication order, complete fixed-subtree marking under `Shared.admission`, the linearization boundary between `closing` and marker commit, concurrent parent/child/sibling close, equivalence after `Entry::live()` stops walking ancestors, the race between `CallFlight::begin(owner)` and the final `live()` check in `Entry::start()`, and unchanged sync/async, cancellation, shutdown and cross-process behaviour;
- service replacement, event import/export, observers and `dispatch` chain boundaries;
- whether synchronous calls, asynchronous posts, reverse callbacks and cancellation can form a wait cycle;
- resource release: tasks, references, listeners and internal records return to baseline after close.

#622 uses shuttle and reference models to explore real Rust implementation interleavings, adding models for spawn, locks, clock substitutes, dispose admission, in-flight unload counts, root/subtree close, dependency eviction, emit chains and observers, and strengthening the three historical race models. #613's TLA+ models describe participant-to-participant rules without forcing single-process implementation details into a protocol model.

### 3.3 Layer B: controlled scheduling in language runtimes (#630)

Model checking cannot replace checking each runtime's scheduler. For every supported runtime, check that pausing, inserting and delaying frames still obeys the same protocol:

- Node/Bun: frame pumping, synchronous waits, reentrancy, event posting and cancellation;
- Python: event-loop scheduling, cancellation propagation, delayed callbacks and frames during close;
- Go: writes after close, reference release, concurrent calls and runtime stop;
- Swift: Task cancellation, strict concurrency and data races, including TSan;
- Kotlin: coroutines, cancelled emits, deep calls and scheduler interleavings, including Fray/Lincheck;
- each language's call ids, write atomicity and snapshot/unload races, with unsupported capabilities recorded as explicitly not guaranteed.

This verifies that an implementation follows the protocol. It does not prove that the language runtime or user plugin has no bugs.

### 3.4 Layer C: cross-runtime, remote leases and hosted nodes (#613, #624)

Cover the cases where “old state affects new state” is most likely:

- forwarding cross-runtime synchronous calls, rewriting `path`, mixed reentrant/non-reentrant runtimes and `SyncWaitCycle`;
- event delivery across a boundary: entry boundaries, ordering, cancellation and callback returns;
- process crash, session disconnect, half-open connection, delay and reconnect, with failure limited to the written scope;
- remote lease establishment, takeover, old-session close, old-row cleanup and the ordering of the new session's hello response;
- concurrent handshakes and replacement for one id: the new session does not register before the old session is fully unloaded, and late commands from the old session cannot affect the new one;
- dial/listen stop, reconnect backoff, reset after a stable connection and handshake deadlines;
- application-hosted rows, services imported from nodes, capability declarations, service announcements and scopes; service announcements retain their current Cordis-compatible semantics, and this design does not add #1040's policy refusal;
- old processes, connections, leases and registry entries after runtime restart, takeover and migration.

#613's TLA+ models answer “is every design interleaving safe?”; #624's trace validation answers “does a real execution belong to the model's allowed traces?”; the conformance suites answer “do different languages implement the same rule?” None replaces the others.

### 3.5 Layer D: inputs, values, resources and long runs (#631, #639)

Cover input spaces that models and hand-written cases cannot enumerate:

- property tests for value tags, serde, `path`, handshakes, error graphs and frame sequences;
- cross-language value round-trips, explicit rejection of unrepresentable values, and no reference-count leak for arbitrary grant/release sequences;
- fuzzing session frames, WebSocket/RFC 6455, line framing, configuration, patch/edit merges and plugin manifests;
- property tests for core registries, decision tables, listener dispatch, event export/import, service announcements, scopes and backoff;
- Miri for undefined-behaviour-sensitive Rust paths and Kani for finite-state core properties where it fits; neither is advertised as proof of the whole system;
- deterministic virtual-clock checks: 30-second local-runtime greeting deadline, 10-second network/access-point handshake deadline, 30-second wait after refusal, and backoff reset after 60 seconds of stable connection;
- the 161 missing conformance cases and the 18 ordinary tests in #639's tracker, where formal methods cannot reach them;
- process, fd, task, thread, memory and internal-record trends after repeated create/close/reload/reconnect.

Use property testing for explicit properties, fuzzing for malformed inputs, virtual clocks for timing, and ordinary/black-box tests for operating-system behaviour and user-visible output. “Not suitable for a formal model” is not a reason to remove something from coverage.

## 4. Coverage logic: what each kind of evidence proves

| Evidence | In plain language | What it cannot prove |
| --- | --- | --- |
| Current specification and guarantee registry | We first say exactly what we promise | The document itself does not prove the implementation |
| TLA+ design model (#613) | Shrink the participants, enumerate message interleavings and check deadlocks/invariants | That the implementation follows it; scales outside the model; single-process details |
| Shuttle/controlled scheduling (#622, #630) | Run real or close-to-real code under different schedules to find races, lost wakeups and deadlocks | Every language, every input or an unbounded state space |
| Trace validation and conformance suites (#624, #639) | Record a real execution and check it is allowed by the model; run the same cases in every implementation | Paths never exercised; rules absent from the model |
| Property, fuzz, Kani/Miri and ordinary/black-box/platform tests (#631, #639) | Use many values, bad inputs, boundaries, real processes and platforms for the remaining risks | They cannot replace the protocol model or prove every concurrent interleaving |

The fixed chain is therefore:

```text
current rules
  → design model checking (is the rule itself coherent?)
  → controlled implementation scheduling (does the code survive interleavings?)
  → trace validation (does the implementation follow an allowed path?)
  → cross-language conformance (does each implementation obey the same rule?)
  → property/fuzz/ordinary/black-box/platform checks (inputs, faults, environments and long runs)
```

A failure at one step cannot be hidden by a pass at another. For example, a passing TLA+ model with a failing trace means the implementation diverged from the design; conformance passing while the model has a deadlock means everyone consistently implemented the wrong rule; fuzzing that never crashes does not prove that reference counts cannot leak.

## 5. The tracker and status rules

#634 is the shared tracker. At minimum, every rule records:

| Field | Contents |
| --- | --- |
| Current source | Current specification/design/user-document section; whether an old source is superseded |
| Plain-language promise | Observable success, failure, scope and recovery |
| Level | Core promise, contract behaviour or explicitly not guaranteed |
| Applicability | Rust, Node, Python, Go, Bun, Swift, Kotlin; transport; platform |
| Evidence | Model, test, trace, property/fuzz, black box or clock; exact path/case name |
| Mutation or counterexample | A bug the verification caught, or a deliberately injected error |
| Status | Verified, partly verified, not verified or explicitly not guaranteed |
| Gap | Next issue; never call an uncovered rule “passing” |

Keep status meanings strict: a passing model marks only the model as passing; a rule is verified only with implementation evidence. Record explicitly-not-guaranteed behaviour in one place so users do not mistake “not promised” for an intermittently valid promise.

Release gates:

1. no model sourced from a superseded design is treated as current proof;
2. every core promise links to a current source, implementation evidence and its applicable matrix;
3. every core promise has rule-, composition- and environment-level evidence, with at least two independent methods;
4. every applicable implementation has conformance cases, and every skipped cell has a reason;
5. timing promises have deterministic virtual-clock checks;
6. models and tests are non-vacuous: deliberate mistakes are caught;
7. every added or changed rule updates the specification, tracker and tests together;
8. under Q9.4.4, a release has zero unverified core promises and no more unverified contract behaviours than the previous release.

## 6. Implementation order and dependencies

### Step 0: rebuild the baseline (#635, #634)

- establish the unified session protocol specification;
- clean up old document references and mark rules current, superseded or explicitly not guaranteed;
- rebuild the guarantee registry and tracker rule list;
- reconcile the existing `specs/cross-runtime-sync`, `specs/remote-lease` and `rutis` shuttle models against it;
- turn known questions, including the object-reference/session-termination question, into reproducible cases.

Until this step is complete, “passing” can only mean that an old model passes, not that the new architecture is verified.

### Step 1: verify high-risk semantics first (#613, #622, #630)

These proceed in parallel while sharing Step 0's rules:

- #613: TLA+ models for events, cross-runtime calls, row and call lifecycles, and hosted node rows;
- #622: controlled scheduling models for the Rust core, including the #1025 close/admission race;
- #630: controlled scheduling and static checks for applicable Node, Python, Go, Swift, Kotlin, Bun and other runtimes.

When a model finds a design problem, change the specification first; do not use an implementation workaround to hide the counterexample.

### Step 2: connect the design to real executions (#624)

- establish one trace-event format and the allowed state transitions;
- add trace validation to leases, host-gone, sync-cycles and AccessEvent scenarios;
- add Swift/Kotlin and applicable Go/Bun conformance columns;
- check that the old lease is fully cleaned before the new hello is answered, rather than checking only that a new connection eventually succeeds.

### Step 3: cover input spaces and what models cannot reach (#631, #639)

- add property testing, fuzzing, Kani and Miri;
- use virtual clocks for every timing promise;
- complete the missing conformance and ordinary tests in the tracker;
- turn every found counterexample into a stable regression test and link it from the tracker.

### Step 4: continuous and release close-out (#634)

- on a pull request, run P0 checks affected by the change;
- run checks according to their CI triggers and release gates: affected P0 checks on pull requests, path-triggered model jobs and scheduled nightly/stress checks where configured, and the complete existing model set on Monday's weekly run; before release, produce the required model, matrix, fault, fuzz, soak and real-network report;
- maintainers decide “verified” from the tracker and quality standard, not from code coverage or the fact that tests ran;
- record model bounds, skipped platforms, explicitly-not-guaranteed behaviour and remaining risks in quality status.

## 7. What developers see

| Role | On success | On failure (verbatim) | Record and fix |
| --- | --- | --- | --- |
| Plugin authors | Existing APIs and event behaviour stay unchanged | A closed Rust API: `root has been shut down`; the error kind remains `ErrorKind::Closed`, with no new `code`/`data` | This is the existing runtime error, not a model diagnostic; fix it through the existing close/dependency path |
| Host authors and deployers | Existing commands, status lines, exit codes and service-announcement behaviour stay unchanged | This design adds no host error line, exit code or status field | No migration; contributors fix verification failures in CI |
| Runtime authors and contributors | This design adds no runtime or verification-tool output; existing scripts, models and shuttle output remain unchanged | This design adds no failure output, exit code or diagnostic format; verification diagnostics are not runtime output | Keep the existing complete log/schedule for replay; fix the rule, model or implementation |

### Plugin authors

This design adds no plugin API, configuration field, protocol message or runtime error. Existing successful paths and failure output stay unchanged: a closed Rust API still returns `ErrorKind::Closed`, whose current human-readable text is `root has been shut down`; this design adds no `code` or `data` and does not present verification diagnostics as runtime errors. The only visible change is that pull requests touching these behaviours may run deterministic model, concurrency, property or conformance checks.

### Host authors and deployers

This design changes no `rutis-host` command, status line, exit code, service-announcement semantics, lease semantics or migration format. It does not add #1040's announcement policy, an allowlist or a `policy-refused` state.

### Runtime authors and contributors

New rules require the same conformance cases in every applicable language and an explanation for every unsupported language/platform cell. Existing TLA+, shuttle and other verification tools keep their current commands, exit codes, success output and failure output; this design adds no verification tool and therefore makes no new diagnostic-text promise. These diagnostics are not runtime output seen by plugin users.

### Concept budget

For plugin authors, host authors and deployers: zero new concepts. For contributors: six work concepts—rule, model, trace, conformance case, property test and fuzz test. None enters the runtime API.

## 8. Compatibility and migration

This design phase changes no source code, public API, wire protocol, configuration, persisted format, error code, service-announcement behaviour or release version, so users have no migration step.

If implementation shows that current behaviour disagrees with the new model, update this design and the relevant current specification first. Then decide whether to fix the implementation, make the behaviour explicitly not guaranteed or record an incompatible change. Do not change code first and then edit the model to “prove” it.

#1040 remains deferred: this design verifies only the existing Cordis-compatible dynamic service-announcement behaviour. It does not design host refusal for undeclared services or move the trust boundary for untrusted plugins into this layer.

## 9. Risks and explicit boundaries

### 9.1 Main risks

| Risk | Control |
| --- | --- |
| A model still references superseded documentation | Step 0 unifies the specification and marks every source |
| The model passes but the implementation diverges | Trace validation, implementation tests and cross-language conformance |
| Only Rust is tested | The same conformance cases and support matrix |
| The model is too small or a test is vacuous | Publish bounds; require mutations to be caught; add representative configurations |
| Timing tests depend on machine speed | Virtual clocks; real waits only supplement the environment layer |
| Coverage grows without limit | Every method must map to a risk; remove duplicate checks |
| Explicit non-guarantees are mistaken for guarantees | One registry and regression checks that pin “not provided” |

### 9.2 What is not claimed

- This does not prove arbitrary user-plugin business logic, and out-of-process execution is not treated as a sandbox for untrusted code;
- it does not enumerate infinitely many plugins, nodes, calls or unbounded time; models publish finite bounds and environment tests cover scale risks;
- it does not replace platform, installation, real-network, signal or soak testing;
- it does not register implementation details as contracts;
- it does not restore deleted or superseded mechanisms as current promises;
- it does not add a new security policy or trust boundary for #1040.

## 10. Definition of done

This design being done does not mean every verification is already complete. The design phase is complete when:

- coverage, layering, dependencies and boundaries are decided;
- every kind of rule has a suitable evidence method and a stated limitation;
- existing models have been reconciled with the new architecture;
- the four layers and eight issues have clear responsibility boundaries;
- “What developers see”, errors, compatibility and migration conclusions are written down.

Implementation is complete when every applicable rule in the tracker has a current source, implementation evidence, applicability matrix and status; the key models, traces, conformance, property/fuzz, virtual-clock, platform and long-run checks pass; and the release gate meets Q13.1.

## Related files

- [Quality standard](quality-standard.en.md) §§3, 8, 9.4 and 11
- [Quality status](quality-status.en.md)
- [Guarantee registry](guarantees.en.md)
- [Session trace contract](formal-trace.en.md)
- [Design-model directory](../specs/README.md)
- [Cross-runtime synchronous-call model](../specs/cross-runtime-sync/README.md)
- [Remote-lease model](../specs/remote-lease/README.md)
- [Event close/admission design](design-event-close-admission-2026-11-08.en.md)
