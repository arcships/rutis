# feat(link): `link/cloudflared` 用 Cloudflare Tunnel 做链接层

状态：已发布为 [#953](https://github.com/arcships/rutis/issues/953)。优先级 P1。依赖契约 issue 与 `link/tcp`。

## 问题

很多场景不能在拨号侧装 VPN 客户端：手机、浏览器、临时机器、第三方。监听侧又常在 NAT 后，不能开公网端口。Cloudflare Tunnel 的“监听侧出站、拨号侧普通 HTTPS/WSS”正好覆盖这个场景。

它应该是 link plugin 的 adapter：Cloudflare 负责到达（和可选的 Access 准入），link plugin 做 framing 和 `sessions.open(far)`；rutis 不需要知道 Cloudflare，也不需要 token 之外的第二层认证。

## 范围

1. 新增 `link/cloudflared` 参考实现，先做 **access 模式**：
   - 拨号侧跑 `cloudflared access tcp --hostname <host> --url 127.0.0.1:<port>`；
   - link plugin 连本地回环 TCP，走通用 `link/tcp` bridge；
   - 准入由 Cloudflare Access 服务令牌/身份负责；不把 Access 身份传给 session。
2. 再做 **public 模式**（可选）：
   - 监听侧跑 `cloudflared tunnel`；
   - link plugin 连 `wss://<host>` 或 HTTPS；
   - 准入由 Cloudflare Access 或链接层自己的 token 负责；
   - 明确公开入口的风险。
3. 文档：
   - `docs/guide/link-plugins.md` 增加 Cloudflare 一节；
   - 写清 access / public / WARP 三种形态、Access 服务令牌、长连接限制、错误输出；
   - 明确“Cloudflare 是反向隧道，不是 mesh；不是 P2P”。
4. 错误输出：`cloudflared` 未启动、Access 拒绝、隧道断开、WebSocket/HTTP2 代理超时、公开入口配置错误，各写一条。
5. 验收测试用本地 fake tunnel / 本地 `cloudflared access` 能覆盖的部分，不依赖真实 Cloudflare 账号。

## 非目标

- 不实现 Cloudflare Tunnel 服务端。
- 不把 Cloudflare 变成核心依赖。
- 不把 Access 身份映射成 session identity。
- 不把 Cloudflare Tunnel 的 HTTP 反代当成 WebSocket link 的唯一方案；access TCP 更通用。
- 不做 Cloudflare Workers/Pages 集成。

## 验收

- [ ] access 模式：本地 `cloudflared access tcp` + `link/tcp` 跑通 session；远端插件以 `<id>:<plugin>` 出现。
- [ ] public 模式：`wss://` 连上并跑通；Access 拒绝时错误清楚。
- [ ] 长连接断开/重连/session 卸载行为符合契约；重连在 link plugin。
- [ ] 文档写明公开入口的安全边界（token/Access、不要裸露 rutis）。
- [ ] 至少一个 Linux/macOS 手工 e2e 步骤；不依赖 CI 里的 Cloudflare 账号。
- [ ] `cargo test` / 语言测试 / 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/tcp` 与反向隧道](06-link-tcp-and-reverse-tunnels.md)
- [`link/tailscale`](04-link-tailscale.md)
- [连接节点](../../guide/nodes.md)
