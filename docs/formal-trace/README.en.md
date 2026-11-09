# Rule trace table

[中文](README.md) · [Session trace contract](../formal-trace.en.md) · [Guarantee register](../guarantees.en.md) · [Quality standard](../quality-standard.md)

Status: first version, partial coverage. Date: 2026-11-09. Baseline: `main` `2452baee`.
Issue: [#634](https://github.com/arcships/rutis/issues/634) (the 0.21 trace table, [#776](https://github.com/arcships/rutis/issues/776)); plan: [0.22 plan](../plan/formal-verification-0.22.md) WS3; fields and status rules: §5 of the [formal verification design](../design-formal-verification-2026-11-08.md).

Updated the same day: WS1 landed the core of #1025 (marker publication, `Entry::live`, deterministic race tests, a shuttle model), which lifts blockers G3 and G4 of plan §3; the `ECA-*` rows with implementation or tests became five `部分验证` rows, the rest stay `未验证`.

Each row is one rule rutis promises, with its current source, the plain-language promise, the level, the implementations it applies to, the evidence (a path, a test name or a line range), a mutation or counterexample, the status and the gap. Status follows §3 of the [guarantee register](../guarantees.md). Numbers and evidence checks come from the script:

```sh
node tools/formal-trace.mjs            # status counts, then the checks
node tools/formal-trace.mjs --write    # regenerate the guarantee rows, refresh the index
node tools/formal-trace.mjs --check    # the checks only
```

## 1. What this version covers, and what it does not

In the table:

- **All 210 rows of the guarantee register**: 199 current, 8 withdrawn, 3 duplicate numbers that are not counted on their own (`core/D20`, `core/D26`, `core/D27`). The script extracts them from the register's Markdown tables; id, level, status and evidence are copied, never remembered.
- **The nine categories of the [session trace contract](../formal-trace.en.md)** (success, failure and boundary of each), its 8 trace invariants and its 9 explicit non-guarantees: 44 rows, ids `TRC-*`.
- **The 10 proposed rules in §3 of the [event-close admission design](../design-event-close-admission-2026-11-08.en.md)**: ids `ECA-*`. The design now stands at "core implemented, verification in progress" (2026-11-09): the core of rules 1, 2 and 5 is implemented, with deterministic tests in `crates/rutis-loader/tests/event_close_admission.rs` and a shuttle model in `crates/rutis/src/fiber/shuttle_tests.rs`; the Loading-owner, `once`, remote-entry, cross-process and reload coverage of rules 3, 6, 7, 8 and 9 is not complete. Five rows with implementation or tests are `部分验证`; the rest stay `未验证`. None is `已验证`. Blockers G3 and G4 of plan §3 are lifted: the detach happens under the same admission lock, and the marker uses `crate::sync::AtomicBool`, so the model can switch on the publication order.

**This is not the full set, and it does not show that the rule list is complete.**

- The 1368 rules of #634 (374 register rows, 994 protocol rules, plus 184 replaced) live only in seven issue comments; the repository has no source to extract them from (plan §3 G2). None of the protocol prefixes (`EVP`, `EVD`, `SES`, `VAL`, `TAG`, `M1`, `CHN`, `SYN`, `M2`, `MNT`, `REQ`, `RTP`, `INS`, `MSV`, `REM`, `DIA`, `APP`, `NOD`) is in the table yet.
- The session protocol specification (`docs/protocol-session.md`, landed by WS2 on 2026-11-09, 79 `SESS-*` rules: 61 current, 11 explicit non-guarantees, 7 replaced) is not in the table yet. Its vocabulary says whether a rule is in force, not whether it is verified, so every row needs its own status (§6 step 1): a `当前` rule must not be copied as `已验证`.
- Behaviour written in design and user documents but not in the register (other than Q3.3 implementation details) has not been extracted row by row.

So the release gate (design §5, quality standard Q13.1) **cannot** be judged from this table. What it does do: name the source, evidence and gap of every registered rule, count `未验证` and `明确不保证`, and make the evidence paths checkable.

## 2. Numbering

| Form | Source | Example | Rule |
| --- | --- | --- | --- |
| `<family>/<name>` | guarantee register | `core/D6`, `bridge/in-flight-call-fails-once` | kept as the register writes it; never rewritten or reused; withdrawn rows stay |
| `TRC-<category>-<n>` | session trace contract §3 | `TRC-GREET-2` | categories `LIFE`, `FRAME`, `GREET`, `REF`, `CANCEL`, `ERR`, `LIMIT`, `MALFORMED`, `NET`; `1` success, `2` failure, `3` boundary |
| `TRC-INV-<n>` | session trace contract §4 | `TRC-INV-2` | the eight trace invariants |
| `TRC-NG-<n>` | session trace contract §1, "not guaranteed" | `TRC-NG-4` | explicit non-guarantees |
| `ECA-<n>` | event-close admission design §3 | `ECA-4` | the design's own rule numbers |

Numbers are only added, never changed: moving a row to another section, renaming it or moving it to another file keeps its number; a deleted row stays with a status and a reason.

## 3. Fields and status

| Field | Meaning | Written by |
| --- | --- | --- |
| id | see §2 | script (register rows), hand (trace rows) |
| current source | the specification, design or user document and section in force; register rows name `guarantees.md §<section>` plus the register's own source column | script |
| plain-language promise | what a user observes: success, failure, scope and recovery; register rows use the register's content column verbatim | script / hand |
| level | core commitment, contractual behaviour, explicit non-guarantee (quality standard §3) | script / hand |
| scope | the implementations (Rust, Node, Bun, Python, Go, Swift, Kotlin), transport and platform | **to fill**: the register does not record it, so all 210 register rows say `未登记`; trace rows say what the document says |
| evidence | model, test, trace, property/fuzz, black-box or timing evidence, as a path, test name or line range | script (register's verification column verbatim) / hand |
| mutation or counterexample | a bug the verification caught, or an injected error | **to fill**: register rows say `未登记`; the mutations §4 of the trace contract describes are on their rows |
| status | `已验证`, `部分验证`, `未验证`, `明确不保证`; `已撤回` and `不单独计数` are not counted | script / hand |
| gap | the next issue; an uncovered rule is never written as passing | register rows start from the register's notes column |

Explicit non-guarantees follow the register's convention (as `core/D15` does): level and status are both `明确不保证`, and they are not counted as unverified. A model passing marks only the model; a rule is `已验证` only with implementation evidence. A trace row is `部分验证` only when a named test asserts a point of it; source code or comments alone leave it `未验证`.

**Language.** The register-driven rows are generated from the Chinese register; the hand-written columns and the trace rows are Chinese only. The English register (`docs/guarantees.en.md`) carries the same ids, so the rows line up, but the English trace rows are a gap (#634). Counts are printed by the script and kept in [the Chinese README](README.md) §4.

## 4. Maintenance

- **After changing the register or adding a test**: `node tools/formal-trace.mjs --write`, then `--check`. `--write` rewrites the automatic columns and keeps the manual ones by id.
- **Adding a `TRC`/`ECA` row**: write it in `rules/` with the same columns; `--check` verifies the id form, the status vocabulary and the evidence paths.
- **Moving a row**: keep the id; change the source and the index.
- **CI**: not wired yet (WS8). Until then the two commands are run by hand.
- **English rows**: a gap (#634), see above.

## 5. Gaps found by this version

1. **A real hole in the register's evidence**: `loader/remote-runtime-lease` (the only `未验证` core commitment) lists `rutis-loader/tests/leases.rs`, deleted with the connection plugin (`ec47944c`); §16 of the register already says the test is to be added. The checker skips paths inside parentheses, as `tools/check-guarantees.mjs` does (there the parenthesis says there is no evidence), so the hole sits in the known blind spot of both checks.
2. **The register's §16 totals do not match its rows**: §16 says 194 current rows, the tables have 199; by section, §10 has 1 more, §12 2 more and §14 2 more. The table counts the rows; §16 needs a maintainer's check.
3. **`TRC-GREET-3` (refusing a missing required capability) has no test**: the code is at `crates/rutis-bridge/src/session/plugin/greet.rs:33-70`; searching `Incompatible` finds only version and transport cases.
4. **`TRC-INV-2` (the handshake gate) has no Rust assertion**: the text `protocol handshake incomplete` is implemented and untested; Go's `TestACallBeforeTheHandshakeEndsTheSession` asserts its own words.
5. **Scope and mutation are mostly empty**: they come from the support matrix (quality standard §10) and the mutation results of #624, #630 and #631.
6. **No protocol rule is in the table yet**: see §1 and the next step below.

## 6. Next step: extracting the 1368 rules

1. **Start with the specification that exists**: `docs/protocol-session.md` names itself the extraction source for #634's `session` section, so its 79 `SESS-*` rules can go in now. Give every rule its own verification status, not the specification's vocabulary: `当前` starts as `未验证` and becomes `部分验证` only when a named test asserts a point of the rule (a code line is a location, not an assertion); `明确不保证` becomes `明确不保证` in both level and status; `已取代` becomes `已撤回`, with the replacing rule named in the gap. Suggested file `rules/protocol-session.md`, same nine columns, plus a drift check against the specification.
2. **Then the historical list**: export the seven #634 section comments (`registry-1`, `registry-2`, `events`, `session` parts 1–2, `runtime`, `remote`) into `rules/protocol-<section>.md`, keeping each row's id (`<prefix>-<section>-<number>`), source, rule, participants, means, existing formal check, status, owning issue and notes.
3. **Extend the script**: a section-file format aligned with the nine columns; a mapping from the issue's vocabulary (`已覆盖`, `部分`, `未覆盖`, `不适用`, `明确不保证`, `未登记`, `已取代`) to the table's status, written down in §3; replaced rows kept but not counted; `--check` gains the protocol id form and the per-section totals of #634's summary table.
4. **Reconcile, do not copy**: the issue's list was extracted at `3496669b`; the 11 design documents of 0.17–0.19 and the new #635 specification have to be re-extracted from the current text. Until then, protocol rows say `未验证` and note that they come from the issue comment and are not yet checked against the current specification.
5. **Attach models and conformance cases**: once WS4 (`#613`), WS5 (`#624`) and WS7 (`#631`) produce TLA+ properties, the trace validator and conformance test names, they go into the evidence column, and only then can a status move up.
6. **Wire CI**: `--check` on pull requests. The "trace table references" check of #776 is this script; the "model drift" check belongs to WS4/WS8.

## Related files

- [Session trace contract](../formal-trace.en.md) · [Event-close admission design](../design-event-close-admission-2026-11-08.en.md)
- [Guarantee register](../guarantees.en.md) · [Quality standard](../quality-standard.md) · [Quality status](../quality-status.en.md)
- [Formal verification design](../design-formal-verification-2026-11-08.en.md) · [0.22 plan](../plan/formal-verification-0.22.en.md)
- Scripts: `tools/formal-trace.mjs`; register check: `tools/check-guarantees.mjs`
