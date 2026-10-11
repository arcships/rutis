# S9 Install and run: install smoke tests and documented examples (design)

[中文](design-e2e-install-2026-10-11.md)

Status: design, under review. Date: 2026-10-11. Base: `main` `0941ef4`.
Issue: #193 (step 1 of the [quality tracker #183](https://github.com/arcships/rutis/issues/183); the acceptance check for #195 and #196). It takes over the install smoke, documented examples, E7 and `xtask dev` items of the closed #180.
Basis: [quality standard](quality-standard.en.md) Q6.15, Q6.6.5, Q2.13, Q10.2, Q12.1, Q13.1.6, Q13.2; [quality status](quality-status.en.md) risks B1, A2, I5, controls IN, MX, DOC, §8.4, §9.1; [CI](ci.en.md); the black-box framework `tests/e2e/` (#221).

Out of scope: the template projects' tests and `dev` loop (S2 #186); the minimum-version test matrix of CI itself (#204); distributing rutis-dsh (#89); cross-version meetings (#209).

## 1. Decisions

| # | Decision | Section |
| --- | --- | --- |
| D1 | The install smoke tests are a set of scenarios in `tests/e2e`, run in an "installed" mode: no repository runtimes injected, no repository checkout, only the artifacts the build jobs upload | 3, 8 |
| D2 | This build's packages can only come from local sources: npm from a local registry (no upstream for `@arcships/*`), PyPI from a local index (ahead of PyPI), the binary from the built archive | 3 |
| D3 | Each cell: install → check it is this build → `--version`, `new`, `check`, `run` a minimal project → stop it the way a process manager does → uninstall → residue checks | 5 |
| D4 | The artifacts are made by the same build steps as release.yml (moved into a reusable workflow), shared by main and the release | 8 |
| D5 | When: on main, 3 platforms (4 recommended) × npm / PyPI / binary; before a release, all 5 platforms as a release gate; after the release, again with the registry's packages, plus crates.io and the Go module | 4, 9 |
| D6 | Minimum-version cells run on Linux: Node 22, Python 3.10 (3.12 until #196 lands), websockets 15.0; macOS and Windows use the middle and latest versions | 6 |
| D7 | Documented examples: an HTML comment marker before each code block, a script extracts them into projects that run with the installed packages; a code block without a marker fails the check | 10 |
| D8 | E7 may no longer be skipped; `cargo xtask dev` does not exist yet, so the SDK_ID pre-check test moves to the issue that implements it | 11 |

## 2. What exists and what is missing

| Existing | Where | Does | Missing |
| --- | --- | --- | --- |
| `release-dry-run` | `.github/workflows/ci.yml:525` | builds every crate from its package (`cargo package`); `npm pack --dry-run` of 4 npm packages; `uv build python/rutis`; `maturin build` | no install, no run; Linux only |
| `release-windows` | `ci.yml:555` | builds the Windows binary and runs `help`; makes the zip; `npm pack --dry-run` of the platform package; builds the wheel | only `help`; no install |
| `release-wheel-aarch64` | `ci.yml:591` | cross-compiles the aarch64 wheel | no install |
| platform package test | `node/rutis-host/test/platform.test.mjs` | the platform package's name, `os`, `cpu`, version | which platform package npm actually picks is not checked |
| version consistency | `scripts/train.mjs` | the packages' versions agree in the sources (the source side of Q6.15.4) | the built artifacts' versions are not checked |
| pre-release smoke | `docs/release.md` "Smoke" | a WebSocket smoke on two machines; by hand, "walk through the two guides from scratch with the published packages" | manual, not recorded |
| black-box framework | `tests/e2e/` (#221) | `Scenario`, `Host`, probes, residue checks | always injects the repository's runtimes (`tests/e2e/src/lib.rs:320-322`); the host comes from `cargo build` by default (`lib.rs:84-112`) |
| E7 | `tools/test-sdk-bundle.sh:398-420` | `pack-plugin` refuses a plugin workspace whose toolchain pin differs from the bundle's | prints "skipped" and goes on when no second toolchain is installed (`:406-407`) |
| #180 | closed | no merged code. Install smoke, documented examples, E7 and `xtask dev` went into #193; templates into #186 | — |

Problems found while reading the code that matter for this design:

1. **The websockets lower bound disagrees**: the declaration is `websockets>=15` (`python/rutis/pyproject.toml:17`, changed by #218); CI installs `websockets>=13` (`ci.yml:247` and four other places), which gets the latest; quality status §2.2 still says ≥ 13. The lower bound 15.0 has never run.
2. **`cargo xtask dev` does not exist**: `crates/rutis-xtask/src/main.rs` has only `inspect`, `pack-sdk-bundle` and `pack-plugin`; the status line of `docs/design-host-dev-mode-2026-09-25.en.md` says "`cargo xtask dev` [is] not implemented". The SDK_ID pre-check test of #193 item 5 cannot be written (see 11).
3. **The Python guide's example is incomplete**: `docs/guide/python-plugin.en.md` §4 uses `src/fake_llm.py` but does not show it; following the guide does not run.
4. **"The PyPI distribution brings the Python runtime" needs checking**: `docs/guide/rutis-host.en.md` says the PyPI distribution brings `rutis`, used when the project has no runtime of its own. The host looks for the interpreter in the order `runtimes.py.python` → `$VIRTUAL_ENV` → `<project>/.venv` → `python3` (`crates/rutis-host/src/host.rs:492-497`). Whether `uvx rutis-host run` finds the `rutis` installed with it depends on whether uvx sets `VIRTUAL_ENV`. The uvx step of S9 answers it.
5. **PyPI has no wheel for musl Linux**: release.yml builds manylinux wheels only, and `rutis-host` publishes no sdist, so `pip install rutis-host` fails on Alpine and other musl systems. The support statement says just "Linux" (`docs/guide/README.en.md`).
6. **A relative rutis.json (#226)**: the documented form is `rutis-host run` in the project directory. S9 runs the documented form, so it depends on the fix of #226; until then that step is marked as a known failure pointing to #226 (Q7.3).
7. **Post-release verification cannot be triggered by the `release` event**: release.yml creates the GitHub Release with `GITHUB_TOKEN`, and events made that way do not trigger other workflows. The verification goes in release.yml (`needs: github`), or uses `workflow_run`.

## 3. What a "clean environment" is

### 3.1 Rules for every channel

| # | Rule | How |
| --- | --- | --- |
| C1 | No repository sources | The install job does not check out. It only downloads the artifacts the build jobs uploaded and the compiled scenario program (the test executable of `rutis-e2e`). Repository paths compiled into the scenario program do not exist on that machine, so code that uses the repository by mistake fails at once |
| C2 | New user directory and caches | Each scenario gets a new `HOME` (on Windows also `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`); `npm_config_cache` and `UV_CACHE_DIR` point into the scenario directory; `PIP_NO_CACHE_DIR=1` |
| C3 | An environment allowlist | `env_clear()`, then only the allowlist: `PATH`, the C2 variables such as `HOME`, `TMPDIR`/`TMP`/`TEMP`, Windows' required `SystemRoot`, `ComSpec`, `PATHEXT`, and the registry/index variables the scenario sets. So `RUTIS_*`, `NODE_PATH`, `NODE_OPTIONS`, `PYTHONPATH`, `PYTHONHOME`, `VIRTUAL_ENV`, `CONDA_PREFIX`, `CARGO_*`, `RUSTUP_*` never get in |
| C4 | A minimal PATH | System directories, the `node`/`npm`, `python`, `uv` of the versions the job asks for, and the directories the channel itself adds. No `~/.cargo/bin`, no repository `target/` |
| C5 | Language versions are asserted | At the start the scenario runs `node --version`, `python --version`, `uv --version`, compares them with the versions the job asks for, and logs them |
| C6 | This build's packages only from local sources | See 3.2: the same name and version on the public registry cannot be fetched |

The Linux minimum-version cells run in official slim containers (`node:22-bookworm-slim`, `python:3.10-slim-bookworm`, `debian:bookworm-slim`): besides the image and the downloaded artifacts there is nothing on the machine. For that, the Linux scenario program is compiled for `x86_64-unknown-linux-musl` (statically linked, it runs in any container). macOS and Windows use GitHub's standard images, isolated by C1–C5.

### 3.2 Install sources per channel, and "it really is this build"

| Channel | Install source (main, release gate) | Where third-party dependencies come from | How we know this build runs (Q6.15.2) |
| --- | --- | --- | --- |
| npm | A local npm registry (Verdaccio, pinned version) on loopback. `@arcships/*` has no upstream and holds only the tgz files this build published into it; other names are proxied to npmjs | npmjs (proxied by the local registry) | ① `@arcships/*` has no upstream, so the registry's versions cannot be fetched; ② after install, the sha256 of `node_modules/@arcships/rutis-host-<platform>/bin/rutis-host` equals the build manifest; ③ `rutis-host --version` equals the train version |
| PyPI | A local PEP 503 index (generated by a script from the wheel directory, served by `python -m http.server` on loopback), placed ahead of PyPI with `--index`; under uv's default `first-index` strategy, `rutis` and `rutis-host` only come from it. pip uses `--no-index --find-links` | `rutis` and `rutis-host` have no third-party dependencies; websockets (lower-bound cell) and hatchling (template) from PyPI | ① as above, first-index / `--no-index`; ② the sha256 of the `rutis-host` executable in the venv equals the one in the wheel; ③ `rutis.__file__` is inside the venv, `importlib.metadata.version("rutis")` equals the train version |
| Binary | The release archive the build job uploaded (`.tar.gz`; `.zip` on Windows) | The binary has none; for Node rows `@arcships/rutis-runtime` is installed into the project from the local npm registry, for Python rows `rutis` into a venv from the local index (as in the guide's "Deployment" section) | The archive matches its `.sha256`; the extracted binary's sha256 equals the manifest |
| crates.io | Only after the release (9). Before it, the existing `cargo package` (every crate built from its package) covers it, because unpublished dependency crates cannot be resolved from crates.io | crates.io | `cargo install --locked rutis-host@<version>`, then `--version` |
| Go module | main: `go mod edit -replace` to the `go/rutis` source archive the build job uploaded, `GOPROXY=off`; after the release: `proxy.golang.org` | none (the SDK uses only the standard library) | After the release: `go list -m github.com/arcships/rutis/go/rutis@<version>` gives that version |

The build jobs also write `manifest.json`: each artifact's file name, package name, version and sha256, and the sha256 of each binary inside an artifact. It also checks that every artifact has the same version (the tgz `package.json`, the wheel `METADATA`, the binary's `--version`): the artifact side of Q6.15.4.

## 4. Platform × channel matrix

The released targets come from release.yml: 5 binaries (the `binaries` matrix), 5 wheels (the `wheels` matrix), and one npm platform package per binary.

| Platform (npm name) | Machine | npm | PyPI | Binary |
| --- | --- | --- | --- | --- |
| linux-x64 | `ubuntu-24.04` + slim containers | **main**: Node 22 (minimum) | **main**: Python 3.10 (minimum; 3.12 before #196), `python -m venv` + pip, websockets 15.0 | **main**: `debian:bookworm-slim`, Node 22 and Python 3.10 installed from official packages |
| linux-arm64 | `ubuntu-24.04-arm` | release gate (main recommended, see 17-4) | release gate | release gate |
| darwin-arm64 | `macos-15` | **main**: Node 26 (latest) | **main**: Python 3.14 (latest), uv | **main** |
| darwin-x64 | GitHub's Intel macOS image | release gate | release gate | release gate |
| win32-x64 | `windows-2025` | **main**: Node 24 | **main**: Python 3.12, uv | **main** (`.zip`) |

After the release, all 5 platforms × 3 channels run again with the registry's artifacts, plus crates.io (linux-x64) and the Go module (linux-x64).

Why each cell is in or out:

| Cell | Status | Reason |
| --- | --- | --- |
| three main platforms × three channels | in | B1 is P0 (quality status §4.2); Q12.1 requires clean installs on main. The three platforms are packaged differently (static musl binary, Mach-O, `.exe` + `.zip`), and each can break on its own |
| version spread | — | One version per platform; together they cover Node 22 / 24 / 26 and Python 3.10 / 3.12 / 3.14. The minimum is on Linux, as ci.md §6 says ("change the Linux job to the minimum, keep macOS on the latest") |
| linux-arm64 | release gate; main recommended | Same code as linux-x64; it differs only in cross-compiling and the manylinux_2_28 wheel, and such problems show up while building. Linux machines are not scarce and arm64 machines are free for public repositories, so main costs little |
| darwin-x64 | release gate | Intel macOS machines are few and queue long, and GitHub is retiring the Intel images; same code as darwin-arm64. It must pass before a release (Q13.1.6) |
| crates.io | only after the release, only linux-x64 | Before the release, unpublished dependency crates cannot be resolved; `cargo package` already builds each crate from its package. The source package does not depend on the platform, and CI compiles it on three platforms every time |
| Go module | main: only the linux-x64 binary cell; after the release: linux-x64 | The Go SDK uses only the standard library and does not depend on the platform; what depends on the platform is the Go plugin binary, which users build themselves |
| Bun (`@arcships/rutis-bun`) | main: a `bun:` row in the linux-x64 npm cell, Bun 1.4.0 | It goes through the npm channel, about 10 seconds; Bun columns on other platforms belong to #194 (B2, B3 not done) |
| win32-arm64, 32-bit, FreeBSD | out | Not released, support not declared |
| musl Linux (Alpine) × PyPI | out | No musllinux wheel, it does not install (2-5). The support statement needs a decision first (17-9) |
| musl Linux × npm / binary | out for now | The binary is static musl and is expected to run; support is not declared; a `node:22-alpine` cell can be added later at little cost |

## 5. The path each cell takes

Every step runs in the scenario directory with the environment of 3.1. "Host command" is how the channel runs `rutis-host`: `npx rutis-host` for npm, `uv run rutis-host` for uv, `.venv/bin/rutis-host` for pip, `rutis-host` in the extracted directory for the binary.

### 5.1 The common minimal path

| Step | What | Assertion |
| --- | --- | --- |
| 1 Install | by channel (5.2–5.4) | exit status 0 |
| 2 Identity | the checks of 3.2; `<host command> --version` | sha256 matches the manifest; prints `rutis-host <train version>` |
| 3 `new` | `<host command> new demo --lang node` (npm) or `--lang python` (PyPI); install its dependencies (`npm install` / `uv sync`, this build's packages from the local sources); `<host command> check` | all files present; the dependencies installed are this build; `check` exits 0 and lists the row `demo`. The template's tests and `dev` belong to #186 |
| 4 Minimal project | A fixed project built into the scenario program (`tests/e2e/fixtures/install/`): a `greeter` row and a probe row. npm: TS row `./greeter.ts`; PyPI: `py:greeter`; binary: both, plus a Go row on linux-x64 | `check` exits 0; after `run` (in the project directory with no arguments, the documented form) the probe's call `greeter.hello("Ada")` returns `"Hello, Ada"`; in the binary cell TS calls Python and gets the result |
| 5 Stop | As a process manager does, SIGTERM to the main process only: for npm the Node launcher (`node/rutis-host/bin/rutis-host.mjs`), for uv the `uv` process, for the binary `rutis-host` itself | exit status 0; the probe prints `stopped`; residue checks pass (7) |
| 6 Uninstall | npm: `npm uninstall @arcships/rutis-host`; uv: `uv remove --dev rutis-host`; pip: `pip uninstall -y rutis-host rutis`; binary: delete the extracted directory | the uninstall exits 0; the uninstall checks of 7 pass |

On Windows step 5 uses `kill` for now, and the process item of the residue checks is reported as skipped per #232 (the framework already does this); sending Ctrl-C to the npm launcher on Windows comes with #232.

### 5.2 Extra steps in the npm cells

| Step | What | Assertion | Risk |
| --- | --- | --- | --- |
| npx without install | in a new empty directory, `npx --yes @arcships/rutis-host@<version> --version` (the guide's first form) | prints the train version | B1 |
| platform package selection | list `node_modules/@arcships/` after the install | only this platform's `rutis-host-<platform>`, no other platform's package | B1 |
| missing platform package | `npm install --omit=optional`, then the host command | exit status 1, prints `no binary for <platform>` (`bin/rutis-host.mjs:15`) | B1, B6 |
| the bundled Node runtime | the minimal project does not install `@arcships/rutis-runtime` | the rows run: the runtime comes from `@arcships/rutis-host`'s dependency (`bin/rutis-host.mjs:19-21`) | B1 |
| Bun (linux-x64 only) | install `@arcships/rutis-bun` in the project, add a `bun:` row | the row runs, the probe's call returns | B1 |

### 5.3 Extra steps in the PyPI cells

| Step | What | Assertion | Risk |
| --- | --- | --- | --- |
| project form (uv) | `uv init --bare`, `uv add --dev rutis-host==<version>`, then `uv run rutis-host` throughout (the documented form) | as 5.1 | B1 |
| uvx | `uvx rutis-host@<version> --version`; in a `py:` project with no `.venv`, `uvx rutis-host@<version> run` | the first prints the train version. Per the guide, the second uses the `rutis` installed with it; if it fails, fix the code or the guide (2-4), decided in the implementation PR | B1, Q2.13 |
| pip (linux-x64 only) | `python -m venv .venv`, `.venv/bin/pip install --no-index --find-links <wheel dir> rutis-host==<version>` | as 5.1 | B1 |
| wheel contents | after the install, `python -c "import rutis.runtime"` and the other modules the runtime uses | they import; a wheel missing files fails | B1 |
| websockets lower bound (linux-x64 only) | `pip install websockets==15.0`; `python -m rutis listen:ws://127.0.0.1:<port>/rutis --id gpu --peer main <dir>` (the form of `docs/guide/nodes.en.md`, without TLS, on loopback); the host connects with a peer row and runs `gpu:greeter` | the probe's call returns; the token appears in no output | MX, P11 |
| missing runtime package | an interpreter without `rutis` (`runtimes.py.python` pointing to it) | exit status 1, the output gives the install command (`host.rs:511-516`) | B6 |

### 5.4 Extra steps in the binary cells

| Step | What | Assertion | Risk |
| --- | --- | --- | --- |
| archive | check the `.sha256`, extract | the archive has `rutis-host` (`.exe`), `README.md`, `LICENSE` | B1 |
| system dependencies | on Linux `ldd rutis-host` (or `file`) | statically linked, no glibc | B1 |
| missing runtime package | no `@arcships/rutis-runtime` in the project, run a Node row | exit status 1, prints `npm install @arcships/rutis-runtime` (`host.rs:448-452`) | B6 |
| Go row (linux-x64 only) | `rutis-host new weather --lang go`, `go mod edit -replace` to the source archive, `GOPROXY=off go build`, put it in `runtimes.go.dir` | `check` lists the binary; after `run` the probe's call returns | B1 |

## 6. Minimum-version cells

| Dependency | Declared | Cell | Version used | Note |
| --- | --- | --- | --- | --- |
| Node | `engines.node >=22` (three npm packages, #219) | linux-x64 npm, binary | the declared minimum | Node minor versions add features, so the "minimum" of Q10.2.1 is at the minor level: `>=22` means 22.0.0. Whether to use the latest 22 patch or 22.0.0: see 17-8 |
| Python | `requires-python >=3.12`; `>=3.10` after #196 | linux-x64 PyPI, binary | 3.12, 3.10 once #196 lands | Patch versions add no features, so the latest patch of that minor. One version number to change |
| websockets | `websockets>=15` (`python/rutis/pyproject.toml:17`) | linux-x64 PyPI | `==15.0` | The declared lower bound (Q10.2.2). The websockets column of CI's test jobs belongs to #204 |
| Bun | `engines.bun >=1.4` | linux-x64 npm | 1.4.0 | Same as the `runtimes-bun` job |
| Go | `go 1.24` (`go/rutis/go.mod`) | linux-x64 binary | latest 1.24 patch | To build the Go plugin |
| Rust | `rust-version = "1.85"` | crates.io after the release | stable | MSRV belongs to #204; not repeated in the install smoke (Q12.7) |

The install acceptance of #195 (Node 22, merged) comes from the linux-x64 npm cell; that of #196 (Python 3.10) from the linux-x64 PyPI cell once #196 lands.

## 7. Residue checks

The checks of `tests/e2e/src/residue.rs` stay (every process the host started has exited, no socket file left, ports can be bound again, the temporary directory is empty, no credential in the output), plus three:

| Check | How | Finds |
| --- | --- | --- |
| wrapper processes | npm's Node launcher and the `uv` process of `uv run` are registered as processes the host started; they must exit after the stop | a launcher that does not forward the signal, or forwards it but does not exit |
| user directory | at the end of the scenario (after the uninstall), `HOME` has no new files except the allowlist (the redirected cache directories) | the host or an install script writing into the user's directory |
| clean uninstall | npm: no `node_modules/@arcships/rutis-host*`, no `node_modules/.bin/rutis-host*`; uv / pip: no `rutis-host` in the venv's `bin` (`Scripts`), no `rutis_host*` in `site-packages`; binary: no new files outside the extracted directory | files left by the uninstall |

Each check has a self-test that proves it finds the problem (as the residue checks of #221 do).

## 8. Where the artifacts come from: building and running apart

```text
package (reusable workflow, shared by release.yml and main)
  ├─ build-<target>: the same steps as release.yml's binaries / wheels
  │    → archive + .sha256, wheel, platform package directory
  │    → scenario program: cargo test -p rutis-e2e --test install --no-run (musl target on Linux)
  ├─ pack: npm pack of the four packages and the platform packages; uv build python/rutis; a source archive of go/rutis
  └─ manifest: manifest.json (sha256 of each artifact and the binaries in it), every artifact's version checked equal
         │  actions/upload-artifact
         ▼
install (reusable workflow; inputs: source local | registry, version)
  └─ install-<platform>-<channel>: no checkout; download the artifacts and the scenario program;
       start the local registry / index (when the source is local);
       RUTIS_E2E_ARTIFACTS=<dir> RUTIS_E2E_CHANNEL=<channel> <scenario program> --ignored
       on failure upload the scenario directories (logs, residue reports)
```

- One copy of the build steps (Q12.7): release.yml's `binaries` and `wheels` move into `package`, which release.yml calls. On main, `package` replaces ci.yml's `release-windows` and `release-wheel-aarch64` (they only build, and `package` builds the same things). `release-dry-run` stays under the PR `packaging` switch as the cheap packaging check.
- The scenarios go in `tests/e2e/tests/install.rs`, marked `#[ignore = "needs this build's packages: run by install.yml with RUTIS_E2E_ARTIFACTS"]`; `cargo test --workspace` does not run them, and the reason is stated (Q7.8).
- Framework changes: `Scenario::installed(name, channel)`: the environment of 3.1, no `RUTIS_NODE_RUNTIME` / `RUTIS_PYTHON_PATH`; the host command comes from the channel (`npx rutis-host`, `uv run rutis-host`, …) and may have leading arguments; a helper for one-shot commands (`--version`, `check`, `npm install`) with the hang guard; the new checks of 7. On Windows `npx` and `npm` are `.cmd` files, called through `cmd /C`.
- The local registry and index scripts go in `tools/install/`: `registry.mjs` (starts the pinned Verdaccio with its configuration in the scenario directory, publishes the tgz files, waits for the port), `simple-index.mjs` (makes a PEP 503 directory from the wheel directory). Verdaccio itself is installed outside the environment of 3.1; the scenario's npm only talks to it.

## 9. When it runs

| When | What | Time budget | Basis |
| --- | --- | --- | --- |
| PR (any change) | No new job. Under the `docs` switch, the `links` job adds `node tools/doc-examples.mjs --check` (seconds, 10) | unchanged | ci.md §1-8: ordinary PRs ≤ 10 minutes |
| PR (`packaging` switch) | Recommended: the linux-x64 npm and PyPI cells (17-5) | about 8 minutes, parallel to the tests, not on the critical path | Q12.1, packaging errors found before merging |
| main | `package` (the main platforms' targets) + `install` (the cells marked main in 4) + documented examples | ≤ 25 minutes (main budget 60 minutes, quality status §8.4) | Q12.1, Q6.15.1 |
| release gate | release.yml: `verify` → `package` (all 5 targets) → `install` (source local, 5 platforms × 3 channels) → the publish jobs | about 10 minutes more before publishing | Q13.1.6 |
| after release | the last job of release.yml (`needs: github`): `install` (source registry, 5 × 3) + crates.io + the Go module. First poll the registries until the version can be fetched (every 15 seconds, at most 15 minutes: a hang guard, not a way to synchronise). On failure, open an issue automatically, with the version in its title | about 15–20 minutes, once per release | Q6.15.3, Q13.2 |
| manual | `install.yml` accepts `workflow_dispatch` with a version and a source, for investigation | — | — |

When the post-release verification fails, per Q13.2 the maintainer decides whether to withdraw or mark the version (`npm deprecate`, PyPI yank) and publishes a fixed version; the steps go into `docs/release.md`.

## 10. Documented examples

### 10.1 Markers

In `docs/guide/*.md`, the line before every code block must be an HTML comment marker (not shown on GitHub):

| Marker | Meaning |
| --- | --- |
| `<!-- example: <name> file=<path> -->` | write this code block to `<path>` of example `<name>` |
| `<!-- example: <name> run -->` | run each line of this shell block in order; `cd` changes the directory for the commands after it |
| `<!-- example: <name> run until="<text>" -->` | a long-running command (`dev`, `run`): start it, wait for `<text>` in its output, send SIGINT, assert exit status 0 |
| `<!-- example: skip reason="<reason>" -->` | not run; the reason is required (for example "needs another machine", "fragment: one row of rutis.json") |

`npm publish` publishes to the local registry (its configuration allows the example package names), and the later `npm install greeter` installs from there, so the "Publish" and "Use from a host" sections run too. `uv publish` is marked skip with its reason.

The Rust code blocks (`rust-host.md`, `cordis.md`) are fragments of a function body: after extraction they are joined at the `// {{blocks}}` position of `tests/doc-examples/<name>/template.rs` and compiled (not run); on main against the workspace crates (path), after the release against the crates.io versions. The Go code blocks are written into the template project, then `go test` and `go build`.

### 10.2 Keeping docs and tests from drifting

`tools/doc-examples.mjs` has two modes:

- `--check` (PR, `docs` switch, inside the `links` job, seconds):
  1. Every code block has a marker; a block without one fails, with its file and line.
  2. The Chinese and English files have the same sequence of markers; under the same marker, the code is equal once line comments (`//`, `#`) are removed (comments are translated, the code itself may not differ).
  3. The version ranges in the docs (`"^0.8.0"`, `rutis>=0.8,<0.9`, `rutis = "0.8"`, …) match the train version of `scripts/train.mjs`.
  4. Self-test: a sample file with an unmarked block must be reported.
- `--out <dir>` (main): extracts the example projects and a step file `steps.json`; `tests/e2e/tests/doc_examples.rs` reads it and runs it with the installed packages in the environment of 3.1 (on the same machine as the linux-x64 npm and PyPI cells).

Rules: changing a code block in the docs means making it run, or marking it skip with a reason; a new guide file is checked automatically, nothing to register. Examples found not to run when first marked (such as 2-3) are fixed in the docs in the same PR.

Only `docs/guide/` is checked. `docs/development-handbook.md` is already compiled as a doctest of the kernel crate (`crates/rutis/src/lib.rs:57`), and migration guides are covered by tests such as `crates/rutis-loader/tests/migration_example.rs`; not repeated.

## 11. E7 and `xtask dev`

### 11.1 E7

Before compiling, `pack-plugin` compares the channel in the plugin workspace's and the bundle's `rust-toolchain.toml` (`crates/rutis-xtask/src/main.rs:502-508`). It is a plain string comparison and needs no second toolchain to exist. The script now looks for a different one among the installed toolchains (`tools/test-sdk-bundle.sh:405`) and skips when there is none. Two ways to change it:

| Way | Change | Cost | Covers |
| --- | --- | --- | --- |
| A: a pin that does not exist | write the workspace pin as `0.0.0-e7`, set `RUSTUP_AUTO_INSTALL=0`, assert the whole refusal message (both pins named), with no rustup install and no E0514 | no download, deterministic | the string-comparison branch; if the check were removed, the build would fail in rustup with a different message, and the test fails |
| B: install a second toolchain | CI runs `rustup toolchain install <the version before the pin> --profile minimal` in the dylib jobs | about 30 seconds of download per dylib job | same |

Both remove the skip branch: when the condition cannot be set up, the test fails instead of printing "skipped". A is recommended (17-7).

### 11.2 The SDK_ID pre-check of `xtask dev`

The design (`docs/design-host-dev-mode-2026-09-25.en.md` §6 item 1) has `cargo xtask dev` handshake with the host before compiling and fail at once on an SDK_ID or toolchain mismatch. The command does not exist, so the test cannot be written. The only pre-compile check that exists is `pack-plugin`'s pin comparison (covered by E7). This item should move out of #193 into the acceptance criteria of the issue that implements `cargo xtask dev`; the status of I5 in quality status is noted when step 1 ends (this PR does not change `docs/quality-status.md`).

## 12. CI time and cost

These are estimates; after implementing, measure with `node tools/ci-stats.mjs` and put the numbers in the PR.

| Item | Machine | Estimate | How many |
| --- | --- | --- | --- |
| `build-<target>` (release build + wheel + scenario program) | one per target | 4–8 minutes with cache, 10–15 without | main 3–4 targets; release 5 |
| `pack` + `manifest` | Linux | 2–3 minutes | 1 |
| `install-<platform>-<channel>` | one per cell | 3–5 minutes (start the registry, install, about 10 scenarios) | main 9 cells (12 recommended); release gate 15; after release 15 + 2 |
| documented examples | Linux | 3–5 minutes | main 1 |
| `--check` | Linux (inside `links`) | < 5 seconds | PRs that change docs |
| E7 | — | A: 0; B: about 30 seconds per dylib job | PRs that change dylib |

- main: the critical path is build (≤ 15 minutes) + install (≤ 5 minutes), about 20 minutes, within the 60-minute main budget. Each main push adds macOS jobs: 1 build + 3 install. The three channels can run one after another on one macOS machine (each channel with a new scenario directory and a new `HOME`), as 1 install job, which takes the macOS jobs from 4 to 2. Linux and Windows keep one job per cell, so it is clear which cell failed.
- With ci.yml's `release-windows` and `release-wheel-aarch64` gone from main, the net increase is about 4 Windows and Linux jobs.
- PR: nothing by default; under the `packaging` switch (recommended) 1 Linux job, parallel to the tests.
- Release: about 10 more minutes for the gate; the post-release verification takes about 20 minutes and does not block the release.

## 13. Risks covered and not covered

| Risk / clause | How | When |
| --- | --- | --- |
| **B1** (P0) | three channels × the main platforms, this build's artifacts; the other platforms at the release gate; the registry's packages after the release | main, release gate, after release |
| Q6.15.1 | install → minimal path → uninstall, in a clean environment | same |
| Q6.15.2 | local sources without upstream; sha256 against the manifest | same |
| Q6.15.3, Q13.2 | post-release verification | after release |
| Q6.15.4 | `manifest` checks every artifact's version | main, release |
| Q13.1.6 | release gate | before release |
| A2 (P1), in part | template `new` + dependencies installed from this build + `check`; the template's tests and `dev` belong to #186 | main |
| B6, in part | the messages for a missing platform package, a missing Node runtime package, a missing `rutis` | main |
| B2, B3, in part | wrapper processes (npm launcher, `uv run`) clean up and exit on SIGTERM | main (Unix) |
| MX, in part | Node 22, Python 3.10, websockets 15.0, Bun 1.4.0 in the install scenarios | main |
| Q2.13, DOC | documented examples run; marker, Chinese/English and version checks | PR (check), main (run) |
| I5, in part | E7 no longer skipped; `xtask dev` see 11.2 | PRs that change dylib |
| Q6.6.5, in part | the template installs its dependencies and passes `check` in a clean environment | main |

Not covered:

| Not covered | Why |
| --- | --- |
| The hint for a macOS binary quarantined after a browser download (`crates/rutis-host/src/main.rs:75-94`) | Files downloaded by CI carry no quarantine attribute; a scenario with `xattr -w com.apple.quarantine` can be added later |
| musl Linux (Alpine) | Support not declared; no PyPI wheel (17-9) |
| pnpm, yarn, `bun install` as package managers; global installs (`npm i -g`, `uv tool install`) | The docs do not describe them |
| Upgrading from the previous version, versions side by side | J2, E13, #209 |
| Corporate proxies, private mirrors, offline installs | Not declared |
| Windows process residue and Ctrl-C | #232 |
| Real cross-machine environments | Q13.1.7, by hand (`docs/release.md`) |
| Functional correctness beyond the minimal path | IN does not cover it by definition (quality status §3) |
| Packaging problems of linux-arm64 (if not on main) and darwin-x64 between releases | Found at the release gate, not when merging |

## 14. Files added and changed (when implemented)

| File | Content | Phase |
| --- | --- | --- |
| `tests/e2e/src/install.rs` | channels (npm / uv / pip / binary), the environment of 3.1, identity checks, one-shot commands | 1 |
| `tests/e2e/src/residue.rs` | the three checks of 7 and their self-tests | 1 |
| `tests/e2e/tests/install.rs` | the scenarios of 5 (`#[ignore = …]`) | 1 |
| `tests/e2e/fixtures/install/` | the minimal project (compiled into the scenario program) | 1 |
| `tools/install/registry.mjs`, `simple-index.mjs`, `manifest.mjs` | local registry, index, artifact manifest and version check | 1 |
| `tools/test-sdk-bundle.sh` | E7 without the skip | 1 |
| `.github/workflows/package.yml`, `install.yml`; `release.yml` | reusable build and install; release gate; post-release verification (who changes them: 17-3) | 1, 2 |
| `docs/release.md` (zh, en) | the release gate; what to do when the post-release verification fails | 2 |
| `tools/doc-examples.mjs`, `tests/e2e/tests/doc_examples.rs`, `tests/doc-examples/` | documented examples | 3 |
| `docs/guide/*.md` (zh, en) | markers; fixes to examples that do not run | 3 |

## 15. Phases

| Phase | Content | Depends on | Done when |
| --- | --- | --- | --- |
| 1 | The framework's installed mode; npm / PyPI / binary scenarios; local registry and index; artifact manifest; `package` and `install` workflows; the main cells; E7 | #221 (merged); the `run` step with a relative path depends on #226 | the install smoke passes on main; the install acceptance of #195 |
| 2 | Release gate (5 platforms); post-release verification (registries, crates.io, Go module); `docs/release.md` | phase 1 | the next release goes through the gate and passes the post-release verification |
| 3 | Documented examples: mark every guide; `--check` on PRs; examples run on main; fix the docs | phase 1 (uses its install environment) | every example passes; an unmarked code block fails the PR |
| — | Python minimum-version cell to 3.10 | #196 | the install acceptance of #196 |

Phase 3 matches the tracker #183's "documented examples run in step 3".

## 16. Acceptance (all automatable)

1. Every cell of the `install` workflow passes on main; each cell's log has the language versions, `rutis-host <train version>`, and the sha256 matching the manifest.
2. The checks find problems (self-tests, run as part of the scenarios):
   - a wheel without the `rutis` package → the PyPI cell fails;
   - a platform package with a wrong `cpu` → the npm cell fails at "platform package selection";
   - a binary whose sha256 differs from the manifest → the identity step fails;
   - requesting `@arcships/rutis-host@<previous released version>` from the local registry → not found (proving there is no upstream).
3. Every cell's residue report is empty (on Windows the process item is reported as skipped per #232).
4. In release.yml's job graph, every publish job `needs` all release-gate install jobs.
5. The post-release verification runs automatically after a release; a forced failure (`workflow_dispatch` with a version that does not exist) opens an issue automatically.
6. `node tools/doc-examples.mjs --check` passes on main and reports the sample with an unmarked code block; every marked example runs and passes on main.
7. E7 in `tools/test-sdk-bundle.sh` has no skip branch; the dylib jobs' logs show E7 ran and passed.
8. The minimum-version cells' logs show Node's declared minimum version, Python 3.10.x (after #196) and websockets 15.0.
9. The critical path of ordinary PRs is unchanged (`tools/ci-stats.mjs` before and after); `package` + `install` on main ≤ 25 minutes.

## 17. Decisions for the maintainer

| # | Question | Options | Recommendation |
| --- | --- | --- | --- |
| 1 | Local source for npm | A: Verdaccio, no upstream for `@arcships/*`; B: `npm install ./*.tgz` directly | **A**. Only an install through a registry makes npm pick the platform package by `optionalDependencies` (the main case of B1); the guide's `npx @arcships/rutis-host …` runs unchanged; the registry holds only this build, so identity is guaranteed by construction |
| 2 | Does the install job check out | A: no checkout, download the compiled scenario program; B: check out and run in a directory outside the checkout | **A**. "No repository sources" is then guaranteed by construction, and using a repository path by mistake fails at once; the cost is one more uploaded test executable |
| 3 | Who changes the workflows | A: the new files `package.yml`, `install.yml` and the `release.yml` change go in this PR's implementation commits, `ci.yml` untouched, removing `release-windows` and `release-wheel-aarch64` goes to #203 / #204; B: everything to #203 / #204 | **A**. The working rule covers only `ci.yml`; moving the build steps out of release.yml and the install scenarios are one piece of work, and changing them apart invites mismatch |
| 4 | Platforms on main | A: linux-x64, darwin-arm64, win32-x64; B: plus linux-arm64 | **B**. Linux machines are not scarce, arm64 machines are free for public repositories; darwin-x64 only at the release gate |
| 5 | Run on PRs with the `packaging` switch | A: no, found on main; B: run the linux-x64 npm and PyPI cells | **B**. Few PRs change packaging files; this cell takes about 8 minutes in parallel to the tests and does not lengthen the critical path; packaging errors are found before merging |
| 6 | Release gate | A: no publishing unless the install smoke passes on all 5 platforms; B: verify on main only | **A** (Q13.1.6). About 10 more minutes, instead of withdrawing a broken package after publishing |
| 7 | E7 | A: a non-existent pin + `RUSTUP_AUTO_INSTALL=0` + assert the whole message; B: install a second toolchain | **A**. Same branch covered, no download, deterministic; both remove the skip |
| 8 | The Node minimum-version cell | A: 22.0.0 (the literal minimum of `>=22`); B: the latest 22 patch | **A**; if 22.0.0 does not pass, raise `engines` to the lowest minor that does (Q10.3: do not declare what is not verified) |
| 9 | PyPI on musl Linux | A: the docs say the PyPI distribution supports glibc Linux only; B: add musllinux wheels | **A**, change only the support statement now; B when users need it |
| 10 | The SDK_ID pre-check test of `xtask dev` | A: move it out of #193 into the acceptance of the issue implementing `cargo xtask dev`; B: implement a minimal `xtask dev` in #193 | **A**. The command does not exist; implementing it is feature work, not install smoke |
| 11 | Splitting PRs | A: this PR does phase 1 and E7, phases 2 and 3 one PR each, #193 closes when phase 3 merges; B: everything in this PR | **A** (keep PRs small). With A, this PR's `Closes #193` becomes `Part of #193` |
| 12 | When the post-release verification fails | A: the job fails and opens an issue automatically, a person decides on withdrawal; B: `npm deprecate` / yank automatically | **A**. Withdrawal cannot be undone and needs a person's judgment (Q13.2) |
