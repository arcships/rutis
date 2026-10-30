# feat(link): `link/ssh` 用 SSH 做链接层

状态：已发布为 [#951](https://github.com/arcships/rutis/issues/951)。优先级 P1。依赖契约 issue。

## 问题

很多开发者已经有 SSH 访问：一台笔记本、一台 GPU 机器、一台家里的服务器。对他们来说，最便宜的“第二台机器”不是装 VPN，也不是开公网端口，而是用现有的 SSH。

现在 rutis 没有 SSH link plugin：用户只能自己 `ssh -L` 再配 `wss://127.0.0.1`，或者手写一个连接插件。认证、重连、framing 都要自己处理。

## 范围

1. 新增一个任意语言可实现的 `link/ssh` 参考实现（先 TS 或 Rust，其他语言复用契约）：
   - **stdio 模式**：起 `ssh host -- <rutis 运行时/桥>`，把 stdin/stdout 当字节流；做 framing（长度前缀或换行），然后 `sessions.open(far)`。
   - **forward 模式**：用 `ssh -L` / `ssh -R` 建立本地 TCP，再交给通用 `link/tcp` bridge。
   - **库模式**（可选）：Rust `russh`/`ssh2`、Go `x/crypto/ssh`、Python `asyncssh`、Node `ssh2`、Swift `swift-nio-ssh` 直接开 `direct-tcpip` channel。
2. 认证/准入完全由 SSH 负责：host key、用户 key、SSH agent、`authorized_keys`；不引入 rutis token，不把 SSH 身份传给 session。
3. 远端在 session `Hello` 里注册自己的 id；本端用这个 id 命名 `<id>:<plugin>`。
4. 生命周期：
   - SSH 进程/channel 结束 → `near.end`；
   - 重连策略在 link plugin（默认退避；不放进 session）；
   - listen 侧如果支持，多连接各自一个 session。
5. 文档：`docs/guide/link-plugins.md` 增加 SSH 一节，写清两种模式、host key、agent、重连、错误输出。

## 非目标

- 不实现 SSH 服务端；用系统 `sshd` 或现有 SSH 服务。
- 不把 SSH 认证映射成 rutis token。
- 不做 SSH 证书颁发/CA 管理。
- 不把 SSH 变成核心依赖；只是 link plugin。

## 验收

- [ ] stdio 模式：本端 `ssh` 起远端 rutis 运行时，消息双向、边界/顺序正确，远端插件以 `<id>:<plugin>` 出现。
- [ ] forward 模式：`ssh -L` / `-R` 后，`link/tcp` 能连上并跑通同一套 session。
- [ ] SSH 认证失败时，link plugin 报错清楚，不进入 session；session 代码里没有 token/SSH 概念。
- [ ] SSH 断开/进程退出时，session 卸载，channel 关闭；重连按 link plugin 的退避执行。
- [ ] 至少覆盖 Linux + macOS；Windows 用 OpenSSH 客户端时行为写清。
- [ ] 文档示例可复制运行；测试不依赖公网 SSH 服务（用本地 sshd 或 fake channel）。
- [ ] `cargo test` / 语言测试 / 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/tcp` 与反向隧道](06-link-tcp-and-reverse-tunnels.md)
- [插件 API：自建连接](../../guide/plugin-api.md#自建连接sessionsopen)
