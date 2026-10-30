# Link plugin checklist

## Contract

- [ ] The plugin is a normal plugin; it declares `sessions` in `inject` (or uses `Session::new` in Rust).
- [ ] Admission happens before `sessions.open` / `Session::new`; a rejected connection creates no session.
- [ ] The session layer contains no token, certificate, SSH user, Tailscale identity, Cloudflare Access token, or `peer` parameter.
- [ ] The far end's `Hello` id names remote plugins `<id>:<plugin>`; the link plugin does not assign or rewrite it.
- [ ] One channel = one session; session lifecycle is bound to the channel.
- [ ] Reconnect/backoff lives in the link plugin, not in the session.
- [ ] A listen side opens one session per connection and replaces a same-id session after the new handshake.

## Framing

- [ ] Bytes are framed into messages; length prefix is preferred.
- [ ] Message boundaries and order survive.
- [ ] Maximum message size is enforced; over-limit closes cleanly.
- [ ] Backpressure: a slow receiver does not grow memory without bound.
- [ ] Close / half-close maps to `near.end`; the transport closes exactly once.
- [ ] No unbounded buffering, no hidden retry that replays session messages.

## Lifecycle

- [ ] Far end closes → `near.end` (done/replaced/rejected).
- [ ] Session unloads → adapter connection closes.
- [ ] Link plugin unloads → its sessions unload and connections close.
- [ ] Listen replacement: old remote plugins unload, old channel closes (4002 where the transport has it), new session mounts.
- [ ] Handshake failure does not disturb existing sessions.
- [ ] Errors name the row/connection and the adapter address; no secrets.

## Tests

- [ ] Fake `Far` / `Near`: boundaries, order, max size, close, backpressure.
- [ ] Remote `Hello` id → `<id>:<plugin>`.
- [ ] Listen: multiple connections, same-id replacement.
- [ ] Admission failure before `open`; no session created.
- [ ] Real adapter smoke test with a local/fake service, not a public account.
- [ ] At least one cross-language conformance run if the language SDK provides it.
- [ ] Docs example is copy-paste runnable.
- [ ] `cargo test`, language tests, and `node tools/check-md-links.mjs` (when available) pass.

## Docs

- [ ] `docs/guide/link-plugins.md` and `.en.md` have the adapter section.
- [ ] What to install/configure; admission method; what is not passed to rutis.
- [ ] Normal output and error output, verbatim where possible.
- [ ] Reconnect/backoff and listen replacement.
- [ ] Security boundary: public ingress, relay, DERP, token scope, mTLS, ACL.
- [ ] The adapter is not described as a rutis core dependency.
