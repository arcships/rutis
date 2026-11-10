# TLA+ in a multi-language plugin runtime: from two models to nine

## Project background

rutis is a multi-language plugin runtime. A Rust host loads rows, provides services and runs an event bus. Node, Bun, Python, Go, Swift and Kotlin runtimes execute plugins. Plugins register listeners, emit events, call other runtimes synchronously, connect to other machines through a remote lease, and host rows on nodes.

These interactions have three properties:

- many participants: the host, several runtimes, remote controllers, nodes;
- many interleavings: calls, replies, cancels, disconnects, reconnects, unloads and crashes can arrive in any order;
- tests cannot exhaust them: unit and integration tests cover constructed schedules.

TLA+ entered the project to address these three problems.

## First use: a deadlock in cross-runtime synchronous calls

In October 2026 the project was preparing to add a third runtime. A known risk existed: Node runtimes are non-reentrant, Python runtimes are reentrant, and two Node runtimes calling each other synchronously can deadlock in a specific interleaving.

The deadlock was hard to trigger reliably in tests. The team wrote a TLA+ model:

- participants: the Rust host as relay, two to three runtimes, each marked reentrant or non-reentrant;
- actions: start a synchronous call, the host forwards it and rewrites the call chain, the callee handles incoming calls, returns, returns `SyncWaitCycle`;
- checks: no deadlock; a reverse call returns to its origin; `SyncWaitCycle` only when the wait really forms a cycle.

The model compared two policies:

- Policy A: only leaf runtimes may be non-reentrant; Rust rejects synchronous calls between two non-reentrant runtimes.
- Policy B: Rust tracks cross-session waits and returns `SyncWaitCycle` when a cycle forms.

TLC found deadlocks in the current behaviour and under policy A. Policy B had no deadlock and no false `SyncWaitCycle` at the checked scale. The design chose B, the code changed the next day, and the counterexamples became tests in `rpc_callbacks`.

That use established a working pattern: **model one decision, let TLC produce a counterexample, write the decision back into the design, add code tests.**

## Second use: remote lease takeover

A week later the remote design needed a takeover order: authenticate, close the old session, clean the old lease, answer the new hello. Registration generations, revocation during a handshake, and late results that must not be delivered lived only in code, with limited test coverage.

The model had two controllers, one remote runtime and a revocable identity. TLC explored dial, authenticate, hello, disconnect, backoff reconnect, takeover, cleanup, revocation, late results and old-session commands. It found that a handshake revoked midway could still establish a session. The fix landed in the next commit.

After that, the TLA models stayed in the repository as part of the design.

## Evolution

Over the following months the repository structure changed, the runtime set grew from three languages to seven, and the rules for remote leases, events, row lifecycle and call lifecycle kept growing. The repository had two TLA models: cross-runtime sync and remote lease.

On 2026-11-08 the project established a protocol and formal-verification baseline. The baseline requires model checking for multi-party concurrent protocols at design time and model updates when the design changes. It also recorded that events, row lifecycle, call lifecycle and node-hosted rows had no models, and that the two existing models had not followed later changes to the wait graph, the seven runtimes and remote takeover.

The 0.22 scope followed: expand TLA from two models to nine.

## This round

The nine models cover:

- event close admission;
- the unified event protocol, split into Host, Table, Emit and Link;
- cross-runtime sync and the deadlock policy;
- remote lease takeover, reconnect and app hosting;
- row lifecycle;
- call lifecycle;
- node-hosted rows.

About 215 TLC configurations. Each directory has `expected.txt`, `run.sh` and a README. The README records the design it maps to, simplifications, properties, results and uncovered parts.

The process had five steps:

1. Find promises in design documents and issues.
2. Write promises as safety and liveness properties.
3. Choose participants, messages, failure model and bounds.
4. Write healthy configurations and mutation configurations.
5. Run TLC and record results in `expected.txt`, `run.sh`, the README and the tracker.

Several concrete problems appeared.

**State space.** `table-healthy` once took 10 minutes and 9 GB. `policy-b-posts` had 14 million states and pushed a 4-core CI job past its 30-minute limit. We reduced the bounds: two runtimes by one row, `MaxDispatch=1`, `SYMMETRY`, and `MaxDepth` from 3 to 2. The state count fell to 1.15 million and CI took 7m25s.

