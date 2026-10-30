# feat(link): 链接层插件生态：模板、一致性测试、适配器目录

状态：已发布为 [#958](https://github.com/arcships/rutis/issues/958)。优先级 P2。依赖契约 issue 与至少一个 T1 插件。

## 问题

如果每个语言的 link plugin 都自己实现 framing、重连、listen 顶替、错误输出，生态会分叉；作者也会把认证/身份塞进 session。需要一个可复制的骨架和一致性测试。

同时，用户需要知道“我该用哪个 link plugin”：SSH/Tailscale/Cloudflare/frp/gRPC/ADB/iroh……没有目录和场景说明，插件再多也找不到。

## 范围

1. 各语言模板（先 TS / Python / Rust，Go / Swift / Kotlin 后续）：
   - 以 `.agents/skills/rutis-link-plugin/SKILL.md` 为 agent 指南，模板与它保持一致；
   - 只依赖 `sessions` 服务 / `Session::new(channel)`；
   - 一个 `link/echo` 或 `link/tcp` 最小实现；
   - adapter + bridge 的文件结构；
   - 注释里写清认证在 adapter、身份来自远端 `Hello`、session 不碰 token。
2. 一致性测试套件：
   - 消息边界、顺序、背压、close、half-close；
   - 超长消息；
   - 远端 `Hello` id 命名 `<id>:<plugin>`；
   - listen 多连接 + 同 id 顶替；
   - `open` 之前认证失败不创建 session；
   - session 卸载 → channel 关闭。
3. 适配器目录 `docs/guide/link-plugins.md` 或 `docs/guide/link-plugins/`：
   - 每个 adapter 写：给什么、准入、语言支持、部署前提、场景、优先级；
   - 标注 T1/T2/T3；
   - 不把“计划中”写成“已支持”。
4. CI：
   - 模板各语言编译/测试；
   - 一致性测试在至少一个语言上跑；
   - 文档链接检查。

## 非目标

- 不新增核心 trait / API。
- 不要求所有语言同时支持所有 adapter。
- 不把 adapter 目录做成插件市场；先做文档和模板。
- 不承诺第三方 adapter 的质量；用一致性测试分级。

## 验收

- [ ] TS/Python/Rust 模板可复制、可运行、只依赖 `sessions`。
- [ ] 一致性测试套件覆盖上述行为，至少一个语言接入 CI。
- [ ] `docs/guide/link-plugins.md` 有 adapter 目录，T1/T2/T3 和场景清楚。
- [ ] 每个 T1 adapter 至少有一个可复制示例（SSH/Tailscale/Cloudflare）。
- [ ] 文档链接检查和模板测试通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/ssh`](03-link-ssh.md)
- [`link/tailscale`](04-link-tailscale.md)
- [`link/cloudflared`](05-link-cloudflared.md)
- [`link/tcp` 与反向隧道](06-link-tcp-and-reverse-tunnels.md)
