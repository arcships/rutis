# rutis 0.22 design and delivery roadmap

[中文](roadmap-0.22-2026-11-08.md)

Status: design baseline, not implemented. Date: 2026-11-08.
Tracking: [#694](https://github.com/arcships/rutis/issues/694) (release planning), [#1025](https://github.com/arcships/rutis/issues/1025) (event close admission CPU) and [#183](https://github.com/arcships/rutis/issues/183) (quality control). The [formal-verification redesign](design-formal-verification-2026-11-08.en.md) is the verification baseline; #1040 is explicitly deferred.

## 1. Purpose and current state

0.22 starts from the `v0.21.0` tag and has no release branch or implementation yet. This document freezes the design scope before code work begins. The planned 0.22 change is deliberately narrow:

1. Remove the ancestor walk from event-listener liveness checks while preserving the existing close/admission boundary ([event close design](design-event-close-admission-2026-11-08.en.md)).

Dynamic service announcements remain as the existing Cordis-compatibility behavior. The proposed host-side refusal policy is deferred: defense against untrusted plugins does not belong in this layer without a concrete requirement.

The current workspace package declaration is `version = "0.6.0"` in `Cargo.toml`; release work must first confirm whether this is an intentional workspace convention or a stale declaration. Do not change it as part of either feature until the release procedure is decided.

## 2. Release promise

0.22 promises:

- lower event-listener admission CPU on the `close_cpu` workload without changing public event, shutdown, cancellation or protocol behavior;
- bilingual documentation, migration notes and release notes that state the exact visible output.

0.22 does **not** add a service namespace per process, a global allowlist, `rutis.json` configuration for undeclared announcements, dynamic-name `isolate`, or a cross-language terminal “policy refused” row state. Existing Cordis-compatible dynamic announcements keep their current behavior.

## 3. Workstreams and gates

### W0 — Formal baseline and high-risk semantics (#183, #613, #622, #624, #630, #631, #634, #635, #639)

Formal verification is not deferred to 0.23. In 0.22, first establish a minimal executable model and deterministic race tests for #1025's close/admission boundary. Before implementation, confirm that no new callback is admitted after the marker commit; an admitted callback may begin after the commit but still drains under the existing rules; concurrent parent/child/sibling close cannot leak or run cleanup twice; and `Entry::live()` remains equivalent to owner-marker state after it stops walking ancestors. Reconcile the existing cross-runtime and remote-lease models against the current specification; an old-architecture model is not proof of the new implementation.

The complete four-layer plan (protocol foundations, concurrency/lifecycle, cross-process deployment, and inputs/long runs), and the boundaries between TLA+, shuttle, trace validation, conformance, property/fuzzing, virtual clocks and E2E, are in the [formal-verification redesign](design-formal-verification-2026-11-08.en.md). W0 does not claim complete formal coverage; it means 0.22 verifies the high-risk close semantics before implementation and leaves a traceable basis for the full 0.23 extension.

Gate: both the model and race tests fail when marker publication order, the admission lock or the final `live()` check is deliberately broken; normal model checking, the existing shuttle checks and focused #1025 tests pass, with results linked to guarantee numbers and the tracker.

### W1 — Event close admission (#1025)

Design and implementation follow the linked design document:

- Add permanent `FiberInner::lineage_closed` markers.
- Under `Shared.admission`, mark the complete fixed subtree first; publish existing `closing` flags only after the marker pass; then perform cleanup/shutdown.
- Make `Entry::live()` read owner marker state instead of walking ancestors. Keep `CallFlight::begin(owner)` and the final `live()` check in `Entry::start()`.
- Keep registration checks, instance-event permits, callback ordering, cancellation, shutdown output and cross-process behavior unchanged.

Gate: deterministic ancestor-close races and existing sync/async dispatch modes show that no new callback is admitted after the marker commit; an admitted callback may begin after the commit but still drains. The issue's `creation <= 30s` is a performance target to record, not an absolute-time assertion in the current `close_cpu` test; the automated gate continues to compare `CREATE_RATIO`, `CLOSE_RATIO` and `IDLE_SHARE` without regressing queued-emits memory.

The CI acceptance job is the Ubuntu 24.04 release job in `.github/workflows/stress.yml:381-417`; it runs `close_cpu` and `queued_emits_memory`, and both results must remain in the summary.

### W3 — Release, migration and documentation (#694)

Before implementation is called complete:

1. Decide and document the package/version update procedure, including the `Cargo.toml` workspace declaration and the `v0.21.0` tag.
2. Add `migration-0.22.md` and `migration-0.22.en.md`; state that dynamic announcement behavior is unchanged and that the event optimization has no API migration.
3. Add `docs/releases/0.22.0.md` and `.en.md`, with measured benchmark values only after CI runs.
4. Update the event, host and plugin guidance plus `release.md` and `release.en.md` in both languages.
5. Keep this roadmap and the event-close design synchronized; #1040 remains deferred and must not add a JSON field or wire-visible behavior to 0.22.
6. If a future trust-boundary design is reopened, write a new design document with a concrete requirement and layer ownership before implementation.

Gate: bilingual review finds matching decisions, exact output/error/code/data, role-specific “What developers see”, migration instructions and acceptance criteria.

## 4. Order of work

1. **Baseline:** verify the tag, package-version convention, current status serialization and benchmark environment; record measurements before changing code.
2. **Verification baseline:** reconcile the existing models and shuttle checks under the formal-verification design; write the #1025 close/admission model, counterexamples and deterministic race test first.
3. **Contracts:** land the event-close design and this roadmap; split core, race-test, benchmark and documentation tasks under #1025. Keep #1040 out of the 0.22 implementation plan.
4. **Core change:** implement the marker publication independently and keep it revertible.
5. **Acceptance:** run focused event race tests, `cargo test -p rutis-loader --release --test close_cpu -- --nocapture`, and the CI stress pair; record model, implementation-test and benchmark results separately.
6. **Release preparation:** update migration/release documents from measured results, resolve package-version policy, then perform the normal release review.

No release commit, tag, package-version change or public API implementation is part of this design-only checkpoint.

## 5. What developers see in 0.22

### Plugin authors

Existing Cordis-compatible dynamic announcement behavior remains unchanged. The event-close optimization has no new plugin API, output or error.

### Host owners and deployers

There is no new trust-boundary setting in 0.22. The event-close optimization has no new output, status field, protocol version or migration setting; existing status lines and failure text remain unchanged.

### Rust host authors

Public Rust APIs, the event protocol and shutdown results are unchanged. A closed call still returns `ErrorKind::Closed`, with the text `root has been shut down` and no new `code` or `data`.

### Runtime authors and contributors

This roadmap adds no runtime or verification-tool command, exit code, success output or failure output; existing model, shuttle and CI output remains unchanged. Verification diagnostics are not runtime output seen by plugin users.

## 6. Explicit follow-ups outside 0.22

- Any host-side refusal policy or trust-boundary defense for dynamic announcements; #1040 is deferred because this layer is not the right owner without a concrete requirement.
- A service namespace, global allowlist, `rutis.json` row-level policy or dynamic-name isolation model.
- Typed wire rejection and a terminal policy-refused row state across all SDKs and loader restart handling.
- New memory-order schemes beyond the initial `SeqCst` marker implementation.
- Any package-version change made without first reconciling the repository's release convention.

These are separate designs, not implied by the 0.22 promise.
