# From the day TLA+ was introduced: a 0.22 landing record

## 1. The day it started

On 2026-11-09 the task began with reading remote `main`, the formal-verification design documents and the existing `specs/`. The scope then became 0.22: merge 0.23 into it and land the TLA+ models as one part of the work.

## 2. Why it was introduced

Four pieces of background:

- The core 0.22 change, #1025, is event close admission. It involves subtree close, marker publication, listener admission and cleanup, all with concurrent interleavings.
- Quality standard Q9.1.2 / Q11.2 requires model checking for multi-party concurrent protocols at design time, and model updates when the design changes.
- The formal-verification design splits the protocol into events, cross-runtime calls, remote leases, row lifecycle, call lifecycle and node-hosted rows.
- Issue #613 lists the modules, properties and a set of historical bugs. At that point the repository had only two old models, `cross-runtime-sync` and `remote-lease`.

Past reviews and intermittent failures had many concurrency problems. Tests only covered constructed schedules and could not exhaust interleavings. The goals were:

- exhaust participant, message, disconnect, retry and crash interleavings at the design level;
- write design promises as properties and search for counterexamples;
- turn historical bugs into pre-fix configurations that guard against regression;
- give implementation and review an executable specification.

## 3. The landing process

Three batches:

- Batch one: first versions of `event-close`, `row-lifecycle` and `call-lifecycle`, made to run.
- Batch two: shuttle, trace validation and property tests in parallel; TLA expanded to the four `events` modules and `peer-rows`, with `row` / `call` / `lease` / `sync` filled in.
- Batch three: six directories expanded to the design scope, about 215 configurations. Every new configuration was run; all seven CI `specs` jobs passed.

## 4. Incidents

**State space.** `table-healthy` once took 10 minutes and 9 GB. `policy-b-posts` had 14 million states and pushed a 4-core CI job past its 30-minute limit. We reduced `MaxDepth` from 3 to 2, added `SYMMETRY`, and used two runtimes by one row; the state count fell to 1.15 million and CI took 7m25s.

**Model mistakes.** In `peer-rows`, `PFailed` wrote one variable both as an assignment and in `UNCHANGED`, so the action was always false. The failure path was never explored while the configuration still passed. A rewrite of `row-lifecycle` also put `cleanCount` into an `UNCHANGED` it did not belong in. We added reachability checks and mutation configurations.

**Configurations describing old code.** `bail-policy-b` kept showing a deadlock. Printing the real `forward` input showed the current compliant runtime already puts the dispatch id on the listener's `path`, equivalent to `bail-recorded`. `bail-policy-b` describes the pre-fix behaviour. We almost changed `waits.rs` for it.

**Changing code from a model broke the implementation.** `async-policy-b` says an asynchronous call on a marked chain recorded as a hop causes false refusals. Changing `waits.rs` to count only `sync` made the object-relay test `a_synchronous_cycle_through_objects_is_refused` hang. The frame only has `sync`, and cannot distinguish "fire and forget" from "awaited later". The code change was reverted and #1098 was opened.

**Real bugs.** The old-generation reinsertion in `events::link::accept`, the Kotlin `RutisApp` start/stop race, and the Node/Bun 5-second kill firing before the cleanup deadline. All three are fixed.

**Suspected bugs.** A peer-row `unload` before `load` may leave an orphan, the Kotlin heartbeat starts at accept, and the host single-worker wait-graph blind spot. Issues #1101–#1103.

**Design gaps.** The async "will await" signal, cancel-table cleanup and cancel replies, and the peer-row key identity. Issues #1098–#1100.

## 5. The value of TLA+

Results from this work:

- three real bugs found and fixed;
- the `bail` implementation confirmed correct, avoiding one wrong change;
- three design gaps and three suspected implementation problems turned into trackable issues;
- a reusable asset of nine models, about 215 configurations, `expected.txt` / `run.sh` / READMEs / CI / tracker;
- design review now has an executable specification, and historical bugs have pre-fix configurations.

TLA+ is a design tool. It shows "no counterexample in this bounded model". Code correctness needs trace validation, shuttle, conformance tests and ordinary tests.

TLA+ fits concurrent, multi-party protocols with interleavings, failures and retries. Deterministic logic, timing, scheduling, memory ordering, encoding and performance do not fit TLA+.

## 6. Environment and setup cost

**Software.** Java 21 and `tla2tools.jar` v1.7.4 with a pinned sha256. No other dependencies.

**Local runs.** `TLA2TOOLS=/tmp/tla2tools.jar specs/<dir>/run.sh`. On 32 cores the full set takes a few minutes.

**CI.** A `specs` matrix of seven jobs. Each job installs Java 21, downloads the jar and runs that directory's `run.sh`. Path filtering triggers it only when `specs/**` changes, or on full / nightly runs. Ordinary pull requests do not run it.

**CI time.** `cross-runtime-sync` 7m25s, `remote-lease` 3m7s, `event-close` 1m31s, `events` 1m20s, `row-lifecycle` 1m13s, `call-lifecycle` 1m9s, `peer-rows` 45s. Each job has a 30-minute limit.

**People.** Six agents wrote models in parallel; the main thread reviewed, integrated, ran the full suites and changed CI. The main cost was model writing, state-space debugging and specification review. LLMs speed up the first draft; humans still review the specification and bounds.

**Maintenance.** Models move with design changes. Each directory's README records properties, simplifications, results and uncovered parts. New configurations go into `expected.txt` and `run.sh`.

## 7. Next

- Confirm or close #1098–#1103;
- add trace validation, Kani/Miri and the #639 cases;
- leave refinement for later.
