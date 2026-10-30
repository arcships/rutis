# feat(link): `link/tailscale` 用 tailnet TCP 做链接层

状态：已发布为 [#952](https://github.com/arcships/rutis/issues/952)。优先级 P1。依赖契约 issue。

## 问题

个人开发者/小团队最常见的跨网络场景是：笔记本、GPU 机器、home lab、手机，分布在不同的 NAT 后面。用户不想开公网端口，也不想自己管证书和 DNS。

Tailscale 提供到达、MagicDNS、NAT 穿透和 ACL。它应该是一个 link plugin 的 adapter，不是 rutis 的 underlay：link plugin 用 tailnet TCP 建连接，做 framing，然后 `sessions.open(far)`。认证/准入由 tailnet ACL 负责；远端 id 仍由 session `Hello` 注册。

## 范围

1. 新增 `link/tailscale` 参考实现（先 TS 或 Rust）：
   - **system 模式**：使用系统 `tailscaled`；link plugin dial/listen tailnet TCP 地址（MagicDNS 名或 `100.x`）；做 framing；每条连接一个 session。
   - **tsnet 模式**（可选，Go）：用 `tailscale.com/tsnet` 在进程内加入 tailnet；`Listen`/`Dial` 直接拿 TCP。
   - **sidecar 模式**（其他语言）：本机跑一个 `tsnet` sidecar，link plugin 连本地回环 TCP。
2. 认证/准入由 Tailscale ACL / tailnet 身份负责；不引入 rutis token；不把 Tailscale 身份传给 session。
3. 远端在 `Hello` 里注册 id；本端命名 `<id>:<plugin>`。
4. 文档：
   - `docs/guide/link-plugins.md` 增加 Tailscale 一节；
   - 写清 system / tsnet / sidecar 三种模式、ACL、MagicDNS、证书/DERP、重连；
   - 明确“Tailscale 不是 link 层，是 adapter”；不要写成“让现有 WebSocket 跑在 Tailscale 上”。
5. 错误输出：Tailscale 未启动、DNS 不解析、ACL 拒绝、DERP 回退、证书/密钥过期，各写一条。

## 非目标

- 不把 Tailscale 变成 rutis 核心依赖。
- 不要求用户安装 Tailscale 才能用 rutis。
- 不把 tailnet 身份映射成 session identity。
- 不做 Tailscale Serve / Funnel 的封装；那是部署/入口，不是 link plugin。
- 不实现 Headscale 控制面；只在文档里写兼容性。

## 验收

- [ ] system 模式：两台 tailnet 机器，一台 dial、一台 listen，跑通 `sessions.open(far)`；远端插件以 `<id>:<plugin>` 出现。
- [ ] tsnet 模式（Go）或 sidecar 模式至少一种跑通，证明不依赖系统 daemon。
- [ ] ACL 拒绝时连接失败，错误清楚；session 不感知 Tailscale。
- [ ] `tailscale status` 能区分 direct / relay；文档写明 DERP 回退的吞吐/延迟风险。
- [ ] 断开/重连/session 卸载行为符合契约；不把重连逻辑放进 session。
- [ ] 文档示例可复制运行；测试不依赖真实 tailnet（unit fake + 手工 e2e 步骤）。
- [ ] `cargo test` / 语言测试 / 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/tcp` 与反向隧道](06-link-tcp-and-reverse-tunnels.md)
- [`link/cloudflared`](05-link-cloudflared.md)
- [连接节点](../../guide/nodes.md)
