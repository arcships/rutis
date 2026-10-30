# 链接层 link plugins：issue 草案

状态：已发布为 [#949](https://github.com/arcships/rutis/issues/949)–[#958](https://github.com/arcships/rutis/issues/958)。本地保留草案用于评审和追溯。Agent 指南：[.agents/skills/rutis-link-plugin/SKILL.md](../../../.agents/skills/rutis-link-plugin/SKILL.md)。

这组 issue 承载“链接层 adapter / link plugin”的契约、跨网络方案分析、场景优先级和后续拓展。设计依据：

- [远程连接：只留一个会话插件](../../design-remote-session-2026-10-28.md)
- [协议与 Channel 解耦](../../design-protocol-channel-decoupling-2026-10-03.md)
- [插件 API：自建连接](../../guide/plugin-api.md#自建连接sessionsopen)
- [连接节点](../../guide/nodes.md)

## 已对齐的结论

- 链接层是**普通插件，任何语言都能写**；不是 Rust trait，也不是 underlay。
- 链接层负责：到达、准入/认证、framing、心跳、重连、listen 顶替。
- 链接层**不负责身份**：远端在 session `Hello` 里注册自己的 id，rutis 信任这个注册。
- rutis 完全信任链接层放进来的连接；session 不做认证，不碰 token、TLS、Tailscale、ACL。
- `sessions.open(far)` 足够：不加 peer 参数，session 生命周期只绑 channel。
- 建议形态：**adapter + bridge**。adapter 是网络工具相关的一层；bridge 是 framing + `sessions.open(far)` + 生命周期，每种语言一份。

## Issue 列表

| 顺序 | Issue | 草案 | 优先级 | 依赖 |
| --- | --- | --- | --- | --- |
| 1 | [#949 链接层 link plugin 契约（adapter + bridge，任意语言）](https://github.com/arcships/rutis/issues/949) | [01](01-link-plugin-contract.md) | P0 | 无 |
| 2 | [#950 跨网络 link 方案全景、场景与优先级](https://github.com/arcships/rutis/issues/950) | [02](02-link-landscape-and-priorities.md) | P0 | 1 |
| 3 | [#951 `link/ssh`：用 SSH 做链接层](https://github.com/arcships/rutis/issues/951) | [03](03-link-ssh.md) | P1 | 1 |
| 4 | [#952 `link/tailscale`：tailnet TCP 做链接层](https://github.com/arcships/rutis/issues/952) | [04](04-link-tailscale.md) | P1 | 1 |
| 5 | [#953 `link/cloudflared`：Cloudflare Tunnel 做链接层](https://github.com/arcships/rutis/issues/953) | [05](05-link-cloudflared.md) | P1 | 1、6 |
| 6 | [#954 `link/tcp` 与反向隧道 wrapper（frp/rathole/ngrok/SSH -R）](https://github.com/arcships/rutis/issues/954) | [06](06-link-tcp-and-reverse-tunnels.md) | P1 | 1 |
| 7 | [#955 `link/grpc`：云控制面 / relay / 分布式云](https://github.com/arcships/rutis/issues/955) | [07](07-link-grpc-cloud.md) | P1（云业务）/ P2 | 1 |
| 8 | [#956 移动设备链路与远控服务（ADB/usbmuxd/WebRTC + screen/input）](https://github.com/arcships/rutis/issues/956) | [08](08-mobile-and-remote-control.md) | P2 | 1、6 |
| 9 | [#957 `link/iroh`：Rust P2P / relay 原型](https://github.com/arcships/rutis/issues/957) | [09](09-link-iroh-and-p2p.md) | P3 | 1 |
| 10 | [#958 链接层插件生态：模板、一致性测试、适配器目录](https://github.com/arcships/rutis/issues/958) | [10](10-link-ecosystem.md) | P2 | 1、3–6 |

## 发布状态

已用 `gh issue create` 发布；本地草案与 GitHub issue 内容一致。发布时 `../../` 文档链接和 issue 之间的草案链接已换成 GitHub URL。新增或修改时，先改本地草案，再用 `gh issue edit <number> --body "$(tail -n +2 <file> | …)"` 同步。
