# The Swift SDK: the macOS plugin runtime and reaching Apple apps (design draft)

[中文](design-swift-sdk-2026-10-12.md)

Status: design draft, not implemented (an issue should track it). Date: 2026-10-12; revised the same day: while drafting, the goal grew from the master plan M3's "macOS system capabilities" to "bridging the Apple ecosystem and apps", adding shapes ② and ③ (§2, §17). Base: `main` `6f91ce8`.
Depends on: [the multilang master plan](design-multilang-runtimes-2026-10-03.en.md) (this is its M3, with its dlopen approach revised — appendix A), [M1](design-multilang-m1-2026-10-04.en.md), [M2](design-multilang-m2-2026-10-04.en.md), [the Go plugin runtime](design-go-runtime-2026-10-10.en.md) (shape ① borrows much of its machinery and decisions; a side-by-side is appendix C), [the Bun runtime](design-bun-runtime-2026-10-09.en.md), [Connecting nodes](guide/nodes.en.md) (the machinery behind shape ②b), [the plugin API](guide/plugin-api.md).

Out of scope: a Swift-side Cordis; build systems other than SwiftPM; the host compiling Swift code; Linux and Windows (§13); the App Store submission process itself (only compliance notes).

## 1. Decisions

| # | Decision | Appendix |
| --- | --- | --- |
| S1 | Shape ①: one binary, one runtime process; the host runs several Swift runtimes side by side | A |
| S2 | Shape ①: one binary holds one or more plugins, as its `main.swift`'s `Rutis.serve(...)` says | A |
| S3 | Shape ① row name `swift:<plugin>`, routed by `SwiftResolver` through manifests; qualified form `<runtime>:<plugin>` | — |
| S4 | Shape ① runtime name comes from the file name (prefix `swift-`), the only user-visible name | — |
| S5 | Shape ① binaries describe themselves: `--rutis-manifest` prints the manifest; resolving needs no running process | — |
| S6 | Shape ①: while a process runs, resolving uses the manifest it started with; a replaced file takes effect on restart, never automatically | — |
| S7 | Start on demand, stop when idle; `eager` starts all and never stops | — |
| S8 | Calls within one binary skip IPC; across binaries and languages, rutis forwards by name | — |
| S9 | One Task per incoming call: reentrant, declares `reentrant-sync`; does not declare `sync-stack`. Applies to every shape | B |
| S10 | The call chain lives in a `@TaskLocal`; a synchronous wait suspends the current Task and never blocks a thread; methods default to `async`, `.sync` marks the rest | B |
| S11 | Leaf plugin; plugin API version 1; config decoded via `Codable`, its schema written by hand on the config type | C |
| S12 | Providing services: an explicit closure table (Swift has no method reflection); same-process calls go direct | C |
| S13 | Values split along the type system: value types copy as data, closures cross as references; `Rutis.ref` passes an object by explicit reference | D |
| S14 | Using services: a struct of closure fields conforming to `RutisClient`, built from a proxy (parameter-pack inference) | C |
| S15 | The host does not compile; `run` only runs ready-made binaries, `dev` only rebuilds the project itself | — |
| S16 | Binaries of different SDK versions coexist; host and binary agree only on the wire protocol, the manifest format, and the plugin API version | — |
| S17 | The SDK lives at `swift/rutis`, distributed through a `Package.swift` at the repository root, versioned by the main release line's `v` tags; the core depends only on Foundation | E |
| S18 | Platforms: macOS 13+ (shapes ① and ②) and iOS 17+ (shape ② only); Linux is a follow-up, Windows is out | — |
| S19 | The SDK is a cross-Apple-platform library: its core (session + leaf API) assumes nothing about processes or the file system; on iOS it links statically into the app, same API (§2) | — |
| S20 | Shape ② near path: the app is a remote-runtime listener (`listen:`, Go G2 semantics, one controller at a time); the host side adds only `remote[].language: "swift"` | — |
| S21 | Shape ② long path: the app is a dialing peer (dials a listening node, exports/imports services, reconnects with per-failure backoff); the host machinery exists, the dialer is the SDK's new role | — |
| S22 | iOS background survival: v1 is foreground connection + reconnect; APNs wakeup and background modes are follow-ups | — |
| S23 | The app is not a node: it does not listen to run other people's plugins (never `host: true`); the App Intents bridge (the Apple ecosystem consuming rutis) is a follow-up | — |

## 2. Deployment shapes: one SDK, three ways in

The goal is bridging the Apple ecosystem and apps. On macOS plugins run right beside the host; on iOS the plugin model simply does not hold — the integration unit is the **app**, and the direction is the **app dialing out**:

| | macOS | iOS |
| --- | --- | --- |
| Can a rutis host spawn runtime processes? | yes | no: sandbox, no exec, App Store rules forbid downloading and executing code |
| Can plugins load dynamically? | yes | no: everything links statically into the app |
| Integration unit | the plugin (a process group) | the app itself |
| Connection direction | host spawns / dials in | the app dials out (or listens in the foreground) |
| Authorization (TCC / entitlements) | on the plugin binary | on the app's signature and provisioning profile |
| Background survival | processes stay | the app is suspended; needs reconnect / APNs |

Three deployment shapes follow, sharing one SDK core (session protocol, leaf API, value rules, concurrency model — everything in §4–§6 holds for all three):

