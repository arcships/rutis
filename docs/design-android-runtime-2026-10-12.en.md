# The Android Runtime (Kotlin Remote Node)

[中文](design-android-runtime-2026-10-12.md)

Status: design draft, not implemented. Date: 2026-10-12. Baseline: `main` `ebf0711` (0.9.1).
Based on: [the multilingual design](design-multilang-runtimes-2026-10-03.en.md), [the remote plugins design](design-remote-plugins-2026-10-03.en.md) (§4.4 channels and leases), [the Go plugin runtime](design-go-runtime-2026-10-10.en.md) (the G2 remote implementation), [the Bun runtime](design-bun-runtime-2026-10-09.en.md) (§4 remote), [the plugin API](guide/plugin-api.en.md). The desktop-JVM Kotlin runtime (Maven ecosystem, dev companion) is a separate design ([the Kotlin runtime](design-kotlin-runtime-2026-10-12.en.md)); the two share one KMP SDK. This document is the main line.

Positioning: the Kotlin/ART runtime exists to **connect to the Android ecosystem and apps** — a host and its agents on a desktop or server use device capabilities and app bridges through a helper APK installed on the device. This fills the slot the overall design gave Swift/M3: "system capabilities exposed through a helper application with its own permission identity."

Out of scope (D22): topology B (the Rust core embedded in an Android app; evaluated in §13); cloud relay and device dial-out; USB/ADB; Play-store compliance; the host compiling anything at run time.

## 1. Decisions

