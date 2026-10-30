# Link plugin API

This file is the part of the rutis plugin API a link plugin needs. It is copied from the 0.19 API; the repository is authoritative.

## The contract

A link plugin gets a bidirectional, ordered, reliable stream of messages. It admits the connection (its own authentication), frames bytes into messages if needed, and hands the stream to the session layer.

```text
link plugin (any language)
  admission / reachability / framing / lifecycle
        │
        │ trusted far (messages in, messages out)
        ▼
session: open(far) → Near
  session lifecycle == channel lifecycle
  far end registers its id in Hello
```

- The link plugin owns **admission** and **transport**.
- The far end owns **its id** (session `Hello`).
- The session owns **plugins, services, events, lifecycle**.
- The session has no credentials and no authentication.

## Rust

```rust
use rutis_bridge::Session;

let channel = adapter.connect(&config).await?; // Channel
let session = ctx.plugin(Session::new(channel));
```

`Session::new(channel)` handshakes and mounts in one plugin. If you need the far end's id before mounting, use:

```rust
let greeted: Greeted = rutis_bridge::Session::handshake(&ctx, channel).await?;
println!("{}", greeted.id());
let session = ctx.plugin(greeted);
```

The host provides `LocalId` at the root context (`rutis-host` does this from `rutis.json`'s `id`). There is no default: a session without one fails with `the host provides no id for its sessions`.

`Channel` is the 0.19 transport contract: sender, receiver, closer; ordered, reliable, keeps message boundaries. `pipe::framed(read, write, closer)` can frame a byte stream as newline-delimited messages (16 MiB limit, no raw newline in a message). For a new adapter, a length-prefixed framing is preferred.

## Any language

The session plugin `rutis-bridge/session` provides the service `sessions`:

```ts
type Close = { kind: 'done' } | { kind: 'replaced' } | { kind: 'rejected'; reason: string }
interface Far  { send(message: Uint8Array): void; close(why: Close): void }
interface Near { receive(message: Uint8Array): Promise<void>; end(why: Close): void }

sessions.open(far: Far): Near
```

- Declare `sessions` in `inject`; without it, `open` fails with `declare sessions in inject`.
- The session is mounted under the plugin that calls `open`: when that plugin unloads, its sessions and their remote plugins unload.
- `receive` completes when the message is in the session's receive queue; the queue is 64 messages, and a full queue waits, carrying backpressure to the connection.
- The far end's plugins here are called `<far id>:<plugin>`; to make the far end run a plugin, write a row `<connection row>:<plugin>` (for example `relay:contacts`). While the far end is absent, that row waits.
- There is no `peer`, `token`, `credential` or `identity` parameter. Do not add one.

## Close semantics

| Where | What to do |
| --- | --- |
| Far end closed normally | `near.end({ kind: 'done' })` |
| Far end replaced by a newer connection of the same id | `near.end({ kind: 'replaced' })` |
| Far end refused after the channel was established | `near.end({ kind: 'rejected', reason })` |
| The session unloads | close the adapter connection once |
| The link plugin unmounts | close the adapter connection; its sessions unload |

Rust `Channel` has `Close::Done`, `Close::Replaced`, `Close::Rejected(reason)`; map them to the transport's close code where it has one (WebSocket: 1000, 4002, 4003).

## Lifecycle rules

- One channel = one session. A listen side opens one session per accepted connection.
- `Channel` closes → session unloads; session unloads → channel closes. Do not create a second lifecycle.
- Reconnect/backoff belongs to the link plugin. The session never reconnects.
- `rutis-host/listen` uses the far end's `Hello` id for replacement: a newer connection with the same id unloads the old session's remote plugins, closes the old connection with 4002, then mounts the new session.
- A handshake failure on a listen side does not print; the dialer writes the reason.

## What not to put in the session

- token, certificate, SSH user, Tailscale identity, Cloudflare Access token;
- a `peer` / `expected_id` parameter;
- a transport type check or a vendor name;
- reconnect/backoff;
- framing/encoding of the transport.

If you need any of these in the session, the boundary is wrong: put it in the link plugin.
