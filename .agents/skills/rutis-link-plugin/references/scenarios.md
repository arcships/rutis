# Link plugin scenarios and candidates

Use this to choose an adapter, not to decide that every adapter must be built. The user's scenario decides the priority.

## Scenario → adapter

| Scenario | How the user uses it | First choice | Friction |
| --- | --- | --- | --- |
| Personal GPU / remote machine | laptop host uses a Python plugin on a GPU box | SSH / Tailscale | SSH is already there but tunnels/reconnect are manual; Tailscale needs an account/client |
| Phone / device app | app plugins provide camera/sensors/notifications; host on a laptop/server | Cloudflare Tunnel / Tailscale / iroh | Cloudflare has a public ingress; Tailscale takes a VPN slot; iroh needs embedding |
| Home lab / self-hosted | NAS, Pi, old PC; plugin on each | Tailscale / Headscale / frp | Tailscale is easiest; Headscale is self-hosted; frp needs a VPS |
| Team / enterprise | VPC/office rutis; SSO/ACL/audit; no public ports | Tailscale / Cloudflare Access / NetBird / OpenZiti | procurement, compliance, vendor |
| Demo / tutorial | phone connects to laptop in five minutes | Cloudflare quick tunnel / ngrok / SSH | temporary URL, public exposure, rate limits |
| China / VPS | CGNAT home network, one public VPS | frp / rathole / SSH `-R` | VPS, TLS, operations; no mesh |
| Deployment platform | rutis in K8s/Docker/Fly.io/Railway; local or another service connects | platform network integration | platform-specific; ephemeral ports |
| Distributed cloud | hosts connect to a cloud control plane/relay/runtime | gRPC | HTTP/2 proxies, long-lived streams, session affinity |
| Remote control | screen/input/audio of a device | link + remote-control plugin | RDP/VNC are app protocols, not links |
| IoT / outbound-only | device can only connect out; cloud calls its plugins | MQTT / NATS / AWS IoT / Azure Relay | not connection semantics; request/reply and session mapping |
| Browser | web page uses a local device's plugins | WebRTC / WebTransport / WebSocket | signaling, TURN, permissions |
| Local / offline | same machine/process | pipe / memory / Unix socket | none |

## Candidate landscape

| Category | Examples | What it gives | Link plugin shape |
| --- | --- | --- | --- |
| Direct stream | TCP, Unix socket, pipe, WebSocket, QUIC, WebTransport, gRPC, WebRTC data channel, ADB, usbmuxd | bytes or messages | adapter + bridge |
| Overlay | Tailscale/Headscale, ZeroTier, Nebula, NetBird, WireGuard, EasyTier, OpenZiti, Twingate, Netmaker, Innernet, Firezone, Yggdrasil, n2n, tinc, Cloudflare WARP | IP/TCP | adapter connects TCP; admission by overlay |
| Reverse tunnel | Cloudflare Tunnel, ngrok, frp, rathole, SSH `-R`, bore, chisel, sish, zrok, Pangolin, Azure Relay, Tailscale Funnel | public/local TCP | adapter connects TCP |
| P2P library | iroh, libp2p, WebRTC, OpenZiti SDK, Veilid, Hyperswarm | QUIC/data channel | library + framing |
| Cloud link | gRPC control plane/relay, cloud runtime, K8s/service mesh, distributed cloud | bidi stream | `link/grpc` |
| Remote control | RDP, VNC, SPICE, Citrix, PCoIP, TeamViewer, AnyDesk, RustDesk, Sunshine/Moonlight | screen/input/audio | usually an application plugin; RDP DVC / WebRTC data / ADB are the link exceptions |
| Message bus | MQTT, NATS, Redis, Kafka, RabbitMQ, PubNub, Ably, AWS IoT, Azure Service Bus, Google Pub/Sub | messages/topics | request/reply + session mapping |
| Local/physical | pipe, Unix socket, fd, Bluetooth, serial, USB, NFC, LoRa | bytes/messages | OS API + framing |

## Priority

**T1: SSH, Tailscale, Cloudflare Tunnel.**

- SSH is the zero-friction “I already have it” path.
- Tailscale is the private-network / NAT / ACL path.
- Cloudflare Tunnel is the “no client install / public or controlled ingress” path.

**T2: frp/rathole, K8s/Docker/Fly.io/Railway, Headscale/NetBird, gRPC.**

- frp/rathole: self-hosted, VPS, cost-sensitive, China.
- K8s/Docker/Fly.io/Railway: deployment platforms are distribution.
- Headscale/NetBird: self-hosted control plane / data sovereignty.
- gRPC: T1 if the business is a cloud control plane/relay; otherwise T2/T3.

**T3: iroh/WebRTC/libp2p, MQTT/NATS, enterprise zero-trust, remote control.**

- iroh/WebRTC/libp2p: consumer apps, browser, no system VPN; technical brand and Rust ecosystem.
- MQTT/NATS/AWS IoT/Azure Relay: IoT/outbound-only devices; later.
- OpenZiti/Twingate/Zscaler/Teleport: enterprise zero-trust; longer sales.
- RDP/VNC/SPICE: app plugins/services, not link layer.

## Rules of thumb

- If it gives **IP**, put TCP + framing inside the link plugin.
- If it gives **bytes**, frame them; prefer length prefix.
- If it gives **messages**, pass them through.
- If it is **screen/input/audio**, it is an application plugin; link only carries it.
- If it needs a **reachable cloud endpoint**, it is a cloud link, not a device link.
- If it needs a **VPN client on every device**, it is an adapter, not a core dependency.
