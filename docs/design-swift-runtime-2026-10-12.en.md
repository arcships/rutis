# The Swift plugin runtime (design draft)

[中文](design-swift-runtime-2026-10-12.md)

Status: design draft, not implemented (an issue should track it). Date: 2026-10-12. Base: `main` `6f91ce8`.
Depends on: [the multilang master plan](design-multilang-runtimes-2026-10-03.en.md) (this is its M3, with its dlopen approach revised — appendix A), [M1](design-multilang-m1-2026-10-04.en.md), [M2](design-multilang-m2-2026-10-04.en.md), [the Go plugin runtime](design-go-runtime-2026-10-10.en.md) (this design borrows much of its machinery and decisions; a side-by-side is appendix C), [the Bun runtime](design-bun-runtime-2026-10-09.en.md), [the plugin API](guide/plugin-api.md).

Out of scope: a Swift-side Cordis; build systems other than SwiftPM; the host compiling Swift code; Linux and Windows (§14).

## 1. Decisions

| # | Decision | Appendix |
| --- | --- | --- |
| S1 | One binary, one runtime process; the host runs several Swift runtimes side by side | A |
| S2 | One binary holds one or more plugins, as its `main.swift`'s `Rutis.serve(...)` says | A |
| S3 | Row name `swift:<plugin>`, routed by `SwiftResolver` through manifests; qualified form `<runtime>:<plugin>` | — |
| S4 | The runtime name comes from the file name (prefix `swift-`) and is the only user-visible name | — |
| S5 | Binaries describe themselves: `--rutis-manifest` prints the manifest; resolving needs no running process | — |
| S6 | While a process runs, resolving uses the manifest it started with; a replaced file takes effect on restart, never automatically | — |
| S7 | Start on demand, stop when idle; `eager` starts all and never stops | — |
| S8 | Calls within one binary skip IPC; across binaries and languages, rutis forwards by name | — |
| S9 | One Task per incoming call: the runtime is reentrant, declares `reentrant-sync`; does not declare `sync-stack` | B |
| S10 | The call chain lives in a `@TaskLocal`; a synchronous wait suspends the current Task and never blocks a thread; methods default to `async`, `.sync` marks the rest | B |
| S11 | Leaf plugin; plugin API version 1; config decoded via `Codable`, its schema written by hand on the config type | C |
| S12 | Providing services: an explicit closure table (Swift has no method reflection); same-process calls go direct | C |
| S13 | Values split along the type system: value types copy as data, closures cross as references; `Rutis.ref` passes an object by explicit reference | D |
| S14 | Using services: a struct of closure fields conforming to `RutisClient`, built from a proxy (parameter-pack inference) | C |
| S15 | The host does not compile; `run` only runs ready-made binaries, `dev` only rebuilds the project itself | — |
| S16 | Binaries of different SDK versions coexist; host and binary agree only on the wire protocol, the manifest format, and the plugin API version | — |
| S17 | The SDK lives at `swift/rutis`, distributed through a `Package.swift` at the repository root, versioned by the main release line's `v` tags; depends only on Foundation | E |
| S18 | Platforms: macOS only (13 and up); Linux is a follow-up, Windows is out | — |

## 2. Runtime model

```text
rutis host
 ├─ LocalRuntime::node / ::python ...
 ├─ SwiftRuntimes (SwiftResolver + lifecycle)
 │    ├─ plugins/swift/weather ── swift-weather ── weather
 │    ├─ plugins/swift/kits   ── swift-kits    ── calendar, contacts
 │    └─ plugins/swift/ml     ── not started (no row uses it)
 └─ LoaderPlugin + one RuntimeRowsPlugin per running runtime
```

- Identical to the Go runtime's model: a row depends on `RuntimeRows#<runtime>` plus every name the plugin `inject`s (a leaf runtime; rutis gates all names); the services a plugin `provides` are published from the row's fiber to `host_key(name)`; when the process exits, its rows stop, and rows using its services stop with them.
- `isolate`, instance labels, service ids, exported handles: same as the Python and Go runtimes.
- TCC is attributed to the binary the plugin lives in (§9).

## 3. Binary and manifest

### 3.1 Binary

- A plugin author distributes: a Swift package (exporting plugin definitions) plus `cmd/<name>/main.swift` (serving its own plugins), or one package with several plugins compiled into one binary.
- The host accepts any executable that answers `--rutis-manifest` (sources in A.5).

```swift
// example.com/kits/cmd/kits/main.swift
import Rutis
import WeatherKits

Rutis.serve(WeatherKits.calendar, WeatherKits.contacts)
```

| `serve` rules | |
| --- | --- |
| Arguments | `--rutis-manifest`: print the manifest and exit; otherwise `<channel> [--id <endpoint>] [--peer <endpoint>] <project>` |
| Duplicate plugin names | exit at startup, naming both definitions |
| Registration | only in `serve`'s arguments; no static-initializer registry |
| Subcommands | `Rutis.serveArgs(args, defs...)`; also `Rutis.serveConn(channel, defs...)` (in-memory session, for tests) |

