# research(link): 跨网络 link 方案全景、场景与优先级

状态：已发布为 [#950](https://github.com/arcships/rutis/issues/950)。优先级 P0。依赖契约 issue。

## 问题

“链接层可以用 Tailscale / SSH / Cloudflare / gRPC / 远控协议……”这类讨论容易变成协议清单，但真正决定做哪个的是场景：

- 用户是把插件放到另一台自己的机器，还是把手机/设备接进来？
- 宿主在 NAT 后，还是在云上/部署平台？
- 用户能装 VPN 客户端，还是只能开一个公开入口？
- 是个人开发者、家庭实验室、团队/企业，还是设备厂商？

没有场景分析，就会把“协议最多”当成“最值得做”。

## 范围

1. 在 `docs/guide/link-plugins.md` 增加“方案全景与场景”一节（或单独 `docs/research-link-landscape.md`），至少覆盖：
   - 直连流：SSH、TCP、WebSocket、QUIC、WebTransport、gRPC、WebRTC data channel、ADB、usbmuxd、pipe/Unix。
   - 覆盖网络：Tailscale/Headscale、ZeroTier、Nebula、NetBird、WireGuard、EasyTier、OpenZiti、Twingate、Netmaker、Innernet、Firezone、Yggdrasil、n2n、tinc、Cloudflare WARP。
   - 反向隧道：Cloudflare Tunnel、ngrok、frp、rathole、SSH `-R`、bore、chisel、sish、zrok、Pangolin、Azure Relay、Tailscale Funnel。
   - P2P：iroh、libp2p、WebRTC、OpenZiti SDK、Veilid、Hyperswarm。
   - 云链接：gRPC 控制面/relay、云运行时、K8s/service mesh、分布式云。
   - 远控：RDP、VNC、SPICE、Citrix、PCoIP、TeamViewer、AnyDesk、RustDesk、Sunshine/Moonlight；说明大多数是应用协议，不是 link。
   - 消息总线：MQTT、NATS、Redis、Kafka、RabbitMQ、PubNub、Ably、AWS IoT、Azure Service Bus、Google Pub/Sub。
   - 本地/物理：pipe、Unix socket、fd、蓝牙、串口、USB、NFC、LoRa。
2. 场景表：每个场景写“用户怎么用 / 首选 / 摩擦”，至少覆盖：
   - 个人 GPU/远程机器、手机/设备 App、home lab/自建、团队/企业、demo/教程、中国/VPS、部署平台、分布式云、远控、IoT。
3. 优先级：T1（SSH、Tailscale、Cloudflare Tunnel）、T2（frp/rathole、K8s/Docker/Fly.io/Railway、Headscale/NetBird、gRPC）、T3（iroh/WebRTC、MQTT/NATS、企业零信任、远控）。
4. 业务/宣发/厂商/生态四个维度各写一段：为什么这个顺序，不做什么，什么条件下重排。
5. 不写实现；只产出可评审的分析和后续 issue 拆分建议。

## 验收

- [ ] 每个候选都写清“给什么”（字节流/消息流/IP/公网入口/屏幕输入）和“怎么接 link plugin”。
- [ ] 每个场景有“用户怎么用”的具体步骤，不只写技术特性。
- [ ] T1/T2/T3 的理由能对应到场景和业务/宣发/合作/生态，而不是“协议流行度”。
- [ ] 明确写出“远控协议大多不是 link”“gRPC 是云链接不是设备链接”“消息总线不是连接语义”。
- [ ] 分析结论能反推出 3–5 个后续 issue，并与本目录的 issue 列表一致。
- [ ] 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [远程连接：只留一个会话插件](../../design-remote-session-2026-10-28.md)
- [连接节点](../../guide/nodes.md)
