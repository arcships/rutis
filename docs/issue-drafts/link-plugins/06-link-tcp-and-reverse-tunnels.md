# feat(link): `link/tcp` 通用字节流 bridge 与反向隧道 wrapper

状态：已发布为 [#954](https://github.com/arcships/rutis/issues/954)。优先级 P1。依赖契约 issue。

## 问题

大量网络工具最终只给一条 TCP 字节流：SSH 转发、Tailscale、WireGuard、ZeroTier、Nebula、NetBird、frp、rathole、ngrok、Cloudflare access。现在每个 link plugin 都要自己处理 framing、最大消息、close、背压，重复且容易做错。

需要一层通用 `link/tcp` bridge：把字节流按消息边界接成 `Channel`，再 `sessions.open(far)`。反向隧道只做“到达到本地/公网 TCP”的 adapter，不重复 session 逻辑。

## 范围

1. 新增 `link/tcp` bridge（任意语言优先 TS/Rust）：
   - 长度前缀 framing（优先）或换行 framing；
   - 最大消息（对齐 WebSocket 默认 16 MiB）、超长关闭；
   - close / half-close 语义；
   - 背压：发送阻塞、接收队列；
   - 每条连接一个 session；
   - listen 侧 accept 多条连接、按远端 `Hello` id 顶替。
2. 明确 `link/tcp` **不做认证**；认证在它下面的 adapter（SSH/Tailscale/frp/Cloudflare）或上面的链接层插件。
3. 反向隧道 wrapper（可以是薄脚本/配置，不一定是新 crate）：
   - frp / rathole：`frpc`/`rathole` 把本地 TCP 暴露到 VPS；`link/tcp` 连本地或 VPS；
   - ngrok：`ngrok tcp` 给公网端点；
   - SSH `-R`：反向端口转发；
   - 文档写清各自的准入（token/TLS/SSH）和公网风险。
4. 文档：`docs/guide/link-plugins.md` 增加“字节流 bridge”和“反向隧道”两节。

## 非目标

- 不把 `link/tcp` 做成新的核心 trait。
- 不做可靠 UDP/QUIC；那是 `link/quic` 或 P2P 的事。
- 不实现 frp/rathole/ngrok 服务端。
- 不在 bridge 里做 token/身份；身份来自远端 `Hello`。

## 验收

- [ ] 本地 TCP echo：消息边界、顺序、16 MiB 上限、超长关闭、half-close 全部符合契约。
- [ ] listen 侧多连接各自 session；同 id 新连接顶替旧连接。
- [ ] frp 或 rathole 任一跑通（本地 VPS 或 loopback 模拟），`link/tcp` 不需要改。
- [ ] SSH `-R` 或 ngrok 至少一个跑通，文档写清准入和公网风险。
- [ ] 断开/重连/session 卸载行为符合契约；重连在 link plugin。
- [ ] 测试不依赖公网服务；`cargo test` / 语言测试 / 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/ssh`](03-link-ssh.md)
- [`link/tailscale`](04-link-tailscale.md)
- [`link/cloudflared`](05-link-cloudflared.md)
- [`link/grpc`](07-link-grpc-cloud.md)