### 3.2 Manifest

```json
{
  "manifest": 1,
  "sdk": "0.10.0",
  "pluginApi": 1,
  "runtime": "rutis-swift-runtime:1",
  "platform": "macos/arm64",
  "plugins": {
    "calendar": {
      "config": { "type": "object", "properties": { "calendars": { "type": "array", "items": { "type": "string" } } } },
      "inject": [],
      "provides": { "calendar": { "events": "async", "units": "sync" } },
      "version": null
    }
  }
}
```

| Item | Rule |
| --- | --- |
| Output | stdout, exit code 0; no channel connection, no `apply` |
| Plugin entries | same as the `rows.schema` reply, generated by the same code |
| `runtime` | the SDK marker string `rutis-swift-runtime:1`: directory scans only execute files containing it (the constant is used by `Manifest`, so it is not dead-stripped) |
| `platform` | `macos/<arch>` (`#if os(macOS)` + `ProcessInfo`); the host reports "not built for <os>/<arch>" on mismatch |
| `sdk` | the constant `Rutis.sdkVersion` |
| Plugin `version` | in v1, a static string the author writes on the definition, `null` by default; generating it from git is §16 (S3) |
| Execution timeout | 30 seconds (as Go: the first run of a new binary on macOS waits for the system to assess it) |
| Cache key / bypass / trust | as Go: path, size, mtime (Unix adds ctime); bypassed on start, `restart`, `check`; only sources given by the configuration are executed |

## 4. `SwiftResolver`

In rutis-loader (feature `swift`), implements `Resolver`. Structurally the same as `GoResolver`:

| Item | Rule |
| --- | --- |
| Sources | `SwiftBinaries`: a file list plus a directory list; directories are rescanned once when an unknown plugin name appears |
| Candidates in a directory | executable, not starting with `.`, containing the SDK marker string; only candidates are executed |
| Manifest failure | in `binaries`, a configuration error; in a directory, skipped with a diagnostic; `check` lists them and exits non-zero |
| Runtime name | strip extension → lowercase → non-`[a-z0-9-]` to `-` → prefix `swift-` (`Net.Kits_v1` → `swift-net-kits-v1`) |
| `swift:<plugin>` | search all manifests; none: `NotFound`; several: an ambiguity error suggesting `<runtime>:<plugin>` |
| `<runtime>:<plugin>` | claims row names starting with its runtime name plus `:`, searches only that manifest |
| Name collisions | with other runtimes (`node`, `py`, `bun`, `go`, remote) or each other: a configuration error in `binaries`; skip + diagnostic in a directory |
| Resolution result | a leaf result; shares the "build `Resolved` from declarations" code with `GoResolver` |
| meta | `{ source: "swift", binary, runtime, version, sdk }` |
| Replaced file while running | resolve from the startup manifest; plugins only in the new binary report "replaced, effective after `restart(\"<runtime>\")`" |
| Second-stage release | `RuntimeRowsPlugin` already takes a trait (the Go G1 change); `SwiftResolver` implements the same trait per runtime |

The only differences from `GoResolver` are three constants (marker, prefix, feature name); the implementation goes through shared code (§10).

## 5. `SwiftRuntimes`

Structurally the same as `GoRuntimes`, in rutis-loader, mounted after the loader.

```rust
let swift = Arc::new(SwiftResolver::new(SwiftBinaries::new().dir("plugins/swift").file("bin/weather")).with_catalog(&catalog));
chain = chain.with_shared(swift.clone());
root.plugin(SwiftRuntimes::new(swift, project).idle(Duration::from_secs(60)));
```

| Item | Rule |
| --- | --- |
| Structure | each running runtime = a child `Ctx`: `LocalRuntime::swift(binary, project).named(name)` + `RuntimeRowsPlugin`; stopping = dropping the child `Ctx` |
| Start | started when a row resolves to a not-yet-running runtime (plus a periodic check); cold starts in parallel |
| Idle stop | after its last row unloads, wait `idle`; stop if no enabled entry resolves to it; at most one start / restart / stop in progress at a time |
| Crash | rows stop with the reason recorded; no automatic restart. Restart conditions: `SwiftRuntimes::restart(name)`, or the binary file changed |
| `eager()` / `runtimes()` | as Go |
| One fixed binary | `LocalRuntime::swift` + `RuntimeResolver::modules` without a resolver |

## 6. The Swift SDK

At `swift/rutis` (SwiftPM target `Rutis`), Swift 5 language mode, toolchain 5.9 and up (parameter packs need 5.9), deployment target macOS 13. Depends only on Foundation (swift-foundation; `FoundationEssentials` on non-Apple platforms, leaving room for §14).

### 6.1 Plugin

