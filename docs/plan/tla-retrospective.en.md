# Landing TLA+ in 0.22: practice, incidents and reflection

This is not a model guide. It records how TLA+ went from one line in a design document to nine models, about 215 configurations, CI and a rule tracker — mostly the mistakes and the judgements we changed along the way.

## 1. The starting point is not writing a model; it is deciding what to prove

The easiest mistake is to open an editor and write `Init`/`Next`. The order we ended up using was:

1. Find the promises in the design documents and issue #613: which behaviours cross processes, interleave, retry, disconnect or crash.
2. Turn each promise into a property: safety and liveness separately; what can be an invariant becomes one, and what cannot is explicitly left out.
3. Only then choose participants, messages, failure model and bounds.

After that, every property can point back to a design rule. If it cannot, it is usually an implementation detail and does not belong in TLA.

## 2. Three batches; "the first version passes" means little

The first batch — event close, row lifecycle, call lifecycle — ran and passed, but the coverage was shallow: many configurations only checked the happy path, and the mutations only flipped a boolean.

The second batch, with shuttle, trace validation and property tests, started exposing implementation problems: the old-generation reinsertion in `events/link.rs`, the Kotlin `RutisApp` start/stop race, and the Node/Bun 5-second kill. These were not model mistakes; the models made real windows in the code visible.

Only the third batch, expanding six directories to the design scope, hit state explosion, CI timeouts and models that lie.

## 3. Concrete incidents

### 3.1 Bigger state spaces are not better

`table-healthy` in `events` once took 10 minutes and 9 GB. `policy-b-posts` in `cross-runtime-sync` had 14 million states and pushed a 4-core CI job past its 30-minute limit.

The fix was not a bigger machine. It was smaller bounds: two runtimes by one row, `MaxDispatch=1`, `SYMMETRY`; and `MaxDepth` from 3 to 2 for `policy-b-posts`, taking it from 14M to 1.15M states. The properties stayed, the counterexamples stayed, and CI went from timeout to seven minutes.

The lesson: **TLC bounds are part of the design, not a knob to tune afterwards.** Every configuration has to answer whether its bounds can still produce the counterexample it is meant to catch. If not, change the configuration; do not keep running.

### 3.2 Models make quiet mistakes

In `peer-rows`, `PFailed` wrote one variable both as an assignment and in `UNCHANGED`, so the action was always false. The failure/restart path was never explored, while the configuration still reported success.

A rewrite of `row-lifecycle` also put `cleanCount` into an `UNCHANGED` it did not belong in. The old state counts looked unchanged, but the coverage had already changed.

TLC does not report these; the model simply "passes" for no reason. We added two habits:

- every new action gets a reachability check: can it actually happen?
- every passing configuration needs at least one mutation that fails the same property.

### 3.3 A configuration may describe old code

`bail-policy-b` in `cross-runtime-sync` kept reporting a deadlock. We almost changed `waits.rs` for it. Printing the real `forward` input showed that the current compliant runtime already puts the dispatch id on the listener's `path`, equivalent to `bail-recorded`; `bail-policy-b` describes the pre-fix behaviour.

The model was not wrong. We were wrong to treat its "current" configuration as current code. Every model README now says which behaviour the base configuration matches and whether a mutation is pre-fix or deliberately wrong.

### 3.4 Changing code from a model can break the implementation

`async-policy-b` says recording an asynchronous call on a marked chain as a hop causes false refusals. Changing `waits.rs` to count only `sync` made the object-relay test `a_synchronous_cycle_through_objects_is_refused` hang. The frame only has `sync`; it cannot distinguish "fire and forget" from "awaited later".

We reverted the code, recorded the finding as a known false alarm, and opened #1098. It is not a pretty outcome, but it is better than "the model says so, so break the implementation".

### 3.5 Separate real bugs, suspected bugs and design gaps

The real bugs confirmed and fixed were:

- the old-generation reinsertion in `events::link::accept`;
- the Kotlin `RutisApp` start/stop race;
- the Node/Bun kill firing before the cleanup deadline.

Suspected but unconfirmed issues went to #1101/#1102/#1103. Undecided design rules went to #1098/#1099/#1100. We did not write "suspected" as "fixed", and did not force a design gap into an implementation patch.

## 4. How we decide whether TLA coverage is enough

Not by model count or configuration count, but by four questions:

1. Does every promise have a property?
2. Does every property have a healthy configuration and a counterexample-catching mutation?
3. Are the results in `expected.txt`, `run.sh` and the tracker?
4. Are the bounds, simplifications and uncovered parts written down?

All four: covered. Missing one: partially verified. An explicit non-guarantee is acceptable; an unwritten gap is not.

## 5. Reflection

- **TLA is a design tool, not a code proof.** It can show "no counterexample in this bounded model"; it cannot show the Rust/Kotlin/Node code is correct. Code needs trace validation, shuttle, conformance and ordinary tests.
- **An LLM can write a model, but cannot decide the specification.** Syntax passing and TLC running do not mean the model matches the code; a human has to review the specification and the bounds.
- **Do not chase 100% of code paths.** State space and review cost explode. Add models only for new promises, historical bugs, implementation ambiguities and undecided decisions.
- **CI cost has to be designed in.** Large configurations blow through a 30-minute job; path filtering keeps TLA out of ordinary PRs, but a PR that changes `specs/**` has to survive the full run.
- **Passing is not the deliverable.** The real outputs are fixed bugs, recorded design gaps and confirmed or rejected implementation suspicions. Configuration count is only a process metric.

The biggest gain was not "nine models". It was separating model conclusions from code behaviour: the model may assume boldly; the code change must return to the implementation and its tests. The next step is not more configurations, but settling #1098–#1103 and adding trace validation plus Kani/Miri.
