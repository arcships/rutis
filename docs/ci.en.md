# Continuous integration: design and process

[中文](ci.md) · Standard: [quality standard](quality-standard.en.md) §12 · Measurements and the improvement plan: [quality status](quality-status.en.md) §8

This document describes **how the repository's continuous integration works today**, and **the process to follow when changing it**: adding a job, connecting a language runtime, changing which changes select which jobs. The rules come from quality standard Q12. The measurements and the plan for further changes are in quality status §8 and are not repeated here.

## 1. Principles

Taken from Q12; every rule below follows from them:

1. **Select by change** (Q12.3): before merging, run only the checks the change affects; on main, run every check every time.
2. **Cancel superseded runs** (Q12.4): when a pull request gets a new commit, its older run is cancelled at once. Runs on main are never cancelled.
3. **One merge condition** (Q12.5): the summary check `ci-ok` is the only check branch protection requires.
4. **Time is what the developer waits** (Q12.6): from push to result, queueing included. On scarce runners (macOS), each run should take as few jobs as possible.
5. **No duplication** (Q12.7): a kind of check runs in one place per tier.
6. **CI configuration is code** (Q12.10): a pull request that changes it runs every check, and is reviewed like code.

## 2. Tiers and workflows

| When | Workflow | What |
| --- | --- | --- |
| Pull request | `ci.yml` | The jobs the change selects (§3, §4), summarised by `ci-ok` |
| Merge to main | `ci.yml` | Every job, never cancelled |
| A pull request touching the Windows side of dylib; `v*` tags; by hand | `dylib-windows.yml` | Dylib plugin tests on Windows (25–40 minutes) |
| Nightly; by hand | `stress.yml` | Kernel tests repeated; network stack soaks |
| Weekly; by hand | `ci-stats.yml` | Measurements of CI itself (§8) |
| `v*` / `cli-v*` tags | `release.yml` / `release-cli.yml` | Releases |

The time budget of each tier is in quality status §8.4.

## 3. Selecting by change

The first job of `ci.yml`, `changes`, uses `dorny/paths-filter` to tell what changed and sets a few outputs. Later jobs run or not depending on them. On a push to main, every output is `true`.

| Output | `true` when a pull request touches | Meaning |
| --- | --- | --- |
| `all` | `Cargo.lock`, the root `Cargo.toml`, `rust-toolchain.toml`, `.github/workflows/**` | The reach cannot be told (Q12.3.2, Q12.10): every other output is `true` too |
| `code` | Any file except `docs/**` and `*.md` | A code change |
| `dylib` | The kernel `crates/rutis/**`; `rutis-cli`, `rutis-dylib*`, `rutis-sdk`, `rutis-dev`, `rutis-xtask`; `tools/*dylib*`, `tools/test-sdk-bundle.sh`, `tools/lib/**`, `tests/dylib-fixtures/**` | The dylib SDK and what goes into it |
| `packaging` | `scripts/train.mjs`, every `pyproject.toml` / `package.json`, `crates/*/Cargo.toml`, `node/rutis-host/scripts/**` | What a release packages |
| `docs` | `**/*.md` | Documentation |

The mapping lives in the `changes` job of `ci.yml`. It is CI configuration, changed as §6.3 says.

## 4. Job catalogue

| Job | Platform | Selected by | What it does |
| --- | --- | --- | --- |
| `changes` | Linux | Always | Tells what changed (§3) |
| `links` | Linux | `docs` | Relative links in Markdown |
| `test` | Linux | `code` | The full set: `cargo test --workspace`; every kind of loader row (also on loopback); bridge with every feature; builds of all targets and of each feature set; the Node and Python packages' tests |
| `network-macos` | macOS | `code` | Bridge with every feature, every kind of loader row, rutis-host, on macOS; the static build of the workspace. **The one macOS job of an ordinary change** (Q12.6.2) |
| `runtimes-windows` | Windows | `code` | Every kind of loader row, bridge with every feature, rutis-host, on Windows; the static build of the workspace |
| `runtimes-bun` | Linux, macOS × Bun 1.4.0, latest | **None (runs every time, see §9)** | The Bun runtime's `bun test`; bridge and loader Bun tests (also on loopback); rutis-host's Bun tests |
| `semver-rutis` | Linux | `code` | Public API changes since the last release (warns only) |
| `dylib-linux-launcher` / `-repro` / `-sdk-bundle` | Linux | `dylib` | Three jobs in parallel: the dylib host environment and plugin swaps; reproducible builds on one machine; external plugins built against a prebuilt SDK |
| `dylib-macos` | macOS | `dylib` | The same three, plus quarantine and the hardened host, one after another in one job |
| `release-dry-run` / `release-windows` / `release-wheel-aarch64` | Linux / Windows / Linux | `packaging` | Release artifacts build and pack |
| `sdk-repro` / `sdk-repro-macos` (×2 each), and their comparisons | Linux / macOS | `all` | The SDK built on two machines and the hashes compared; uncacheable, so only on main and on full runs (Q12.6.3) |
| `ci-ok` | Linux | Always | The summary (§5) |

## 5. The merge condition: `ci-ok`