```swift
import Rutis

struct Config: RutisConfig {          // the config type carries both schema and decoding
    static let schema = Schema.object(
        properties: ["city": .string(description: "The city to look up")],
        required: ["city"])
    var city: String                   // synthesized Codable
}

struct LLM: RutisClient {             // using services: a struct of closure fields
    var ask: (RutisCtx, String) async throws -> String
    var listModels: ((RutisCtx) async throws -> [String])?   // only the methods used
    init(_ s: RutisService) {         // built from the proxy; parameter packs infer from the field type
        ask = s.method("ask")
        listModels = s.method("listModels")
    }
}

final class Weather {
    var llm: LLM; var city: String
    func today(_ ctx: RutisCtx, _ question: String) async throws -> String { ... }
}

let weather = Rutis.define(Plugin(
    name: "weather",
    inject: ["llm"],
    provides: ["weather": Shape(.async("today"), .sync("unit"))],   // declare shapes (into the manifest)
    config: Config.self,
    apply: { ctx, config in                                          // the implementation (at load time)
        let llm = ctx.use("llm", LLM.self)
        let weather = Weather(llm: llm, city: config.city)
        ctx.provide("weather", [
            .async("today") { try await weather.today($0, $1) },
            .sync("unit") { _ in "celsius" },
        ])
        ctx.effect { print("weather unloaded") }
    }))
```

