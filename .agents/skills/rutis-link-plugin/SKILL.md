---
name: rutis-link-plugin
description: Use when extending the rutis link layer — adding a connection plugin or adapter for SSH, Tailscale, Cloudflare Tunnel, gRPC, ADB/usbmuxd, iroh/libp2p/WebRTC, frp/rathole, a cloud relay, or any other way of reaching a far end. Covers the link-layer contract, adapter vs bridge, admission vs identity, framing, sessions.open(far), lifecycle, tests, and the anti-patterns that would couple authentication or identity into the session layer. Any language; the core does not change.
---

# Adding a link plugin to rutis

rutis (https://github.com/arcships/rutis) is a plugin host with a Rust core and plugins in several languages. A **link plugin** (also called a connection plugin) is how a rutis host reaches a far end: a language runtime, another rutis, a device app, a cloud service.

This skill is self-contained; reference material is in `references/`:

| File | Contents |
| --- | --- |
| `references/link-plugin-api.md` | `sessions.open(far)` / `Session::new(channel)`, `Far` / `Near`, close, lifecycle |
| `references/scenarios.md` | Which adapters fit which user scenario; candidate landscape and priority |
| `references/checklist.md` | Test matrix and review checklist |

Keep one sentence in mind: **the link layer owns admission, reachability, framing and lifecycle; it does not own identity.** The far end registers its id in the session `Hello`; rutis trusts that registration. The session layer has no credentials and no authentication.

## 1. Decide whether it is a link plugin

A link plugin produces a **message channel** to a far end. It is not:

- an application plugin that happens to use the network (a remote-control plugin, a screen/input service, an HTTP client);
- a session-layer feature (remote plugin naming, services, events, lifecycle);
- a core trait. 0.19 removed `Transport`, `LinkPlugin` and `ChannelInfo`; do not bring them back. The extension point is a normal plugin calling `sessions.open(far)` (or Rust `Session::new(channel)`).

Ask: **does this thing give a bidirectional, reliable, ordered message stream (or a byte stream I can frame), and is it responsible for admitting the connection?** If yes, it can be a link plugin. If it gives IP packets, put TCP (and framing) inside the plugin. If it is a remote-control protocol (RDP/VNC/SPICE/RustDesk), it is usually an application plugin, not a link plugin.

## 2. Choose the adapter shape

A link plugin has two parts; keep them separate:

```text
link plugin (any language)
  ├─ adapter: SSH / Tailscale / Cloudflare / gRPC / ADB / WebRTC / frp / iroh / …
  │    reachability + admission, gives a stream
  └─ bridge: framing + sessions.open(far) + lifecycle
```

| Adapter shape | Examples | What the adapter gives | What the bridge does |
| --- | --- | --- | --- |
| Direct stream | TCP, Unix socket, pipe, WebSocket, QUIC, WebTransport | bytes or messages | frame if bytes; `sessions.open` |
| Overlay network | Tailscale/Headscale, ZeroTier, Nebula, NetBird, WireGuard, EasyTier, OpenZiti | TCP over the overlay | frame; `sessions.open` |
| Reverse tunnel | SSH `-L`/`-R`, Cloudflare Tunnel, frp, rathole, ngrok, bore, chisel | local or public TCP | frame; `sessions.open` |
| P2P library | iroh, libp2p, WebRTC data channel | QUIC stream / data channel | frame if bytes; `sessions.open` |
| Cloud link | gRPC bidi stream, cloud relay | messages | `sessions.open` per stream |
| Mobile device | ADB `forward`/`reverse`, usbmuxd/`iproxy` | local TCP | frame; `sessions.open` |

Any language works. Pick the cheapest implementation that fits the target language:

- shell out to the vendor CLI and use stdio or a loopback TCP port;
- use the vendor SDK/library;
- run a small sidecar (for example a Go `tsnet` sidecar) and connect to it over loopback.

The bridge is shared; write it once per language. Do not reimplement framing and lifecycle in every adapter.

## 3. Admission is the adapter's job

Decide how the adapter admits a connection **before** any session exists:

| Adapter | Typical admission |
| --- | --- |
| SSH | host key + user key / agent / `authorized_keys` |
| Tailscale | tailnet ACL / device identity |
| Cloudflare Tunnel | Cloudflare Access service token / WARP identity |
| gRPC | mTLS, OAuth/JWT, API key |
| ADB | ADB key + device authorization |
| usbmuxd | iOS trust / pairing |
| frp/rathole/ngrok | tunnel token / TLS / provider auth |
| iroh/libp2p/WebRTC | endpoint key / DTLS identity / app token |

If admission fails, close the connection and do not call `sessions.open`. Never pass a token, certificate, Tailscale identity or SSH user into the session layer.

**Identity is not admission.** After the connection is admitted, the far end says who it is in the session `Hello` (`endpoint`). rutis uses that id to name remote plugins (`<id>:<plugin>`) and trusts it. Do not add a `peer` parameter to `sessions.open`, and do not try to pass the adapter's identity into session naming.

## 4. Normalize the stream to messages

`Channel` / `Near` carries messages with boundaries. If the adapter gives bytes, frame them in the bridge:

- length-prefixed framing is preferred; newline framing is acceptable when the payload cannot contain a raw newline (the session JSON codec escapes them);
- set and enforce a maximum message size (the WebSocket transport defaults to 16 MiB);
- preserve order and backpressure; do not buffer without bound;
- map close / half-close to `near.end` and close the far end exactly once;
- do not parse or rewrite the session protocol; the bridge is transport, not a second session layer.

## 5. Mount the session and bind its lifecycle to the channel

Rust:

```rust
let channel = adapter.connect(&config).await?;
let session = ctx.plugin(Session::new(channel));
// The session ends when the channel ends; reconnect is this plugin's decision.
```

Other languages:

```ts
const near = ctx.sessions.open({
  send: message => connection.send(message),
  close: why => connection.close(why),
})
connection.onMessage(message => near.receive(message))
connection.onClose(() => near.end({ kind: 'done' }))
ctx.effect(() => () => connection.close({ kind: 'done' }))
```

- One channel = one session. If the far end has several connections, open one session per connection.
- A listen side accepts connections, opens one session per connection, and uses the far end's `Hello` id for replacement. A newer connection with the same id unloads the old session's remote plugins, then replaces it.
- `Channel` closes → session unloads; session unloads → close the channel. Do not create a second lifecycle for identity or authentication.
- Reconnect/backoff lives in the link plugin, not in the session. The session never reconnects.

## 6. Test the contract, not the vendor

Use a fake `Far` / `Near` or a local echo for the bridge:

- [ ] message boundaries and order survive;
- [ ] maximum message is enforced; over-limit closes cleanly;
- [ ] backpressure: a slow receiver does not grow memory without bound;
- [ ] close: far end closes → `near.end`; near end ends → far end closes;
- [ ] remote `Hello` id → `<id>:<plugin>` naming;
- [ ] listen: multiple connections; same id replacement;
- [ ] admission failure happens before `open`; no session is created;
- [ ] session unload does not leave the adapter connection open;
- [ ] no token/credential appears in session code or output.

Then test the real adapter separately (SSH/Tailscale/Cloudflare/etc.) with a local or fake service; do not make CI depend on a public account.

## 7. Document what the user sees

Write a short section in `docs/guide/link-plugins.md` (and the `.en.md` counterpart):

- what the adapter needs installed/configured (SSH server, Tailscale, `cloudflared`, ADB, relay);
- admission method and what is *not* passed to rutis;
- how to configure the plugin (address, mode, credentials source);
- normal output and error output, verbatim where possible;
- reconnect/backoff and listen replacement;
- security boundary (public ingress? relay? DERP? token scope?).

## Anti-patterns

- ❌ Adding `peer`, `token`, `credential` or `identity` to `sessions.open`.
- ❌ Making the session layer authenticate, or know about Tailscale/SSH/Cloudflare.
- ❌ Reintroducing `Transport`, `ChannelInfo.peer`, `Identity` / `Admission` in the core.
- ❌ Making the link plugin Rust-only; it is a normal plugin, any language.
- ❌ Treating RDP/VNC/SPICE as a generic link; they are application protocols. Use RDP DVC / WebRTC data channel / ADB only as the specific exception.
- ❌ Putting reconnect, listen replacement or transport errors into the session.
- ❌ Hard-coding a vendor as a rutis dependency; keep it an adapter.

## References

- `references/link-plugin-api.md` — `sessions.open`, `Session::new`, `Far`/`Near`, close.
- `references/scenarios.md` — scenario → adapter choice; candidate landscape; T1/T2/T3.
- `references/checklist.md` — test and review checklist.
- Repository: `docs/guide/plugin-api.md` (your own connection), `docs/design-remote-session-2026-10-28.md`, `docs/design-protocol-channel-decoupling-2026-10-03.md`, `crates/rutis-bridge/src/channel/mod.rs`, `crates/rutis-bridge/src/session/plugin/mod.rs`.
