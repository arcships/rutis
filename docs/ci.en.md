# Continuous integration (CI)

[中文](ci.md)

This document describes how CI runs today and what to do when changing it. The rules come from [quality standard](quality-standard.en.md) §12; timings are in [quality status](quality-status.en.md) §8.

## 1. Rules

1. A pull request runs only the jobs related to its change; main runs every job on every push.
2. A new commit on a pull request cancels the older run; runs on main are not cancelled.
3. Merging depends on one check: `ci-ok`.
4. macOS runners are few, so a run uses as few macOS jobs as possible, ideally one.
5. A check runs in one job only.
6. A pull request that changes the CI configuration runs every job.
7. Rust build caches are saved on main only; pull requests read main's caches and save none of their own.

## 2. Workflows

| Workflow | When it runs | What it does |
| --- | --- | --- |
| `ci.yml` | Every pull request; every push to main | Tests and checks (below) |
| `dylib-windows.yml` | Pull requests touching dylib files; every push to main; `v*` tags; by hand | Dylib tests on Windows |
| `stress.yml` | Nightly; by hand | Kernel tests repeated; long network tests |
| `ci-stats.yml` | Weekly; by hand | CI timings and failures |
| `release.yml`, `release-cli.yml` | Tags | Releases |

## 3. How a pull request picks its jobs

The first job of `ci.yml`, `changes`, looks at the files the pull request changed and sets the switches below. Each later job runs or not depending on a switch. On a push to main every switch is on.

| Switch | On when these files change |
| --- | --- |
| `all` | `Cargo.lock`, the root `Cargo.toml`, `rust-toolchain.toml`, `.github/workflows/**`. Turns every other switch on too |
| `code` | Any file except `docs/**` and `*.md` |
| `dylib` | The kernel `crates/rutis`; `rutis-cli`, `rutis-dylib*`, `rutis-sdk`, `rutis-dev`, `rutis-xtask`; `tools/*dylib*`, `tools/test-sdk-bundle.sh`, `tools/lib/**`, `tests/dylib-fixtures/**` |
| `packaging` | `scripts/train.mjs`, every `pyproject.toml` and `package.json`, `crates/*/Cargo.toml`, `node/rutis-host/scripts/**` |
| `docs` | Any `.md` file |
| `bun` | `bun/**`, `node/rutis/**`, `crates/rutis-bridge`, `crates/rutis-loader`, `crates/rutis-host` |
| `repro` | `crates/rutis-sdk`, `crates/rutis-cli/build.rs`, `tools/test-dylib-repro.sh`, `tools/build-dylib-bundle.sh`, `tools/lib/**`. Kernel changes reach the SDK too, but its reproducibility check for them runs on main |

## 4. Jobs

| Job | Runner | Switch | What it does |
| --- | --- | --- | --- |
| `links` | Linux | `docs` | Checks relative links in Markdown |
| `test` | Linux | `code` | All Rust tests and build checks; the Node and Python packages' tests |
| `network-macos` | macOS | `code` | Bridge, loader (Bun rows included) and rutis-host tests on macOS, with the latest Bun; a build check of the whole workspace |
| `runtimes-windows` | Windows | `code` | Bridge, loader and rutis-host tests on Windows; a build check of the whole workspace |
| `runtimes-bun` | Pull requests: Linux, Bun 1.4.0 and latest; main adds macOS, Bun 1.4.0 | `bun` | All Bun runtime tests |
| `semver-rutis` | Linux | `code` | Whether the public API changed incompatibly since the last release (warning only) |
| `dylib-linux-launcher`, `dylib-linux-sdk-bundle` | Linux | `dylib` | Dylib tests, split into two jobs that run at the same time |
| `dylib-linux-repro` | Linux | `repro` | Reproducible SDK builds (built twice on one machine, in two directories) |
| `dylib-macos` | macOS | `dylib` | The same dylib tests, one after another in one job, sharing one bundle build; the reproducible-build step runs only when `repro` is on |
| `release-dry-run`, `release-windows`, `release-wheel-aarch64` | Linux, Windows, Linux | `packaging` | The release packages build |
| `sdk-repro`, `sdk-repro-macos`, and their comparisons | Linux, macOS | `all` | Builds the SDK on two machines and checks the results are identical |
| `ci-ok` | Linux | Always | The summary (§5) |

## 5. Merge condition

- `ci-ok` is the only check required on main.
- `ci-ok` looks only at the jobs listed in its `needs`. It passes when each of them succeeded or was skipped, and fails when any of them failed or was cancelled.
- A job not listed in `ci-ok` does not stop a merge, even when it fails.

## 6. Changing CI

### Adding a job

1. Give it `needs: changes` and `if: needs.changes.outputs.<switch> == 'true'`. If no switch fits, add one to `changes` and to §3.
2. Add it to `ci-ok`'s `needs`.
3. Check whether an existing job already does the same check. If the work fits in an existing job, put it there instead of adding a job.
4. Before using macOS, see whether the work fits in `network-macos`. An ordinary change uses at most 1 macOS job; a dylib change at most 2.
5. Add it to §4.

### Adding a language runtime

1. Add a switch for the language to `changes`, covering its runtime directory and `crates/rutis-bridge`, `crates/rutis-loader`, `crates/rutis-host`.
2. Install the language in `test` and `network-macos`, and add its feature to their existing bridge, loader and host test steps.
3. To test several versions, add a `runtimes-<language>` job: it runs on the switch from step 1, is listed in `ci-ok`, and runs only on Linux for pull requests. Older versions on macOS run only on main (see `runtimes-bun` and the `bun-matrix` output of `changes`).
4. Releases: add its package to `release-dry-run`, `scripts/train.mjs` and `release.yml`.
5. Update §3 and §4.

### Changing which files turn a switch on

- Changing `changes` changes `.github/workflows/**`, so that pull request runs every job.
- If a job was skipped on a pull request and then failed on main after the merge, a switch is missing some files. After fixing the failure, add those files to the switch.

### Changing the CI configuration

- Use a pull request of its own, with commit type `ci:`.
- After it merges, check the first run on main.

## 7. Failures

- Fix failures on main first. Find which merge caused them; until they are fixed, do not merge changes they may affect.
- If a run on main is cancelled by hand, run it again. Every merge needs a complete result.
- For tests that fail now and then, first find a way to reproduce the failure, then fix it. Do not hide it with automatic retries.

## 8. Build caches

- Every job uses `Swatinem/rust-cache` with `save-if: github.ref == 'refs/heads/main'`: only runs on main save caches.
- A pull request can read only main's caches and its own. Pull requests save none, so that they do not fill the repository's 10 GB and push main's caches out.
- New jobs do the same (see the note at the first `rust-cache` in `ci.yml`).
- Checks that cannot use caches (reproducible SDK builds) run on main, and on pull requests only when `repro` is on.