| # | Decision | Section |
| --- | --- | --- |
| D1 | Topology A: the host runs on desktop/server; a helper APK on the device joins as a **remote runtime**; rows `<name>:<plugin>`, default name `droid` | 2 |
| D2 | The device side is pure Kotlin, no Rust on the device: a session layer (endpoint protocol 3) + a ws listener + the leaf SDK | 2 |
| D3 | Connection direction unchanged: **the device only listens, the host dials** (remote design §4.4); relay/dial-out is a later addition | 4 |
| D4 | TLS relaxed (an explicit deviation from Python/Bun's "loopback only"): a plain `ws://` listener is allowed, but it accepts connections **from private network ranges only** (RFC 1918 / ULA / link-local), and a token is mandatory; non-private sources are refused; `wss://` has no such restriction | 4 |
| D5 | Authentication: a token (host side `RUTIS_TOKEN_<NAME>`, device side generated on first run or user-set), constant-time comparison; refusal codes 404/401/403/400 as the remote design | 4 |
| D6 | Capability plugins are DEX modules: **built-ins compiled into the APK** (loaded by the app classloader directly, no DexClassLoader); **dev-pushed ones** arrive through an authenticated upload endpoint into app-private storage and load via `DexClassLoader`; the protocol changes zero (the `rows.load` entry resolves on the device) | 5 |
| D7 | A plugin pack is a zip (`classes.dex` + a `rutis.json` descriptor); when the descriptor disagrees with the loaded declaration, the declaration wins, with a warning | 5 |
| D8 | Bounded hot reload: ART does not unload classes; each plugin gets a reload budget (default 50); when exhausted the runtime restarts itself (the foreground service returns via `START_STICKY`; the host's link reconnects, which is a lease takeover) | 5 |
| D9 | **Permissions are lifecycle**: the capability plugins' Android permissions are declared in the APK manifest; not granted → the load fails naming the permission; revoked at run time → the services are withdrawn (rutis revocation stops the consumers); enablement guidance for Accessibility / NotificationListener flows through a `droid:system` service | 6 |
| D10 | Threading: one platform thread per incoming call (the Go shape; device concurrency is low); capabilities `sync-wait` + `reentrant-sync`, no `sync-stack`, no `objects` | 7 |
| D11 | The plugin API is suspend (Kotlin coroutines); `ctx.launch` runs on the row's scope (`Dispatchers.Default`), cancelled and joined at unload | 7 |
| D12 | No runtime reflection: schemas and descriptors are generated at compile time (KSP, A4) or given explicitly by `Plugin.schema`; kotlin-reflect does not go on the device | 8 |
| D13 | The SDK is a KMP library `io.github.arcships:rutis` (android + jvm targets); one plugin source for desktop and device | 8 |
| D14 | The device runtime is packaged as an AAR `io.github.arcships:rutis-droid` (embeddable in any app) plus a helper APK embedding it (the `rutis-droid` app module) | 9 |
| D15 | Methods are `async` by default; `sync` is explicit (synchronous methods block event-loop peers) | 8 |
| D16 | Value rules as the other leaf runtimes: JSON-compatible types copied, functions and `Ref` by reference, `ctx.provide` always by reference, `undefined` a dedicated value | 8 |
| D17 | Plugin API version `PLUGIN_API = 1`, the existing mechanism as-is | 8 |
| D18 | Session details per the protocol: 16 MiB per frame (over it, 1009), heartbeat 30 s default (`RUTIS_HEARTBEAT` adjustable), a new connection takes over the old lease (old connection 4002), text frames | 4 |
| D19 | One host-side change: the remote language mapping in `rutis-host/src/host.rs` gains `"kotlin"|"kt"` → `modules`; everything else exists already (the peer row + remote configuration) | 10 |
| D20 | Test strategy: conformance passes first on the **jvm target** (fast iteration), then on an emulator (device-specific paths: permissions, DexClassLoader, the upload endpoint); `multilang.rs` gains a remote Kotlin column | 11 |
| D21 | The desktop-JVM runtime stays a separate design ([the Kotlin runtime](design-kotlin-runtime-2026-10-12.en.md)), scheduled after the Android phases | 12 |
| D22 | Out of scope: topology B, cloud relay/dial-out, USB/ADB, Play compliance, host-side compilation | 13 |

## 2. Model

```text
desktop / server                              Android device
rutis host (Rust)                             rutis-droid helper APK (foreground service)
 ├─ local rows: node / py / bun / go / dylib   ├─ session layer (KMP android target, endpoint protocol 3)
 ├─ RuntimePlugin::remote("droid")             ├─ ws / wss listener + token auth + plugin upload endpoint
 │   (waits for RuntimeSession#droid,          ├─ capability plugins (DEX):
 │    provided by the peer row)                │   contacts / notifications / Intent / sensors /
 └─ rows droid:contacts / droid:notifications  │   Accessibility automation / app bridges
     droid:apps / droid:automation ...         └─ permission bridge (grant state ↔ row lifecycle)
```

- On the host it is simply a **remote language runtime**: one `runtimes.remote` entry plus one `rutis-bridge/peer` row dialing the device; rows `<name>:<plugin>` resolve on the device (`RuntimeResolver::modules`, uncached, two-stage refresh).
- Several devices are several named remote runtimes (`droid-pixel`, `droid-tablet`), independent of each other.
- Device process death/restart: the session drops, all rows of the runtime stop; after the host's link reconnects it is a new generation of the runtime whose rows load again in dependency order (existing lease/takeover semantics, proven by Go G2).
- Plugins on one device call each other on the object itself; across devices and languages, by name through the host.

## 3. The device side: APK structure

```text
app (helper APK, foreground-service style)
 └─ depends on the rutis-droid AAR
     ├─ DroidService: a foreground service (connectedDevice type, persistent notification "rutis node running")
     ├─ Listener: an HTTP + WebSocket server (a small implementation: NanoHTTPD+websockets or Ktor CIO; text frames only)
     │    ├─ GET /rutis        → WebSocket upgrade (subprotocol rutis.3)
     │    └─ PUT /rutis/plugin/<name>  → plugin pack upload (same token auth)
     ├─ Session layer: the KMP android target (frames, reference counts, cancellation, call chains, lease takeover)
     ├─ Plugin registry: built-in modules (APK classes) + uploaded modules (private-directory zips → DexClassLoader)
     └─ Permission bridge: grant-state watching → row loading / service withdrawal
```

- Process model: one foreground-service process; `START_STICKY`, restarted by the system after a kill; the service rebuilds the listener and session.
- Battery/Doze: a foreground service plus an active socket; the heartbeat defaults to 30 s (adjustable). Deep-Doze network restrictions are a documented reality (most reliable while charging or with the screen on).
- UI: minimal — first run generates and shows the token and LAN address (for the host configuration), a permission-request entry point, the remaining reload budget.

## 4. Channels, authentication, and the TLS relaxation

| Item | Rule |
| --- | --- |
| Listen address | all interfaces, port `:7443` by default, path `/rutis`, subprotocol `rutis.3` (endpoint format, protocol 3) |
| Direction | the device only listens, the host dials (D3; the remote design's rule unchanged) |
| TLS | `wss://` with a device-generated self-signed certificate (generated on first run, fingerprint shown in settings, pinned host-side via `RUTIS_CERT`) |
| **Relaxation** (D4) | a `ws://` (no TLS) listener is allowed, but the **source address is checked per connection**: only private ranges (10/8, 172.16/12, 192.168/16, fc00::/7 ULA, fe80::/10 link-local) are accepted; a non-private source is refused outright. Python/Bun's "no TLS on loopback only" is relaxed exactly this far for this runtime — an explicit, recorded deviation |
| token | mandatory (both `ws` and `wss`); generated/set on the device, `RUTIS_TOKEN_DROID` (or the generic `RUTIS_TOKEN`) on the host; constant-time comparison |
| Refusal codes | 404 wrong path, 401 no credentials, 403 wrong token, 400 wrong subprotocol (as the remote design) |
| Heartbeat | dropped after 30 s without an answer by default; `RUTIS_HEARTBEAT` adjustable |
| Frames and limits | text frames; 16 MiB per frame, over it closes with 1009 |
| Takeover | a new connection: authenticate first → close the old one (read no further frames) → clean up every row and proxy of the old lease → answer the new `hello`; the old connection gets 4002 |
| Startup output | once listening, `rutis: listening on …` in logs (the service notification shows the address too) |

## 5. Plugins: the DEX model and hot reload

### 5.1 Two sources

| Source | Loading | Reload |
| --- | --- | --- |
| Built-in (compiled into the APK) | loaded directly by the app classloader (just APK classes; no DexClassLoader) | an APK change (normal app update) |
| Uploaded (dev) | `PUT /rutis/plugin/<name>` (same token): a plugin pack zip = `classes.dex` + `rutis.json` into app-private storage; `rows.load` loads it by name via `DexClassLoader` | see §5.2 |

- Entry resolution as the JVM draft §8.2: `droid:contacts.ContactsPlugin` (a dot) → a fully-qualified class name; `droid:contacts` (no dot) → matched by the `rutis.json` `name`; several matches is an ambiguity error.
- `rows.schema` is answered on the device from the declarations (built-in and uploaded share the path); when a descriptor disagrees with the actual declaration, the declaration wins with a warning.

### 5.2 Bounded hot reload (D8)

- ART does not unload classes: a reload is a new `DexClassLoader`; the old loader's classes stay. The budget defaults to 50 reloads per plugin (the remaining count is visible in settings and `check`).
- When exhausted: the runtime `dispose`s every row in order → exits the process → the foreground service returns via `START_STICKY` → the host's link reconnects (a lease takeover) → rows reload in dependency order. From the host this is an ordinary restart, not an error.
- Uploading a new version spends budget once; identical content is skipped (zip content hash).

### 5.3 The dev loop (A4)

```text
desktop: plugin project (Gradle, KMP/android library)
  → gradle build → plugin pack zip (dex + rutis.json, descriptor generated by KSP)
  → rutis-host dev: PUT the upload to the device → trigger the row's rows.load reload
```

The host does not compile Android code; `gradle build` is the project's own build (as Go D15). `rutis-host new --lang kotlin` generates this template.

## 6. Permissions are lifecycle (D9)

Android permissions bind to the APK identity, and rutis's revocation semantics map onto that directly:

| Event | Device runtime behaviour | Host view |
| --- | --- | --- |
| permission not granted at `rows.load` | refuse the load, naming the permission and how to request it | the row fails to load with a readable reason |
| granted (settings / guidance) | the plugin can load (a host `reload` or the next resolution) | the service appears |
| revoked at run time (system settings) | withdraw the plugin's provides (`service(id, None)` notification) → the row stops | the provider is revoked → the consumers stop in order |
| Accessibility / NotificationListener not enabled | the capability is unavailable; the `droid:system` service reports status and deep-links into settings | rows depending on it do not start; `check` says why |

- Grant-state watching: check before loading plus periodic reconciliation at run time (revocation has no broadcast; a periodic check is the pragmatic choice; the period defaults to 30 s, the same order as the heartbeat).
- The permission request itself happens in the device UI (the user acts on the phone); the host can query and guide but never grant — the permission prompt **names the helper APK**, which is exactly the Android counterpart of Swift/M3's "signed helper" acceptance item.

## 7. Threading: synchronous calls and reentrancy

ART has no virtual threads (Loom is OpenJDK's); device concurrency is low (one person's phone), so the Go shape applies:

```text
APK process
 ├─ reader thread: blocking frame reads, parse, dispatch
 ├─ one new platform thread per incoming invoke/call/get/await/control operation
 ├─ one write lock: frames serialized
 └─ row coroutine scope: ctx.launch tasks (Dispatchers.Default), cancelled and joined at unload
```

| Item | Rule |
| --- | --- |
| Outgoing synchronous call | the current thread blocks on a `CompletableFuture` the reader completes |
| Reentrancy | an incoming call can run at any time (a new thread); declares `reentrant-sync`. The cost, as Python/Go: **a service may be called again while it is itself inside a synchronous call; do not hold locks across rutis calls** |
| Capability combination | `sync-wait` + `reentrant-sync`; no `sync-stack` (concurrent stacks, chain edges only); no `objects` |
| `SyncWaitCycle` | never produced locally; received ones surface as remote errors |
| Cancellation | a `cancel` frame cancels the call's coroutine and interrupts its thread; a non-interruptible computation runs to completion and its result is discarded, reported honestly |
| Suspend bridging | a suspend method runs inside `runBlocking` on its dedicated thread (it blocks its own call thread — legitimate); `apply`/cleanups likewise |
| Exceptions | an exception in a service method returns as an error graph; from `apply` it fails the load with cleanups still run; an uncaught exception in a plugin-spawned thread ends the process, stopping every row, with no automatic restart (the host's link reconnects into a new generation) |

## 8. The KMP SDK

### 8.1 Shape

- `io.github.arcships:rutis` (KMP: android + jvm). Plugin authors depend only on it; which target they compile for is the project's choice (a device plugin → android; desktop companion → jvm).
- The SDK depends on kotlin-stdlib only (suspend is a language feature; `kotlin.coroutines` lives in the stdlib; coroutine builders come from the runtime).
- No runtime reflection (D12): `rutis.json` descriptors and config schemas are generated at compile time by a KSP processor (A4); until then `Plugin.schema` is given explicitly.

### 8.2 Writing a plugin (illustrative; the same source as the JVM draft)

```kotlin
data class Config(val group: String? = null)

interface Llm { suspend fun ask(question: String): String }

class Contacts(private val resolver: ContactsResolver) {
    suspend fun list(query: String): List<Contact> = resolver.query(query)
    fun count(): Int = resolver.count()
}

val plugin = rutis.definePlugin(
    name = "contacts",
    inject = listOf("llm"),
    provides = rutis.provides("contacts" to rutis.methodsOf<Contacts>(sync = "count")),
    config = rutis.schemaOf<Config>(),
) { ctx, config ->
    val llm = ctx.use<Llm>("llm")
    ctx.provide("contacts", Contacts(ContactsResolver(llm, config.group)))
}
```

- Implementations needing `android.*` live in the plugin's own `androidMain` source set; `ctx.use`/`provide`/`effect`/`launch` and the declarations are common source.
- Method naming/signatures/errors and the value rules (D15/D16/D17) are the same as the JVM draft §7.2–7.4 and are not repeated.

## 9. Repository and packaging

| Item | Rule |
| --- | --- |
| Location | `kotlin/` (a Gradle multi-project): `sdk` (the KMP library), `droid` (the AAR runtime), `app` (the helper APK), `tools` (the KSP processor, A4) |
| Release | AAR + KMP library to Maven Central (the `io.github.arcships` namespace); the helper APK via GitHub Releases (sideload; Play listing is separate compliance work, out of scope) |
| Versions | managed by `scripts/train.mjs` (maven coordinates and the APK version added) |
| Device requirements | Android 10 (API 29) and up; arm64-v8a primarily, x86_64 for emulators |
| WS server | NanoHTTPD+websockets or Ktor CIO (one of the two, decided in A2), text frames only |

## 10. The host side

One change (D19): the remote language mapping in `rutis-host/src/host.rs` gains `"kotlin"|"kt"` → `RuntimeResolver::modules`. Everything else in the configuration exists already:

```json
{
  "runtimes": {
    "remote": [{ "name": "droid", "language": "kotlin" }]
  },
  "rows": [
    { "id": "droid-peer", "name": "rutis-bridge/peer",
      "config": { "peer": "droid", "dial": "ws://192.168.1.42:7443/rutis" } },
    { "id": "contacts", "name": "droid:contacts" },
    { "id": "notify", "name": "droid:notifications" }
  ]
}
```

- token: the environment variable `RUTIS_TOKEN_DROID` (the existing rule, `token()` in `config.rs`).
- with `wss`, the host pins the device's self-signed certificate via `RUTIS_CERT` (the fingerprint is shown in the device settings).
- `mount` reply: `implementation: { name: "io.github.arcships:rutis-droid", version }`, `engine: { name: "kotlin", version, vm: <ART version> }`; `check` prints them.
- Several devices: several `remote` entries with their own peer rows; row-name prefixes keep them apart.

## 11. Tests

| Test | Where | Content |
| --- | --- | --- |
| Session unit | `kotlin/sdk` (jvm target, fast) | encoding, reference counting, callbacks during sync calls, cancellation, call chains, concurrency, lease takeover |
| Session conformance (desktop) | `runtime_conformance.rs` + a `conformance` entry of the KMP **jvm** target (memory/ws channel) | the full `session::testing::session` — shake the protocol out on the desktop first |
| Session conformance (device) | instrumented tests on an emulator (android target, loopback ws) | the same assertions on ART |
| Remote contract | `websocket_conformance.rs` gains the Kotlin far end | endpoint protocol 3, token, heartbeat, takeover 4002 |
| Contract × channels | `session_matrix.rs` | the upload endpoint, DexClassLoader loading, both loading paths |
| Behaviour | instrumentation in `kotlin/` + `multilang.rs` (feature `kotlin`) | permission↔row lifecycle (refused load / withdrawal on revoke), reload-budget exhaustion and self-restart, reconnection and reload after process death, cross-language calls |
| Soak | instrumented | memory stable across many load/unload cycles within budget; long-connection heartbeat and Doze behaviour recorded |
| CI | `ci.yml` | a JDK job (jvm-target tests + conformance) + an emulator job (android target); Gradle builds run first |

## 12. Phases

| Phase | Work | Acceptance |
| --- | --- | --- |
| A1 | KMP SDK skeleton + session layer (jvm target) | passes the `session::testing::session` conformance on the desktop |
| A2 | the android target of the session layer + listener + token + the TLS relaxation rule; emulator loopback | a Rust host connects over ws to the runtime inside an emulator, `websocket_conformance` passes; a non-private source is refused by `ws` |
| A3 | the plugin framework (built-in/upload/bounded reload) + permission bridge + the helper APK + the first capabilities (contacts, notifications, Intent) | real-device acceptance: permission prompts name the APK; revoking a permission stops the consumers; after budget-exhausted self-restart everything recovers |
| A4 | the dev loop (KSP descriptors/schemas, upload, the `new --lang kotlin` template, `check`) | desktop edit → upload → row reload, end to end in seconds to minutes |
| A5 (later) | relay/device dial-out (needs protocol-side design), the topology-B increment, more capability plugins (Accessibility automation, app bridges) | separate designs |

The desktop-JVM runtime (D21, [the Kotlin runtime](design-kotlin-runtime-2026-10-12.en.md)) plugs in after A4 (the `Launcher::kotlin` line), as the Maven-ecosystem runtime and desktop debugging companion.

## 13. Open questions and the topology-B evaluation

- **Heartbeat and Doze**: network availability in deep sleep varies by vendor; document honestly (most reliable charging/foreground), consider event buffering if needed.
- **Revocation reconciliation** (§6): a 30 s period versus a more precise hook (there is no broadcast; periodic or at key operations only).
- **Upload-endpoint abuse surface**: the same token already authenticates it; consider a content-hash allowlist or signatures (fine for dev; a public exposure demands wss and stronger pairing).
- **The topology-B increment (core embedded in an app)**: all of A's Kotlin layer is reused (the session layer runs over rutis-bridge's existing `transport::memory`, contract unchanged); the increment is the core's NDK cross-compilation (cargo-ndk, tokio viable), JNI bootstrap and lifecycle adaptation, packaging the `.so` into an AAR, and Play DCL disclosure. The work is mostly on the Rust side; start it only with a real on-device-host need.
- **Accessibility automation boundaries**: automating third-party apps is constrained by both permissions and policy; it is a separate capability-plugin design (A5), not in the first release.

## Appendix: relation to the desktop-JVM draft

One KMP SDK, two runtimes: `rutis-droid` (this document; Android/ART, remote node) and the desktop-JVM runtime ([the Kotlin runtime](design-kotlin-runtime-2026-10-12.en.md); `Launcher::kotlin` local rows, the Maven ecosystem + dev companion). Shared: plugin declarations and the SDK API, method shapes/value rules/`PLUGIN_API`, the session-layer core (the jvm target doubles as the desktop implementation). Different: loading (DEX/jar), threading (platform/virtual threads), channels (network listener/local dial-back), lifecycle (app permissions/host process).
