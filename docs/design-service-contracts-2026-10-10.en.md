# Service contracts: keeping the implementations of one service consistent

[中文](design-service-contracts-2026-10-10.md)

Design proposal · Related to [#211](https://github.com/arcships/rutis/issues/211) · Covers [quality standard](quality-standard.en.md) Q6.9, Q6.7; [status](quality-status.en.md) scenario G, risk G1 (P0) · See also [#206](https://github.com/arcships/rutis/issues/206), [#188](https://github.com/arcships/rutis/issues/188) (S4)

## 1. The problem

The main line of the [design philosophy](design-philosophy.en.md) §4: `planner` depends on the service name `calendar`. `calendar` is first provided by a Python plugin, then by a plugin on a node in the internal network, and finally by a Rust implementation. `planner` does not change a line, and the host does not restart.

The kernel guarantees that when the provider changes, the plugins that depend on it stop and start again. It does not guarantee that the new provider returns the same data as the old one. Today:

- a plugin can only declare a service's method names and method shapes (`sync` / `async`);
- the data shape (field names, which fields may be missing, error types) is only a convention;
- nothing checks whether two implementations agree.

Examples of disagreement. They all come naturally when crossing languages:

| Disagreement | Example |
| --- | --- |
| Field names | Python returns `{"start_time": ...}`, TypeScript returns `{startTime: ...}` |
| A field with no value in an object | Python writes `{"note": None}` (the other side receives `null`); the usual TypeScript style leaves the field out (the other side receives no `note`) |
| A method with no result | A TypeScript method without `return` sends `undefined`, which a Python consumer receives as the SDK's `UNDEFINED`; a Python method returning `None` sends `null`, which a TypeScript consumer receives as `null`. Checks like `=== undefined` and `is None` flip after the switch |
| Empty results | One implementation returns `[]`, another `null` |
| Errors | When nothing is found, one raises `KeyError`, one raises `NotFound`, one returns `null`; Rust's `native_error` is always named `RustError` |
| Method shapes | One implementation declares `list` as `sync`, another as `async`; consumers call them differently |
| Collection types | Python returns a tuple, the other side receives an array; it returns a set, which cannot cross |

The consumer breaks after the switch. The failure shows up in the consumer, the cause is in the provider, and the switch happens while running. This is risk G1: high impact (it defeats, in practice, the core promise "consumers depend only on the service name"), high likelihood (different people write the implementations, in different languages).

## 2. What exists today

| Existing | What it does | Relation to this problem |
| --- | --- | --- |
| `provides` declarations ([plugin API](guide/plugin-api.en.md)) | Service name, method names, `sync` / `async` | Method shapes are already a contract; data shapes are not |
| Value passing rules (plugin API, "How values are passed") | What is copied across processes, what passes by reference, what cannot cross | They define which values can appear on the wire, not which value a given service should return |
| SDK test tools (`@arcships/rutis/testing`, `rutis.testing`) | Load a plugin without a host and call the services it provides; strict mode passes values by the cross-process rules | They test plugins in their own language only; expectations are written in that language's test code, and other implementations cannot reuse them |
| Runtime conformance checks (`rutis_bridge::runtime::testing`) | A set of calls and expectations written in Rust, run against the same fixture (`conformance-weather`) in the Node and the Python runtime | Exactly "one set of cases, many implementations", but only for the runtimes themselves, with cases hard-coded in Rust |

The last row shows the approach already works in this repository: one set of calls and expectations, run against implementations in different languages. This design extends it from "runtime consistency" to "consistency of any service", and lets plugin authors write the cases.

## 3. Options

### 3.1 Three options

**(a) Contract cases only.** A set of calls and expected results for a service, written once in JSON. An SDK test tool runs it against an implementation in its language; a real host runs it against an implementation in any language, at any location. Data shapes are not declared.

**(b) Declared data shapes.** A JSON Schema for each method's arguments and result, reported to the host with `provides`. The host checks every call, or the check runs only in tests.

**(c) Both, layered.** Cases first; an expectation inside a case may refer to a Schema fragment, meaning "any value of this shape is fine". The Schema only appears in case files. It does not go into `provides`, is not reported to the host, and is not checked at run time.

### 3.2 Comparison

| | (a) Contract cases | (b) Declared data shapes | (c) Layered (Schema only in cases) |
| --- | --- | --- | --- |
| What the author writes | A few concrete calls and results | A full Schema for every method's arguments and result, kept in sync with the code in every language | (a), plus Schema fragments where needed |
| What it detects | Any difference on the inputs the cases cover: field names, `null` versus missing, error names, method shapes, semantics ("the same input gives the same thing") | Shape differences on every call, including inputs no case covers; not semantic differences (right shape, wrong value) | All of (a); for values that vary (ids, times) it can check the shape, not just the type |
| What it misses | Inputs no case covers | Semantics; differences a loose Schema allows | Inputs no case covers |
| Run-time cost | None (runs only in tests) | The host validates every call, at a cost that grows with the size of values; none if checked only in tests | None |
| Kernel and host changes | None; `rutis-bridge` gets a test module, `rutis-host` gets a command | `provides` format extended, runtimes report it, the host stores and validates it; node exports carry it too | Same as (a) |
| Principle 6 (contracts only what really crosses the boundary, no general type layer) | Fits: no new declaration | Breaks it: every method's data shape becomes a declaration across the boundary, and the natural next step is generating each language's types from the Schema, which is a type layer | Fits: the Schema is only a way of comparing in tests, not a declaration of the service |
| Principle 8 (dependency declarations contain only service names) | Fits: cases run by service name and do not care where or in what the implementation is written | Dependency declarations do not change, but the host gains knowledge that "a service has a shape", and must decide what to do on a mismatch at switch time (refuse? warn?) | Fits |
| Maintenance | One file format, three runners (Node, Python, Rust) | A format extension, reporting in three SDKs, host-side validation and mismatch handling, the node protocol | Same as (a), plus a Schema validator dependency in the Rust runner |

### 3.3 On option (b)

The strength of (b) is real: it checks every call, not just the few a case covers. But:

1. It turns data shapes into a declaration across the boundary, which is exactly what principle 6 rejects. The reasoning behind principle 6 still holds: each language has its own type system, a common type layer across the boundary needs a mapping maintained in every SDK, and rutis's value passing rules already define which values can appear on the wire.
2. The most dangerous differences in G1 are semantic: for the same "not found", one implementation returns `null` and another raises. Both can match a loose Schema. Only a concrete case can write down "this input should produce this result".
3. Run-time validation makes the host decide what to do about "a different shape" at switch time. That is a new core behavior, with new guarantees and new failure handling.

So this design does not choose (b). If cases later prove insufficient, the next step is (c): the Schema only as a way of comparing inside cases, still not in `provides`.

## 4. Conclusion

- **Adopt (a), and leave room for (c).** A service's contract is a JSON file: method shapes plus a set of cases. No new declaration, no kernel change, no protocol change.
- **One file, run in three places.** The Node test tool, the Python test tool, and Rust (embedding hosts and the `rutis-host contract` command) all run the same file and reach the same verdict.
- **Values are compared after crossing the boundary.** Cases describe what the consumer actually receives, so differences such as `null` versus missing or array versus tuple are found.
- **A contract belongs to a service name, not to an implementation.** The file can live anywhere; rutis does not build a registry.
- **Contracts run only in tests.** `rutis-host run` does not run contracts, and contracts do not change routing or gating.

## 5. The contract file

### 5.1 Example

```json
{
  "format": 1,
  "service": "calendar",
  "revision": "2026-10-10",
  "methods": { "add": "async", "list": "async", "get": "sync" },
  "cases": [
    {
      "name": "add an event, then list it",
      "steps": [
        {
          "call": "add",
          "args": [{ "title": "standup", "start": "2026-10-11T09:00:00Z" }],
          "result": { "id": { "$type": "string" } },
          "as": "added"
        },
        {
          "call": "list",
          "args": ["2026-10-11"],
          "result": [
            { "id": { "$from": "added.id" }, "title": "standup", "start": "2026-10-11T09:00:00Z", "note": null }
          ]
        }
      ]
    },
    {
      "name": "raises NotFound when nothing is found",
      "steps": [
        { "call": "get", "args": ["no-such-id"], "error": { "name": "NotFound" } }
      ]
    }
  ]
}
```

### 5.2 Fields

| Field | Meaning |
| --- | --- |
| `format` | The file format version, currently `1`. A runner refuses a version it does not know, and says so |
| `service` | The service name. Only the name: no language, no location |
| `revision` | Optional. The contract's own revision mark, any string, shown only in reports |
| `methods` | Method name to `sync` / `async`. The runner first compares the implementation's declared method shapes: every method in the contract must be declared by the implementation, with the same shape. Extra methods in the implementation are not an error |
| `cases` | The cases, run in order |
| `cases[].name` | The case name, shown in reports |
| `cases[].steps` | The steps, run in order |
| `steps[].call` | The method name; must be in `methods` |
| `steps[].args` | The argument array, passed by position. Cross-language calls have positional arguments only |
| `steps[].result` | The expected result (see §5.3). Either this or `error` |
| `steps[].error` | The expected error (see §5.4) |
| `steps[].as` | Optional. Names this step's result so later steps can refer to it with `$from` |

The file is external input under [quality standard](quality-standard.en.md) Q6.4: an unknown field, an unknown `$` matcher, or a `call` missing from `methods` is reported before any call runs, and the report gives the location (`cases[1].steps[0].call`).

### 5.3 Comparison rules

Expected values are plain JSON values, compared in the form they have after crossing the boundary:

| Rule | Explanation |
| --- | --- |
| Exact comparison | An object's set of fields must be identical: one missing or one extra field fails |
| Missing, `null` and `undefined` are all different | `{"note": null}` differs from `{}`. The wire has a third value, `undefined` (a TypeScript method that returns nothing, an omitted argument), written `{"$undefined": true}` in expectations. All three affect how consumers write their code, so all three are told apart |
| Arrays are compared in order | Same length, same value at every position |
| Numbers are compared by value | `1` equals `1.0`. JavaScript has one number type, so the integer / float distinction does not hold across languages (not guaranteed, §10) |
| Strings and booleans are compared exactly | |

There are only three matchers. They deal with values that differ on every run, with dependencies between steps, and with `undefined`, which JSON cannot express. They are ways of comparing, not type declarations:

| Matcher | Meaning |
| --- | --- |
| `{"$type": "string"}` | Any value of this JSON type: `string`, `number`, `integer`, `boolean`, `null`, `array`, `object` |
| `{"$from": "added.id"}` | Equal to a value in an earlier step's result. Also usable in `args`, to pass an earlier result into a later call |
| `{"$undefined": true}` | The wire's `undefined`. JSON cannot express it, so it needs a notation; also usable in `args` |

Every key starting with `$` in an expectation is a matcher. If a service's data really has a field name starting with `$`, write `{"$literal": {...}}`.

Exact comparison is deliberate: a consumer may use a field that implementation A returns in addition, and break when switched to B. A notation for allowing extra fields waits for the decision in §14.

### 5.4 Errors

Across the boundary, only an error's name and message are guaranteed (plugin API, "How values are passed"). A contract compares only the **name**:

```json
{ "call": "get", "args": ["no-such-id"], "error": { "name": "NotFound" } }
```

- Messages are for people and are written differently in each language; they are not compared.
- The implementation chooses the name: in Python it is the exception class name, in JavaScript `error.name`, in Rust the `name` of `Error::Remote`. Today Rust's `native_error` always gives `RustError`, so a helper that builds an error with a given name is needed (§12, acceptance criterion 8).
- Expecting `result` when the implementation raises, or expecting `error` when it returns normally, fails; the report shows what was actually received.
- Transport errors (connection lost, process exited) and `SyncWaitCycle` cannot be expected: when one occurs, the run fails and the report says it is a run failure, not a contract difference.

### 5.5 Sync and async

Method shapes come from `methods`; cases do not repeat them. The runner calls by shape:

- `sync`: called synchronously and the return value compared. A Promise or awaitable returned by the implementation is reported as a shape mismatch (the test tools already do this);
- `async`: called, awaited, and the awaited value compared. A raise and a rejection are treated the same;
- every step has a timeout, 10 seconds by default, only to prevent hangs (Q7.1.2). A timeout fails the step, and the report says it timed out.

### 5.6 Not covered in the first version

| Not covered | Why |
| --- | --- |
| Functions and live objects (arguments or results passed by reference) | JSON cannot say how a callback should be called; see open question §14 |
| Events | Contracts cover service methods only |
| Timing and performance | Cases look only at results |
| Externally visible side effects (files written, requests sent) | Observable only through the service's own methods |
| Ordering between concurrent calls | Steps run one at a time, in order |

## 6. Where contract files live

A contract belongs to a service name, and different people are responsible for a service name at different stages. So rutis defines the format but not the location, and builds no registry. The suggested practice:

| Stage | Location | Why |
| --- | --- | --- |
| Trying: one implementation | In the first implementation's project, `contracts/<service>.json` | The author of the implementation knows best what it should return; run with the SDK test tool |
| Validating: consumers appear | A consumer may write its own case file for the same service (`contracts/calendar.planner.json`), describing the part it depends on | The consumer knows best what it depends on; the provider runs these files too |
| Hardening: a second implementation appears | Moved to a shared place: a repository directory holding only contracts, a package (npm, PyPI or a crate; the file does not depend on language), or simply copied | The new implementation must pass the same files |

A service may have several contract files. The runners accept several files, and all must pass. If two files give the same method different shapes in `methods`, that is reported before running.

This design does not choose "always with the consumer" or "always with the provider": both happen in how rutis is used, and a shared format is enough; the location is up to the project. The maintainer needs to confirm this (§14, decision 2).

## 7. Running contracts

### 7.1 The Node test tool

```ts
import { load, runContract } from '@arcships/rutis/testing'
import contract from '../contracts/calendar.json' with { type: 'json' }
import plugin from '../src/index.ts'

test('calendar contract', async () => {
  const t = await load(plugin, { services: { store: fakeStore() } })
  await runContract(contract, t.service('calendar'))   // throws ContractError on failure, listing each failed step
  await t.unload()
})
```

`runContract` uses the service from `t.service(name)`, that is, "the one rutis sees": only declared methods, and in strict mode values pass by the cross-process rules. So it compares values after they cross the boundary. The runner also checks that `contract.service` matches the name given to `t.service`, and that `methods` matches the `provides` declaration.

### 7.2 The Python test tool

```python
from rutis.testing import load, run_contract

async def test_calendar_contract():
    async with load(calendar_plugin, services={"store": FakeStore()}) as t:
        await run_contract("contracts/calendar.json", t.service("calendar"))
```

It accepts a file path or an already parsed dict. It behaves like the Node version.

### 7.3 Rust: implementations and embedding hosts

`rutis-bridge` gets a `contract` module under the `testing` feature:

```rust
let contract = rutis_bridge::contract::Contract::load("contracts/calendar.json")?;
let calendar = ctx.get_as::<dyn HostDispatch>(host_key("calendar")).unwrap();
rutis_bridge::contract::run(&contract, calendar).await?;   // the Err carries the same report
```

The runner only uses `HostDispatch`: it finds the service by name and calls methods by name. So to the runner, a Python row, a Node row, an import from a remote node, and the host's own Rust implementation are all the same thing. Authors of Rust implementations use it in `cargo test`; the S4 fixture Rust host uses it too.

The runner compares the session layer's `Value`, not the result of `Value::json()`: `json()` turns `undefined` into `null`, which would erase the distinction in §5.3.

### 7.4 rutis-host contract: real hosts and CI

```
rutis-host contract [rutis.json] contracts/calendar.json [contracts/calendar.planner.json ...]
```

1. Start the host from the configuration (assembled as for `run`);
2. Wait for the contract's service to appear, for at most 30 seconds. If it does not, fail and print the row's current state (the same information as `check`);
3. Run all cases against it;
4. Shut the host down, check that nothing is left behind (Q2.4), and exit with the result.

| Exit code | Meaning |
| --- | --- |
| 0 | All passed |
| 1 | Some case failed |
| 2 | Could not run: invalid contract file, invalid configuration, the service did not appear, or a transport failure during the run |

`--json` prints a machine-readable report; its format is an interface (Q6.6.6).

With a `rutis.json` prepared for testing, it runs against any implementation: a local Python row, a `peer:B/calendar` import, a row on a `remote` runtime. In CI, write one configuration per implementation and run the same contract file against each.

Contracts really call the service. Running them against a production instance makes the user responsible for the side effects; the documentation must say to use a test configuration (not guaranteed, §10).

### 7.5 Reports

All three runners produce reports with the same structure:

```
calendar (revision 2026-10-10): 2 cases, 1 failed
  ✗ add an event, then list it
    step 2 list("2026-10-11")
      result[0].note: expected null, the field is missing
  ✓ raises NotFound when nothing is found
```

Paths are written the same way in all three runners, so reports can be compared (§9).

### 7.6 Contracts as consumer test doubles (second phase)

A consumer's tests often need a fake `calendar`. The same contract file can produce one: it matches a call against the steps in the cases by `args`, returns the expected result or raises the expected error, and raises `ContractError` on a call that matches nothing. The data shapes in the consumer's tests are then the same ones the providers are checked against. It is not required for the first version (§15).

## 8. Versions and evolution

- **The file format** has a `format` version. When matchers or fields are added, an old runner reports a newer file and says which package to upgrade; new runners keep accepting old formats.
- **The contract content** has an optional `revision`, used only in reports. rutis does not compare the revision a consumer expects with the revision a provider passed: dependency declarations contain only service names (principle 8), without versions.
- **Compatible changes**: adding cases; adding methods (an old implementation fails the new contract, on purpose: the new method is now part of the contract).
- **Incompatible changes** (renaming a field, renaming an error, changing a method shape): rutis has no service version numbers, so an incompatible change means a new service name (`calendar2`), or switching provider and consumers in the same configuration edit. This design does not change that. It only makes such a change visible to contract cases, instead of to a consumer at run time.

This answers part of the open question "evolution of contracts" in the design philosophy §8: consistent data shapes no longer rest on convention alone, but on a set of cases that can run against any implementation; without adding a type layer, and without enforcement at run time.

## 9. Relationship with #206 and S4

**#206 (test tools agree with real runtimes, risk A1).** #206 asks for the same calls to run in the test tools and in the real runtimes, with the results compared. The contract runners provide exactly that:

1. Write the parts of `conformance-weather` that JSON can express (`today`, `later`, errors) as a contract file;
2. Run it against the fixture in the Node and Python test tools, and with `rutis-host contract` against a Node row and a Python row;
3. The four reports should be identical. Where they differ is what #206 is looking for, and goes into the list of allowed differences.

`each` (callbacks) and `crash` (process exit) cannot be written as JSON cases and stay in the Rust conformance checks.

The other way round, the contract runners are only as trustworthy as #206 makes them: if the test tools' strict mode passes values differently from the real runtimes, a contract that passes in the SDK may fail in a host. So the #206 comparison should include the contract runners themselves. The two can be done together: first the format and the three runners, then the weather contract completes the #206 comparison.

Two possible differences seen while reading the code, to be confirmed by the #206 comparison:

- The Node test tool's strict mode keeps `undefined` fields inside plain objects; the real Node runtime encodes data-only objects as JSON, and `undefined` fields become `null`. For the same implementation, a contract would see `{"$undefined": true}` in the test tool and `null` in a host. This is K4;
- In the Python test tool, an exception raised by the implementation does not go through `_cross`, so the test receives the original exception object. Contracts compare only the name and are not affected, but #206 should list it as a known difference.

**S4 (#188, moving an implementation).** S4 checks the switch itself: calls during the switch do not hang (G2), the old implementation leaves nothing behind (G3), the host does not restart (G5). G1 is handled by contracts. In S4:

- write a `calendar` contract (including the calls `planner` makes);
- each of the three implementations (Python row, the implementation on node B, the Rust implementation) passes it first;
- after each switch, the S4 fixture Rust host runs it again with `rutis_bridge::contract::run`, confirming that the service consumers see at that moment still meets the contract.

S4 then proves the whole sentence: the implementation changed, `planner` did not, and neither did the data it sees.

## 10. Behaviors and their levels

This design does not change the kernel and adds no core promise. "Consumers depend only on the service name, not the implementation" is already a core promise (Q3.1); contracts are a way of verifying it at the data level.

| Behavior | Level |
| --- | --- |
| The same contract file, against the same implementation, gives the same pass / fail and the same failure paths in the Node test tool, the Python test tool and the Rust runner (allowed differences written down and bounded) | Contract |
| Comparison rules in §5.3: exact object fields; missing, `null` and `undefined` all different; arrays in order; numbers by value; the meaning of `$type`, `$from`, `$undefined`, `$literal` | Contract |
| Errors compared by name only (§5.4); a transport error fails the run rather than counting as a difference | Contract |
| Method shape check (§5.5): every contract method declared by the implementation with the same shape; a `sync` method returning a Promise / awaitable fails | Contract |
| A 10-second default timeout per step; a timeout is a failure | Contract |
| An invalid contract file is reported, with its location, before any call runs | Contract |
| `rutis-host contract`: its steps, exit codes 0 / 1 / 2, the `--json` format; nothing left behind afterwards | Contract |
| `rutis-host run` and the loader neither read nor run contracts; contracts do not affect routing or gating | Contract |
| The textual layout of reports | Implementation detail |
| The host checking at run time that an implementation meets a contract | Explicitly not guaranteed |
| Passing a contract implying equal behavior on inputs outside the cases | Explicitly not guaranteed |
| The integer / float distinction (`1` versus `1.0`) holding across languages | Explicitly not guaranteed: JavaScript has one number type |
| Equal error messages | Explicitly not guaranteed |
| The contract revision a consumer expects matching the one a provider passed | Explicitly not guaranteed: dependency declarations have no versions |
| Functions, live objects, events, concurrent ordering, performance covered by contracts | Explicitly not guaranteed (first version) |
| Running a contract against a production instance having no side effects | Explicitly not guaranteed: contracts really call the service |

Two of the "not guaranteed" items might be mistaken for guarantees and get a test that pins them (Q3.4): `rutis-host run` does not run contracts (not even with a contract file in the configuration); `1` and `1.0` compare equal.

## 11. Risk assessment

Scenario: plugin authors use contracts to keep data shapes unchanged when switching implementations (scenario G). The table lists what can go wrong with this design itself.

| # | What can go wrong | Dimension | Impact | Likelihood | Priority | Control |
| --- | --- | --- | --- | --- | --- | --- |
| K1 | Too few cases: the contract passes while implementations still differ outside the cases | Semantics | High | High | P0 | Cannot be fully automated. Controls: consumers can add their own case files (§6); the S4 contract covers every call `planner` makes; user docs say "a contract covers only the cases written down". The residual risk is recorded in the status document |
| K2 | The three runners compare differently: the same result passes in Node and fails in Rust, or the reverse | Semantics | High | Medium | P0 | One conformance file for the comparison rules: a list of `(expected, actual, verdict, failure path)` covering every rule in §5.3 and every matcher; all three runners run it before merge |
| K3 | The comparison treats different values as equal (e.g. `null` versus missing, an extra field) and misses typical G1 differences | Semantics | High | Medium | P0 | Each rule has counter-examples in the K2 conformance file; the counter-example implementations in §12, criterion 5 |
| K4 | Passes in the SDK test tool, fails in a real host (the test tool passes values differently from the runtime) | Semantics | High | Medium | P0 | Shared with A1: the #206 comparison includes the contract runners (§9) |
| K5 | The implementation hangs and the contract run hangs with it | Failure | Medium | Medium | P1 | Per-step timeout; `rutis-host contract` waits for the service with a limit; a test with an implementation that never returns |
| K6 | `rutis-host contract` leaves processes or links behind | Residue | Medium | Medium | P1 | Shares the E + R residue check |
| K7 | Rust implementations cannot name their errors, so error names in contracts never match a Rust implementation | Compatibility | Medium | High | P1 | A helper that builds an error with a given name; Rust positive and negative tests |
| K8 | After a format change old files no longer run, or an old runner silently ignores a new matcher | Evolution | Medium | Medium | P1 | The `format` version; unknown fields and matchers are errors; boundary and mutation tests of the parser (Q6.4) |
| K9 | Running a contract against a production instance causes side effects | Security | Medium | Low | P2 | User docs; `rutis-host contract` reads only the configuration the user names |
| K10 | Contracts misused as a run-time check, e.g. someone adds "run the contract before switching" to the loader | Semantics | Medium | Low | P2 | §10 states it is not guaranteed; a test pins that `run` does not run contracts |

## 12. Acceptance criteria

Each can be written as an automated check:

1. The contract file format has a description (Chinese and English) and a JSON Schema describing the file itself (a test file, not a service);
2. A comparison-rule conformance file exists, and the Node, Python and Rust runners give the same verdict and failure path for every entry;
3. Parsing: an unknown `format`, field or matcher, a `call` missing from `methods`, and conflicting method shapes across files are all reported before any call, with their location; there are boundary and mutation tests;
4. `runContract` (Node), `run_contract` (Python) and `rutis_bridge::contract::run` (Rust) pass against a correct implementation;
5. Counter-example implementations, each differing in one place, each fail, and the report points to the right case, step and path: a renamed field; `null` becoming missing; `null` becoming `undefined`; an extra field; a different error name; returning `null` where an error is expected; `sync` declared as `async`; a missing method;
6. A method that never returns fails after the timeout, and the test itself does not hang;
7. Black-box tests of `rutis-host contract`: the cases for exit codes 0, 1 and 2; `--json` output; the error when the service never appears; the residue check passes afterwards;
8. A Rust implementation can raise an error with a chosen name, and a contract's `error.name` matches it;
9. The weather contract (§9) gives the same report in the Node and Python test tools and in `rutis-host contract` (Node row, Python row) (shared with #206);
10. Under `rutis-host run`, a contract file present is not read (pins the "not guaranteed");
11. S4: the `calendar` contract passes against the Python, node and Rust implementations, and passes again when the host reruns it after each switch.

## 13. Component types

| Type (§6) | Part | How the requirements are met |
| --- | --- | --- |
| Q6.9 Service contracts across implementations | The design as a whole | 1. Method shapes are already declarable; data shapes are expressed by cases rather than declarations (§3.3 explains why not literally "declared"). 2. One set of cases runs against any implementation (§7). 3. The switch itself is verified by S4 (§9) |
| Q6.7 Plugin SDKs and test tools | `runContract`, `run_contract` | Same semantics in both SDKs (criteria 2, 4); agreement with real runtimes (criterion 9, #206); strict mode compares values after crossing, exposing early what fails only across processes |
| Q6.6 User-facing entry points | `rutis-host contract` | Black-box tests, exit codes documented (criterion 7); `--json` is an interface |
| Q6.4 External input parsing | Contract files | Boundary and mutation tests, errors give the location (criterion 3) |
| Q6.5 Resource holders | The host started by `rutis-host contract` | Nothing left behind (criterion 7) |

Q6.9.1 says "data shapes must be declarable". This design expresses data shapes with cases, not a Schema; §3.3 gives the reasons. If the maintainer holds that Q6.9.1 must be met literally, option (c) in §3 meets it without breaking principle 6: the Schema appears only in contract files. This needs a decision (§14, decision 1).

## 14. Open questions and decisions needed

**Decisions for the maintainer:**

1. **Reading of Q6.9.1.** Do cases count as "data shapes are declarable"? If not, should Q6.9.1 be reworded, or should the first version already include the `$schema` matcher of (c)? This design leans to the former: cases only in the first version, `$schema` once a real need appears.
2. **Where contract files live.** This design only suggests (§6), it does not prescribe. Should there be a default location (e.g. `contracts/` in a project) that `rutis-host contract` finds when no file is given?
3. **Extra fields.** Comparison is exact by default; one extra field fails. Is a notation for "extra fields allowed" needed (e.g. `"$extra": true` in an object)? It would make contracts easier to maintain, but lets through "a consumer uses a field only A returns".
4. **State between cases.** Cases run in order against one instance; a stateful service relies on the cases themselves to avoid interfering (e.g. different ids). Is an optional reset convention needed (e.g. a `reset` method declared in the contract and called before each case)?
5. **Whether `rutis-host dev` runs contracts.** In dev mode, running the project's `contracts/` after each reload would let an agent's iterations notice shape changes sooner. But it adds a behavior to dev mode. Do it, and when?

**Open questions:**

6. **Functions and live objects.** How should a method whose argument is a callback (e.g. `each(fn)`) be written as a JSON case? One possible notation is `{"$callback": [...expected arguments], "returns": ...}`, but it makes the format much more complex. Not in the first version.
7. **Events.** Should events emitted by different implementations of a service have contracts too? Across the boundary events are only notifications today (boundary rules §5); not for now.
8. **Contracts and the types generated for Cordis mounts.** Cordis plugins have types generated from TypeScript when mounted (risk F2). Whether those types could be exported as part of a contract has not been studied.

## 15. Phases

| Phase | Content | Related |
| --- | --- | --- |
| C1 | Format description and file Schema; the Rust runner (`rutis-bridge` `contract`); the comparison-rule conformance file; errors built by name; counter-example tests | G1, K2, K3, K7, K8 |
| C2 | Node `runContract`, Python `run_contract`; the weather contract; comparing the reports from the three places | #206, A1, K4 |
| C3 | The `rutis-host contract` command, exit codes, `--json`, residue check; user docs (a section in the plugin API guide) | K5, K6, Q6.6 |
| C4 | The S4 `calendar` contract, three implementations, rerun after switches | #188, G1 |
| Later | Consumer test doubles (§7.6); whatever §14 decides to do | |

CI: the C1 comparison-rule conformance and counter-example tests run with the `rutis-bridge` tests; C2 with the Node and Python SDK tests; the C3 black-box tests with the `rutis-host` tests. No new CI job is needed; if S4 needs one, #188 will ask for it.