- A job listed in `ci-ok`'s `needs` passes with `success` or `skipped`. `failure` or `cancelled` fails `ci-ok`.
- `ci-ok` is the only check branch protection requires. **A job not in its `needs` does not block a merge, even when it fails.**
- So every new job needs both a selection condition and a place in `ci-ok`'s `needs` (§6.1).

## 6. Process

### 6.1 Adding a job

1. **Choose the tier.** How soon must the problems it finds be found (Q12.1)? Something that can only break at release goes on main or tags. Long, uncacheable checks that rarely find anything stay off the pre-merge critical path (Q12.2, Q12.6.3).
2. **Give it a selection condition**: `needs: changes` and `if: needs.changes.outputs.<output> == 'true'`. If no output fits, add one to `changes` (§6.3) rather than leaving the condition out.
3. **List it in `ci-ok`'s `needs`.**
4. **Check for duplication** (Q12.7). Is the same kind of check already run by another job? If it can run on the same machine one step after another, add it to that job instead.
5. **Scarce platforms.** Before taking a macOS runner, see whether the work fits in `network-macos`. An ordinary change takes at most one macOS job, and a dylib change at most two (quality status §8.4).
6. **Add it to the catalogue in §4.**

### 6.2 Connecting a language runtime

A language runtime's tests (Node, Python, Bun, and later Go and others) come in three kinds, each with one place:

| Tests | Where |
| --- | --- |
| The runtime's own unit tests (`bun test`, `npm test`, `unittest`) | Linux: `test`; macOS: `network-macos`; where several versions are needed, the language's version job (next row) |
| Rust contract, row and host tests (`--features <language>`) | The same: add the feature to the bridge, loader and host steps of `test` and `network-macos`, and of `runtimes-windows` once Windows is supported |
| The oldest and latest version matrix | One `runtimes-<language>` job: **Linux only on pull requests**, selected by the language's output (§6.3); the oldest version on macOS runs on main |

Checklist:

- [ ] `changes` gets a `<language>` output covering the runtime package's directory, `crates/rutis-bridge/**`, `crates/rutis-loader/**` and `crates/rutis-host/**`.
- [ ] `test`, `network-macos` (and `runtimes-windows` once supported) install the language, and their existing steps gain the feature. No new job for this.
- [ ] The version matrix job `runtimes-<language>`: `needs: changes` and its condition; listed in `ci-ok`; no macOS runner on pull requests.
- [ ] `release-dry-run` packs it; `scripts/train.mjs` and `release.yml` include it.
- [ ] §3 and §4 of this document are updated.

### 6.3 Changing which changes select which jobs

- Changing the paths in `changes`, or adding an output, changes `.github/workflows/**`. That pull request therefore runs every job (`all`).
- A new output gets a row in the table in §3 saying when it is `true`.
- **A job skipped before merging that then fails on main** means the mapping missed that kind of change. After fixing the failure, add the missing path (Q12.3.3).

### 6.4 Changing the CI configuration

- A pull request of its own, of type `ci:`, with the reason in the commit message or in this document.
- It runs every check before merging (`all`).
- After it merges, watch the first run on main. If the measurements (§8) change noticeably, update quality status §8.

## 7. Failures

- **A failure on main** is the highest-priority defect (Q12.8). Find the merge that introduced it, and merge nothing it may affect until it is fixed.
- **Runs on main are never left cancelled.** Every merge needs a complete result (Q12.4); a run cancelled by hand is run again.
- **Intermittent failures**: reproduce first, by repeating locally, or by a temporary repetition step in CI removed before merging. Retries must not hide them. A test that is wrong gets fixed; a product that is wrong gets fixed. What cannot be fixed at once gets an issue, noted in the test.

## 8. Measurements

`ci-stats.yml` measures the latest runs every week: time from push to result (queueing included), job durations, failures and cancellations. The results go into that run's summary. When they exceed the budgets in quality status §8.4, adjust as Q12.2 and Q12.6 say.

## 9. Current deviations and fixes (2026-10-10)

Against `main` (`45506e8`):

| Deviation | Breaks | Fix |
| --- | --- | --- |
| `runtimes-bun` has no selection condition and is not in `ci-ok`'s `needs`. Every pull request, documentation-only ones included, runs it on 2 macOS runners, and a failure does not block the merge. #200 and #213 merged at the same time, and the Bun job was not connected to selection | Q12.3, Q12.5, Q12.6.2 | Per §6.2: a new `bun` output; `runtimes-bun` gets its condition and a place in `ci-ok`; Linux only on pull requests |
| Bun tests run three times on macOS: `network-macos` (bridge with every feature, host) and both versions in `runtimes-bun`. On Linux, `test` and `runtimes-bun` overlap too | Q12.7 | `network-macos`'s loader step gains `bun`; the oldest Bun on macOS runs only on main |
| Four runs on main, `45506e8` among them, were cancelled and have no complete result | Q12.4, Q12.8 | Run the latest commit on main again |
| Quality status §8.4 runs dylib-windows on merges to main, but `dylib-windows.yml` has no `push` trigger for main | §8.4 | Add a `push` trigger for main, or change §8.4 to tags only (to be decided) |

The fixes go in a pull request of their own, as §6.4 says.
