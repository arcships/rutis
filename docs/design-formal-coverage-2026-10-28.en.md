# Formal coverage in 0.20

[中文](design-formal-coverage-2026-10-28.md)

Status: draft, not implemented. Date: 2026-10-28. Base: `main` `00b9fd3f` (0.18 finishing, 0.19 being implemented). Scheduled: 0.20.

Issue: 0.20 in the roadmap [#694](https://github.com/arcships/rutis/issues/694); step 4 of the quality umbrella [#183](https://github.com/arcships/rutis/issues/183). Implemented by: [#634](https://github.com/arcships/rutis/issues/634) (rule list and trace table), [#635](https://github.com/arcships/rutis/issues/635) (specs and the guarantee registry), [#613](https://github.com/arcships/rutis/issues/613) (TLA+), [#622](https://github.com/arcships/rutis/issues/622) (shuttle), [#624](https://github.com/arcships/rutis/issues/624) (trace validation and conformance columns), [#630](https://github.com/arcships/rutis/issues/630) (controlled scheduling for non-Rust runtimes), [#631](https://github.com/arcships/rutis/issues/631) (property tests, fuzzing, Kani, Miri), [#639](https://github.com/arcships/rutis/issues/639) (conformance cases and plain tests).

Those 8 issues already say what each technique does. This document settles only what they share: which version the scope is taken from, when the work starts, where "no behaviour change" ends, what done means, where the checks run, and what people changing rutis see.

## 1. Where things stand

**People using rutis** (plugin authors, host owners, those assembling with `rutis-host`) rely on promises written in the [guarantee registry](guarantees.en.md). Its "verification" column lists only tests: it does not show which rules are checked under every interleaving and which hold only under the interleavings the tests happen to reach. What went wrong shows the difference: after 0.13, users were the first to hit the emit order and unload rules of unified events (#513, #527, #536, #557, #587), all problems that appear only under particular timings. Some non-guarantees are written only inside notes, not as rows of their own (#634, "problems found while extracting").

**People changing rutis**: quality standard Q9.1.2 requires a model check before a multi-party protocol is settled, and Q11.2.3 requires the model to change with the design, but nothing enforces them; unified events were settled without a model check. The rule list and trace table live in comments on #634, and nobody updates them when code changes.

**The trace table's base is out of date**: #634 was extracted at `main` `3496669b` (0.16, 2026-10-24): of 1368 live rules, 1114 should have formal checks and 22 were fully covered. Since then 0.17–0.19 added 11 design documents with rules (§3.1) and changed session frames (the `objects` capability for object references), calls (time limits and cancellation) and remote runtimes (one host versus several). None of that is in the table.

## 2. Decisions

### 2.1 The scope is taken at the 0.19 release

The scope stays what #634 set: **every entry of the guarantee registry + every rule of the protocol specs**, not chosen by suspicion. The base moves from `3496669b` to **the 0.19 release tag**: the designs of 0.17–0.19 come in under the prefixes of §3.1, and old rules they replaced are marked "superseded".

Not in scope:

- Designs not implemented yet: hosts writing their own plugins (#185, [design](design-host-writes-plugins-2026-10-27.en.md), implementation after 0.20); its rules are extracted when it is implemented.
- Designs with documents only and no behaviour: [guides by layer](design-guides-by-layer-2026-10-27.en.md), [design document conventions](design-docs.en.md).
- The example projects `rutis-agent` and `rutis-cli`.

Keeping `3496669b` was dropped: rules changed by 0.18 and 0.19 would be modelled from their old text, and the models would check rules that no longer hold.

### 2.2 The work starts after 0.19 is released

Before the 0.19 tag, 0.20 consists of this document and updates to the issue bodies. The reason is the one the roadmap already gives: formal coverage changes no behaviour, so it waits until 0.18 and 0.19 have finished changing behaviour and rules are not rewritten twice. 0.19 touches almost every partition (session frames, calls, rows, remote), so no partition can safely start early and none is split out.

### 2.3 Where "no behaviour change" ends

0.20 adds no API, changes no config format and no default, bumps neither the protocol version nor `PLUGIN_API`, and has no migration guide. What the checks find is handled like this:

| Finding | What is done |
| --- | --- |
| The code does not follow the spec | Fixed in 0.20 with a test; the spec stays. If it affects people already using rutis, it also goes into 0.19.x under the patch rules |
| The spec is wrong, the code is right | The spec is corrected (both languages); the old rule is marked superseded and rules are extracted again |
| Neither spec nor code can guarantee what users assume, and guaranteeing it needs an incompatible change | 0.20 does not change behaviour: the registry gets a "not guaranteed" row and the spec says so; whether to change it goes to the next incompatible version, added to an existing issue |
| A suspicion turns out false | Its configuration is changed to expect a pass and the README says so |

### 2.4 Done

When 0.20 is released, in the trace table:

1. every rule that should have a formal check is "covered": the check passes, and a pre-fix configuration or a deliberately introduced fault (a mutation) makes it fail, showing it is not vacuous;
2. every rule out of reach of formal methods has a test, status "not applicable", the test named in the note;
3. every rule moved to another technique says why;
4. every registry entry lists in its "verification" column the checks that cover it (§3.4).

These four go into the release check (§3.5). Rules added after 0.20 need checks too: the number of uncovered rules may not grow from one release to the next.

### 2.5 The trace table moves into the repository

The table moves from #634 to `docs/formal-trace/`: one file per partition (`registry.md`, `events.md`, `session.md`, `runtime.md`, `remote.md`; the new designs of §3.1 join the partition each belongs to), each in both languages; `README.md` gives the scope, the meaning of the columns and the counts. The counts are computed by `scripts/formal-trace-stats` from the partition tables, not written by hand. Once in the repository, the repository is the reference and #634 is closed.

A JSON file was dropped: reviewers have to read each row directly in the PR diff.

### 2.6 Where the checks run

- **New models, shuttle models, fuzz targets, trace validation**: in full in the nightly and full runs (settled in #183). On pull requests, only when their paths change, a short version runs (TLA: every configuration; shuttle: 1000 iterations per model), the same as the current `specs` job and shuttle models.
- **Two small required checks** run on every PR and are required for merging; they read files only, run no model, and take under a minute each:
  - **Model drift** (item 7 of #612, merged into #635): a PR that changes an implementation file listed in a model's README without changing that `specs/` directory has to say in its description why the model is not affected;
  - **Trace references**: the check names written in the trace table and the registry exist, the IDs written in test comments are registered, and the counts are current. The registry's opening says checking the table against tests would be reconsidered "beyond 300 entries"; it now has 374 entries and the rules number over a thousand, so the condition is met. Only whether references exist is checked, not whether checks are sufficient, so there are no false alarms.

The PR CI time target (#577) is unaffected: the required set grows by two jobs of under a minute, and the path-triggered ones run among the existing parallel jobs.

### 2.7 No new issues

Rules of the 0.17–0.19 designs are assigned to the existing 8 issues by technique (§3.2); each issue gets a section "rules added in 0.17–0.19" at the end of "rules of the trace table owned by this issue". Problems the checks find go first into the issue that owns the rule.

## 3. Rules

### 3.1 Prefixes for the new designs

Rule IDs stay `<prefix>-<section>-<number>`; registry entries keep their IDs. New prefixes:

| Prefix | Design | Version |
| --- | --- | --- |
| `RAP` | [Rust API convergence](design-rust-api-convergence-2026-10-24.en.md) | 0.17 |
| `DEF` | [Plugin definitions across languages](design-plugin-values-2026-10-25.en.md) | 0.17 |
| `DGN` | [Diagnostics: errors, current state, time limits](design-diagnostics-2026-10-26.en.md) | 0.18 |
| `CFG` | [Config schema](design-config-schema-2026-10-27.en.md) | 0.18 |
| `OBJ` | [Objects by reference](design-object-refs-2026-10-27.en.md) | 0.18 |
| `RRC` | [Remote runtime consistency](design-remote-runtime-consistency-2026-10-26.en.md) | 0.19 |
| `LIM` | [Plugin limits](design-host-plugin-limits-2026-10-27.en.md) | 0.19 |
| `CTR` | [Host contract types](design-host-contract-types-2026-10-27.en.md) | 0.19 |
| `TRY` | [Trying and testing plugins](design-try-and-test-plugins-2026-10-27.en.md) | 0.19 |
| `PKG` | [Plugin install](design-plugin-install-2026-10-27.en.md) | 0.19 |
| `ENT` | [SDK entry convergence](design-sdk-entry-convergence-2026-10-27.en.md) | 0.19 |

Rules are extracted from each design's text at the 0.19 tag; where it was revised during implementation (the "same-day revision" and "corrected during implementation" notes in its status line), the latest text counts.

### 3.2 Which issue owns the new rules

The technique follows the participants, as in #634 (across processes: TLA; concurrency inside a process: controlled scheduling; pure logic: property tests / fuzzing; implementation against model: trace validation and the conformance suite; the rest: tests). The table gives where each design mainly goes; each rule is settled while extracting:

| Prefix | What the rules cover | Technique → issue |
| --- | --- | --- |
| `OBJ` | Object references forwarded across sessions, release when the holder exits, wait cycles of synchronous calls carrying objects | TLA → #613 (objects added to the call lifecycle and cross-runtime sync models); reference-count PT → #631; `objects.expected.json` → #639 |
| `LIM` | Call time limits, cancellation, `kill_on_timeout` | TLA → #613 (time limits and process kill on timeout added to the call lifecycle model) |
| `LIM` | Supervision of one process per plugin | SH → #622 (the Launcher seam models) |
| `LIM` | The `allowed_services` decision | PT → #631 |
| `LIM` | Greeting time limit | Test (virtual clock) → #639 |
| `RRC` | One host and several sharing discovery and checks, `Unavailable`, the three forms of `runtimes.remote` | TLA → #613 (remote lease and node-hosted rows follow the new design); access point TV → #624 |
| `DGN` | Current-state snapshot agreeing with calls in flight | SH → #622 |
| `DGN` | Error structure kept through relays, `site`, error codes | Conformance cases → #639; formatting never fails → PT, FZ → #631 |
| `DGN` | Output text | Test → #639 (word-for-word comparison; existing acceptance tests get the IDs) |
| `CFG` | Keywords, defaults, rules that must match word for word | PT, FZ → #631; `specs/config-schema/cases.json` recorded as conformance cases → #639 |
| `CTR` | Generated types agreeing with the contract description, strict checks in testing tools | Differential PT → #631; conformance cases → #639 |
| `PKG` | Editing install records and rows, `remove` / `upgrade` | PT (against a reference model) → #631; the rest tests → #639 |
| `DEF` | Definition values across languages | Conformance cases → #639 |
| `TRY`, `ENT`, `RAP` | Command behaviour, public surface, where names went | Test → #639 (existing snapshots and acceptance tests get the IDs) |

#635 writes the protocol parts of these designs into the current specs: the `objects` capability, the `timeout` code and call cancellation into `docs/protocol-session.md`; the remote runtime changes into the remote and dial-out specs. The specs change first; the models are built from them.

### 3.3 Not vacuous

For every formal check marked "covered", the table's "check" column gives two things: the check's name, and the configuration or mutation that makes it fail. Without the second it does not count as covered. Same as `specs/remote-lease/expected.txt` (`lease-before-fix violated:RevokedStaysOut`).

### 3.4 The registry's "verification" column

Besides tests, the column lists formal checks, always written `<technique> <path>::<name>`:

| Technique | Example |
| --- | --- |
| TLA | `TLA specs/events/EventsEmit.tla::EmitOrder` |
| SH | `SH rutis/src/shuttle_tests.rs::dispose_sees_cancel` |
| PT | `PT rutis-bridge/src/session/rpc/waits.rs::cycle_among_matches_dfs` |
| FZ | `FZ fuzz/fuzz_targets/frame_decode.rs` |
| KANI | `KANI rutis-loader/src/dylib/meta.rs::proof_header_bounds` |
| TV | `TV specs/events/EventsTrace.tla` |

Paths follow registry §4 (relative to `crates/`, from the repository root outside it). The meaning of the "status" column does not change.

### 3.5 Release check

Registry §1 gains item 3, and the trace table's README says the same:

> 3. **From 0.20**: the number of rules in the trace table that should have a formal check and are not "covered" is zero for 0.20 and may not exceed the previous release afterwards. The number goes into the release PR description with the other two.

### 3.6 Rules for changing rutis from now on

- A design that changes multi-party rules has a "model check" section before it is settled (standard Q9.1.1 item 5, #635).
- A PR that changes a spec extracts the affected rules again in the same PR (old rows marked superseded).
- A PR that adds a check marks the rules it covers "covered" in the same PR, with the check's name and the configuration that makes it fail.

## 4. What developers see

### 4.1 Plugin authors, those assembling with `rutis-host`, host owners

No behaviour changes; code, config and output stay as they are. Problems fixed in 0.20 behave, once fixed, as the documentation already says; there is no new error text.

What they can see is the registry. Emit order of unified events, for example, is two rows of `docs/guarantees.en.md` after 0.20:

```
| `events/emit-order` | Emits of one event name from one emitter start their listener calls in the order they were emitted | Contract behaviour | Verified | `TLA specs/events/EventsEmit.tla::EmitOrder`, `rutis-bridge/tests/event_hub.rs::emits_start_in_order` | Both the fast path and the path through the host are checked |
| `events/no-total-order` | Emits from different emitters have no order; processes are not promised to see the same order | Not guaranteed | Not guaranteed | `TLA specs/events/EventsEmit.tla::sees-all` (counterexample) | Protocol spec §4.4, §5.5 |
```

(IDs and check names are settled during implementation; what is settled here is the form: each guarantee names the model that checks it under every interleaving, and each non-guarantee is a row of its own with its counterexample.)

The first sentence of the 0.20 release notes is: "This release changes no behaviour and needs no code changes"; then the fixed problems follow, one line each: what happened, in which versions, and how it behaves now.

New concepts: none. "Not guaranteed" has long been in the registry.

### 4.2 People changing rutis

This role is not in the concept budget of #663 (the budget is per layer for plugin authors and host owners); the new concepts are listed for maintainers: rule IDs, the trace table, the six technique codes, "not vacuous".

**When everything works**: a PR changing `crates/rutis-bridge/src/events/hub.rs` also changes `specs/events/`, or has this line in its description

```
Model: events not affected (only log text changed)
```

The Chinese form `模型：events 不涉及（只改了日志文字）` is accepted too. Both required checks pass, and the `specs` job runs every `events` configuration.

**When it fails**, one line each: the cause, the output word for word, the fix:

1. **A file listed by a model changed, the model did not, and no reason is given.** The `model-drift` check fails:

   ```
   model-drift: crates/rutis-bridge/src/events/hub.rs is listed in specs/events/README.md, but specs/events/ is unchanged.
   Change the model, or add this line to the PR description: 模型：events 不涉及（<理由>）
   ```

   Fix: change the model, or add that line to the PR description; the check reruns when the description is edited.

2. **A TLC result differs from what is expected.** The `specs` job fails; after `run.sh`'s line (current format) comes a line with the rules the property covers:

   ```
   events-emit-healthy                violated:EmitOrder             expected ok                             (1,204,331 distinct states found; 212s; log /tmp/tmp.Xa81/events-emit-healthy.log)
   covers: EVP-4.4-2 EVP-5.5-1 events/emit-order (docs/formal-trace/events.md)
   ```

   Every step of the counterexample is in the log (TLC's own output). Fix: wrong code, change the code; wrong spec, change the spec and the model, per §2.3.

3. **A shuttle model fails.** The last two lines of the failing test's output:

   ```
   shuttle model dispose_sees_cancel failed at iteration 48213 (rules: core/D24 core/features.4.2)
   replay: RUTIS_SHUTTLE_REPLAY='<schedule>' RUSTFLAGS="--cfg rutis_shuttle" cargo test -p rutis --lib shuttle_tests::dispose_sees_cancel -- --exact
   ```

   The second line replays the same schedule locally. Controlled scheduling in other languages (#630) prints its own replay command in the same form.

4. **Trace validation does not match** (nightly):

   ```
   trace-validation events: line 1834 of host.log matches no action of EventsHost: {"op":"reply","emit":42,"row":"weather"}
   last matched: StartCall(emit=42, listener=3) at line 1833
   ```

   Fix: the implementation did something the model does not allow, handled per §2.3; if the log point is wrong, fix the log point.

5. **The trace table names a check that does not exist, or a test comment names an unregistered ID.** The `formal-trace` check fails:

   ```
   formal-trace: docs/formal-trace/events.md row EVP-4.4-2 names TLA specs/events/EventsEmit.tla::EmitOrder, which is not defined there.
   formal-trace: crates/rutis/tests/contract.rs:212 names guarantee core/D99, which is not in docs/guarantees.md.
   ```

   Fix: correct the name or ID; when a check is renamed, update the table in the same PR.

6. **The counts are out of date:**

   ```
   formal-trace: the counts in docs/formal-trace/README.md are out of date; run scripts/formal-trace-stats --write
   ```

   Fix: run that command and commit the result.

7. **A check added without a configuration that makes it fail** (seen in review, not CI): the table's "check" column has only the check name; review asks for the pre-fix configuration or mutation, and until then the row is not changed to "covered".

## 5. Compatibility and migration

No code, config, protocol or output changes, and no migration guide. Release notes are written as in §4.1. After 0.20 is released the role evaluations of #663 are rerun as usual; the results should equal 0.19's, and a difference means a 0.20 fix changed behaviour it should not have, treated as a bug.

## 6. Implementation

After the 0.19 tag, in this order (matching the "first, then, after that" of #183 step 4):

1. **Extract again (#634)**: every partition re-extracted at the 0.19 tag with the prefixes of §3.1; old rows that were replaced marked superseded; counts updated; each issue body gains "rules added in 0.17–0.19". Done: every live rule has a technique and an issue.
2. **First: specs, registry, trace table into the repository (#635, #634)**: the current session frame spec (with `objects`, `timeout` and cancellation from §3.2), the revisions of the remote, network stack and multi-language specs, registry corrections, quality status, Q9.1.1 item 5; `docs/formal-trace/`, the counting script, the two required checks (§2.6). Done: every item in #635's list is ticked, `model-drift` and `formal-trace` are required for merging, #634 is closed.
3. **Then: the shared bases**: the three shuttle substitutes and the Launcher seam (#622), log points (#624), the shared `EventsHost` module (#613), the replay output when shuttle fails (§4.2 item 3).
4. **After that, by technique in parallel**: #613, #622, #624, #630, #631, #639, each per its body and its added rules. When two change the same file at the same time, the one merging later syncs with main first.
5. **Release**: the four items of §2.4 and the number of §3.5 go into the release PR; registry §1 items 1–2 as usual; release notes per §4.1.

Each issue is done when the rules listed at the end of its body (the added ones included) all meet §2.4 in the trace table. When implementation starts, the PR plan (one row per group) goes into #694 and progress into a tracking issue of its own, as #771 did for 0.19.