**Model mistakes.** In the node-row model, a failure action wrote one variable both as an assignment and in `UNCHANGED`, so the action was always false. The failure path was never explored while the configuration passed. A row-lifecycle rewrite also put `cleanCount` into an `UNCHANGED` it did not belong in. We added reachability checks: every new action must be shown to happen; every passing configuration needs a mutation that fails the same property.

**Configurations describing old code.** `bail-policy-b` in the cross-runtime model kept showing a deadlock. Printing the real input showed the current compliant implementation already puts the dispatch id on the listener's call chain, equivalent to the post-fix configuration. The model was fine; the configuration described pre-fix behaviour. We almost changed code because of it.

**Changing code from a model broke the implementation.** The async model said recording an asynchronous call on a marked chain as a hop causes false refusals. Changing the code to count only `sync` made the object-relay synchronous-cycle test hang. The call frame only has `sync`, which cannot distinguish "fire and forget" from "awaited later". The code change was reverted and the design gap became an issue.

**Real bugs, suspected bugs and design gaps.** This round confirmed and fixed three real bugs: old-generation reinsertion in the event link, the Kotlin app start/stop race, and the Node/Bun kill firing before the cleanup deadline. Three suspected issues became issues: #1101 row unload before load, #1102 Kotlin heartbeat from accept, #1103 host single-worker wait-graph blind spot. Three design gaps became issues: #1098 async wait signal, #1099 cancel-table rules, #1100 node-row key identity.

## The value of TLA+

Direct results:

- three real bugs found and fixed;
- the `bail` implementation confirmed correct, avoiding a wrong change;
- three design gaps and three suspected implementation problems turned into trackable issues;
- a reusable asset of nine models, about 215 configurations, READMEs, `expected.txt`, `run.sh`, CI and a tracker;
- design review has an executable specification, and historical bugs have pre-fix configurations.

TLA+ is a design tool. It shows "no counterexample in this bounded model". Code correctness needs trace validation, shuttle, conformance tests and ordinary tests.

TLA+ fits concurrent, multi-party protocols with interleavings, failures and retries. Deterministic logic, timing, scheduling, memory ordering, encoding and performance do not fit TLA+.

## Environment and setup cost

**Software.** Java 21 and `tla2tools.jar` v1.7.4 with a pinned sha256. No other dependencies.

**Local runs.** `TLA2TOOLS=/tmp/tla2tools.jar specs/<dir>/run.sh`. On 32 cores the full set takes a few minutes.

**CI.** A `specs` matrix of seven jobs. Each job installs Java 21, downloads the jar and runs that directory's `run.sh`. Path filtering triggers it only when `specs/**` changes, or on full / nightly runs. Ordinary pull requests do not run it.

**CI time.** `cross-runtime-sync` 7m25s, `remote-lease` 3m7s, `event-close` 1m31s, `events` 1m20s, `row-lifecycle` 1m13s, `call-lifecycle` 1m9s, `peer-rows` 45s. Each job has a 30-minute limit.

**People.** Six agents wrote models in parallel; the main thread reviewed, integrated, ran the full suites and changed CI. The main cost was model writing, state-space debugging and specification review. LLMs speed up the first draft; humans still review the specification and bounds.

**Maintenance.** Models move with design changes. Each directory's README records properties, simplifications, results and uncovered parts. New configurations go into `expected.txt` and `run.sh`.

## Lessons for an external audience

1. Start with one concrete concurrency decision. The first model answered only "do policy A and policy B deadlock?", a small scope with a direct conclusion that changed code the next day.
2. Every model needs a counterexample. A healthy configuration passing shows only that this bound produced no counterexample; a mutation that fails shows the model can catch the problem.
3. Set bounds early. State space gets out of control and CI times out. Reduce participants, repetitions and depth before talking about coverage.
4. Models make mistakes. Always-false actions, `UNCHANGED` conflicts and configurations describing old code all produce meaningless passes. Reachability checks and human review are required.
5. Keep model conclusions and code behaviour separate. The model may assume boldly; code changes must return to the implementation and its tests. Changing code directly from a model can break a real scenario.
6. Count coverage by promises. A promise is covered when it has a property, a healthy configuration, a counterexample configuration and a recorded result. Do not chase a percentage of code paths.
7. Put TLA+ in the design stage. Code correctness, timing, performance and scheduling belong to other tools.