```text
                 Swift SDK core (session + leaf API, macOS / iOS)
       ┌──────────────────┬─────────────────────┬──────────────────┐
   ① serve + manifest  ②a listener (listen)    ②b dialer (dial)     ③ App Intents (later)
   macOS plugin binary  app as remote runtime   app as a peer        app exposes rutis
   host starts it via   host dials in and       app dials out to     services as Shortcuts /
   the manifest machine controls its plugins   a node               Siri actions
```

| Shape | Use | Rides on | Control |
| --- | --- | --- | --- |
| ① macOS plugin binary | Mac system capabilities (EventKit, Contacts, Accessibility, screen capture, Core ML), AppleScript / AX automation of Mac apps | `SwiftResolver` / `SwiftRuntimes` (shared with Go's manifest machinery) | rutis |
| ②a app·listener | the app, foregrounded, is a remote runtime; the host dials in and manages its built-in plugins | the remote-runtime machinery (Go G2): `runtimes.remote`, leases, one controller | rutis (rows and gating live in the host's config) |
| ②b app·dialer | the app dials a listening rutis node and exports its capabilities as services; reconnects in the background | the nodes / link machinery (`PeerPlugin`): dial, token, export/import, backoff reconnect | the app (what it exports is its call; the node imports) |
| ③ App Intents bridge | the Apple ecosystem consumes rutis: the app defines rutis services as App Intents driven by Shortcuts / Siri / Spotlight | the App Intents framework | the user |

②a and ②b are not old and new — they are two governance models: the device belongs to the host's side (a company-issued iPhone, the host's config decides what runs) takes ②a; the app is something the user installed (it decides what to expose) takes ②b. One SDK supports both.

## 3. Shape ①: the macOS plugin runtime

### 3.1 Runtime model

```text
rutis host
 ├─ LocalRuntime::node / ::python ...
 ├─ SwiftRuntimes (SwiftResolver + lifecycle)
 │    ├─ plugins/swift/weather ── swift-weather ── weather
 │    ├─ plugins/swift/kits   ── swift-kits    ── calendar, contacts
 │    └─ plugins/swift/ml     ── not started (no row uses it)
 └─ LoaderPlugin + one RuntimeRowsPlugin per running runtime
```

- Identical to the Go runtime: a row depends on `RuntimeRows#<runtime>` plus every name the plugin `inject`s; the services a plugin `provides` are published from the row's fiber to `host_key(name)`; when the process exits, its rows stop, and rows using its services stop with them.
- `isolate`, instance labels, service ids, exported handles: same as the Python and Go runtimes.
- TCC is attributed to the binary the plugin lives in (§8).

### 3.2 Binary and manifest

- A plugin author distributes: a Swift package (exporting plugin definitions) plus `cmd/<name>/main.swift`, or one package with several plugins compiled into one binary.
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
| `runtime` | the SDK marker string `rutis-swift-runtime:1`: directory scans only execute files containing it |
| `platform` | `macos/<arch>`; the host reports "not built for <os>/<arch>" on mismatch |
| `sdk` | the constant `Rutis.sdkVersion` |
| Plugin `version` | in v1, a static string the author writes, `null` by default; git-generated in §15 (S3) |
| Execution timeout | 30 seconds (as Go: the first run of a new binary on macOS waits for the system to assess it) |
| Cache key / bypass / trust | as Go: path, size, mtime (Unix adds ctime); bypassed on start, `restart`, `check`; only sources given by the configuration are executed |

### 3.3 `SwiftResolver`

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

The only differences from `GoResolver` are three constants (marker, prefix, feature name); the implementation goes through shared code (§9).

### 3.4 `SwiftRuntimes`

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
| Idle stop | after its last row unloads, wait `idle`; stop if no enabled entry resolves to it; at most one start / restart / stop in progress |
| Crash | rows stop with the reason recorded; no automatic restart. Restart conditions: `SwiftRuntimes::restart(name)`, or the binary changed |
| `eager()` / `runtimes()` | as Go |
| One fixed binary | `LocalRuntime::swift` + `RuntimeResolver::modules` without a resolver |

## 4. The Swift SDK (shared core)

At `swift/rutis` (SwiftPM target `Rutis`), Swift 5 language mode, toolchain 5.9 and up (parameter packs need 5.9), deployment targets macOS 13 / iOS 17. Layered:

| Layer | Contents | Platforms |
| --- | --- | --- |
| Core | session protocols (compat / endpoint), leaf API, values, concurrency, `RutisTest` | macOS + iOS (Foundation only; touches no system facilities beyond processes and the file system) |
| Channel·process | `fd:3`, `unix:`, `tcp:` (POSIX) | macOS (shape ①) |
| Channel·listen | the WebSocket server for `listen:` (Network.framework `NWListener` + `NWProtocolWebSocket`) | macOS + iOS (shape ① remote, shape ②a) |
| Channel·dial | the WebSocket client for `dial:` (Foundation `URLSessionWebSocketTask`) | macOS + iOS (shape ②b) |
| serve packaging | `Rutis.serve` / `serveArgs` / the manifest CLI | macOS (shape ①; on iOS the same logic is driven through `RutisApp`, §7) |

### 4.1 Plugin

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
| `RutisCtx` | the row scope: cancellation (unload cancels the row's Task tree), `project`, the call chain (§5) |
| `Rutis.NoConfig` | no configuration |

### 4.2 Providing services: the closure table

Swift has no method reflection (Mirror cannot enumerate a type's methods, nor synthesize methods of arbitrary signatures), so instead of Go's reflected `MethodsOf` or Python's `inspect`, the table is **explicit closures**:

- Declaration (at define time): `Shape(.async("today"), .sync("unit"))` → manifest and `rows.schema`.
- Implementation (at apply time): `.async("today") { (ctx, city) in ... }` with any closure signature (arguments decode positionally into the closure's parameter types; return values encode per §4.5); the SDK checks the table against the declared shape.
- Same-process calls: the closure is called directly — no encoding, no crossing (S8).
- An optional macro later (§15 S3): `@RutisService` generates shape and table from the type, restoring Go's `MethodsOf` ergonomics.

### 4.3 Using services: `RutisClient`

```swift
protocol RutisClient { init(_ service: RutisService) }
```

- The struct's closure fields are the methods used: the field name is the method name (Swift is already lowerCamelCase; no Go-style initialism conversion); write only what you use; every field must match a method the service declares, or construction reports it to `use`.
- `s.method("ask")` is a parameter-pack generic: `func method<each A, R>(_ name: String) -> (RutisCtx, repeat each A) async throws -> R`, inferring the closure signature from the field type (verified with Swift 6.3 on this machine; the PoC is appendix E.3).
- Cross-process proxies are `async` only (S10): there is no blocking `callSync`.

### 4.4 Configuration

| Item | Rule |
| --- | --- |
| Decoding | `JSONDecoder`, `Config: Codable`; a failure fails the load |
| Schema | `RutisConfig.schema`, written by hand (Swift reflection cannot produce schemas — like Python's hand-written `Config`; the macro is S3) |
| Value mapping | JSON Schema semantics as Go §6.3 |

### 4.5 Values

| Swift value | Across processes |
| --- | --- |
| `nil`, `Bool`, `Int` (within ±2^53−1), `Double`, `String` | copied; out-of-range integers error |
| `Optional`, `Array`, `Dictionary<String, T>`, `Codable` structs / enums | copied recursively, encoded per `Codable` |
| Closures (function references) | reference; arguments and return values encode per this table |
| `Rutis.ref(v)` | reference; `v: AnyObject & RutisExportable` (carries its own closure table, §4.2); the far end gets a proxy calling its methods |
| A class instance | cannot cross without an explicit `ref`: an error suggests a struct or `Rutis.ref` |
| `Error` | name and message copied; the name is the error type name (`String(reflecting:)`), overridable via `RutisErrorName` |
| `RutisFuture` | an async result; `await fut.value()` |
| `Data` (bytes), non-string-keyed dictionaries, cycles, non-`Codable` | cannot cross (as Go's `[]byte` and friends) |

- The boundary comes from the type system itself (value types are data; reference types need an explicit `ref`) — no Go-style rule for "is this struct data or an object" (appendix D).
- Receiving: function types → closures calling the remote function (built with parameter packs); everything else decodes via `Codable` into the declared type. Object references are not received (S3, as Go G3).

## 5. Concurrency, call chains, cancellation

Applies to every shape.

| Item | Rule |
| --- | --- |
| Session structure | one reader thread (blocking channel reads; frames enter the async world through an `AsyncStream`; blocking reads never sit on the cooperative pool); one actor serializes frame writes; one new Task per incoming `call` / `invoke` / `get` / `await` / control operation; row state under its own locks |
| Reentrancy | any incoming call may run at any time; declares `reentrant-sync`. Does not declare `sync-stack`: calls run concurrently on separate Tasks with no single-stack order (the Go situation — see the master plan §9) |
| Plugin requirements | service closures are concurrency-safe; no locks held while calling other services (in the guide, as Go) |
| Call chain | `@TaskLocal var path: [String]`: an incoming call sets `path + [id]` in its Task; outgoing calls take the chain from the current TaskLocal; `apply` and cleanups carry the `rows.load` / `rows.unload` chains |
| Synchronous waits | after sending a `sync: true` frame, the current Task suspends on a continuation until the reply — **no thread is blocked**. While waiting, other Tasks keep executing incoming calls |
| `sync` methods | a `.sync` closure must not make cross-process calls (all SDK proxies are async, so this cannot compile); the only behavioral difference from Go; goes in the guide |
| Incoming cancellation | a `cancel` frame → `Task.cancel()` on that call; `Task.checkCancellation()` throws `CancellationError` |
| Outgoing cancellation | `async`: send `cancel`, throw `CancellationError` immediately, drop late results; `sync`: throw immediately, drop late results, the peer completes anyway |
| Row unload | cancels the row's Task tree; after withdrawing the service, in-flight calls are awaited (the deadline is the rutis side's) |
| `SyncWaitCycle` | never produced here; received as a `Rutis.RemoteError`, `isSyncWaitCycle(err)` |

This answers the master plan §9 open question: **Swift confirms reentrancy**. Python, Bun, Go and Swift now agree, and "leaf runtimes are reentrant, only Cordis runtimes are not" becomes the rule (the implementation PR updates the §9 progress note).

## 6. The runtime process: channels and contract

### 6.1 Channels

| channel | Shape | Implementation |
| --- | --- | --- |
| `fd:3` | ① | Unix local inheritance: `dup`/`FileHandle` + POSIX read/write |
| `unix:<path>` | ① | dial-back: POSIX socket |
| `tcp:127.0.0.1:<port>` | ① (reserved) | send `RUTIS_CHANNEL_TOKEN` + newline first, then remove it from the environment |
| `listen:ws://…` / `listen:wss://…` | ① remote (S3), ②a | §7.1 |
| `dial:` (an SDK-side API, not a channel string) | ②b | §7.2 |
| bare `ws://…` | — | ② errors: apps only listen, or dial a node |

- Line framing; the per-message size limit as [#173](https://github.com/arcships/rutis/issues/173); the `project` argument is provided through `Rutis.project` (in ②, the app's agreed directory).

### 6.2 Contract (shapes ①, ②a)

Compat protocol version 2; features `["rows.v2", "hosts", "leaf", "scopes"]`; capabilities `signals`, `reentrant-sync`; call ids in this session use the `node:` prefix (as every leaf runtime).

| Control operation | Behavior |
| --- | --- |
| `mount` | `{ services: {}, features, implementation: { name: "github.com/arcships/rutis", version: Rutis.sdkVersion }, engine: { name: "swift", version: <the toolchain version embedded at build> } }` |
| `rows.schema(entry)` / `rows.load` / `rows.update` / `rows.unload` | as Go §8.2; `rows.load` ignores `inject` (a leaf runtime's gating is done by rutis from the declared `inject`) |
| `hosts.provide` / `hosts.withdraw`, `release` / `get`, `dispose` | as Go |

### 6.3 Exit and cleanup

| Situation | Rule |
| --- | --- |
| Errors thrown by `apply`, methods, cleanups | captured as error replies |
| `fatalError` / `try!` / force-unwrap crashes, uncaught errors in `ctx.spawn` | not catchable in Swift → in shape ① the process exits, all rows stop (aligns with Go); in shape ② it is **the app crashing**: the connection drops, the host's rows stop / services withdraw — the same recovery path |
| Session already ended | unload all rows, cleanups take at most 5 seconds total; past that, remaining cleanups are abandoned, the affected rows logged to stderr; shape ① `exit(0)`, shape ② the serve loop ends (the app stays alive) |
| `rows.unload`, `dispose` deadlines | none set here (as Go / Python); the deadline is the rutis side's |

## 7. Shape ②: reaching apps

### 7.1 Near path: the app as listener (remote-runtime semantics)

The app hands the plugin definitions compiled into itself to the host via `listen:` — semantics identical to Go G2's remote runtimes (one controller at a time; a new connection takes over, the old lease finishes cleaning up before the new session greets; `RUTIS_TOKEN` / `RUTIS_CERT` / `RUTIS_KEY` credentials, constant-time token comparison; 16 MiB message limit; 30-second heartbeat, `RUTIS_HEARTBEAT` to tune). The host treats it as a Swift runtime on a remote machine:

```swift
// in the app: start a remote runtime in the foreground
try await RutisApp.serve(
    plugins: [health, location],          // plugin definitions built into the app
    listen: "wss://0.0.0.0:7443/rutis",
    id: "my-iphone", peer: "mac-main")    // credentials via the environment (RUTIS_TOKEN, …)
```

```json
// the host (a Mac): the same configuration as a remote Go runtime
{
  "runtimes": { "remote": [{ "name": "iphone", "language": "swift" }] },
  "rows": [
    { "id": "iphone", "name": "rutis-bridge/peer", "config": { "peer": "iphone", "dial": "wss://192.168.1.20:7443/rutis", "runtime": "iphone" } },
    { "id": "health", "name": "iphone:health", "config": { "days": 7 } }
  ]
}
```

| Item | Rule |
| --- | --- |
| Row names | `iphone:<plugin>`: plugins resolve by name inside the app (declared via `rows.schema`); no local manifest |
| Plugin loading | the host's rows decide which of the app's built-in plugins load; a row unloading stops the plugin — `apply` and cleanups run as usual |
| Authorization | happens at `apply` (or on a call); the TCC / HealthKit prompt points at **the app** (§8) |
| TLS | mandatory: a remote runtime without TLS allows loopback only, and the app's host is on another machine; self-signed certificate + the host's `RUTIS_CA` |
| Local network | iOS 14+ local-network permission (`NSLocalNetworkUsageDescription`) fires depending on connection direction; goes in the app's Info.plist |
| Foreground limit | the connection drops when the app backgrounds (suspension); it waits for the host to dial again on return; background survival is S22 |
| Implementation | Network.framework `NWListener` + `NWProtocolWebSocket` (subprotocols, TLS and ping all supported), semantics aligned with Go G2 §9; the combination is PoCed during S1/A1 (§16) |

### 7.2 Long path: the app as dialer (peer semantics)

The app dials out to a **listening rutis node** (the node machinery exists: `listen` config, token checks, `PeerPlugin`), exporting its capabilities as services, and may import the node's services. **Zero host-side changes**; the dialer role today exists only in the Rust host — this is the SDK's new role:

```swift
let link = RutisLink(
    dial: "wss://office.example.com/rutis",
    token: ProcessInfo.processInfo.environment["RUTIS_TOKEN"]!)

try await link.connect()                          // greeting, auth; reconnects with per-failure backoff

link.export("health", HealthService())            // the app decides what to expose; published on the link
let llm = try await link.imported("llm", LLM.self) // optional: use the node's services (the same RutisClient)
```

| Item | Rule |
| --- | --- |
| Protocol | the endpoint protocol (subprotocol `rutis.3`) + link semantics: the same set `PeerPlugin` speaks; `export` / `import` (events later) |
| Reconnect | per-failure backoff: retryable failures double from 0.5 seconds to at most 30 (a stable session resets); rejected credentials retry every 30 seconds; protocol mismatch stops with a report |
| Service visibility | link down → the app's exported services withdraw from the node, rows depending on them stop; after reconnecting they republish; call chains hold across nodes |
| Call safety | cross-node synchronous calls and callbacks-into-callbacks are covered by the node machinery and wait-cycle detection (#228); the SDK only needs to carry the chain correctly (§5) |
| Connections | one `RutisLink` is one connection; an app may dial several nodes at once |
| Certificates | a self-signed CA via the app-side trust configuration (the `RUTIS_CA` semantics surfaced in the SDK API) |

### 7.3 iOS specifics

| Item | Rule |
| --- | --- |
| Packaging | the SDK links statically into the app (an SPM library dependency); no dynamic loading, plugin definitions compile in — no App Store "downloading executable code" issue |
| Authorization | entitlements (HealthKit and friends) and usage descriptions live in the app's Info.plist / provisioning profile; prompts point at the app (§8) |
| Background | v1: foreground connection + reconnect (S22); later: APNs wakeup (with node-side help) or background modes — listed as open |
| Lifecycle | `RutisApp.serve` / `RutisLink` are stoppable (`stop()`): the app can wind down when backgrounding and restart in the foreground — cleaner than waiting for the system to cut it off |
| App Store review | the capabilities offered must match the app's stated purpose (a health-data tool reading HealthKit); the SDK itself is an ordinary library dependency with no review burden |

### 7.4 Shape ③: the App Intents bridge (follow-up)

The app defines rutis services (local or imported over a `RutisLink`) as App Intents, so Shortcuts / Siri / Spotlight can drive rutis-side capabilities — the other half of the Apple ecosystem consuming rutis. An `AppIntent.perform()` calling a rutis service is an ordinary async call, no new protocol; scope (which services, parameter mapping, Spotlight indexing) is designed when needed (S3).

## 8. TCC and signing

Authorization attaches to whoever calls the system API: in shape ① the plugin binary, in shape ② the app.

### 8.1 Shape ① (macOS plugin binaries)

| Item | Rule |
| --- | --- |
| Attribution | the runtime binary itself; the name in the prompt comes from its signature and embedded Info.plist |
| Usage descriptions | a bare executable links its Info.plist into `__TEXT,__info_plist` (linker `-sectcreate`, via SwiftPM `linkerSettings`); the SDK documents the snippet and the `new --lang swift` template ships it |
| Local development | ad-hoc signing is enough (`swift build` binaries are ad-hoc signed by default on Apple Silicon); prompts appear, attributed correctly |
| Distribution | Developer ID signing + notarization + hardened runtime; entitlements ride along |
| Library validation | nothing is dlopened, so no library-validation problem (A.4) |
| Quarantine attribute | the Go `xattr` check and reporting is reused |
| Several plugins, one binary | they share one TCC identity; different granularity means different binaries |

### 8.2 Shape ② (apps)

| Item | Rule |
| --- | --- |
| Attribution | the app; entitlements in the provisioning profile, usage descriptions in the app's Info.plist |
| Prompt timing | when a plugin's `apply` or a call touches a protected capability; a denial crosses back to the caller as any error would |
| No extra signing burden | apps are signed anyway; the SDK is a static library |

Acceptance (the M3 text plus this design): an EventKit plugin runs in a signed binary, the prompt points at that binary (shape ①); in an iOS app, a HealthKit plugin's prompt points at the app, and a denial crosses back cleanly (shape ②).

## 9. Rust-side changes

Go G1/G2 built the machinery for "manifest-carrying binary runtimes"; shape ① attaches by **sharing** it, shape ② is almost zero:

| Change | Where |
| --- | --- |
| Extract the language-independent parts of `go.rs` into a shared module: manifest execution and caching, directory scanning and marker sniffing, runtime-name derivation, the on-demand lifecycle and restart, file-replacement semantics | `rutis-loader/src/runtime/binary.rs` (new); `go.rs` becomes a thin wrapper with unchanged behavior, guarded by the existing tests |
| `SwiftResolver`, `SwiftBinaries`, `SwiftRuntimes` | `rutis-loader/src/runtime/swift.rs` (new, thin) |
| `swift_runtime_name`; `Launcher::swift` (the same spawn path as `Launcher::go`); `LocalRuntime::swift` | `rutis-bridge/src/runtime/{local,process}.rs` |
| features: rutis-bridge `swift = []`; rutis-loader `swift = ["runtimes", "rutis-bridge/swift"]`; rutis-host enables `swift` | the three `Cargo.toml`s |
| Remote npm naming rejects `swift:` and `<swift runtime>:` prefixes | already implemented (any `<name>:` prefix is rejected); tests only |
| **Shape ②a**: `remote[].language` gains `"swift"` | `rutis-host` / loader config |
| **Shape ②b**: nothing (node listening, `PeerPlugin`, tokens, reconnect, service publishing exist; wait-cycle detection #228 covers cross-node sync) | — |
| `scripts/train.mjs` gains the version regex for `swift/rutis/Sources/Rutis/Version.swift` | `scripts/train.mjs` |

If the `go.rs` extraction turns out too entangled, the fallback is: `swift.rs` copies first, unifying later — the design leans toward extraction, and deviations are recorded here with the implementation.

## 10. rutis-host

```json
{
  "runtimes": { "swift": { "dir": "plugins/swift", "binaries": ["bin/weather"], "start": "on-demand", "idle": 60 } },
  "rows": [{ "id": "weather", "name": "swift:weather", "config": { "city": "Oslo" } }]
}
```

The fields are the same as `runtimes.go` (`dir` / `binaries`, at least one; `start`: `on-demand` (default) / `eager`; `idle` in seconds; plus `project`). Shape ②a's configuration is §7.1 (`runtimes.remote`).

| Command | Behavior | Phase |
| --- | --- | --- |
| `check` | per binary: runtime name, platform, SDK / plugin API versions, plugins; ambiguities; each row's target binary; failed candidates; the quarantine attribute | S2 |
| `dev` | without `rutis.dev.json`, detect a Swift project by `Package.swift`; rebuild and restart per §11 | S2 |
| `new <name> --lang swift` | the plugin target, `cmd/<name>/main.swift`, the `Info.plist` with usage descriptions and `linkerSettings`, tests, `rutis.dev.json`, the release workflow | S2 |
| `swift add <URL>` | download a prebuilt binary into `dir`, verify SHA-256 | S3 |
| `run` | no Swift toolchain needed | — |

## 11. Changing code (shape ① only)

As Go §12: `dev` watches `.swift`, `Package.swift`, `Package.resolved` → `swift build -c release` into a new cache file `<name>-<seq>` → on success, replace that runtime's file and restart it; on failure print the error, the old process continues. Deploying: replace by rename → `SwiftRuntimes::restart("<runtime>")`. Shape ② changes code by shipping a new app version (App Store / TestFlight).

## 12. Repository and releases

| Item | Rule |
| --- | --- |
| Location | `swift/rutis`: target `Rutis` (core + API), `RutisTest`; the session layer in `Sources/Rutis/Internal/Peer`; the conformance executable target `RutisConformance` |
| Manifest | a `Package.swift` at the **repository root**, package name `rutis-swift`, targets pointed at `swift/rutis/Sources/...` via `path:`; platforms `macOS(.v13)`, `.iOS(.v17)`. SwiftPM git dependencies only read the root (verified in appendix E.2) |
| Versioning | `Rutis.sdkVersion` follows the main release line: the existing `v0.9.x` tags are exactly the `vx.y.z` form SwiftPM accepts, so no new tag namespace; `from:` starts at the first release containing the root `Package.swift` |
| How dependents write it | plugin authors and apps share one line: `.package(url: "https://github.com/arcships/rutis", from: "0.10.0")` + `.product(name: "Rutis", package: "rutis")`; apps add the package directly in Xcode |
| Dependencies | core: Foundation only; network channels use Network.framework / URLSession (Apple platforms); no uniffi / swift-bridge (appendix B) |
| Compatibility | within one protocol version, a binary / app of any SDK version runs on a host of any version, provided its plugin API is not above what the host supports |
| Language mode | Swift 5 mode to start, the API designed Sendable-friendly; the toolchain policy as Go: "the current two stable releases" (today 6.3 and 6.4) |
| Docs | `docs/guide/swift-plugin.md` (Chinese and English), `docs/guide/swift-app.md` (shape ②, both languages); a Swift column in `docs/guide/plugin-api.md`; README / CONTRIBUTING gain the toolchain notes |

## 13. Platforms

| Platform | Shape ① | Shape ② | Notes |
| --- | --- | --- | --- |
| macOS 13+ | ✅ | ✅ (Mac apps included) | the Swift runtime ships with the system; CLI binaries are self-contained |
| iOS 17+ | — | ✅ | the integration unit is the app; no process spawning, no dynamic loading (§2) |
| watchOS / visionOS / iPadOS | — | later | the iOS model again; join when needed |
| Linux | later | later | SE-0342 static runtime, swift-foundation cross-platform; the Network.framework channels need a replacement (swift-nio or POSIX); join when there is a real need |
| Windows | out | out | the `tcp:` loopback and the platform check exist, and that is all |

## 14. Tests

| Test | Where | What |
| --- | --- | --- |
| Session-layer unit tests | `swift/rutis/Tests` | encode/decode, reference counting, callbacks during a sync wait, cancellation, TaskLocal chains, concurrency, suspended waits not blocking |
| iOS core unit tests | same, CI adds `xcodebuild test -destination 'platform=iOS Simulator'` | the core layer compiles and tests on iOS (values, leaf API, in-memory session) |
| Session conformance | `rutis-bridge/tests/runtime_conformance.rs` + the `RutisConformance` target | `session::testing::session` |
| Contract × channels | `session_matrix.rs` gains `Swift` | `fd:3`, dial-back |
| Swift specifics | `rutis-bridge/tests/swift_runtime.rs` | cancellation reaching the Task; error shapes; `fatalError` exit withdrawing services; references and release; fixtures `conformance-session/weather/greeter`, `crash()` exits 17 |
| Manifest / binaries / on-demand | `rutis-loader/tests/swift_rows.rs` | the full set mirroring Go's `go_rows.rs` |
| Runtime conformance | `multilang.rs` (feature `swift`) | a Swift leaf plugin; cross-language cold starts; a provider going away stops only its users; `inject` gating |
| Cross sync calls | `multilang.rs` | Node→Swift→calling back into Node (with the chain), Swift↔Go, Swift↔Python: no deadlock; `SyncWaitCycle` without false positives |
| Instances | `instance_runtimes.rs` | label isolation, per-instance services |
| Remote (②a) | `remote_rows.rs`, `leases.rs` gain a Swift column | a listener dialed in and controlled; lease takeover; rows stop on disconnect, reload on return |
| Dialer (②b) | `swift/rutis/Tests` + `rutis-loader/tests/swift_link.rs` | `RutisLink`: greeting, rejected-token backoff, services republishing after a reconnect, export/import, chains across nodes |
| TCC / HealthKit (e2e) | new entries in the e2e suite | shape ① EventKit; shape ② an iOS app's HealthKit: prompt attribution, denial crossing back. CI cannot click prompts: the interactive part is a manual checklist, the rest is automated |
| Fixtures | `crates/rutis-loader/tests/fixtures/swift` | built into at least two binaries at test start |
| CI | `runtimes-swift` (a macOS runner, Xcode preinstalled; two stable toolchains; including the iOS Simulator unit tests); `test` and `network-macos` gain the Swift fixture build | `swift test` + `xcodebuild test`; `cargo test -p rutis-bridge --features swift,…`; `-p rutis-loader --features swift,…`; `-p rutis-host`; `cargo check --no-default-features --features swift` |

- E2E milestones: the `new --lang swift` loop (S2), cross-language and crash recovery (S3), running a downloaded binary without a toolchain (S9) each gain Swift.

## 15. Phasing

| Phase | Content | Acceptance |
| --- | --- | --- |
| S1 | the SDK core (session, leaf API, values, concurrency) + shape ①'s `serve` (`fd`/`unix`), manifest, `RutisTest`; the `binary.rs` extraction + the thin `go.rs`; `Launcher::swift`, the feature; `SwiftResolver`; `SwiftRuntimes` (`eager` only) | §14 minus on-demand, remote and TCC, all passing; several binaries in parallel; the iOS Simulator unit tests pass; Go's existing tests stay green |
| S2 | shape ① on-demand start and idle stop; rutis-host (`runtimes.swift`, `dev`, `check`, `new --lang swift`); the guides; TCC / EventKit | the on-demand tests pass; `dev` restarts only its own runtime; the prompt points at the binary, a denial crosses back |
| A1 | shape ②a: the SDK's `listen:` (`NWListener`); `remote[].language: swift`; remote leases and conformance | a foreground app (iPhone or simulator) is dialed in and controlled as a swift remote runtime: load / unload / disconnect-recovery tests pass |
| A2 | shape ②b: `RutisLink` (dial / export / import / reconnect); zero node-side changes | an app dials a node, its exported services are consumed by the host's rows; services recover after a network drop; a wrong token retries on the backoff |
| S3 (on demand) | downloading prebuilt binaries; receiving object references; the `@RutisService` / schema macro; the version build plugin; the App Intents bridge; events forwarding; watchOS / visionOS; Linux; APNs wakeup | when there is a need |

A1 needs only S1's session layer and can run in parallel with S2; A2 needs S1 plus A1's network-channel layer.

## 16. Open questions

| Item | Current leaning |
| --- | --- |
| How manifests are read | executing the binary (as Go); the alternative is writing a file at build time |
| `idle` default | 60 seconds (as Go) |
| sync methods cannot call out | already stopped at compile time; whether a dev-mode runtime warning is worth it falls out of implementation |
| Minimum iOS version | 17; can go lower (aligned with the App Intents era) |
| `NWListener` + `NWProtocolWebSocket` subprotocol / self-signed TLS / ping combination | semantics aligned with Go G2; details finalized by a local PoC during S1/A1 |
| Background survival strategy | v1 foreground + reconnect; APNs wakeup (with node-side help) or background modes, decided in S3 |
| Shape ②b events forwarding | v1 exports / imports only; events when needed |
| App Intents bridge scope | S3 |
| Universal binaries | v1 ships per-arch; whether releases are universal is the `new` workflow's call |
| Binary size | a few MB in release; no premature optimization |

## 17. Departures from the master plan

| The master plan says | This design |
| --- | --- |
| M3's goal: "macOS system capabilities" | widened: bridging the Apple ecosystem and apps (three shapes, §2). Shapes ② and ③ are beyond the master plan; the implementation PR should record the milestone there (as Go's A.3 revised M4) |
| "A signed auxiliary program loading plugin bundles with `dlopen`" | revised: shape ① is one signed binary per plugin group (reasons and costs in appendix A) |
| Row name `swift:Calendar.bundle` | shape ① `swift:<plugin>`; shape ②a `<remote>:<plugin>` (remote-runtime naming) |
| §2.7 files Swift under "runtimes that load code by name" | filed with Go under "the plugin is a compiled program"; on iOS the integration unit is the app |
| M3 acceptance | kept and widened (§8) |
| §9 open question "Swift (M3) confirms the reentrancy rule" | answered: reentrant (§5); the rule "leaf runtimes are all reentrant" is adopted |
| Apple platforms only (§7) | narrowed and layered: macOS + iOS (shape ②); Linux later; Windows out |

## Appendix

### A. Why not a dlopen auxiliary program (the revision of the master plan's M3)

The master plan files Swift with the "load code at runtime" runtimes (as Node / Python), loading plugin bundles via `dlopen`. This design switches to Go's "compiled program" model, reasons in order of importance:

**A.1 Go (M4) landed first and built the machine.** `GoResolver`, `GoRuntimes`, the two-stage release, on-demand start, restart, `check`, `dev`, `new` all exist (#227); shape ① attaches through shared code (§9). The dlopen route would instead build: bundle discovery and loading, a second API layer between the auxiliary program and its bundles, and a separate answer to the manifest / caching / platform-check problems Go already solved. On iOS, dlopen is not even a topic (§2).

**A.2 Swift's cross-toolchain binary compatibility is not dependable.** When bundles and the auxiliary program compile separately, Swift-side cross-module calls need module stability + library evolution — not guaranteed in practice for bundles by arbitrary authors on arbitrary Swift versions. The workaround is pushing the boundary down to a plain C ABI, which means inventing a layer neither Go nor Python needs. SwiftPM also has no loadable-bundle product.

**A.3 TCC granularity.** One auxiliary program means one TCC identity: grant calendar to plugin A, and plugin B in the same process can read calendars too. In the executable model each plugin group gets its own narrow grant — a better privacy story, still satisfying M3's acceptance. In shape ② authorization belongs to the app anyway.

**A.4 Signing and library validation.** A signed auxiliary program with the hardened runtime cannot dlopen bundles not signed by the same team without `disable-library-validation` (widening the attack surface) or every author signing with one team. Executables and statically linked apps have no such problem.

**A.5 The loss and its substitutes.** What is lost: dropping a new bundle into a running process and having it load. Substitutes: (1) the deployment composes several plugin packages into one binary in `cmd/main.swift` (dependencies merge through the deployment's manifest, as Go appendix H.3); (2) several plugins each in their own binary, started on demand, stopped when idle. Distribution sources are Go's H: author-published, `swift add <URL>` (S3), deployment-composed, `dev` builds; in shape ② it is shipping a new app version.

### B. Swift facts (research sources)

| Fact | Source |
| --- | --- |
| `@_cdecl` became official `@c` in Swift 6.3 (SE-0495); this design needs no C ABI (the session protocol is self-implemented, as Go) | [SE-0495](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0495-cdecl.md) |
| macOS 10.14.4+ ships the Swift runtime; CLI binaries bundle nothing | [swift.org ABI stability](https://swift.org/blog/abi-stability-and-apple/) |
| The Swift 6.4 (2026-09) toolchain's minimum deployment target is macOS 13; releases land each March and September | [the swift-evolution version table](https://github.com/swiftlang/swift-evolution/blob/main/README.md), [platform-support](https://swift.org/platform-support/) |
| Linux: since SE-0342 the runtime is statically linked by default; the Static Linux SDK (musl) is fully static but disables `dlopen` | [SE-0342](https://github.com/apple/swift-evolution/blob/main/proposals/0342-static-link-runtime-libraries-by-default-on-supported-platforms.md), [the Static Linux SDK](https://www.swift.org/documentation/articles/static-linux-getting-started.html) |
| swift-foundation provides FoundationEssentials, cross-platform | [swift-foundation](https://github.com/swiftlang/swift-foundation) |
| UniFFI's Swift 6 support is marked partial (async generated code is not Sendable); swift-bridge is 0.x with a roughly 16-month hiatus — neither is adopted | [UniFFI Swift](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html), crates.io |
| setup-swift@v3 supports Ubuntu / macOS; macOS runners come with Xcode | [setup-swift](https://github.com/swift-actions/setup-swift) |
| Blocking reads must not sit on Swift's cooperative pool (its width is the core count); a dedicated thread + continuation is the standard shape | Swift concurrency docs and community consensus |
| Linking an Info.plist into a bare executable's `__TEXT,__info_plist` (`-sectcreate`) is long-standing practice | CMake / xamarin.macOS / swift-forums threads |
| Network.framework's `NWListener` / `NWProtocolWebSocket` serve WebSocket with subprotocols and TLS; `URLSessionWebSocketTask` is the client | Apple documentation (the combination is finalized by the §16 PoC) |
| iOS: no process spawning, the sandbox forbids downloading executable code, background apps are suspended, local-network permission (iOS 14+) | Apple platform documentation |

### C. Side-by-side with the Go runtime (shape ①)

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
| Platforms | Linux / macOS / Windows | macOS (①) / macOS + iOS (②) |
| Distribution | git tag `go/rutis/vX.Y.Z` + the module proxy | root `Package.swift` + the main line's `v` tags (one line for plugins and apps) |
| Dependencies | standard library only | core: Foundation only; network channels Network.framework / URLSession |

### D. Why the value boundary needs no `Ref`-style rule

Go needs an explicit `Ref` because a Go struct can be either data or an object — the language does not say. Swift's value types (struct / enum) and reference types (class) are two different worlds in the type system: structs are naturally data, classes naturally object identity. So the default rule reads off the language: struct / enum copies as data; a class crosses only through an explicit `Rutis.ref` and must carry its own method table (Swift cannot reflect its methods out, so the author provides the table — the same mechanism as §4.2's closure table).

### E. Local PoCs (2026-10-12, Swift 6.3.2 / Xcode)

**E.1**: a root `Package.swift` declaring targets at a subdirectory path (`path: "swift/rutis/Sources/Rutis"`) builds and runs fine.

**E.2 SwiftPM's version-tag rule**: against a git dependency, `1.0.0` (plain semver) and `v1.1.0` (v-prefixed) are accepted as versions; `rutis-swift-1.0.0` (package-name prefix), `swift/rutis/1.0.0` (slash), `rutis-mono-1.2.0` (URL-name prefix) are all ignored. Conclusion: this repository's existing `v0.9.x` release tags are immediately usable as SwiftPM versions; the Swift SDK releases at the main repo's version, with none of Go's `go/rutis/v…` forms (SwiftPM rejects slash tags).

**E.3 Parameter-pack contextual inference**: `func method<each A, R>(_:) -> (Ctx, repeat each A) async throws -> R` infers from the field's closure type at `LLM(ask: s.method("ask"), …)` and compiles (a Swift 5.9 feature). §4.3's `RutisClient` shape follows from it; if extreme signatures infer poorly, the fallback is a set of overloads for arities 0–3.
