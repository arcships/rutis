# S2 End to End: the Plugin Author's Development Loop (Design Draft)

[中文](design-e2e-dev-loop-2026-10-11.md)

Status: design, under review. Date: 2026-10-11. Baseline: `main` `0941ef4`.
Related: [#186](https://github.com/arcships/rutis/issues/186) (step 1 of [#183](https://github.com/arcships/rutis/issues/183)). Based on: the [quality standard](quality-standard.en.md) ("the standard") Q6.6, Q7; the [quality status](quality-status.en.md) ("the status document") scenarios A and B, §5, §9.1; the E2E framework [#221](https://github.com/arcships/rutis/pull/221) (`tests/e2e/`); the [rutis-host guide](guide/rutis-host.en.md); [CI](ci.en.md).

Out of scope: installing from npm / PyPI (S9 [#193](https://github.com/arcships/rutis/issues/193)); cross-language crashes (S3 #187); long runs (S8 #192); changing the behaviour of `rutis-host` itself.

## 1. Conclusions

| # | Conclusion | Section |
| --- | --- | --- |
| C1 | One test file `tests/e2e/tests/dev_loop.rs`, one test per language: `node`, `python`, `bun`, `go`; each goes through the same steps: `new → template tests → check → dev → change → break → fix → Ctrl-C → check (missing dependency) → run → Ctrl-C → restart after kill` | 3 |
| C2 | The plugin's v2, v3 and broken versions are written whole by the scenario; v1 is the template `new` generates. v2 and v3 print a line in apply and in their cleanup, to assert "the old instance was unloaded, exactly once" | 3.1 |
| C3 | Step 2 uses the checkout's packages (links to `node/rutis`, `bun/rutis-bun`, tsx; `RUTIS_PYTHON_PATH`; a Go `replace`), not a registry; installing from a registry is #193's job | 3.3, 14 |
| C4 | Two kinds of assertion: exit codes and the lines rutis itself writes are golden; third-party error text (tsx, Python, the Go compiler) is only asserted to contain the file name and line number | 5 |
| C5 | Every synchronisation point is an output line or a process exit; files are changed through "dot file in the same directory + rename", with the mtime guaranteed to change; no sleep | 7 |
| C6 | Linux and macOS first; Windows after #232 (Job Object), Ctrl-C sent by a small program attached to the host's console | 8 |
| C7 | A new parallel Linux `e2e` job; macOS inside `network-macos`; the looping variant for A5 nightly | 9 |

## 2. Current state (code verified)

**Existing tests**

| Test | What it tests | What it does not |
| --- | --- | --- |
| `crates/rutis-host/src/new.rs` `projects_are_created_with_their_names_filled_in` | the generated files' contents | whether the generated project installs, tests, runs |
| `crates/rutis-host/tests/project.rs` | `project::dev_config` on several kinds of project (library calls) | the binary, reloading |
| `crates/rutis-host/tests/check.rs` | `check`'s stdout / stderr / exit code, word for word (hand-written configurations) | projects made by the templates |
| `crates/rutis-host/tests/signals.rs` (Unix only) | exit codes and cleanup counts of `run` / `dev` on SIGINT, SIGTERM, SIGHUP, past the deadline, a second Ctrl-C, a signal ignored at start | Windows; reloading after a change; residue checks |
| `tests/e2e/tests/cross_language.rs` | cross-language calls under `run`; residue after `kill` | `new` / `dev` / `check` |

**How `dev` behaves** (`crates/rutis-host/src/main.rs` `dev`, `project.rs`)

- Once started it takes a snapshot of the files (`project::sources`), then prints `rutis-host dev: running <id>; changes reload it (Ctrl-C ends)`. A change made after that line is always seen.
- Every 400 ms it takes the snapshot again and compares the `(path, mtime)` lists. It skips `node_modules`, `.git`, `.venv`, `venv`, `target`, `dist`, `build`, `__pycache__`, `.pytest_cache`, and names starting with `.` (except `.env`).
- On a change (not a Go project): `host.invalidate()`, then `loader.reload` of **every** non-peer row in turn, printing `<id>: reloaded` or `<id>: cannot reload: <error>` for each. Rows added by `rutis.dev.json` (the scenario's probe among them) are reloaded too.
- `Loader::reload` (`crates/rutis-loader/src/loader/api.rs`) is all or nothing: if the new module does not resolve, the old one keeps running.
- Go project: `go build` again into `.rutis/go/<name>-<n>`; on success swap the binary, restart the runtime, print `<runtime>: rebuilt and restarted`, delete the previous binary; on failure print `<name>: the build failed; the last build keeps running` and the compiler's output. The other rows of a Go project are not reloaded.
- State lines (`status::follow`, compared every 200 ms) are printed only when a state changes, and intermediate states can be skipped. The scenario does not assert their order; it only waits for stable states (`<id>: running`).

**`check`**: in a plugin project without `rutis.json` it checks the project itself (`dev_config`); with rows that cannot run it writes why on stdout, `rutis-host: <n> row(s) or binaries cannot run` on stderr, and exits 1. The Bun runtime adds `runtime bun: @arcships/rutis-bun <version>, bun <version>` (`runtime_line`). `check` only resolves rows; **it does not check that some row provides each `inject`ed service** (item 6 of §14).

**Exit codes** (guide, "Stopping and exit codes"): 0 ended normally (also cleanups finished after a signal), 1 cannot start or run, `check` found rows that cannot run, 2 stopping did not finish.

## 3. The scenario

### 3.1 Common parts

- Scenario directory `<root>/<scenario>-<pid>-<n>/project/`; `rutis-host new demo --lang <language>` runs there and makes `project/demo/`. Later commands run in `demo/` (the user's `cd demo`).
- The plugin's four versions:

| Version | Source | `greeter.hello("Ada")` | Prints |
| --- | --- | --- | --- |
| v1 | the template `new` generates, unchanged | `Hello, Ada!` | nothing |
| v2 | written whole by the scenario | `Hello, Ada! (v2)` | in apply `demo v2 applied <pid> <interpreter>`, in its cleanup `demo v2 cleanup` |
| broken | v2 with a syntax error at a fixed line 5 (Go: a compile error) | — | — |
| v3 | written whole by the scenario | `Hello, Ada! (v3)` | `demo v3 applied …`, `demo v3 cleanup` |

  v2 and v3 are written with CRLF line ends (on every platform), covering "a file saved by a Windows editor" (A8). Written whole rather than by replacing strings in the template: rewording the template does not break the scenario, and the template itself is tested by the v1 steps.

- The probe (`tests/e2e/probes/`) is an extra row in `rutis.dev.json`, written after step 3 and before step 4, so step 3 checks the template project as generated.

| Project | Probe | Placed at | Runtimes added to `rutis.dev.json` |
| --- | --- | --- | --- |
| node | TS (the same Node runtime) | `demo/probe.ts` | none |
| python | Python (the same Python runtime) | `demo/src/probe.py` (the Python runtime's project is `src/`, see `project::python_row`) | none |
| bun | Python | `demo/dev/probe.py` | `"py": { "project": "dev" }` |
| go | Python | `demo/dev/probe.py` | `"py": { "project": "dev" }` |

  The probe file is in the project, so it is in the `sources()` snapshot; it does not change, so it triggers no reload.

### 3.2 Steps

"Wait" always means wait for an output line or a process exit (the harness's `Host::expect` / `wait_for` / `wait_exit`), under the hang guard.

| # | Action | How the harness does it | Assertions |
| --- | --- | --- | --- |
| 1 | `rutis-host new demo --lang <language>` | `Scenario::host` in `project/`, `wait_exit` | exit code 0; stdout matches golden `new.txt` (`created demo/`, `next: …`); the list of generated files matches golden `files.txt` |
| 2 | the template's own tests | prepare the dependencies as in 3.3, then `Scenario::program` runs the template's command | exit code 0 |
| 3 | `rutis-host check` (no argument) | `host_in("demo", ["check"])` | exit code 0; stdout golden `check-project.txt` (row name, `inject`, `provides`, `config` schema; Bun has the runtime line; Go has the `go binaries:` part); stderr empty |
| 4 | `rutis-host dev` | write the probe and `rutis.dev.json`; `host_in("demo", ["dev"])`; wait for `rutis-host dev: running demo; …` (a golden line) and the probe's `started` | `hello("Ada")` = `Hello, Ada!` |
| 5 | change to v2 | `Scenario::replace`; wait for `demo: reloaded`, `<probe>: reloaded`, the probe's `started`, `demo v2 applied` | result = `… (v2)`; Python: the interpreter in the `applied` line is under `demo/.venv` (A8) |
| 6 | break it | `replace`; wait for a line starting `demo: cannot reload: ` and the probe `started` again | the error names the file (`index.ts` / `__init__.py` / `plugin.go`) and line 5 (A7); result still `… (v2)`; so far `demo v2 cleanup` appeared 0 times and `demo v2 applied` once (the old instance was not unloaded, A4) |
| 7 | fix it (v3) | `replace`; wait for `demo: reloaded`, `demo v3 applied`, the probe's `started` | result = `… (v3)`; `demo v2 cleanup` exactly once (A3) |
| 7a | Ctrl-C in dev | `Host::ctrl_c`; `wait_exit` | exit code 0; stderr line `rutis-host: SIGINT: stopping; cleanups have 10s (again to exit at once)` (Windows: `Ctrl-C`); `demo v3 cleanup` exactly once; the probe `stopped` (B2) |
| 8 | write a `rutis.json` with a missing dependency, `check` | `rutis.json`: the `demo` row (per language, see 3.3) + `{ "id": "llm", "name": "fake-llm" }`; `host_in("demo", ["check", <absolute path>])` | exit code 1; stdout golden `check-missing.txt`: `demo (…): ok …`, `llm (fake-llm): no plugin named "fake-llm"` and the install hint; stderr = `rutis-host: 1 row(s) or binaries cannot run` |
| 9 | `rutis-host run`, Ctrl-C | `rutis.json` without `llm`, with the probe row; `host_in("demo", ["run", <absolute path>])`; wait for `demo: running`, the probe's `started`; call; `ctrl_c` | result = `… (v3)`; exit code 0; in this host's output `demo v3 cleanup` exactly once; the probe `stopped` (B2) |
| 10 | restart right after a kill | `run` again; wait for `demo: running`; `Host::kill` (the host alone); `wait_exit`; at once a third `run`; wait for `demo: running`, the probe's `started`; call; `ctrl_c` | the third one starts, the call succeeds, exit code 0 (B10); Unix: the killed host's output eventually shows `demo v3 cleanup` once (guide: the runtimes see the channel close and unload their rows, B3) |
| — | end | `Scenario::finish()` | every residue check passes (6) |

Steps 8, 9 and 10 pass the absolute path of `rutis.json` because of #226 (a relative `rutis.json` leaves `./` rows unresolved); the code comment points to #226. Once #226 is fixed they use the guide's form (no argument), see item 5 of §14.

### 3.3 Differences between languages

| | node | python | bun | go |
| --- | --- | --- | --- | --- |
| Step 2 preparation | link `node_modules/@arcships/rutis` → `node/rutis`, `node_modules/tsx` → `node/rutis-runtime/node_modules/tsx` | `python -m venv --without-pip .venv` (no packages; `rutis` comes from `RUTIS_PYTHON_PATH`) | link `@arcships/rutis`, `@arcships/rutis-bun` → `bun/rutis-bun` | append `replace github.com/arcships/rutis/go/rutis => <repo>/go/rutis` to `go.mod` (as `project.rs` `a_go_project_is_built_into_rows` does) |
| Step 2 command | `npm test` (Windows: `npm.cmd`), that is the template's `node --import tsx --test test/*.test.ts` | the `.venv` python `-m unittest discover -s tests`, `PYTHONPATH=src` + `python/rutis` | `bun test` | `go test ./...`, `GOTOOLCHAIN=local`, `GOPROXY=off` (no download) |
| Where the runtime comes from | `RUTIS_NODE_RUNTIME` (set by the harness) | the `.venv` interpreter + `RUTIS_PYTHON_PATH` | the linked `node_modules/@arcships/rutis-bun` (`host::bun_runtime` has no environment fallback) | `dev` builds it |
| Output of steps 5–7 | `demo: reloaded` / `demo: cannot reload: …` | same | same | `go-demo: rebuilt and restarted` / `demo: the build failed; the last build keeps running` + compiler output |
| Step 6 | tsx's error | `SyntaxError` (with `__init__.py`, `line 5`) | Bun's error | compile error `plugin.go:5:…`; the old process keeps serving |
| Extra step 7 assertions | — | — | — | the old runtime process has exited (its pid is not in the process table); `.rutis/go/` holds only the latest binary |
| The probe in steps 5–7 | reloaded each time | reloaded each time | reloaded each time | not reloaded; it stops and starts again with `greeter` when the Go runtime restarts: wait for its `stopped`, `started` |
| Step 8 `demo` row | `./src/index.ts`, `runtimes.node = {}` | `py:demo`, `runtimes.py = { "project": "src", "python": <the .venv interpreter> }` | `bun:./src/index.ts`, `runtimes.bun = {}` | first `go build -o plugins/demo ./cmd/demo` (the guide's way to deploy), `go:demo`, `runtimes.go = { "dir": "plugins" }` |
| Not done | — | `uv sync` (needs a registry, #193) | `bun run check` (tsc needs `@types/bun`, not in the checkout); `bunx --bun @arcships/rutis-host` (needs the published package) — both to #193 | — |

The Bun runtime itself also compares the entry file's mtime and size (Bun design §3.8), so `replace` guaranteeing an mtime change matters for Bun as well.

## 4. What the harness needs

All in `tests/e2e/src/`, depending on no rutis crate (Q7.6).

| Addition | File | Purpose |
| --- | --- | --- |
| `Scenario::host_in(dir, args)` | `lib.rs` | start a host in a subdirectory of the project (`cd demo`); same environment as `host_with` |
| `Scenario::program(dir, program, args, env) -> Host` | `lib.rs` | run npm / python / bun / go: the same temporary directory and credential isolation, output capture, process group, residue registration; `Host::wait_exit` gives the exit code |
| `Scenario::replace(relative, contents)` | `lib.rs` | write `.<name>.part` in the same directory (`sources()` skips dot files), then `rename`; if the new mtime equals the old one, `File::set_modified(old + 1 s)` |
| `Scenario::probe_in(dir, id, lang, inject)` | `lib.rs` | the probe file in a subdirectory; `Probe::row`'s `name` relative to the directory of `rutis.dev.json` |
| `Scenario::link(project, package, target)` | `lib.rs` | `link_node_sdk` generalised: link any package into any project (Unix symbolic link, Windows junction) |
| `Host::count(text)` | `host.rs` | the number of lines so far containing `text`; "exactly once" is counted after the host exited (its output ended) |
| `Host::ctrl_c()` | `host.rs` | Unix: `killpg(SIGINT)` (what a terminal does); Windows: see 8 |
| `Host::ctrl_break()` | `host.rs` (Windows only) | `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, host pid)` |
| `golden::check(name, text)`, `golden::normalize` | `golden.rs` (new) | compare with `tests/e2e/golden/dev_loop/<language>/<name>.txt`; with `RUTIS_E2E_BLESS=1` rewrite the file (Q7.7) |
| `rutis-e2e-console` | `src/bin/` (new, with content on Windows only) | see 8 |

`windows-sys` (0.61, already used in the workspace by `rutis-bridge`) is added as a Windows-only dependency; no new crate.

## 5. Golden output

**Locked** (rutis's own output, documented behaviour; Q3.2, Q5.6.3, Q6.6.6):

| Output | Source |
| --- | --- |
| `new`'s stdout, the list of generated files | `main.rs` `new_project`, `new.rs` |
| `check`'s stdout, stderr (steps 3, 8) | `main.rs` `check` |
| `rutis-host dev: running demo; changes reload it (Ctrl-C ends)` | `main.rs` `dev` |
| `demo: reloaded`, the `demo: cannot reload: ` prefix, `go-demo: rebuilt and restarted`, `demo: the build failed; the last build keeps running` | `main.rs` `dev` |
| `rutis-host: SIGINT: stopping; cleanups have 10s (again to exit at once)` | `stop.rs` `stop` |
| every step's exit code | guide, "Stopping and exit codes" |

**Not locked**: the error text of tsx, Bun, Python and the Go compiler (only "contains file name and line number" is asserted, A7); the order of state lines (Q3.3, an implementation detail); the output of tools such as `npm test`.

**Normalisation** (`golden::normalize`): the scenario directory → `<dir>` (also as a `file://` URL and with Windows `\`); the `rutis-host` version → `<version>`; Bun and Go toolchain versions and the platform → `<bun>`, `<go>`, `<platform>`.

**Storage**: files, regenerated by `RUTIS_E2E_BLESS=1 cargo test -p rutis-e2e --test dev_loop` (Q7.7). `crates/rutis-host/tests/check.rs` keeps its expectations in the code, with no command to regenerate them; files are used here because each of four languages has several, and they must be regenerable (item 2 of §14).

## 6. Residue checks

Each test ends with `Scenario::finish()`, using #221's checks (`tests/e2e/src/residue.rs`):

| Check | Linux | macOS | Windows |
| --- | --- | --- | --- |
| every process the hosts (and the npm, python, go that `program` starts) started has exited | process groups + reaping children (`PR_SET_CHILD_SUBREAPER`) | `ps` by process group | before #232 reported as skipped (Q7.8); after it, by Job Object |
| no socket file under the scenario directory | yes | yes | by `*.sock` file name |
| the hosts' temporary directory is empty (except tsx's `tsx-<uid>`) | yes | yes | yes |
| no credential in the output | yes (the scenario uses none; the check runs anyway) | yes | yes |

Checks the scenario adds:

- After each host exits, `demo v2 cleanup` / `demo v3 cleanup` appear in its output exactly as often as expected (cleanup exactly once, Q3.1).
- Go: after each rebuild the old runtime process has exited; `.rutis/go/` holds only the current binary.
- Whether Bun leaves files in the temporary directory is checked during the implementation; an exception, if needed, is written down with its reason as for tsx.

The items of status §5 that need in-process sampling (the test process's fds and threads, tokio task count, kernel and runtime registries) are not black-box and are not part of this scenario.

## 7. Determinism (Q7.1)

| What could be nondeterministic | What is done |
| --- | --- |
| changing a file before `dev` took its first snapshot | change only after `rutis-host dev: running …` (printed after the snapshot) |
| two changes with the same mtime (file system time granularity), the change missed | `replace` checks and pushes the mtime 1 s later |
| the 400 ms poll seeing a half-written file and loading it | write a dot file, then `rename`: the replacement is atomic |
| two changes within one poll | after each change wait for every result line of this reload (each row's `reloaded` / `cannot reload`) and the probe's `started` before the next |
| a call lost while the probe reloads | wait for the probe's `started` event before calling |
| intermediate state lines skipped | wait only for the stable `<id>: running`, assert no order |
| "something did not happen" (the old instance not unloaded) | rewritten as "another thing happened (the `cannot reload` line, the probe `started` again) and it still has not" (Q7.1.1) |
| "exactly once" | count after the host exited and its output closed |
| timeouts | only the harness's hang guard (30 s by default, `RUTIS_E2E_TIMEOUT`); a first `go build` on a cold CI cache could come close, so the Go test's step 2 (`go test`) warms the cache first |
| four tests in parallel in one binary | an orphan that left its process group may be attributed to the wrong test (already documented in #221); the failure is real; `--test-threads=1` to attribute it |

No sleep. The 20 ms / 50 ms polls in `Host::wait_exit` and the residue checks wait for the process table to change (the standard library has no wait with a timeout); they do not stand in for an event.

## 8. Platforms

| Platform | Languages | Signal | Notes |
| --- | --- | --- | --- |
| Linux | node, python, bun, go | `killpg(SIGINT)` | every check |
| macOS | node, python, bun, go | `killpg(SIGINT)` | process check with `ps` |
| Windows | node, python, go | Ctrl-C (main), Ctrl-Break (additional) | after #232; bun skipped: the Bun runtime's Windows support is B3 (Bun design §10) and not declared yet (the reason written down, Q7.8) |

**How Ctrl-C is sent on Windows**: `GenerateConsoleCtrlEvent` reaches only processes that share the caller's console, and `CTRL_C_EVENT` can only go to the whole console (process group 0). If the host shared the test process's console, Ctrl-C would hit `cargo test` itself. So:

1. The host starts with `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`, with a (hidden) console of its own;
2. `Host::ctrl_c` runs `rutis-e2e-console ctrl-c <host pid>`, which does `FreeConsole`, `AttachConsole(host pid)`, `SetConsoleCtrlHandler(NULL, TRUE)` (it ignores the event itself), then `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)`. This is the same as a user pressing Ctrl-C in the host's console: the runtime processes are in process groups of their own and do not respond to Ctrl-C (guide), and the host unloads the rows.
3. If `AttachConsole` does not work under `CREATE_NO_WINDOW`, use `CREATE_NEW_CONSOLE` (CI has no desktop, the window is not seen). To be confirmed on `runtimes-windows` during the implementation.

Ctrl-Break goes to the host's process group only (`GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, host pid)`), asserting exit code 0 and one cleanup. Ctrl-Break pressed in a console also reaches the runtime processes, and the guide says their cleanups may then not run — that case is not asserted (Q3.4, explicitly not guaranteed).

**What Windows adds** (A8): `.venv\Scripts\python.exe` is chosen (the interpreter assertion of step 5); `file:///C:/…` and `\` paths (golden after normalisation); CRLF sources; the `.exe` names of Go binaries and the deletion of old ones.

## 9. CI and time

**Expected time** (estimates; measured on CI during the implementation and recorded in the PR description): the `cross_language` scenario takes about 1 s on Linux (measured in #221). S2 takes about 10–15 s per language (npm test about 1–2 s; making the venv about 1 s; two `check`s, a `dev` and three `run`s, each starting in about 0.5–1 s; three changes of about 0.5–1 s each); Go adds `go test` and three builds, about 10–20 s. With the four tests in parallel, the whole file takes about 20–30 s on Linux. The looping variant for A5 (200 reloads) takes about 2–3 minutes per language, nightly only.

**CI changes needed** (`ci.yml` is changed by #203 / #204; this PR does not change it, they are stated in the PR description):

| Change | Level | Time |
| --- | --- | --- |
| a new job `e2e` (Linux, `code` switch): the same minimum versions of Node, Bun, Python, Go as `rust`; `npm --prefix node/rutis-runtime ci`; `cargo test -p rutis-e2e`; on failure upload `RUTIS_E2E_DIR`. Added to `ci-ok` | PR | parallel with `rust`, off the critical path: building `rutis-host` (from main's cache) plus the scenarios, expected 3–5 minutes |
| `--exclude rutis-e2e` on the `rust` job's `cargo test --workspace` | PR | the critical path shortens by the E2E time (Q12.7, a check runs in one place) |
| `network-macos`: `cargo test -p rutis-e2e` | PR (`code`) | about +1 minute; no new macOS job (ci.md §6) |
| `runtimes-windows`: `cargo test -p rutis-e2e` (after #232) | PR (`code`) | about +1–2 minutes; no new Windows job |
| `stress.yml`: `cargo test -p rutis-e2e --test dev_loop -- --ignored` (A5) | nightly | about 10 minutes |

An ordinary code PR stays within 10 minutes: E2E runs in a parallel job, and macOS and Windows only get a step in jobs they already run.

## 10. Risks covered

| Risk | Level | How this scenario covers it | What is left |
| --- | --- | --- | --- |
| A2 the template project does not install, test, run | P1 | steps 1–4: the generated project passes its own tests, `check`, `dev` (with the checkout's packages) | installing from a registry: #193 |
| A3 old code still runs after a reload / the old instance not unloaded | P0 | steps 5, 7: the result changes; the old instance's cleanup runs exactly once; Go: the old process exits | — |
| A4 `dev` crashes after a broken file, or the old version stops serving | P1 | step 6: `cannot reload`, the old version keeps serving and is not unloaded, `dev` keeps running | calls **concurrent** with the failed reload: the probe is reloaded too, so it cannot make them (item 9 of §14) |
| A5 resources grow after hundreds of reloads | P1 | steps 5–7 looped 200 times (nightly): one cleanup each; the number of runtime processes unchanged; on Linux the fd count of the host and runtime processes (`/proc/<pid>/fd`) at most the starting count + 16 (the existing soaks' tolerance) | fd sampling on macOS, Windows |
| A6 `check` misjudges | P1 | step 3 (what can run passes), step 8 (a missing package fails), on template projects | `check` passes when no row provides an `inject`: item 6 of §14 |
| A7 an error does not say which file, which line | P1 | step 6: the error has the file name and line number; step 8: names the row id and name | — |
| A8 paths, venv, line ends on Windows | P1 | the Windows column (8); CRLF sources on every platform | Windows does not run before #232 |
| B2 no cleanup on a signal, exit code not as documented | P0 | steps 7a, 9: Ctrl-C (Windows Ctrl-C and Ctrl-Break), one cleanup, exit code 0, the probe `stopped`, no residue | SIGTERM, SIGHUP, the deadline, a second Ctrl-C are covered by `signals.rs` (Unix) and not repeated (Q12.7); closing the Windows console is not done |
| B3 runtimes orphaned after the host is SIGKILLed | P0 | step 10 + the residue checks | — |
| B10 the previous socket / port not released on restart | P1 | step 10: a restart right after a kill comes up | configurations with `listen` (this scenario listens on nothing) |

**Explicitly not covered**: A1 (#206); A9, A10 (unit tests exist); A11 (Python re-imports only the entry module, a documented limitation; a test locking "indeed not provided" could be added, not in this scenario); A12 (actionlint, #204); B1, registry installs, `bunx --bun` (#193); B4 (`run` does not watch its configuration file, status §10 item 2); B5, B6 (done in #205); `go add`.

## 11. What the standard requires (Q9.1.1)

- **Behaviour defined**: this design defines no new behaviour; it verifies documented behaviour: `dev` reloads after a change and keeps the old version when a change is broken (guide, "Commands", and each language's guide) — contract behaviour (Q3.2); cleanup exactly once after a signal and no residue at the end — core promises (Q3.1); exit codes — contract behaviour.
- **What core promises require**: every supported platform (Linux, macOS in this PR, Windows after #232); two independent ways (Q8.3): the loader's library-level reload tests, `signals.rs`, and this black-box scenario. The fault injection is the broken file and the SIGKILL.
- **Component type**: Q6.6 user-facing entry point. Q6.6.1 black box (only the binary and files; `rutis-e2e` depends on no rutis crate); Q6.6.2 the success paths, common error paths (broken file, missing package) and exit codes of `new` / `check` / `dev` / `run`; Q6.6.3 termination signals on each platform; Q6.6.5 the template project tests and runs (the clean-environment install is #193); Q6.6.6 golden output.
- **Risk assessment**: §10; the risk ids are those of status §4.2, and the status document is not changed.

## 12. Phases

All in this PR, as separate commits; the Windows phase becomes a follow-up PR if #232 has not landed.

| Phase | Content |
| --- | --- |
| E1 | the harness additions (§4, without the Windows parts); `golden.rs`; the node and python tests of `dev_loop.rs`; passing on Linux, macOS |
| E2 | the bun and go tests |
| E3 | the looping variant for A5 (`#[ignore = "nightly: stress.yml runs it with --ignored (A5)"]`) |
| E4 | Windows: `rutis-e2e-console`, the host's creation flags, Ctrl-C / Ctrl-Break; after #232 |

## 13. Acceptance criteria

Each can be checked automatically:

1. `cargo test -p rutis-e2e --test dev_loop` passes on Linux and macOS with all four tests running (none `#[ignore]`d except the A5 loop); each ends with `finish()` and no residue.
2. Every step's exit code is as in 3.2: `new` 0, template tests 0, `check` 0, `check` (missing package) 1, Ctrl-C in dev 0, Ctrl-C in run 0.
3. The golden files match the output; after regenerating with `RUTIS_E2E_BLESS=1`, `git diff --exit-code tests/e2e/golden` is empty.
4. It also passes under `RUTIS_LOCAL_HANDOVER=loopback` (Unix).
5. Five consecutive local runs all pass (recorded in the PR description).
6. No `sleep` in the scenario code (`grep -n "sleep" tests/e2e/tests/dev_loop.rs` finds nothing).
7. The A5 loop passes with `--ignored`: 200 reloads, cleanup count = 200, the number of runtime processes unchanged, fd growth on Linux ≤ 16.
8. After E4: the node, python and go tests pass on Windows, and the residue checks no longer report the process check as skipped.

In addition, once by hand during the implementation, recorded in the description: temporarily make `dev` unload the old instance when a reload fails, or skip the cleanups on Ctrl-C, and confirm the scenario fails (showing the assertions do catch A3, A4, B2).

## 14. Decisions for the maintainer

| # | Question | Options | Recommendation |
| --- | --- | --- | --- |
| 1 | Step 2 with the checkout's packages or installed from a registry | a. link the checkout's packages (3.3); b. a real `npm install` / `uv sync` | **a**. On an unreleased commit the versions the template depends on do not exist on the registry yet, and installing over the network is not deterministic. Installing from a registry is #193's job |
| 2 | Where the golden output lives | a. files in `tests/e2e/golden/` + `RUTIS_E2E_BLESS=1`; b. in the code, as `check.rs` does | **a**: meets Q7.7 (regenerable by a command), and four languages' expected text does not crowd the code |
| 3 | Signals on Windows | a. Ctrl-C (the small program attached to the console) as the main one, Ctrl-Break to the host's process group as an addition; b. only Ctrl-Break (as the issue says) | **a**. Users press Ctrl-C, and the guide tells them to; testing only Ctrl-Break misses the user's path |
| 4 | When Windows is done | a. after #232, E4 as a follow-up PR; b. run on Windows now with the process check reported as skipped | **a**. Without the process check, exactly B2 and B3 go unverified on Windows; b would make Windows look covered |
| 5 | #226 | a. pass absolute paths now, switch to no argument once #226 is fixed; b. wait for #226 | **a**, with a comment pointing to #226; if #226 lands first, use no argument directly |
| 6 | `check` does not check that some row provides each `inject` (found reading the code; a case of A6's "what cannot run passes") | a. a separate issue: it is a behaviour change, its output needs deciding first; b. change it in this PR | **a**. This scenario asserts only the documented behaviour (a missing package) |
| 7 | CI placement | a. a new Linux `e2e` job + `rust` excluding rutis-e2e + a step in the existing macOS, Windows jobs (§9); b. keep running inside `rust`'s `--workspace` | **a**. E2E grows with S3, S9 and others and should not sit on the critical path; ci.md §6 puts new checks in parallel Linux jobs |
| 8 | Bun's `bun run check` (tsc) and `bunx --bun @arcships/rutis-host` | a. to #193 (they need `@types/bun` from the registry and the published package); b. install over the network in this PR | **a**; add both to #193 |
| 9 | What "calls do not fail during step 6" means | a. after the failed reload, the old version keeps serving and was not unloaded; b. calls **concurrent** with the failed reload do not fail | **a**. `dev` reloads every row on each change, the probe included, so b needs a caller that is not reloaded (another host calling through a peer, say), which is costly. A Go project's probe is not reloaded, so the Go test calls while the build fails, which covers part of b |
