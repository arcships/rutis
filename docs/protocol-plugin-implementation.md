# #59 对象协议实施与验收记录

实现分支：`feat/protocol-plugins-59`，基线是 #59 的设计提交 `0d8a5ac`。
目标保持[设计](design-protocol-plugins-2026-09-25.md)的 M0–M5 和 T01–T24 全范围；以下分阶段证据不能代替首版完成验收。

## M0：Go（2026-09-27）

实际依赖：本仓库 `rutis` 0.3.0；npm 发布包 `@deepseek-ai/cordis` **4.0.1**。
TS 独立包的 `package-lock.json` 绑定 npm tarball 与完整性摘要：
`sha512-YBdskTU2Po1kru3GgcUWUbkTsPMA9LkSQDAY8rBkFJeajdgcQad3QPJZE26JyK99Xb6HaASvoXg2DSUTeN/0Nw==`。
运行测试从该包的公开入口导入，没有用参考源码快照替代发行包。

本阶段不需要修改 rutis/Cordis 或维护 Cordis fork。适配层由本仓库维护；锁版本更新必须重跑本阶段契约和后续互通测试。
Rust 使用原生依赖谓词、代取消 token、Plugin/PluginFactory、子插件和 shutdown。
每次 activation 独立创建 native fiber；取消后的 permit 不再打开。必要子服务登记自己的 native token；准入检查同步拒绝，受 effect 管理的观察任务通知失效并刷新依赖。
TS 使用公开 `internal/plugin/status/service` 通知、`Context.isolate/provide` 与 `Fiber.dispose/await`。
失效通知栈内即调用 dispose，先清除 uid，再等待原生清理；内部子插件仍归 Cordis 管理。
恢复或配置更新必须由宿主创建新 activation，禁止在原 fiber 上偷换代。

发行包的两个适配细节已测试：`FiberState` 是没有运行期导出的 const enum；`ctx.plugin()` 返回继承真实 Fiber 的 thenable 包装，而通知传递真实 Fiber。适配层从 `internal/plugin` 保存真实 Fiber。
TS 基础适配接受 Cordis `{ apply, inject, Config }` 插件入口，并保留原生同步、异步、generator effect 的返回形态；类入口需显式适配，不宣称任意插件无改动兼容。

| M0 门槛 | 可执行证据 |
| --- | --- |
| 托管门控、T14 本地失效、依赖恢复不重入旧代 | Rust/TS managed 测试，均覆盖 Active 与 Loading |
| T15 内部子插件必要服务失效 | Rust native token 与 TS internal/service 测试；子树仍由原生框架清理 |
| T21 旧 Ctx / 终态优先 | 旧 Ctx 不能登记 provider/effect/listener/child；原 fiber 不复活 |
| 依赖安装隔离 | 两个 native 插件取得不同同名/同类型依赖；关闭其中一个不关闭另一个 |
| parallel/serial 入口 | 两端原生监听、0 值短路、卸载摘除；跨进程映射仍由 M4 验证 |
| 清理完成确认 | TS 慢 disposer；Rust 丢弃 stop waiter 后清理仍推进、多个 stop join 同一结果 |
| 初次校验失败 | Rust factory build / TS Config 失败闭锁，不可通过本地 retry 重启业务 |

验证命令：

```sh
cargo test -p rutis-protocol
cargo clippy -p rutis-protocol --all-targets -- -D warnings
npm --prefix protocol/ts ci --ignore-scripts
npm --prefix protocol/ts run check
npm --prefix protocol/ts test
```

当前结果：Rust managed 9 项、TS managed 7 项通过；这些是 M0 原生框架机制证据，不是全协议验收。
CI 增加锁定依赖的 TS 检查；Rust crate 纳入现有 workspace 测试。

## 后续阶段与最终验收

| 阶段 | 状态 | 尚需取得的证据 |
| --- | --- | --- |
| M1 | 待完成 | 描述符和两端生成绑定、scope/grant/delivery、终态水位、T04–T07/T22 共享语料 |
| M2 | 待完成 | 真实 Rust↔TS 进程和 native 插件、对象往返、borrow 回调、真实旧桥插件迁移、T24 测量 |
| M3 | 待完成 | 调用取消/完成、更新/失败回滚、组恢复屏障、Linux 进程和受管后代回收 |
| M4 | 待完成 | broker 权威事件列表、parallel/serial、scope、ready、once 与扩展拒绝 |
| M5 | 待完成 | T01–T24 逐项运行时证据、旧桥回归、迁移/回退指南、基础协议冻结 |

设计的第十七节完成定义仍未满足。后续语言与 X01–X04 保持独立扩展；本阶段没有启用这些能力。
