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
每次 activation 独立创建 native fiber；取消后的 permit 不再打开。必要子服务创建声明该服务依赖的 native guard，发布前等待 guard Active；服务单独摘除也会预取消 guard。准入检查同步拒绝，受 effect 管理的观察任务通知失效并刷新依赖。
TS 使用公开 `internal/plugin/status/service` 通知、`Context.isolate/provide` 与 `Fiber.dispose/await`。
失效通知栈内即调用 dispose，先清除 uid，再等待原生清理；内部子插件仍归 Cordis 管理。
恢复或配置更新必须由宿主创建新 activation，禁止在原 fiber 上偷换代。

发行包的两个适配细节已测试：`FiberState` 是没有运行期导出的 const enum；`ctx.plugin()` 返回继承真实 Fiber 的 thenable 包装，而通知传递真实 Fiber。适配层从 `internal/plugin` 保存真实 Fiber。
TS 基础适配接受 Cordis `{ apply, inject, Config }` 插件入口，并保留原生同步、异步、generator effect 的返回形态；类入口需显式适配，不宣称任意插件无改动兼容。

| M0 门槛 | 可执行证据 |
| --- | --- |
| 托管门控、T14 本地失效、依赖恢复不重入旧代 | Rust/TS managed 测试，均覆盖 Active 与 Loading |
| T15 内部子插件必要服务失效 | Rust native guard/token 与 TS internal/service 测试；覆盖仅释放服务及关闭整个子插件，子树仍由原生框架清理 |
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

当前结果：Rust managed 10 项、TS managed 8 项通过；这些是 M0 原生框架机制证据，不是全协议验收。
CI 增加锁定依赖的 TS 检查；Rust crate 纳入现有 workspace 测试。

## 后续阶段与最终验收

| 阶段 | 状态 | 尚需取得的证据 |
| --- | --- | --- |
| M1 | 开发中 | 描述符与两端导入表、broker 交付和执行 pin 已落地；仍需生成绑定、真实对象导出表、对象图物化、borrow 执行登记及严格解析后冻结 |
| M2 | 待完成 | 真实 Rust↔TS 进程和 native 插件、对象往返、borrow 回调、真实旧桥插件迁移、T24 测量 |
| M3 | 待完成 | 调用取消/完成、更新/失败回滚、组恢复屏障、Linux 进程和受管后代回收 |
| M4 | 待完成 | broker 权威事件列表、parallel/serial、scope、ready、once 与扩展拒绝 |
| M5 | 待完成 | T01–T24 逐项运行时证据、旧桥回归、迁移/回退指南、基础协议冻结 |

设计的第十七节完成定义仍未满足。后续语言与 X01–X04 保持独立扩展；本阶段没有启用这些能力。

## M1 已实现的基础与剩余门槛

`rutis-protocol::{contract,identity,broker,imports}` 与 TS `contract.ts/imports.ts`：

- 完整 bundle 原始字节 SHA-256、精确版本、可达接口检查；scope/borrow、record/list/optional 的独立 tagged value；回调按完整签名哈希匹配。
- 不支持的 delegate、持久回调、waterfall、stream 在描述符准入时拒绝。JSON 数据中类似对象 id 的字段不会被当成引用。
- broker 为每次交付产生独立 id 与随机 token，按调用者 activation/scope 和接口视图校验；接收、释放、撤销幂等。
- 两端代理缓存按 scope/对象/权限来源区分；同 scope release 关闭全部别名；新交付只创建/复用当前有效包装，旧包装不复活。
- 子 scope 递归关闭；delivery pin 与 execution pin 分离，等待者释放不能提前释放仍执行的引用；旧 call/activation/scope/epoch 标识不复用。
- 回收必须是已接收、终态的连续前缀；缺口或活 token 阻止回收。低于已确认水位的迟到交付拒绝，迟到控制帧幂等，不按时钟推断。

可执行证据：Rust objects 8 项、contracts 3 项；TS imports 4 项、contracts 3 项。`contract-corpus.json` 的 30 个合法/非法用例由两端消费，包含 Unicode、JS 安全整数边界、嵌套对象、回调签名、反射字段、wire tag 及类型错误。Rust 另使用两个真实线程同时交付/释放，验证两种锁序均收敛。
这些测试证明账本和编码规则，尚未证明真实远程对象返回、回调 handler、循环图物化、生成接口或 IPC。

本阶段工作区回归：`cargo test --workspace` 387 passed / 0 failed / 2 ignored，未设置 Node 跳过变量，旧桥真实 Node TCP e2e 通过。两个 ignored 是依赖外部 min-cordis/dsh 检出的 host e2e 与需要真实模型后端的 agent e2e，不能计入通过。`cargo check --workspace --all-targets`、全仓 fmt、协议 crate clippy `-D warnings`、TS check/test 及旧 host 的断连测试通过；没有执行外部 min-cordis/dsh 整体迁移验收。

M1 后续必须补齐以下内容，之后才能称为 M1 完成或冻结该实验协议：

1. 描述符生成 Rust/TS 的客户端与导出适配，作者调用对象方法而无需操作 id/token。
2. 真对象导出表的稳定身份、grant/execution pin 对真实 Arc/JS 对象的持有、disposer 收敛；完整对象图先建代理再连接不可变关系。
3. borrow 回调执行范围及登记子任务，包含嵌套重入；owner 回传建立受门控 facade，保持统一入站调度。
4. 严格 UTF-8/JSON 解码、重复字段和深度/长度检查；完成相同非法描述符语料，而非只比较合法数据。
5. 将双端回收水位 ACK 接入真实控制帧；闭合 runtime epoch 的记录回收与迟到消息测试。当前 broker/imports 的前缀规则已有内存测试，握手仍待接入。

参考核对：[Cap’n Proto RPC](https://capnproto.org/rpc.html)及其[rpc.capnp](https://github.com/capnproto/capnproto/blob/master/c%2B%2B/src/capnp/rpc.capnp)把 capability、释放和路径顺序作为协议机制。当前用例对照这些时序，但不声称继承它的实现正确性。继续采用设计要求的单 broker 路径；成熟实现复用与生成接口/原生生命周期的集成成本评估尚未完成，pipelining/直连由 T24 测量后另议。
