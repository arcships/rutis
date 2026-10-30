# feat(link): `link/iroh` Rust P2P / relay 原型

状态：已发布为 [#957](https://github.com/arcships/rutis/issues/957)。优先级 P3。依赖契约 issue。

## 问题

如果用户不能/不愿装系统 VPN，又不想过 Cloudflare/ngrok 这类中心入口，就需要一个可嵌入的 P2P/relay 传输。iroh 是 Rust 的 QUIC + NAT 穿透 + relay 库，按公钥拨号，端到端加密，有 FFI；和 rutis 的 Rust 核心、移动端/嵌入式场景契合。

但它还早：NAT 穿透在推进，直连不稳会回退 DERP，DNS/证书能力也要看版本。所以先做原型，不承诺生产。

## 范围

1. `link/iroh` 原型（Rust）：
   - `Endpoint` + `ALPN`；
   - 一条 QUIC 双向流对应一个 session；
   - 长度前缀 framing；
   - `sessions.open(far)` / `Session::new(channel)`；
   - 准入用 endpoint 公钥/应用层 token；不把 iroh 身份传给 session；
   - 远端在 `Hello` 注册 id。
2. 连接形态：
   - 直接 P2P（能打洞时）；
   - relay 回退（不能打洞时）；
   - 公网 relay / 自建 relay 的配置和风险。
3. 文档：
   - `docs/guide/link-plugins.md` 增加 iroh/P2P 一节；
   - 写清 iroh 的成熟度、relay、NAT、移动端 FFI、依赖体积；
   - 明确“iroh 是 link transport，不是 session 层”。
4. 可选：libp2p / WebRTC 的对比只写文档，不实现。

## 非目标

- 不把 iroh 变成核心依赖。
- 不在 iroh 里做 rutis token/身份映射。
- 不承诺生产可用；先原型和 benchmark。
- 不实现 relay 服务端（先用公共/自建测试 relay）。

## 验收

- [ ] 两个 iroh endpoint 在 local/relay 环境跑通一条 QUIC 流，消息边界/顺序正确。
- [ ] `sessions.open` / `Session::new` 跑通，远端插件 `<id>:<plugin>` 出现。
- [ ] 断开/重连/session 卸载行为符合契约；重连在 link plugin。
- [ ] 文档写清直连/relay、NAT、延迟/吞吐、移动端 FFI、依赖体积。
- [ ] 至少一个手工 e2e 步骤；不依赖真实 n0 账号/生产 relay。
- [ ] `cargo test` / 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/tcp` 与反向隧道](06-link-tcp-and-reverse-tunnels.md)
- [`link/grpc`](07-link-grpc-cloud.md)