| API | Rule |
| --- | --- |
| `Rutis.define` | validates the declarations (names, closure table matching the shape) and returns a servable definition |
| `apply` throws | the load fails; cleanups registered so far still run |
| `ctx.use(name, C.self)` | `C: RutisClient`; `name` must be in `inject`; a same-process provider hands over its local implementation |
| `ctx.provide(name, methods)` | the closure table must match the declared `provides` shape; lasts until unload or the returned withdraw function |
| `ctx.effect(cleanup)` | `cleanup` may be `async`; runs in reverse order at unload |
| `ctx.spawn { ... }` | runs in a child Task, errors captured to stderr; cancelled and awaited at unload |
| `RutisCtx` | the row scope: cancellation (unload cancels the row's Task tree), `project`, the call chain (§7) |
| `Rutis.NoConfig` | no configuration |

### 6.2 Providing services: the closure table

Swift has no method reflection (Mirror cannot enumerate a type's methods, nor synthesize methods of arbitrary signatures), so instead of Go's reflected `MethodsOf` or Python's `inspect`, the table is **explicit closures**:

- Declaration (at define time): `Shape(.async("today"), .sync("unit"))` → manifest and `rows.schema`.
- Implementation (at apply time): `.async("today") { (ctx, city) in ... }` with any closure signature (arguments decode positionally into the closure's parameter types; return values encode per §6.5); the SDK checks the table against the declared shape.
- Same-process calls: the closure is called directly — no encoding, no crossing (S8).
- An optional macro later (§16 S3): `@RutisService` generates shape and table from the type, restoring Go's `MethodsOf` ergonomics.

### 6.3 Using services: `RutisClient`

```swift
protocol RutisClient { init(_ service: RutisService) }
```

- The struct's closure fields are the methods used: the field name is the method name (Swift is already lowerCamelCase; no Go-style initialism conversion); write only what you use; every field must match a method the service declares, or construction reports it to `use`.
- `s.method("ask")` is a parameter-pack generic: `func method<each A, R>(_ name: String) -> (RutisCtx, repeat each A) async throws -> R`, inferring the closure signature from the field type (verified on this machine with Swift 6.3; the PoC is appendix E.3).
- Cross-process proxies are `async` only (S10): there is no blocking `callSync`.

### 6.4 Configuration

| Item | Rule |
| --- | --- |
| Decoding | `JSONDecoder`, `Config: Codable`; a failure fails the load |
| Schema | `RutisConfig.schema`, written by hand (Swift reflection cannot produce schemas — like Python's hand-written `Config`; the macro is S3) |
| Value mapping | JSON Schema semantics as Go §6.3: `String/Bool/Int/Double` → `string/boolean/integer/number`, `Array` → `array + items`, `Dictionary<String, T>` → `object + additionalProperties`, `Optional` fields leave `required` |

### 6.5 Values

| Swift value | Across processes |
| --- | --- |
| `nil`, `Bool`, `Int` (within ±2^53−1), `Double`, `String` | copied; out-of-range integers error |
| `Optional`, `Array`, `Dictionary<String, T>`, `Codable` structs / enums | copied recursively, encoded per `Codable` |
| Closures (function references) | reference; arguments and return values encode per this table |
| `Rutis.ref(v)` | reference; `v: AnyObject & RutisExportable` (carries its own closure table, §6.2); the far end gets a proxy calling its methods |
| A class instance | cannot cross without an explicit `ref`: an error suggests a struct or `Rutis.ref` |
| `Error` | name and message copied; the name is the error type name (`String(reflecting:)`), overridable via `RutisErrorName` |
| `RutisFuture` | an async result; `await fut.value()` |
| `Data` (bytes), non-string-keyed dictionaries, cycles, non-`Codable` | cannot cross (as Go's `[]byte` and friends) |

- The boundary comes from the type system itself (value types are data; reference types need an explicit `ref`) — no Go-style rule for "is this struct data or an object".
- Receiving: function types → closures calling the remote function (built with parameter packs); everything else decodes via `Codable` into the declared type. Object references are not received (S3, as Go G3).

## 7. Concurrency, call chains, cancellation

| Item | Rule |
| --- | --- |
| Session structure | one reader thread (blocking FD reads; frames enter the async world through an `AsyncStream`; blocking reads never sit on the cooperative pool); one actor serializes frame writes; one new Task per incoming `call` / `invoke` / `get` / `await` / control operation; row state under its own locks |
| Reentrancy | any incoming call may run at any time; declares `reentrant-sync`. Does not declare `sync-stack`: calls run concurrently on separate Tasks with no single-stack order (the Go situation: one execution unit per call, chain edges only — see the master plan §9) |
| Plugin requirements | service closures are concurrency-safe; no locks held while calling other services (in the guide, as Go) |
| Call chain | `@TaskLocal var path: [String]`: an incoming call sets `path + [id]` in its Task; outgoing calls take the chain from the current TaskLocal; `apply` and cleanups carry the `rows.load` / `rows.unload` chains |
| Synchronous waits | after sending a `sync: true` frame, the current Task suspends on a continuation until the reply — **no thread is blocked** (cheaper than Go's blocked goroutine: a suspended Task yields its thread). While waiting, other Tasks keep executing incoming calls |
| `sync` methods | a `.sync` closure must not make cross-process calls (all SDK proxies are async, so this cannot compile); the only behavioral difference from Go; goes in the guide |
| Incoming cancellation | a `cancel` frame → `Task.cancel()` on that call; `Task.checkCancellation()` throws `CancellationError` |
| Outgoing cancellation | `async`: send `cancel`, throw `CancellationError` immediately, drop late results; `sync`: throw immediately, drop late results, the peer completes anyway |
| Row unload | cancels the row's Task tree (`apply`, the children of `ctx.spawn`); after withdrawing the service, in-flight calls are awaited (the deadline is the rutis side's) |
| `SyncWaitCycle` | never produced here; received as a `Rutis.RemoteError`, `isSyncWaitCycle(err)` |

This answers the master plan §9 open question: **Swift confirms reentrancy**. Python, Bun, Go and Swift now agree, and "leaf runtimes are reentrant, only Cordis runtimes are not" becomes the rule (the implementation PR updates the §9 progress note).

## 8. The runtime process

### 8.1 Channels

| channel | Use | Implementation |
| --- | --- | --- |
| `fd:3` | Unix local inheritance | `dup`/`FileHandle` + POSIX read/write |
| `unix:<path>` | dial-back | POSIX socket |
| `tcp:127.0.0.1:<port>` | reserved (Windows-local; see §14) | send `RUTIS_CHANNEL_TOKEN` + newline first, then remove it from the environment |
| `listen:ws://…` / `listen:wss://…` | remote | not in the first two phases (the host side landed with Go G2; Swift attaches later) |
| `ws://…` | — | startup error: listen only |

- Line framing; the per-message size limit as [#173](https://github.com/arcships/rutis/issues/173); the `project` argument is provided through `Rutis.project`.

### 8.2 Contract

Compat protocol version 2; features `["rows.v2", "hosts", "leaf", "scopes"]`; capabilities `signals`, `reentrant-sync`; call ids in this session use the `node:` prefix (as every leaf runtime).

| Control operation | Behavior |
| --- | --- |
| `mount` | `{ services: {}, features, implementation: { name: "github.com/arcships/rutis", version: Rutis.sdkVersion }, engine: { name: "swift", version: <the toolchain version embedded at build> } }` |
| `rows.schema(entry)` / `rows.load` / `rows.update` / `rows.unload` | as Go §8.2; `rows.load` ignores `inject` (a leaf runtime's gating is done by rutis from the declared `inject`) |
| `hosts.provide` / `hosts.withdraw`, `release` / `get`, `dispose` | as Go |

### 8.3 Exit and cleanup

| Situation | Rule |
| --- | --- |
| Errors thrown by `apply`, methods, cleanups | captured as error replies (a throw in a method → an error frame; in `apply` / a cleanup → a failed load / unload) |
| `fatalError` / `try!` / force-unwrap crashes, uncaught errors in `ctx.spawn` | not catchable in Swift → process exit; all rows stop (aligns with Go's "an uncaught panic in a plugin goroutine exits the process"; in the guide) |
| Session already ended | unload all rows, cleanups take at most 5 seconds total; past that, remaining cleanups are abandoned, the affected rows logged to stderr, `exit(0)` |
| `rows.unload`, `dispose` deadlines | none set here (as Go / Python); the deadline is the rutis side's |

## 9. TCC and signing (the macOS specifics)

M3's acceptance scenario is EventKit: the system authorization (TCC) prompt must point at the right program. In the executable model, **the runtime binary is the master plan's "auxiliary program"**: authorizations attach to it.

| Item | Rule |
| --- | --- |
| Attribution | whoever calls the system API is recorded: the runtime binary itself. The name in the prompt comes from its signature and embedded Info.plist |
| Usage descriptions | a bare executable links its Info.plist into the `__TEXT,__info_plist` section (linker `-sectcreate`, configured through SwiftPM `linkerSettings`); the SDK documents the snippet and the `new --lang swift` template ships it (`NSCalendarsUsageDescription` and friends) |
| Local development | ad-hoc signing is enough (`swift build` binaries are ad-hoc signed by default on Apple Silicon); prompts still appear, attributed correctly |
| Distribution | the author signs with Developer ID + notarization + hardened runtime and publishes the binary; needed entitlements (say, calendar read) ride along |
| Library validation | the executable model dlopens nothing, so there is no library-validation problem (contrast A.4) |
| Quarantine attribute | the Go handling is reused: checked by `check` and at startup (`xattr`), reporting the reason and the remedy, never removed automatically |
| Several plugins, one binary | they share one TCC identity: plugins needing different authorization granularity belong in different binaries (or accept sharing); the guide says so |
| Denied authorization | the `Error` the plugin receives crosses back to the caller as any error would; a failed row load / a failed call are both observable |

Acceptance (the M3 text plus this design): an EventKit plugin runs inside a signed binary, the system prompt points at that binary; after granting, the service works; after denying, the error reaches the caller cleanly.

## 10. Rust-side changes

Go G1/G2 built the machinery for "manifest-carrying binary runtimes"; Swift attaches by **sharing** it:

| Change | Where |
| --- | --- |
| Extract the language-independent parts of `go.rs` into a shared module: manifest execution and caching (timeout, platform check, cache key), directory scanning and marker sniffing, runtime-name derivation, the on-demand lifecycle and restart, file-replacement semantics | `rutis-loader/src/runtime/binary.rs` (new); `go.rs` becomes a thin wrapper with unchanged behavior, guarded by the existing `go_rows.rs` tests |
| `SwiftResolver`, `SwiftBinaries`, `SwiftRuntimes` | `rutis-loader/src/runtime/swift.rs` (new, thin) |
| `swift_runtime_name`; `Launcher::swift(binary, project)` (the same spawn path as `Launcher::go`: `cwd = project`, Unix `inherit_fd`); `LocalRuntime::swift` | `rutis-bridge/src/runtime/{local,process}.rs` |
| features: rutis-bridge `swift = []`; rutis-loader `swift = ["runtimes", "rutis-bridge/swift"]`; rutis-host enables `swift` | the three `Cargo.toml`s |
| Remote npm naming rejects `swift:` and `<swift runtime>:` prefixes | already implemented (any `<name>:` prefix is rejected); tests only |
| `scripts/train.mjs` gains the version regex for `swift/rutis/Sources/Rutis/Version.swift` | `scripts/train.mjs` |

If the `go.rs` extraction turns out too entangled during implementation, the fallback is: `swift.rs` copies first, unifying in S2 — the design leans toward extraction, and deviations are recorded here with the implementation.

## 11. rutis-host

```json
{
  "runtimes": { "swift": { "dir": "plugins/swift", "binaries": ["bin/weather"], "start": "on-demand", "idle": 60 } },
  "rows": [{ "id": "weather", "name": "swift:weather", "config": { "city": "Oslo" } }]
}
```

The fields are the same as `runtimes.go` (`dir` / `binaries`, at least one; `start`: `on-demand` (default) / `eager`; `idle` in seconds; plus `project`).

| Command | Behavior | Phase |
| --- | --- | --- |
| `check` | per binary: runtime name, platform, SDK / plugin API versions, plugins; ambiguities; each row's target binary; failed candidates; the quarantine attribute | S2 |
| `dev` | without `rutis.dev.json`, detect a Swift project by `Package.swift`; rebuild and restart per §12 | S2 |
| `new <name> --lang swift` | the plugin target, `cmd/<name>/main.swift`, the `Info.plist` with usage descriptions and `linkerSettings`, tests, `rutis.dev.json`, the release workflow (tag → `swift test`, `check`, build and sign binaries) | S2 |
| `swift add <URL>` | download a prebuilt binary into `dir`, verify SHA-256 | S3 |
| `run` | no Swift toolchain needed | — |

## 12. Changing code

As Go §12:

| Scenario | Flow |
| --- | --- |
| `dev` | watch `.swift`, `Package.swift`, `Package.resolved` → `swift build -c release` into a new cache file `<name>-<seq>` → on success, replace that runtime's file and restart it, delete the old; on failure print the error, the old process continues |
| Deploy | replace by rename → `SwiftRuntimes::restart("<runtime>")` (rutis-host: restart the host); resolve from the startup manifest before restarting |
| Restart effect | that runtime's rows and their users stop; the new process resolves per the new manifest; other runtimes are unaffected |

- Build output is `.build/release/<name>`; in-place replacement and overwriting running files follow Go appendix E (rename-replace on Unix; `dev` always to a new file name).

## 13. Repository and releases

| Item | Rule |
| --- | --- |
| Location | `swift/rutis`: target `Rutis` (API, `serve`, manifest), `RutisTest`; the session layer in `Sources/Rutis/Internal/Peer`; the conformance executable target `RutisConformance` |
| Manifest | a `Package.swift` at the **repository root**, package name `rutis-swift`, targets pointed at `swift/rutis/Sources/...` via `path:`. SwiftPM git dependencies only read the root `Package.swift` (subdirectory packages are unsupported); verified locally in appendix E.2 |
| Versioning | `Rutis.sdkVersion` follows the main release line: the existing `v0.9.x` tags are exactly the `vx.y.z` form SwiftPM accepts, so no new tag namespace is needed (verified in E.2); tags predating the first release with a root `Package.swift` cannot serve as dependency lower bounds — `from:` starts at that version |
| How dependents write it | `.package(url: "https://github.com/arcships/rutis", from: "0.10.0")` + `.product(name: "Rutis", package: "rutis")` |
| Dependencies | Foundation only (swift-foundation); no uniffi / swift-bridge (appendix B: the former's Swift 6 support is partial, the latter is 0.x with a long hiatus) |
| Compatibility | within one protocol version, a binary of any SDK version runs on a host of any version, provided its plugin API is not above what the host supports |
| Language mode | Swift 5 mode to start (tightening strict concurrency gradually), the API designed Sendable-friendly; the toolchain policy as Go: "the current two stable releases" (today 6.3 and 6.4) |
| Docs | `docs/guide/swift-plugin.md` (Chinese and English); a Swift column in `docs/guide/plugin-api.md`; README / CONTRIBUTING gain the Swift toolchain notes |

## 14. Platforms

- **macOS 13+**: the Swift runtime ships with the system (post-ABI-stability, nothing to bundle); CLI binaries are self-contained single files. This is v1's only platform and the master plan's positioning for Swift (macOS system capabilities: EventKit, Contacts, Accessibility, screen capture, Core ML).
- **Linux**: technically viable (swift-foundation is cross-platform; since SE-0342 the Swift runtime is statically linked on Linux, so binaries depend only on glibc), and neither the protocol nor the SDK touches Apple frameworks. A follow-up: when there is a real need (server-side Swift plugins), it joins through the same manifest model plus `platform: "linux/<arch>"` checks.
- **Windows**: out. If rutis ever needs to drive Swift binaries on Windows, the `tcp:` loopback channel and the platform check already exist.

## 15. Tests

| Test | Where | What |
| --- | --- | --- |
| Session-layer unit tests | `swift/rutis/Tests` | encode/decode, reference counting, callbacks during a sync wait, cancellation, TaskLocal chains, concurrency, suspended waits not blocking (the pool keeps executing) |
| Session conformance | `rutis-bridge/tests/runtime_conformance.rs` + the `RutisConformance` target | `session::testing::session` |
| Contract × channels | `session_matrix.rs` gains `Swift` | `fd:3`, dial-back |
| Swift specifics | `rutis-bridge/tests/swift_runtime.rs` | cancellation reaching the Task; error shapes (type names); `fatalError` exit withdrawing services; references and release; fixtures `conformance-session/weather/greeter`, `crash()` exits 17 |
| Manifest | `swift/rutis` + `rutis-loader/tests/swift_rows.rs` | matches `rows.schema`; a cache hit does not execute; plugin-API-too-high and wrong-platform errors |
| Several binaries | `swift_rows.rs` | routing by plugin name; ambiguity and qualified names; marker-less files not executed, failed files skipped; replaced-while-running resolves from the startup manifest, effective after `restart`; sync / async calls across binaries; one process crashing stops only its rows and their users |
| On-demand start | `swift_rows.rs` | not used → not started; exits when idle; a new row during the idle wait cancels the exit; a crash does not restart, a replaced file can start again |
| Runtime conformance | `multilang.rs` (feature `swift`) | a Swift leaf plugin; cross-language cold starts (Node / Python / Go / Swift); a provider going away stops only its users; `inject` gating |
| Cross sync calls | `multilang.rs` | Node→Swift→calling back into Node (with the chain), Swift↔Go, Swift↔Python: no deadlock; `SyncWaitCycle` detects cycles without false positives |
| Instances | `instance_runtimes.rs` | label isolation, per-instance services |
| TCC (e2e) | a new Swift scenario in the e2e suite | an EventKit plugin: the prompt points at the binary; granted → the service works; denied → the error crosses back. CI cannot click the prompt: the interactive part is a manual acceptance checklist, the rest (error propagation, pre-authorization behavior) is automated |
| Fixtures | `crates/rutis-loader/tests/fixtures/swift` | built into at least two binaries at test start |
| CI | `runtimes-swift` (a macOS runner, Xcode preinstalled; two stable toolchains) | `swift test`; `cargo test -p rutis-bridge --features swift,…`; `-p rutis-loader --features swift,…`; `-p rutis-host`; `cargo check --no-default-features --features swift`; `test` and `network-macos` gain the Swift fixture build |

- E2E milestones: the `new --lang swift` loop (against S2), cross-language and crash recovery (S3), running a downloaded binary without a toolchain (S9) each gain Swift.

## 16. Phasing

| Phase | Content | Acceptance |
| --- | --- | --- |
| S1 | the SDK (session, leaf API, `serve` over `fd`/`unix`, manifest, `RutisTest`); the `binary.rs` extraction + the thin `go.rs`; `Launcher::swift`, `LocalRuntime::swift`, feature `swift`; `SwiftResolver`; `SwiftRuntimes` (`eager` only) | §15 minus on-demand start and TCC, all passing; several binaries in parallel; macOS; Go's existing tests stay green |
| S2 | on-demand start and idle stop; rutis-host (`runtimes.swift`, `dev`, `check`, `new --lang swift`); the guide; the TCC / EventKit scenario | the on-demand tests pass; `dev` restarts only its own runtime on a change; the M3 acceptance: the prompt points at the binary, a denial crosses back as an error |
| S3 (on demand) | downloading prebuilt binaries (`swift add <URL>`); receiving object references; the `@RutisService` / schema macro (generating the closure table and schema, restoring `MethodsOf` ergonomics); a version build-tool plugin (git → `Version.swift`); remote `listen:`; Linux | when there is a need |

## 17. Open questions

| Item | Current leaning |
| --- | --- |
| How manifests are read | executing the binary (as Go, with the 30-second assessment timeout); the alternative is writing a file at build time |
| `idle` default | 60 seconds (as Go) |
| sync methods cannot call out | already stopped at compile time (all proxies are async); whether a dev-mode runtime warning is worth it falls out of M3 |
| Universal binaries | v1 ships per-arch; whether releases are universal (arm64 + x86_64) is the `new` release workflow's call |
| Binary size | a `swift build -c release` binary is a few MB, with Foundation dynamic (system-provided); no premature optimization |
| `swift build` dead-stripping | release defaults to `-O` + strip; the manifest marker string is referenced by `Manifest` and survives (a test covers directory scans recognizing the artifact) |

## 18. Departures from the master plan's M3

| The master plan says | This design |
| --- | --- |
| "A signed auxiliary program loading plugin bundles with `dlopen`" | Revised: one signed binary per plugin group (the manifest model, as Go). Reasons and costs in appendix A |
| Row name `swift:Calendar.bundle` (a bundle path) | `swift:<plugin>`: what follows the prefix is the plugin's name inside the binary, routed by manifest |
| Swift filed under "runtimes that load code by name" with `dlopen` bundles (§2.7) | filed with Go under "the plugin is a compiled program" |
| M3 acceptance "runs in a signed auxiliary program, the authorization prompt points at it" | kept, same meaning: the auxiliary program is the plugin group's binary (§9) |
| §9 open question "Swift (M3) confirms the reentrancy rule" | answered: reentrant (§7); the rule "leaf runtimes are all reentrant" is adopted |
| Apple platforms only (§7) | narrowed to macOS only for v1; Linux is a follow-up (§14) |

## Appendix

### A. Why not a dlopen auxiliary program (the revision of the master plan's M3)

The master plan files Swift with the "load code at runtime" runtimes (as Node / Python), loading plugin bundles via `dlopen`. This design switches to Go's "compiled program" model, reasons in order of importance:

**A.1 Go (M4) landed first and built the machine.** `GoResolver`, `GoRuntimes`, the two-stage release, on-demand start, restart, `check`, `dev`, `new` all exist (#227); Swift attaches through shared code (§10). The dlopen route would instead build: bundle discovery and loading, a second API layer between the auxiliary program and its bundles, and a separate answer to the manifest / caching / platform-check problems Go already solved.

**A.2 Swift's cross-toolchain binary compatibility is not dependable.** When bundles and the auxiliary program compile separately, Swift-side cross-module calls need module stability + library evolution — not guaranteed in practice for bundles by arbitrary authors on arbitrary Swift versions. The workaround is pushing the boundary down to a plain C ABI, which means inventing a layer that neither Go nor Python needs. SwiftPM also has no loadable-bundle product (only executable / library / binary targets).

**A.3 TCC granularity.** One auxiliary program means one TCC identity: grant calendar to plugin A, and plugin B in the same process can read calendars too. In the executable model each plugin group gets its own narrow grant — a better privacy story, and still satisfying M3's acceptance (the prompt points at the auxiliary program, which is that binary).

**A.4 Signing and library validation.** A signed auxiliary program with the hardened runtime cannot dlopen bundles not signed by the same team without `disable-library-validation` (widening the attack surface) or requiring every plugin author to sign with one team. The executable model has no such problem.

**A.5 The loss and its substitutes.** What is lost: dropping a new bundle into a running process and having it load. Substitutes: (1) the deployment composes several plugin packages into one binary in `cmd/main.swift` (dependencies merge through the deployment's SwiftPM manifest, same responsibility as Go appendix H.3); (2) several plugins each in their own binary, started on demand, stopped when idle — the process count is the Go order of magnitude (Go appendix A.4's argument applies: millisecond starts, single-digit-MB idle memory). Distribution sources are Go's H: author-published per-platform binaries, `swift add <URL>` (S3), deployment-composed, and `dev` builds.

### B. Swift facts (research sources)

| Fact | Source |
| --- | --- |
| `@_cdecl` became official `@c` in Swift 6.3 (SE-0495); this design needs no C ABI (the session protocol is self-implemented, as Go) | [SE-0495](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0495-cdecl.md) |
| macOS 10.14.4+ ships the Swift runtime; CLI binaries bundle nothing | [swift.org ABI stability](https://swift.org/blog/abi-stability-and-apple/) |
| The Swift 6.4 (2026-09) toolchain's minimum deployment target is macOS 13; releases land each March and September | [the swift-evolution version table](https://github.com/swiftlang/swift-evolution/blob/main/README.md), [platform-support](https://swift.org/platform-support/) |
| Linux: since SE-0342 the runtime is statically linked by default (binaries depend only on glibc); the Static Linux SDK (musl) is fully static but disables `dlopen` | [SE-0342](https://github.com/apple/swift-evolution/blob/main/proposals/0342-static-link-runtime-libraries-by-default-on-supported-platforms.md), [the Static Linux SDK](https://www.swift.org/documentation/articles/static-linux-getting-started.html) |
| swift-foundation provides FoundationEssentials for non-Apple platforms | [swift-foundation](https://github.com/swiftlang/swift-foundation) |
| UniFFI's Swift 6 support is marked partial (async generated code is not Sendable); swift-bridge is 0.x with a roughly 16-month hiatus — neither is adopted | [UniFFI Swift](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html), crates.io |
| setup-swift@v3 supports Ubuntu / macOS; macOS runners come with Xcode | [setup-swift](https://github.com/swift-actions/setup-swift) |
| Blocking reads must not sit on Swift's cooperative pool (its width is the core count); a dedicated thread + continuation is the standard shape | Swift concurrency docs and community consensus |
| Linking an Info.plist into a bare executable's `__TEXT,__info_plist` (`-sectcreate`) is long-standing practice | CMake / xamarin.macOS / swift-forums threads |

### C. Side-by-side with the Go runtime

| | Go | Swift |
| --- | --- | --- |
| Process model | one binary per plugin group | same |
| Self-description | `--rutis-manifest` | same |
| Row / runtime names | `go:<plugin>` / `go-<name>` | `swift:<plugin>` / `swift-<name>` |
| Execution unit per call | a goroutine (blocking wait) | a Task (suspending wait, no thread held) |
| Reentrancy / sync-stack | reentrant / not declared | same |
| Call chain | `context.Context` | `@TaskLocal` |
| Method enumeration | reflection, `MethodsOf` | explicit closure table (macro later) |
| Schema | reflected, overridable | hand-written (macro later) |
| Values by default | data, `rutis.Ref` for explicit references | value types are data; classes need `Rutis.ref` (the boundary is the type system) |
| Calling out from a sync method | allowed (blocks the goroutine) | not allowed (all proxies are async) |
| panic / fatalError | captured as an error; uncaught in a goroutine → process exit | errors are `throws`; `fatalError` → process exit (not catchable) |
| Platforms | Linux / macOS / Windows | macOS only (Linux later) |
| Distribution | git tag `go/rutis/vX.Y.Z` + the module proxy | root `Package.swift` + the main line's `v` tags |
| Dependencies | standard library only | Foundation only |

### D. Why the value boundary needs no `Ref`-style rule

Go needs an explicit `Ref` because a Go struct can be either data or an object — the language does not say. Swift's value types (struct / enum) and reference types (class) are two different worlds in the type system: structs are naturally data, classes naturally object identity. So the default rule reads off the language: struct / enum copies as data; a class crosses only through an explicit `Rutis.ref` and must carry its own method table (since Swift cannot reflect its methods out, the author provides the table — the same mechanism as §6.2's closure table).

### E. Local PoCs (2026-10-12, Swift 6.3.2 / Xcode)

**E.1**: a root `Package.swift` declaring targets at a subdirectory path (`path: "swift/rutis/Sources/Rutis"`) builds and runs fine.

**E.2 SwiftPM's version-tag rule**: against a git dependency, `1.0.0` (plain semver) and `v1.1.0` (v-prefixed) are accepted as versions; `rutis-swift-1.0.0` (package-name prefix), `swift/rutis/1.0.0` (slash), `rutis-mono-1.2.0` (URL-name prefix) are all ignored (resolution reports "no versions match"). Conclusion: this repository's existing `v0.9.x` release tags are immediately usable as SwiftPM versions; the Swift SDK releases at the main repo's version, with no new tag namespace and none of Go's `go/rutis/v…` forms (SwiftPM rejects slash tags).

**E.3 Parameter-pack contextual inference**: `func method<each A, R>(_:) -> (Ctx, repeat each A) async throws -> R` infers from the field's closure type at `LLM(ask: s.method("ask"), …)` and compiles (a Swift 5.9 feature). §6.3's `RutisClient` shape follows from it; if extreme signatures turn out to infer poorly, the fallback is a set of overloads for arities 0–3.
