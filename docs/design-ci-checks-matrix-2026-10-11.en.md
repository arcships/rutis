# CI Static Checks and the Minimum Version Matrix (Design Draft)

[中文](design-ci-checks-matrix-2026-10-11.md)

Status: design draft, under review (#204). Date: 2026-10-11. Baseline: `main` `0941ef4`.
Based on: [the quality standard](quality-standard.md) Q5.3.7, Q10.2, Q10.3, §12; [the CI document](ci.en.md) (#237); the "scope adjustment (2026-10-10)" section of #204. Part of step 1 of #183.

Hard constraints (from #204):

- an ordinary code PR gets its result ≤ 10 minutes after the push, queueing included;
- no new macOS or Windows jobs on PRs;
- new checks go into parallel Linux jobs and do not lengthen the critical path;
- replace rather than add.

Out of scope: E2E jobs (#186, #193); nightly TLC (`stress.yml`); `rutis-host` tests on Windows (one line in `runtimes-windows`, done separately); actionlint; Go's `govulncheck`; Python style checks.

## 1. Decisions

| # | Decision | Section |
| --- | --- | --- |
| D1 | Versions by machine: Linux jobs use each language's oldest supported version, macOS and Windows the newest | 3 |
| D2 | websockets: Linux installs `websockets==15.*` (the declared lower bound), everywhere else `websockets>=15` | 3 |
| D3 | One new Linux job `lint`, run when the `code` switch is on: `cargo fmt --check`, `cargo clippy -D warnings`, a version consistency check; plus `cargo deny check licenses bans sources` when `all` is on | 4, 6 |
| D4 | The clippy baseline lives in the code: `#[expect(<lint>, reason = "lint-baseline(#<issue>): …")]`. The number of entries may only go down, except in a PR that upgrades the toolchain | 4 |
| D5 | MSRV declared per crate at the version that actually works (measured: 1.85 / 1.88 / 1.90); a new job `msrv` runs when `all` is on (main, and PRs that change `Cargo.lock`, the root `Cargo.toml` or CI) | 5 |
| D6 | Vulnerability checks (`cargo deny check advisories`, `npm audit`, `pip-audit`) go into a new weekly workflow `deps.yml`; a failure opens an issue (or comments on the open one) and does not block merging | 6 |
| D7 | Minimum versions come from the packages' declarations (`engines`, `requires-python`, `websockets>=`, `rust-version`, `rust-toolchain.toml`); `tools/check-ci-versions.mjs` compares the versions written in the workflows against them, in `lint` | 7 |
| D8 | `tools/ci-stats.mjs` splits PR runs into "docs only / ordinary code / dylib / everything"; acceptance looks at "ordinary code" | 9 |
| D9 | Drop `cargo check --workspace --all-targets` from `checks`: the clippy run in `lint` compiles the same code | 4 |

## 2. Current state (measured)

### 2.1 PR times

`node tools/ci-stats.mjs 40 ci.yml`, run on 2026-10-11, covering the 40 runs after #237 merged (2026-10-10 12:58 UTC).

The tool reports a `pull_request` median of 19.5 minutes, but that mixes three kinds of run: PRs with `all` on (28 jobs, about 20–40 minutes), ordinary code PRs, and rerun runs (a rerun counts from when the first attempt was created, e.g. run `38056277205` shows 659 minutes). Ordinary code PRs only (7–9 jobs ran, first attempt, success):

| run | branch | push to result |
| --- | --- | ---: |
| 38097374545 | fix/238-multilang-go-flaky | 5.2 |
| 38056775967 | fix/233-exit-status-flaky | 5.6 |
| 38055889193 | fix/239-handshake-flaky | 7.4 |
| 38098778901 | fix/173-local-line-limit | 8.7 |
| 38098889402 | fix/184-concurrent-launch | 8.7 |
| 38100944545 | fix/247-row-self-dispose | 8.8 |

Median about 8.0 minutes (6 runs, a small sample). The critical path is `rust` (about 4.5 minutes) and `runtimes-windows` (4.2–5.1 minutes). Linux queues too: in run `38098889402` `changes` waited 1.8 minutes to start, in run `38055889193` `ci-ok` waited 2.3 minutes. The cause is that every push to main takes 28 jobs at once. So an ordinary code PR has about 2 minutes left below 10 minutes today, and Linux queueing is what uses most of it.

Median job durations (same measurement): `runtimes-windows` 5.6, `rust` 4.8, `network-macos` 3.9, `checks` 2.4, `runtimes-go` (Linux) 1.7, `runtimes-bun` (Linux) 1.1, `js-py` 0.6 minutes.

### 2.2 fmt

`cargo fmt --all --check` (toolchain 1.98.1): 4 differences in 2 files: `crates/rutis-xtask/src/main.rs` (3), `examples/native-mount/tests/cordis_mount.rs` (1).

### 2.3 clippy

`cargo clippy --workspace --all-targets --all-features` (1.98.1, macOS arm64, 16 cores, empty target directory, 56 seconds): 22 warnings, 9 lints, 10 files, no errors.

| lint | count | where |
| --- | ---: | --- |
| `clippy::await_holding_lock` | 7 | `crates/rutis-agent/tests/session_persist.rs` (the test holds `llm.calls.lock()` across an `await`) |
| `dead_code` | 5 | `tests/dylib-fixtures/greeter-v2/src/lib.rs` (4, see below); `crates/rutis-loader/tests/migration_example.rs` (1) |
| `clippy::doc_lazy_continuation` | 3 | `crates/rutis-agent/src/driver.rs`, `session.rs` |
| `clippy::missing_safety_doc` | 2 | `crates/rutis-sdk/src/lib.rs`, `crates/rutis-dylib/src/loader.rs` |
| `clippy::type_complexity` | 1 | `crates/rutis-bridge/tests/memory_mux.rs` |
| `clippy::map_flatten`, `unnecessary_mut_passed`, `items_after_test_module` | 1 each | `crates/rutis-agent/src/driver.rs`, `tui.rs` |
| `clippy::redundant_closure` | 1 | `crates/rutis-cli/src/main.rs` |

By crate: `rutis-agent` 13, the greeter-v2 fixture 4, `rutis-sdk`, `rutis-dylib`, `rutis-loader`, `rutis-bridge`, `rutis-cli` 1 each. The kernel `rutis` has none.

Two prerequisites showed up while measuring; CI has them too:

1. `--all-features` turns on the `export` feature of `tests/dylib-fixtures/greeter-*`, which reads `RUTIS_SDK_ARTIFACT_SHA256` at compile time and fails without it. CI sets it to 64 `0`s, as `sdk-repro` does.
2. The build scripts of `examples/native-mount` and `examples/interop-experiments` need `npm --prefix node/rutis-runtime ci` first; `rutis-dsh` compiles its web host only when dsh is installed (`npm --prefix crates/rutis-dsh/dsh ci`). Like the `checks` job, `lint` installs Node and these two packages first.

The 4 `dead_code` warnings in greeter-v2 appear only under `--all-features`: `changed_identity`, `fail_once` and the like are different versions of one fixture, not additive features, and with all of them on some code is unused.

### 2.4 MSRV

Declared: `rust-version = "1.85"` in the root `Cargo.toml`, inherited by every crate. Installed locally 1.85.1, 1.88.0 and 1.90.0 (`--profile minimal`) and checked with the current `Cargo.lock` (`--locked`):

| command | result |
| --- | --- |
| `cargo +1.85 check --workspace` | fails: dependencies need a newer rustc, e.g. `icu_*` 2.3 and `darling` 0.24 need 1.88, `idna_adapter` 1.2.2 needs 1.86, `tree-sitter-language` 0.1.8 needs 1.90 |
| `cargo +1.85 check -p rutis -p rutis-bridge --all-features` | passes |
| `cargo +1.85 check -p rutis-dylib-meta -p rutis-sdk -p rutis-dylib -p rutis-dylib-launcher` (default features) | passes |
| `cargo +1.85 check -p rutis -p rutis-bridge --all-features --all-targets` | fails: the test dependencies `rcgen` 0.14 and `time` 0.3.55 need 1.88 |
| `cargo +1.88 check -p rutis-loader -p rutis-host --all-features` | passes |
| `cargo +1.88 check --workspace --exclude rutis-agent --exclude rutis-cli` | passes |
| `cargo +1.88 check -p rutis-agent` | fails: `tree-sitter-language` 0.1.8 needs 1.90 |
| `cargo +1.90.0 check --workspace` | passes |

`rutis-loader`, `rutis-host`, `rutis-dsh` and `aimux-llm` need 1.88 through `url` → `idna` → `icu_*`; `rutis-agent` and `rutis-cli` need 1.90 through rutui's `tree-sitter-language`.

Not verified: Linux and Windows targets; `--all-features` for `rutis-sdk` and `rutis-dylib*` on 1.85; which versions a user's own resolution (without our `Cargo.lock`) picks.

### 2.5 Dependencies

- `cargo deny --all-features check advisories` (cargo-deny 0.20.2, no `deny.toml`): no vulnerabilities; 4 "unmaintained": RUSTSEC-2025-0052 (async-std), RUSTSEC-2025-0141 (bincode), RUSTSEC-2024-0436 (paste), RUSTSEC-2024-0320 (yaml-rust), all through `rutis-agent` and rutui.
- Licenses (`cargo deny list`): MIT, Apache-2.0 (including the LLVM exception), BSD-2/3-Clause, ISC, Unicode-3.0, Zlib, Unlicense, CC0-1.0, MIT-0, 0BSD, BSL-1.0, CDLA-Permissive-2.0, MPL-2.0 (`nucleo`, `nucleo-matcher`, `option-ext`). `r-efi` is MIT / Apache-2.0 / LGPL-2.1-or-later, any one of the three. Only the unpublished fixtures and `rutis-xtask` have no license. The `licenses` check has not yet been run with a real `deny.toml`.
- `npm audit`: `node/rutis-runtime` and `node/baseline` clean; `crates/rutis-dsh/dsh` (`private`, not published) has 13 (7 moderate, 6 high, all in runtime dependencies, e.g. `@modelcontextprotocol/client`, `fflate`, `http-cache-semantics`).
- `pip-audit` (2.10.1): no known vulnerabilities in `websockets==15.0.1`. `python/rutis` itself has no dependencies.

### 2.6 Where versions are written today

| place | Node | Python | websockets | other |
| --- | --- | --- | --- | --- |
| `ci.yml` `rust`, `js-py`, `checks` | 24 | 3.12 | `>=13` | Bun 1.4.0, Go oldstable |
| `ci.yml` `runtimes-go`, `runtimes-bun` | 24 | 3.12 | `>=13` (go only) | their own matrices |
| `ci.yml` `network-macos` | 26 | 3.12 | `>=13` | Bun latest, Go stable |
| `ci.yml` `runtimes-windows` | 24 | 3.12 | `>=13` | Go stable |
| `ci.yml` `release-*` | 24 | — | — | same as `release.yml` |
| `release.yml` | 24 | — | — | Rust 1.98.1, Go stable |
| `release-cli.yml` | — | — | — | `dtolnay/rust-toolchain@stable` (1.98.1 everywhere else) |
| `stress.yml` (`multiprocess` of #245) | 24 | 3.12 | yes | says "same as the `rust` job" |
| declarations | `engines.node` `>=22` (#219 merged) | `requires-python >=3.12` (#196 not merged) | `network = ["websockets>=15"]` (#218 merged) | `rust-version = "1.85"`; `rust-toolchain.toml` 1.98.1; `engines.bun` |

The declarations have changed and CI has not: the Node minimum is 22 and CI runs 24; the websockets lower bound is 15 and CI installs `>=13`. Comments in `ci.yml` still say "lower to 22 when #219 lands" and "pin 15.* when #218 lands". This is what D7 is meant to prevent.

## 3. The version matrix

Rule (D1): for each language, Linux jobs use the oldest supported version, macOS and Windows the newest. Linux runners are plentiful, so the minimum is verified on every PR; macOS and Windows take one job each per run, and verify the newest there. No new jobs, only different version numbers.

| job | machine | Node | Python | websockets | Bun | Go |
| --- | --- | --- | --- | --- | --- | --- |
| `rust` | Linux | 24 → **22** | 3.12 (**3.10** after #196) | `>=13` → **`==15.*`** | 1.4.0 | oldstable |
| `js-py` | Linux | 24 → **22** | as above | `>=13` → **`==15.*`** | — | — |
| `checks` | Linux | 24 → **22** (build only) | — | — | — | — |
| `runtimes-go` (matrix) | Linux; plus macOS on main | 24 → **22** | as `rust` | `>=13` → **`==15.*`** | — | matrix |
| `runtimes-bun` (matrix) | Linux; plus macOS on main | 24 → **22** | as `rust` | — | matrix | — |
| `network-macos` | macOS | 26 (unchanged) | 3.12 → **`3.x`** | `>=13` → **`>=15`** | latest | stable |
| `runtimes-windows` | Windows | 24 → **26** (open, see §13 item 3) | 3.12 → **`3.x`** (open) | `>=13` → **`>=15`** | — | stable |
| `release-dry-run`, `release-windows` | Linux, Windows | 24 (unchanged, follows `release.yml`) | — | — | — | — |
| `stress.yml` `multiprocess` | Linux | as `rust` | as `rust` | as `rust` | as `rust` | as `rust` |

Notes:

- `runtimes-go` and `runtimes-bun` vary only their own language; Node and Python there are only the other side of cross-language rows and use the minimum, in the macOS matrix entries too.
- Python's "newest" is written `3.x` (`actions/setup-python` takes the newest stable release), not a fixed number. The first PR after a release uses it; if it makes the PR fail, that is the problem Q10.2 is there to find: fix it in its own PR, and until then the previous version may be pinned with an issue opened.
- Node's "newest" stays a major version number (26 now), raised by hand when a new even major is released.
- Node in `release.yml` and the `release-*` jobs is the version of the publishing tool (`npm publish --provenance`), not part of the support matrix; it stays 24, the same in all three places.
- `release-cli.yml`'s `dtolnay/rust-toolchain@stable` becomes `@1.98.1`. The repository root has `rust-toolchain.toml`, so cargo runs 1.98.1 anyway; `@stable` only installs a toolchain that is not used.
- When #196 merges, Python on Linux goes from 3.12 to 3.10. Because of the D7 check, #196 changing `requires-python` without changing the workflow makes `lint` fail; so either the #196 PR makes this change too (an exception to "only the #203 / #204 PRs edit `ci.yml`", which needs the maintainer's agreement), or this issue follows up right after #196 merges.
- Effect on the critical path: none. Changing versions adds no steps; the `ubuntu-24.04` image ships Node 22 and Python 3.10, and `pip install "websockets==15.*"` takes seconds.

## 4. fmt and clippy: the `lint` job

### 4.1 The job

```yaml
lint:
  needs: changes
  if: needs.changes.outputs.code == 'true'
  runs-on: ubuntu-24.04
```

Steps, in order:

1. `actions/checkout@v4` (`fetch-depth: 0`; the baseline comparison needs the PR's base commit).
2. `dtolnay/rust-toolchain@1.98.1`, `components: rustfmt, clippy`.
3. `Swatinem/rust-cache@v2`, `save-if: github.ref == 'refs/heads/main'` (as every other job).
4. `node tools/check-ci-versions.mjs` (§7, about 1 second).
5. `cargo fmt --all --check` (about 5 seconds). Before clippy, so bad formatting fails at once.
6. The baseline entry count check (§4.3, about 1 second).
7. `actions/setup-node@v4` (Node 22), `npm --prefix node/rutis-runtime ci`, `npm --prefix crates/rutis-dsh/dsh ci` (§2.3, prerequisite 2).
8. `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, with `RUTIS_SDK_ARTIFACT_SHA256` set to 64 `0`s (§2.3, prerequisite 1). `-D warnings` goes after `--`, not into `RUSTFLAGS`: changing `RUSTFLAGS` rebuilds every dependency and defeats the cache.
9. Only when `all` is on: `cargo deny --all-features check licenses bans sources` (§6).

`lint` is added to the `needs` of `ci-ok`.

Only clippy's default lint groups; no `pedantic` or `nursery`.

It uses the `code` switch, not a new "Rust changed" switch: a PR that only changes Python or Node also runs `lint`, which takes one more Linux runner but no extra waiting time, and saves one switch and one more path list to maintain.

### 4.2 Time

- With a cache (once main has saved one): about 3–4 minutes, estimated. For comparison, `checks` takes 2.4 minutes with a cache for one workspace-wide `cargo check` plus 15 feature-combination checks; `lint` is one workspace-wide clippy run with all features, plus Node and two `npm ci` (about 30 seconds).
- Without a cache (the first run on main after merging, or after the cache is evicted): about 5–7 minutes, estimated, longer than `rust`, so that run's critical path.
- Effect on the critical path of an ordinary code PR: none with a cache (`rust` and `runtimes-windows` take about 4.5–5 minutes); one more Linux runner (a PR run goes from 8 jobs to 9).
- After merging, `ci-stats` shows `lint`'s median duration. If it exceeds `rust`, in this order: check that the cache is hit; then split clippy into two parallel jobs (for example `rutis-agent`, `rutis-cli`, `rutis-dsh` in one, the rest in the other).

D9: drop `cargo check --workspace --all-targets` from `checks`. clippy already compiles every target of the workspace with all features; the default-feature build is covered by `cargo test --workspace` in `rust`, and `checks` still checks the single-feature combinations of bridge and loader. That saves about a minute on main. The same command stays in `network-macos` and `runtimes-windows`, where it checks those platforms' builds.

### 4.3 The baseline

There are only 22 warnings today (§2.3), and all can be fixed. The recommendation is to fix them all in the first phase and start the baseline at 0 (§13 item 2); the mechanism below is still needed, because a toolchain upgrade brings new lints.

**How an entry is written.** A baseline entry is an attribute in the code, on the smallest scope possible (a function or a statement, not a whole file or crate):

```rust
#[expect(clippy::await_holding_lock, reason = "lint-baseline(#NNN): the test holds a lock across await")]
```

- `expect` rather than `allow`: once the code is fixed and the lint no longer fires, `expect` produces an `unfulfilled_lint_expectations` warning, `-D warnings` fails `lint`, and the entry has to be deleted. So the baseline never keeps entries that no longer apply. `expect` and `reason` are stable since Rust 1.81, below the MSRV.
- `reason` starts with `lint-baseline(#<issue>)`, the issue that tracks the entries.
- Note: for clippy lints (`clippy::…`), `expect` is checked only when clippy runs; for rustc's own lints (such as `dead_code`) it is checked on every build. A rustc lint that fires only under some feature combination cannot take an unconditional `expect`; use `cfg_attr(<that combination>, expect(...))`, or fix the code.

**How it is kept from growing.** Step 6 of `lint` counts `git grep -c 'lint-baseline('`: on a PR it counts both the PR's base commit and the head, and fails if the head has more entries than the base, except when the PR changes `rust-toolchain.toml` (new lints of a new toolchain). On pushes to main it only writes the count to the job summary.

**`allow` outside the baseline.** When new code really needs a lint off, it uses an ordinary `#[allow(<lint>, reason = "…")]` with the reason, reviewed with the code. It has no `lint-baseline(` marker and does not count towards the baseline. For example, the features of the greeter-v2 fixture are mutually exclusive versions, and with all of them on some code is unused: that is an ordinary `allow`, not a baseline entry.

**Who owns it, how it is tightened.** Each entry belongs to the crate it is in, and whoever changes that crate deletes it on the way; the tracking issue lists every entry. The count is in the job summary of every `lint` run on main. The target is 0; entries brought in by a toolchain upgrade are cleared before the next upgrade.

## 5. MSRV: the `msrv` job

### 5.1 What to do since 1.85 fails

Measured (§2.4): 1.85 cannot build the whole workspace. Under Q10.3 the declaration must change to versions that are verified. Two ways:

- **A: one value for the whole workspace, raised to 1.90.** Simplest; but the kernel `rutis` and `rutis-bridge` do build on 1.85, and "the core needs Rust 1.85" in the README and guide would become 1.90: users who only embed the kernel in Rust would be asked for 5 more versions than needed.
- **B: declared per crate (recommended).** The root `Cargo.toml` keeps `rust-version = "1.85"`, and these crates override it in their own `Cargo.toml`:

  | rust-version | crates |
  | --- | --- |
  | 1.85 (inherited) | `rutis`, `rutis-bridge`, `rutis-sdk`, `rutis-dylib`, `rutis-dylib-meta`, `rutis-dylib-launcher`, `rutis-dev`, `rutis-xtask` |
  | 1.88 | `rutis-loader`, `rutis-host`, `rutis-dsh`, `aimux-llm` |
  | 1.90 | `rutis-agent`, `rutis-cli` |

  "The core needs Rust 1.85" in the README stays; "Rust 1.85 or later, only when embedding rutis in Rust" in `docs/guide/README.md` and its English version is written per crate (1.85 to embed the kernel and bridge, 1.88 with loader or host).

The MSRV promise covers libraries and binaries building, not tests: the test dependencies (`rcgen`, `time`) need 1.88, so the check does not use `--all-targets`.

### 5.2 The job

```yaml
msrv:
  needs: changes
  if: needs.changes.outputs.all == 'true'
  runs-on: ubuntu-24.04
```

- One step, `node tools/msrv-check.mjs`: it reads each workspace member's `rust-version` from `cargo metadata --no-deps` and groups members by version; for each group it runs `rustup toolchain install <version> --profile minimal`, then `cargo +<version> check --locked --all-features -p <the group's crates>…`. Versions are written only in `Cargo.toml`, never repeated in the workflow.
- `RUTIS_SDK_ARTIFACT_SHA256` as in `lint`; Node with `npm ci` of `node/rutis-runtime` and dsh (the build script of `rutis-dsh`).
- `Swatinem/rust-cache@v2`, `save-if` as every other job.
- Added to the `needs` of `ci-ok`.
- Time: about 20 seconds per toolchain install; 2–3 minutes of compiling per group without a cache; 5–8 minutes in total, estimated. Only when `all` is on, never on ordinary PRs.

**Why `all` and not main only.** A dependency needing a newer rustc almost always comes from a change to `Cargo.lock` or `Cargo.toml` (a new dependency, `cargo update`). Those PRs already turn `all` on and run every job (about 25 minutes); `msrv` runs in parallel, adds nothing to their time, and stops the problem before merging. Only "code uses a standard library API from a newer release" is found after merging, on main.

**When it fails later.** In the same PR, one of two, written in the PR description: hold that dependency at an older version (`cargo update -p <dependency> --precise <version>`); or raise that crate's `rust-version` and update what the README and guide say. Raising `rust-version` is visible to users and is the maintainer's call.

## 6. Dependency checks

| check | where | when | on failure |
| --- | --- | --- | --- |
| `cargo deny check licenses bans sources` | a step of `lint` in `ci.yml` | when `all` is on: main, and PRs that change `Cargo.lock`, the root `Cargo.toml` or CI | like any other check: `ci-ok` fails and blocks merging |
| `cargo deny check advisories` | job `cargo-deny` of the new workflow `deps.yml` | weekly; by hand; PRs that change `deny.toml` or `deps.yml` | opens an issue, does not block merging |
| `npm audit --omit=dev` | job `npm-audit` of `deps.yml`, one matrix entry per lockfile | as above | as above |
| `pip-audit` | job `pip-audit` of `deps.yml` | as above | as above |

**Why split this way.** The result of `licenses`, `bans` and `sources` depends only on `Cargo.lock`: if `Cargo.lock` does not change, neither does the result, so they run in the runs where `Cargo.lock` may change (`all`) and can block merging. The vulnerability databases change every day, so `advisories`, `npm audit` and `pip-audit` can change their result while the code stays the same; on PRs, one new advisory would make every unrelated PR fail at once. So they run weekly and the maintainer handles what they find.

**`deny.toml` (a new file at the repository root).** Main points:

- `[graph] all-features = true`.
- `[licenses]`: `allow` lists the licenses found in §2.5; whether MPL-2.0 is allowed is the maintainer's call (§13 item 7); `private.ignore = true`, so unpublished crates (fixtures, `rutis-xtask`, examples) are not checked.
- `[advisories]`: `ignore` lists the 4 current "unmaintained" advisories (§2.5), each with a reason and a tracking issue, e.g. `{ id = "RUSTSEC-2024-0320", reason = "yaml-rust, through rutui; tracked in #NNN" }`. Any new "unmaintained" or "unsound" advisory and every vulnerability fail the weekly check.
- `[bans]`: `multiple-versions = "allow"` (there are many duplicate versions now, e.g. `windows-sys`; not this issue's job); `wildcards = "deny"` with `allow-wildcard-paths = true` (path dependencies inside the workspace).
- `[sources]`: crates.io only; an unknown registry or git source fails.
- This configuration has not been run yet; the implementation runs it locally first, then in CI.

**`deps.yml`.**

- Triggers: `schedule` (Mondays 02:00 UTC, after `ci-stats.yml` at 01:00); `workflow_dispatch`; `pull_request` with `paths` `deny.toml` and `.github/workflows/deps.yml` only, to verify changes to the configuration before merging.
- `cargo-deny`: `taiki-e/install-action@cargo-deny`, `cargo deny --all-features check advisories`. About 1 minute.
- `npm-audit`: matrix of `node/rutis-runtime` (the dependencies of the published `@arcships/rutis-runtime`, which `@arcships/rutis-host` pulls in), `node/baseline` (tests), `crates/rutis-dsh/dsh` (not published). `npm audit --omit=dev --audit-level=high`. dsh has 6 high findings now; until they are dealt with, its entry only writes the result to the summary and does not fail (§13 item 6).
- `pip-audit`: Python `3.x`, `pip install "./python/rutis[network]"`, then `pip-audit`; once more with `websockets==15.*` to check the declared lower bound.
- `report`: `needs` the three jobs above, `if: failure()`, permission `issues: write`. It looks up an open issue with `gh issue list --label dependencies --state open`, comments on it if there is one, otherwise opens one titled "Weekly dependency check failed", with the failed jobs, the findings (from each job's summary) and the run link. The comment says separately whether a tool failed (e.g. the advisory database could not be downloaded) or something was found.

**Who is notified, how fast.** A new issue is assigned to the maintainer (the account is written in `deps.yml`, §13 item 5); later comments notify everyone subscribed to the issue. Handling: a vulnerability in what is published (the train's crates, the dependencies of `node/rutis-runtime`, `websockets`) is fixed within 7 days by upgrading the dependency, or ignored in `deny.toml` / the audit command with a reason and an expiry date; whether a high-severity one needs a patch release is the maintainer's call under the release gate of the quality standard §13. `deps.yml` is not in `ci-ok` and does not block merging.

## 7. Where versions live, and how they stay consistent

**The only source is what the packages declare:**

| what | declared in |
| --- | --- |
| Node minimum | `engines.node` in the `package.json` of `node/rutis`, `node/rutis-runtime`, `node/rutis-host` |
| Bun minimum | `engines.bun` in `bun/rutis-bun/package.json` |
| Python minimum | `requires-python` in `python/rutis/pyproject.toml` |
| websockets lower bound | `[project.optional-dependencies] network` in `python/rutis/pyproject.toml` |
| Rust toolchain | `channel` in `rust-toolchain.toml` |
| MSRV | each crate's `rust-version` (read directly by the `msrv` job, §5.2) |
| Node for publishing | the `npm` job of `release.yml` |

`uses:` and `with:` in GitHub Actions cannot read files, so the workflows still contain literal values. A new script, `tools/check-ci-versions.mjs` (Node standard library only, reading the YAML line by line), checks the literals against the declarations, as step 4 of `lint`:

1. Every `dtolnay/rust-toolchain@<version>` in `ci.yml`, `stress.yml`, `release.yml`, `release-cli.yml`, `dylib-windows.yml` equals `channel` in `rust-toolchain.toml`.
2. The `node-version` of the "minimum version jobs" equals the major version of the lower bound of `engines.node`. Which jobs are minimum version jobs is a list in the script (`rust`, `js-py`, `checks`, `runtimes-go`, `runtimes-bun`, and `multiprocess` in `stress.yml`), reviewed like code.
3. The `python-version` of the minimum version jobs equals the lower bound of `requires-python`; they install websockets as `==<lower bound>.*`; every other job writes `>=<lower bound>`; no version below the lower bound appears anywhere.
4. The oldest Bun in the `rust` job and in `bun-matrix` equals the lower bound of `engines.bun`.
5. The `node-version` of `release-dry-run` and `release-windows` equals that of `release.yml`.
6. On a mismatch it prints the file, line, expected value and where it is declared, and exits non-zero.

So when a declaration changes (e.g. #196 lowering Python to 3.10) and CI is forgotten, `lint` fails; "declares 22, CI runs 24" cannot happen again.

Not chosen: versions in an `env` block at the top of each workflow. `env` cannot be used in `runs-on` or matrices, and the three workflows would still each carry a copy that people have to keep in step. A composite action installing every language would remove duplication, but touches every job and does not replace the check above; it can come later if needed.

`stress.yml`: the `multiprocess` job of #245 says it uses the `rust` job's versions. Whichever of #245 and this issue's implementation merges second makes the two agree; the script lists `multiprocess` as a minimum version job, so `lint` fails while they disagree.

## 8. Changes to `docs/ci.md`

Chinese and English together (`docs/ci.md`, `docs/ci.en.md`):

- §2 workflow table: add `deps.yml` (weekly; by hand; PRs that change `deny.toml`; vulnerability checks).
- §4 job table: add `lint` (Linux, `code`; on main also `cargo deny check licenses bans sources`) and `msrv` (Linux, only with `all`); rewrite the version notes of `rust`, `js-py`, `network-macos`, `runtimes-windows` per §3 and remove outdated notes such as "lower to 22 when #219 lands"; drop `cargo check --workspace --all-targets` from `checks`.
- §4 "what each change runs on a PR": add `lint` to the kernel, bridge/loader/host and Python package rows; expected times unchanged.
- A new section "Versions": the rule of §3 (Linux oldest, macOS and Windows newest), the declaration table of §7, and what `tools/check-ci-versions.mjs` checks. The document states the rule and where versions are declared, not the numbers, so that the document itself does not go stale.
- §6 "adding a CI check": a check whose result changes with outside data (an advisory database, a new release) and can fail while the code stays the same goes weekly, not on PRs.
- §6: "the clippy baseline": how entries are written (§4.3), the count rule, what to do when upgrading the toolchain.
- §6 "adding a language runtime", step 2: also add the language's declaration to `tools/check-ci-versions.mjs`.
- §7 failures: handling the `deps.yml` issue (§6); the two ways out when `msrv` fails (§5.2).

`docs/quality-status.md` is not changed (per the working agreement, it is updated at the end of each step).

## 9. Measurement

**Changes to `tools/ci-stats.mjs`:**

1. Split `pull_request` runs into four kinds, by which jobs actually ran:
   - docs only: `rust` did not run;
   - ordinary code: `rust` ran, `checks` did not, and no job whose name starts with `dylib-` ran;
   - dylib: a `dylib-` job ran, `checks` did not;
   - everything: `checks` ran.

   The "push to result" table gets one row per kind, plus `push`.
2. A rerun run (`run_attempt > 1`) counts from that attempt's `run_started_at`, not from when the first attempt was created, and is counted separately in the table.
3. A new option `--since <date>`: only runs created after that date.
4. A one-line conclusion: `ordinary code PRs: median X min, p90 Y, N runs; target ≤ 10`.

**Baseline:** before this change (§2.1), the ordinary code PR median is about 8.0 minutes (6 runs).

**Acceptance:** call the merge date of the first phase T. At T + 7 days, `node tools/ci-stats.mjs 100 ci.yml --since T` (the weekly `ci-stats.yml` runs it too) shows an ordinary code PR median ≤ 10.0 minutes. With fewer than 10 runs, extend by a week. The results (median, p90, median durations of `lint` and `rust`, Linux queue times) go into a comment on #204.

**If over:** first check whether `lint` became the critical path (handling in §4.2); then whether Linux queueing grew (every push to main takes 28 jobs, and `lint` and `msrv` add one each). Queueing is not solved in this issue: record the data and hand it to #183.

## 10. Risks

| risk | consequence | handling |
| --- | --- | --- |
| Linux concurrency limit | a main run takes 28 jobs and PR jobs queue (1.8–2.3 minutes already seen, §2.1). `lint` adds one job per PR, `msrv` one per main run | `msrv` and the vulnerability checks stay off ordinary PRs; acceptance looks at queue times separately |
| `lint` without a cache becomes the critical path | that PR takes 1–3 minutes longer | the cache is saved on main; if it stays over, split into two jobs (§4.2) |
| One more Rust cache | the repository's 10 GB cache space gets tighter and main's other caches may be evicted | check Actions cache usage after merging; if needed set `cache-targets: false` on `lint` so it caches only dependencies |
| clippy runs only on Linux | code under `cfg(windows)` or `cfg(target_os = "macos")` is not linted | accepted; no clippy on the scarce macOS and Windows runners |
| Floating "newest" on macOS and Windows (`3.x`, Bun latest, Go stable) | after a release, unrelated PRs fail | that is what Q10.2 is there to find; fix in its own PR, pin the previous version with an issue if needed |
| New clippy lints from a toolchain upgrade | the upgrade PR grows | the upgrade PR may add baseline entries (§4.3), cleared before the next upgrade |
| MSRV verified only with our `Cargo.lock` | a user resolving on their own may get versions that need a newer rustc | the guide says so: users of older Rust can `cargo update` with Cargo's `incompatible-rust-versions = "fallback"` |
| Code uses a newer standard library API | found only on main | handled as any failure on main, `docs/ci.md` §7 |
| Nobody looks at weekly failures | vulnerabilities stay | an issue, assigned (§6); also visible in the weekly `ci-stats` run |
| Conflicts with open PRs | #196 changes the Python minimum, #245 changes `stress.yml` | per §3 and §7: the check script fails `lint` while they disagree |

## 11. Phases

After the design is approved, the implementation is added to this PR, one commit per phase, in order:

1. **Versions**: the version changes of §3 (without Python 3.10); `release-cli.yml` to `@1.98.1`; `tools/check-ci-versions.mjs`.
2. **fmt, clippy**: fix the formatting of the 2 files; fix the clippy warnings (per §13 item 2); the new `lint` job, in `ci-ok`; the baseline count; drop the covered command from `checks`.
3. **MSRV**: change `rust-version` per §13 item 1; `tools/msrv-check.mjs`; the new `msrv` job, in `ci-ok`; the Rust version statements in the README and guide.
4. **Dependencies**: `deny.toml`; the `licenses bans sources` step in `lint`; `deps.yml`; one `workflow_dispatch` run on this branch.
5. **Measurement and docs**: the `tools/ci-stats.mjs` changes; `docs/ci.md`, `docs/ci.en.md`.

After that:

- When #196 merges: Python on Linux to 3.10 (§3).
- T + 7 days: the acceptance of §9, results on #204.
- If the baseline is not 0: open the tracking issue and tighten per §4.3.

This PR changes `.github/workflows/**`, so it runs every job; the part that only ordinary PRs show (`lint`'s time on an ordinary PR) is read from the first ordinary PR after merging.

## 12. Acceptance criteria (all checkable by command)

1. `git grep -n 'websockets>=13' .github` prints nothing.
2. `node tools/check-ci-versions.mjs` exits 0; after changing one minimum version job's `node-version` to 24 it exits non-zero.
3. The jobs in `ci.yml` whose `runs-on` is macOS or Windows are the same as at `0941ef4` (a script compares job names and `runs-on` between the two versions).
4. `ci.yml` has jobs `lint` and `msrv`, both in the `needs` of `ci-ok`; `lint`'s condition is `needs.changes.outputs.code == 'true'`, `msrv`'s is `needs.changes.outputs.all == 'true'`.
5. The first run on main after merging: `lint` and `msrv` succeed; `cargo fmt --all --check` and `cargo clippy … -- -D warnings` also pass locally.
6. `git grep -c 'lint-baseline('` equals the number of entries listed in the tracking issue (0 with the recommended choice).
7. The set of versions `tools/msrv-check.mjs` checks equals the set of all `rust_version` values in `cargo metadata --no-deps`.
8. `deps.yml` has `schedule` and `workflow_dispatch`; one manual run on this branch either succeeds or opens an issue labelled `dependencies`.
9. The `links` job passes (links in `docs/ci.md`, `docs/ci.en.md` and this design).
10. At T + 7 days: `node tools/ci-stats.mjs 100 ci.yml --since T` shows an "ordinary code PR" median ≤ 10.0 minutes over ≥ 10 runs.

## 13. Decisions for the maintainer

| # | question | options | recommendation |
| --- | --- | --- | --- |
| 1 | MSRV 1.85 cannot build the whole workspace | A: 1.90 everywhere; B: per crate 1.85 / 1.88 / 1.90 (§5.1) | **B**. The kernel and bridge are measured to build on 1.85, so the README's promise can stay; the cost is 3 toolchains in the `msrv` job, which runs only with `all` |
| 2 | The 22 existing clippy warnings | fix them all and start the baseline at 0; or make them all baseline entries and remove them over time | **Fix them all**. They are all mechanical (lock scope, doc indentation, `# Safety` sections, …), about 10 files; the baseline mechanism stays for future toolchain upgrades |
| 3 | Node and Python on Windows | newest (Node 26, Python `3.x`); or keep 24 and 3.12 | **Newest**, one rule: "Linux oldest, macOS and Windows newest". Keeping them means Windows verifies a version that is neither the oldest nor the newest |
| 4 | Where `cargo deny` goes | everything weekly (#204's table); or `licenses bans sources` blocking on `all`, `advisories` weekly | **The latter**. Those three depend only on `Cargo.lock` and can be stopped on the PR that changes it, at no cost to ordinary PRs |
| 5 | Who the weekly dependency failure notifies | an issue assigned to an account; a label only | **An issue, assigned**; please name the account |
| 6 | dsh's 13 npm findings (an unpublished package) | report only for now and open an issue to upgrade; or fail now | **Report only** for now, with a separate issue |
| 7 | MPL-2.0 (`nucleo`, `option-ext`) | allow; disallow (replace the dependencies) | **Allow**. MPL-2.0 applies per file and does not reach our own code |
| 8 | The 4 "unmaintained" advisories (all through rutui) | `ignore` them and track them in rutui; or replace now | **`ignore`**, with an issue in rutui |
| 9 | Changing Python on Linux when #196 merges | the #196 PR edits `ci.yml` (an exception to the agreement); or #204 follows up after #196 merges | **The #196 PR**. Otherwise its `lint` fails (§7) and the two PRs must merge in a set order |
| 10 | Drop `cargo check --workspace --all-targets` from `checks` (D9) | drop; keep | **Drop**. It duplicates `lint` (Q12.7) |
