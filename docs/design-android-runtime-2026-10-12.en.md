# The Android Runtime (Kotlin Remote Node)

[中文](design-android-runtime-2026-10-12.md)

Status: design draft, not implemented. Date: 2026-10-12. Baseline: `main` `ebf0711` (0.9.1).
Based on: [the multilingual design](design-multilang-runtimes-2026-10-03.en.md), [the remote plugins design](design-remote-plugins-2026-10-03.en.md) §4.4, [the Go runtime](design-go-runtime-2026-10-10.en.md) (the G2 remote precedent), [the plugin API](guide/plugin-api.en.md). The desktop-JVM version is a separate draft ([the Kotlin runtime](design-kotlin-runtime-2026-10-12.en.md)); the two share one KMP SDK.

Positioning: the host and its agents run on a desktop or server and reach the Android ecosystem and apps through a helper APK on the device — the slot the overall design gave Swift/M3 ("system capabilities exposed through a helper with its own permission identity").

Out of scope: the Rust core embedded in an app (topology B; the increment in §10); cloud relay and device dial-out; USB/ADB; Play compliance.

## 1. Decisions

| # | Decision |
| --- | --- |
| D1 | Topology A: a helper APK on the device joins as a **remote runtime**; rows `droid:<plugin>` (default name, configurable); several devices are several named remote runtimes |
| D2 | The device side is pure Kotlin, no Rust on the device: a session layer (endpoint protocol 3) + a ws listener + the leaf SDK |
| D3 | Direction unchanged: the device only listens, the host dials (the remote design's rule) |
| D4 | **TLS relaxed** (an explicit deviation from Python/Bun's "loopback only"): `ws://` is allowed but accepts private-range sources only (RFC 1918 / ULA / link-local), per connection, with a mandatory token; `wss://` uses a self-signed certificate with fingerprint pinning |
| D5 | Plugins are DEX: built-ins compiled into the APK (loaded by the app classloader); dev ones uploaded as a zip (`classes.dex` + `rutis.json`) through an authenticated endpoint → `DexClassLoader`; zero protocol changes (the entry resolves on the device) |
| D6 | Bounded hot reload: ART never unloads classes, so 50 reloads per plugin; when spent, `dispose` → self-restart → host lease takeover (an ordinary restart from the host's view) |
| D7 | Permissions are lifecycle: not granted → the load fails naming the permission; revoked at run time → services withdrawn (rutis revocation stops the consumers); enablement guidance flows through a `droid:system` service |
| D8 | Disconnects are only translated: a break = stop (cleanups in order), reconnection = start again (`apply` re-runs, dependency-order reload); plugins never see a "disconnect"; persistence is the plugin's business, the runtime offers no storage contract |
| D9 | Threading: one platform thread per incoming call (the Go shape; ART has no virtual threads); capabilities `sync-wait` + `reentrant-sync`, no `sync-stack`/`objects` |
| D10 | The SDK is a KMP library (android + jvm) depending on kotlin-stdlib only; no runtime reflection (KSP generation at compile time; explicit `Plugin.schema` before that) |
| D11 | One host-side change: the remote language mapping gains `"kotlin"|"kt"`; everything else exists |
| D12 | Conformance is shaken out on the desktop jvm target first, then the emulator; the desktop-JVM runtime plugs in after A4 |

## 2. Model

```text
desktop / server                          Android device
rutis host (Rust)                         rutis-droid APK (foreground service)
 ├─ local rows: node/py/bun/go/dylib       ├─ session layer (endpoint protocol 3) + ws listener
 ├─ RuntimePlugin::remote("droid")         ├─ capability plugins (DEX): contacts / notifications /
 └─ rows droid:contacts / droid:apps ...   │   Intent / sensors / app bridges
    (a peer row dialing the device         └─ permission bridge (grant state ↔ row lifecycle)
     provides the session)
```

- Device process death: this runtime's rows stop; reconnection is a new runtime generation whose rows reload in dependency order (existing lease/takeover semantics, proven by Go G2).
- Plugins on one device call each other on the object itself; across devices and languages, by name through the host.

## 3. The device side

```text
APK (foreground service, START_STICKY)
 ├─ listens on :7443 /rutis, subprotocol rutis.3; PUT /rutis/plugin/<name> uploads (same token)
 ├─ token: generated on first run, constant-time comparison; refusal codes 404/401/403/400
 ├─ heartbeat 30 s (RUTIS_HEARTBEAT); 16 MiB per frame, over it 1009; new connection takes over (old gets 4002)
 └─ the notification shows the current LAN address; the listener rebinds on network changes
```

- Battery: a foreground service plus battery-optimization-whitelist guidance (the standard intent); OEM ROM "autostart / unrestricted background" settings can only be documented, not set in code.
- Deep Doze is documented honestly: charging / screen-on / whitelisted is most reliable; deep sleep shows as a disconnect that recovers automatically on wake.

### 3.1 Disconnects

The runtime only translates: the heartbeat detects the break (the only way to find a half-open connection) → the device cleans up the session's plugins (cleanups in reverse, time-limited) → the host's rows cascade to a stop → the host link redials with backoff (existing behaviour) → after reconnecting everything reloads automatically; the configuration lives host-side and survives. A killed process returns via `START_STICKY`; the two ends always meet again. Whether to persist state is the plugin's business: it runs in the app with Android's storage APIs.

## 4. Plugins

- Entry resolution as [the JVM draft](design-kotlin-runtime-2026-10-12.en.md) §8.2 (a dot → a fully-qualified class name; no dot → matched by `rutis.json`); a descriptor disagreeing with the loaded declaration loses to it.
- Uploading a new version spends one unit of budget; identical content is skipped (hash).
- The dev loop: the host does not compile; the project's own `gradle build` → upload → row reload (as Go D15).

## 5. Permissions are lifecycle

| Event | Behaviour |
| --- | --- |
| not granted at `rows.load` | the load fails, naming the permission |
| revoked at run time | the plugin's services are withdrawn → the row stops → the consumers cascade |
| Accessibility / NotificationListener not enabled | the rows needing them do not start; `droid:system` reports status and deep-links into settings |

Granting happens in the device UI and the prompt names the helper APK (the Android counterpart of Swift/M3's acceptance item); revocation has no broadcast — check before loading plus a 30 s reconciliation pass.

## 6. Threading and session

One platform thread per incoming call; an outgoing synchronous call blocks its own thread on a `CompletableFuture`; suspend methods bridge with `runBlocking` on the call's own thread; `ctx.launch` runs on the row's coroutine scope, cancelled and joined at unload; cancellation = coroutine cancel + thread interrupt; an uncaught exception in a plugin thread ends the process and stops the rows. Method shapes, value rules, and `PLUGIN_API=1` are as [the JVM draft](design-kotlin-runtime-2026-10-12.en.md) §7.

## 7. SDK and packaging

- `io.github.arcships:rutis` (KMP: android + jvm), the plugin author's only dependency; kotlin-stdlib only.
- The device runtime is an AAR `io.github.arcships:rutis-droid` (embeddable in any app) plus a helper APK embedding it; Android 10 (API 29) and up, arm64-v8a and x86_64.
- Repository layout `kotlin/`: `sdk`, `droid`, `app`, `tools` (KSP); released to Maven Central, the APK via GitHub Releases (sideload); versions in `scripts/train.mjs`.
- WS server: NanoHTTPD+websockets or Ktor CIO (decided in A2), text frames only.

## 8. The host side

One change: the remote language mapping in `rutis-host/src/host.rs` gains `"kotlin"|"kt"` → `RuntimeResolver::modules`.

```json
{
  "runtimes": { "remote": [{ "name": "droid", "language": "kotlin" }] },
  "rows": [
    { "id": "p", "name": "rutis-bridge/peer",
      "config": { "peer": "droid", "dial": "ws://192.168.1.42:7443/rutis" } },
    { "id": "contacts", "name": "droid:contacts" }
  ]
}
```

Token via `RUTIS_TOKEN_DROID`; with `wss`, `RUTIS_CERT` pins the device certificate; the `mount` reply carries `implementation`/`engine` (kotlin + ART versions), printed by `check`.

## 9. Tests and phases

- Session unit tests (jvm target, fast) → conformance: `runtime_conformance.rs` runs the jvm target first, instrumented emulator tests run ART; `websocket_conformance.rs` gains the Kotlin far end; `multilang.rs` gains a column.
- Device-specific tests: permission ↔ row lifecycle, reload-budget exhaustion and self-restart, reconnection after process death, the upload endpoint.
- CI: a JDK job (jvm target) plus an emulator job; Gradle builds run first.

| Phase | Work | Acceptance |
| --- | --- | --- |
| A1 | KMP SDK skeleton + session layer (jvm target) | passes `session::testing::session` on the desktop |
| A2 | android target + listener/token/TLS rule | the host connects to the emulator over ws, `websocket_conformance` passes; non-private sources refused |
| A3 | plugin framework + permission bridge + helper APK + first capabilities (contacts, notifications, Intent) | real device: permission prompts name the APK; revocation stops consumers; budget exhaustion self-recovers |
| A4 | dev loop (KSP, upload, `new --lang kotlin`, `check`) | edit → upload → reload works end to end |
| A5 | relay/dial-out, topology-B evaluation, further capabilities | separate designs |

## 10. Open questions

- Doze reliability varies by vendor; document honestly, consider event buffering.
- Upload-endpoint abuse surface: the token already authenticates; public exposure demands wss and stronger pairing.
- Revocation reconciliation: a 30 s pass versus checks at key operations.
- The topology-B increment: all of A's Kotlin layer is reused (the session layer runs over the existing `transport::memory`); the increment is the core's NDK cross-compilation + JNI bootstrap + the `.so` into an AAR + Play DCL disclosure — start only with a real on-device-host need.
