# feat(link): 定义链接层 link plugin 契约（adapter + bridge，任意语言）

状态：已发布为 [#949](https://github.com/arcships/rutis/issues/949)。优先级 P0。

## 问题

0.19 把链接层和 remote 插件拆开之后，链接层的扩展点是“写一个连接插件，拿到一条能收发消息的连接，交给 `sessions`”。但文档和示例只覆盖了 WebSocket / pipe / memory，没有把下面这些事写成一条可执行的契约：

- 链接层插件是普通插件，任何语言都能写；不是 Rust 专属 trait。
- 链接层负责到达、准入/认证、framing、心跳、重连、listen 顶替；不负责身份。
- 远端在 session `Hello` 里注册自己的 id；rutis 信任这个注册。
- session 不做认证，不碰 token、TLS、Tailscale、ACL；`sessions.open(far)` 就够，不加 peer 参数。
- adapter（网络工具相关）和 bridge（framing + `sessions.open` + 生命周期）怎么分。

没有这条契约，每个语言的 link plugin 都会重新决定认证、身份、framing、重连，最后又把职责耦合回 session。

## 范围

1. 写一份 `docs/guide/link-plugins.md`（`.en.md` 同步），给出：
   - 链接层与 session / remote 插件的职责表；
   - `sessions.open(far)` 的 `Far` / `Near` 契约，Rust 的 `Session::new(channel)`；
   - 明确：认证/准入在链接层；身份来自远端 `Hello`；session 不认证、不拿 peer 参数；
   - 明确：adapter + bridge 的建议形态，adapter 可以是 CLI、SDK 或 sidecar；
   - 一条最小 TS / Python / Rust 示例，和一条 listen 侧示例；
   - 错误输出、重连、close、顶替的约定。
2. 提供各语言的最小模板（可以先 TS + Python + Rust，Go/Swift/Kotlin 后续）：
   - `link/echo` 或 `link/tcp` 级别的示例；
   - 只依赖 `sessions` 服务，不依赖核心新 API。
3. 给出一致性测试的最小集合：
   - 消息边界、顺序、背压、close；
   - 远端 `Hello` id 命名 `<id>:<plugin>`；
   - listen 顶替；
   - 认证在 `open` 之前，session 不感知。
4. 在 `docs/guide/plugin-api.md` 和 `docs/guide/nodes.md` 的“自建连接”一节补上契约链接；不新增核心 trait，不改 `sessions.open(far)` 签名。

## 非目标

- 不重新引入 0.19 删除的 `Transport` trait、`ChannelInfo.peer`、`Identity` / `Admission` 一族。
- 不给 `sessions.open(far)` 加 peer、token、credential 参数。
- 不让 session 层做认证或身份绑定。
- 不把链接层插件限制在 Rust。

## 验收

- [ ] 文档写清“链接层认证 vs 远端注册身份”，并用代码/文档引用证明：`Channel` 不知道对面是谁，远端在握手时报 id。
- [ ] TS / Python / Rust 各有一个最小 link plugin 示例，能跑通 `sessions.open(far)` / `Session::new(channel)`。
- [ ] 示例的认证在 `open` 之前完成；session 代码里没有 token / TLS / Tailscale。
- [ ] listen 侧示例能接受多条连接、每条一个 session，并能按远端 Hello id 顶替。
- [ ] 一致性测试覆盖消息边界、顺序、背压、close、远端 id。
- [ ] `cargo test` / 各语言测试按仓库现有流程通过；文档链接检查通过。

## 关联

- [远程连接：只留一个会话插件](../../design-remote-session-2026-10-28.md) §2、§5
- [协议与 Channel 解耦](../../design-protocol-channel-decoupling-2026-10-03.md)
- `crates/rutis-bridge/src/channel/mod.rs`
- `crates/rutis-bridge/src/session/plugin/mod.rs`
- [插件 API：自建连接](../../guide/plugin-api.md#自建连接sessionsopen)
- [连接节点：自建连接](../../guide/nodes.md#自建连接)
