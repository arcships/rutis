# Quality control leftovers: guarantee-register tail, agent-written plugins and resource limits (plan)

[中文](design-quality-remaining-2026-10-20.md)

Status: draft, for review. Date: 2026-10-20. Base: `main` `4e23339`.
Related: the [quality-control tracking issue](https://github.com/arcships/rutis/issues/183). Based on: the [quality standard](quality-standard.en.md), the [quality status](quality-status.en.md), and the [guarantee register](guarantees.en.md).

## 1. Scope

Two things in the quality-control tracking issue are genuinely unfinished:

1. eight behaviours in the guarantee register that are not pinned down yet;
2. the end-to-end mainline where an agent writes a plugin, connects it, and iterates.

One item is not a checkbox in that issue but decides whether a bad agent-written plugin can take the host down: a limit on references and calls per session. This plan designs it as a prerequisite for the agent scenario.

Three items are already done with unified events and only need to be ticked in the tracking issue: events carrying their dispatch mode across boundaries, synchronous bail for Cordis events forwarded to rutis, and `internal/*` events across boundaries.

Not in this plan: `cargo xtask dev`, dsh's release and distribution, and the other independent gaps in the quality status (actionlint, licences, install smoke, Swift / Kotlin extensions, and so on). Those get their own issues instead of turning the quality tracker into a bucket for all quality work.

## 2. The eight register items

Each needs a repeatable test; when it passes, the register moves from "partly verified" / "not verified" to "verified". If a behaviour cannot be verified, it is downgraded to an explicit non-guarantee with a test that locks the current behaviour.

| # | Behaviour | What is missing | Test to add |
| --- | --- | --- | --- |
| 1 | Save queue and multiple writers: saves are strictly serial and in submission order; persistent conflicts return `Conflict` and keep the pending queue | Serial order of saves and `Conflict` after persistent conflicts have no test | `save_is_serial_and_in_submission_order`: two concurrent saves, storage writes in call order; `persistent_conflict_keeps_the_queue`: storage always conflicts, after 3 attempts `Conflict` is returned, the queue remains and `flush` retries |
| 2 | Restoring saved layers: after changing isolation or injection, composing the stored layers in a new loader gives the same desired tree and runtime state | Only the stored patch is asserted; changes to `isolate` / `inject` are not restored in a new loader | `isolate_and_inject_changes_restore_in_a_new_loader`: change, persist, build a new loader, compare the desired tree; recreate instances and compare runtime state |
| 3 | Instance edits all-or-none: renaming or reloading across several instances either applies to all or falls back to the old module for all, without writing the editable layer | Applying and rolling back `rename_module` / `reload` across instances has no test | `rename_and_reload_apply_to_every_instance_or_none`: two instances, one copy fails on purpose; both stay on the old module and the editable layer is unchanged |
| 4 | Go runtimes in eager mode: all start and stay running, including when idle | `eager` has no test | `go_runtimes_in_eager_mode_start_all_and_stay`: two binaries, `eager`; both processes start, are still there after `idle`, and exit when the host stops |
| 5 | Stop deadline: name every plugin still stopping when the deadline passes; when startup fails, close what already started under the same rules | Only `did not finish` is asserted, not each plugin; closing after a startup failure (not a signal) has no test | `deadline_names_every_stopping_plugin`: two plugins whose cleanup never returns, both lines appear; `startup_failure_closes_what_started`: the second runtime fails to start, the first is closed and the exit code is right |
| 6 | Exit codes: configuration errors, row-load failures and runtime start failures exit 1; a runtime that misses the exit deadline exits 2 | Configuration / row failures exiting 1 and a missed exit deadline exiting 2 have no test; start failure exiting 1 is only asserted in the release pipeline | `run_exits_1_on_config_and_row_failures`, `run_exits_2_when_a_runtime_misses_the_exit_deadline`, `run_exits_1_when_a_runtime_fails_to_start` (moved from `install.yml` into ordinary tests) |
| 7 | run / dev row state lines: `starting`, `failed: …`, `disabled`, `removed`, and instance-copy spelling | Only `running` and `waiting for its services` are tested | `row_state_lines_cover_every_state`: one configuration triggers each state, assert every line; instance copies assert `<row id> in PluginId(n)` |
| 8 | `rutis-host go add`: install a plugin binary into `runtimes.go.dir` with the local Go toolchain | No test at all | `go_add_installs_the_binary_into_the_configured_dir`: a temporary Go module (`replace` to the repository's `go/rutis`, no network), run the command, assert the binary and configuration; CI has Go, so it does not skip |

Acceptance: all eight are "verified", or explicitly downgraded with a locking test; the tracking issue's checkboxes are updated.

## 3. The agent-written plugin mainline

### 3.1 Product decision: two generic tools, not one tool per method

Give the agent two tools:

- `rutis_services`: no arguments, returns the currently available services. Each entry is `{ name, provider, methods, callable }`; `provider` is the row or host that provides it, `methods` is `{ method: "sync" | "async" }` when the runtime reports it (otherwise `null`), and `callable` says whether the agent can call it by name.
- `rutis_call`: arguments `{ service, method, arguments }`, calls one service method by name and returns JSON or a model-visible `error: ...`.

No "one tool per service method". Reasons:

- service methods have no cross-language JSON Schema; generating one per method would be guessing;
- the tool list would change on every plugin load, unload and hot swap, and the agent's prompt with it, which is hard to reproduce;
- two generic tools survive implementation, language and node changes; the agent reads the current state from `rutis_services` and learns arguments from errors.

Detailed method descriptions come from the plugin's own documentation; the runtime guarantees only names, kinds and results.

### 3.2 Data and call path

Add `Loader::services()`, returning:

```rust
pub struct ServiceInfo {
    pub name: String,
    pub provider: ProviderInfo, // row id, or the host
    pub methods: Option<serde_json::Value>, // { method: "sync" | "async" }
    pub callable: bool,
}
```

It takes names from the loader's current rows and the host service catalog; for each name it tries `ctx.get_as::<dyn HostDispatch>(host_key(name))`:

- present: `callable = true`, `methods` from `HostDispatch::methods()`;
- absent: list it with `callable = false`; `rutis_call` says the service is not exposed by name.

Add `ServiceToolsPlugin` (an optional `loader` feature of `rutis-agent`, depending on `rutis-loader` and the `rutis-bridge` session layer; no dependency cycle):

- `inject = [tools]`, registers the two tools in the `ToolRegistry`;
- `ToolRegistry` gains `register_scoped` (returning a token) and `unregister(token)`; on unload it removes exactly those two tools, and the token prevents deleting a same-named tool from someone else; the driver reads the schema every turn, so a late registration appears on the next turn;
- `rutis_call` invokes through `HostDispatch::invoke`; a future result is awaited with the existing settle helper, a sync method returns directly;
- unknown service, unknown method, not callable and bad arguments all become `error: ...` text; no panic, other tools unaffected.

### 3.3 Permissions and isolation decision

- **Plugins, including agent-written ones, are trusted by default, at the same level as hand-written plugins.** rutis provides no sandbox; stronger isolation means another process, user or container, or a remote connection. This matches the trust assumption in the quality status.
- `ServiceToolsPlugin` exposes only services the host can already see; it does not bypass a row's `isolate` / `inject` or change a service's visibility.
- Optional `allow([...])`: give the agent only the listed services; the default is every visible service. Services outside the list do not appear in `rutis_services` and `rutis_call` returns "not allowed".
- Remote connections keep the existing TLS, identity and access-point rules; the agent tools add no remote entry point.
- The agent's file and command permissions come from its existing bash / file tools; rutis neither adds nor weakens them.

### 3.4 Scenario and assertions

Use a scripted model with six fixed turns:

1. Turn 1: the agent uses a file tool to write a Python plugin providing a `weather` service in a temporary project.
2. The host loads the plugin; `rutis_services` shows `weather`; `rutis_call`'s `today("Oslo")` returns the old value.
3. Turn 2: the agent breaks the plugin (syntax error). The host reports a failed reload, the old version keeps serving; in turn 3 `rutis_services` still shows the old method and `rutis_call` still returns the old value.
4. Turn 4: the agent fixes it and changes the behaviour (new value). The host reloads; in turn 5 `rutis_call` returns the new value.
5. Turn 6: the agent deletes the plugin. `rutis_services` no longer has `weather`, `rutis_call` returns a clear error; with no other rows the Python runtime exits.

Assertions:

- the tool list (`rutis_services` result) and call results at every turn;
- the service does not stop during the failed reload; the old version keeps serving;
- after the fix the behaviour changes; two implementations never serve at once;
- after deletion the service is gone, calls fail explicitly rather than hanging or returning an old value;
- the residue check passes: no Python process, socket or temporary directory left.

### 3.5 Where the tests live

- A new fixture host: assembles the loader, `ToolsPlugin`, `ServiceToolsPlugin`, `AgentDriverPlugin` and a scripted model; writes plugins with a file tool and hands project changes to the loader with a small test-only 50 ms poll. Production apps use `rutis-host dev` or their own watcher; the fixture exists for determinism.
- A new E2E scenario `tests/e2e/tests/agent_writes_plugin.rs`, run on Linux, macOS and Windows. Use a test-only file-writing tool rather than bash, so Windows without bash still works.
- Unit tests inside `rutis-agent`: the `rutis_services` list follows services appearing and disappearing; `rutis_call` covers unknown service, unknown method, not callable and allowlist refusal; unregistered tools disappear from the list.

## 4. Resource limits (prerequisite for the agent scenario)

An agent-written plugin can grant many references or start many calls at once. Today only frame size and 64-level nesting are limited; there is no limit on references or calls.

Design:

- hello gains an optional `limits: { refs, calls }`; absent means unlimited, for old peers.
- Defaults: 4096 live references and 1024 in-flight calls per session; configurable in the host and the SDKs.
- Over the limit returns a structured error and fails only that call: the session is not closed, nothing is silently dropped, nothing is retried.
- References count per grant; incoming calls are checked before execution; a reference in event arguments or results fails only that dispatch.
- Rust and each of the six language implementations get boundary tests (`limit - 1`, `limit`, `limit + 1`); after the failure the session continues and counts return to baseline.

Not done: byte or per-plugin quotas, admission control, queue lengths. This plan does not introduce two resource models.

## 5. Order

| PR | Content | Depends on |
| --- | --- | --- |
| 1 | Three loader tests: save queue, saved-layer restore, instance edits all-or-none | — |
| 2 | Two tests: Go eager and `go add` | — |
| 3 | Three host tests: stop deadline, exit codes, row state lines | — |
| 4 | `Loader::services()` + `ServiceToolsPlugin` + unregister + unit tests | — |
| 5 | Agent-written plugin E2E fixture and scenario (three platforms) | 4 |
| 6 | Allowlist, permission docs, eight register rows moved to "verified" | 1–5 |
| 7 | Resource limits: protocol field and the Rust side | — |
| 8 | Resource limits: Node, Python, Go, Bun | 7 |
| 9 | Resource limits: Swift, Kotlin and cross-version | 7, 8 |

Each PR does one thing; tests and documentation land with the implementation. The PR description states which behaviour it closes and on which platforms it ran.

## 6. Done criteria

- [ ] all eight register items are "verified", or explicitly downgraded with a locking test;
- [ ] the tracking issue ticks the three finished event items and closes the remaining boxes;
- [ ] `rutis_services` and `rutis_call` follow plugin load, failed reload, fix and delete correctly;
- [ ] the old version keeps serving during a failed reload; calls fail explicitly after deletion; the residue check passes;
- [ ] the allowlist and the "plugins are trusted, no sandbox" decision are documented and tested;
- [ ] resource limits pass boundary tests in Rust and all six language implementations, and old peers behave as before;
- [ ] the E2E passes on three platforms; Windows uses the file-writing tool, not bash.

## 7. Risks and treatment

| Risk | Treatment |
| --- | --- |
| `Loader::services()` cannot see host services that exist only in Rust's type system | List them with `callable = false`; document that a service must be exposed by name (`HostDispatch` / export) to be callable by the agent |
| The tool list changes on hot swap, making scripted tests brittle | `rutis_services` is data, not a tool; the tool schema is fixed and scripts assert by service name |
| Loader behaviour on a failed reload differs from `rutis-host dev` | The fixture uses the loader's own edit API; the production watcher's behaviour is covered by another E2E |
| Resource-limit defaults hit normal plugins | Re-check against the existing event matrix and soak data first; defaults may only rise; options exist |
| Turning permissions into half a sandbox | Explicitly no sandbox; only an allowlist and visibility, no new capability model |

## 8. Not in this plan

- a full sandbox, capability permissions, per-plugin file / network restrictions;
- a JSON Schema or a separate tool per service method;
- agent tools for instance-scoped or remote services (the first version only covers global services visible to the host);
- `cargo xtask dev`, dsh's release and distribution;
- actionlint, licences, install smoke, Swift / Kotlin extensions and other quality gaps;
- bulk backfilling of historical design documents.
