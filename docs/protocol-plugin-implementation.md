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

M0 门槛用例：Rust managed 10 项、TS managed 8 项通过；这些是 M0 原生框架机制证据，不是全协议验收。
CI 增加锁定依赖的 TS 检查；Rust crate 纳入现有 workspace 测试。

## 后续阶段与最终验收

| 阶段 | 状态 | 尚需取得的证据 |
| --- | --- | --- |
| M1 | 开发中 | 严格描述符、两端真实对象导出表及原生清理、整批导入、broker 交付和执行 pin 已落地；对象图、生成绑定与 Rust 实际 dispatch/borrow 内存链路已落地；仍需两端完整调度互通与控制帧后冻结 |
| M2 | 待完成 | 真实 Rust↔TS 进程和 native 插件、对象往返、borrow 回调、真实旧桥插件迁移、T24 测量 |
| M3 | 待完成 | 调用取消/完成、更新/失败回滚、组恢复屏障、Linux 进程和受管后代回收 |
| M4 | 待完成 | broker 权威事件列表、parallel/serial、scope、ready、once 与扩展拒绝 |
| M5 | 待完成 | T01–T24 逐项运行时证据、旧桥回归、迁移/回退指南、基础协议冻结 |

设计的第十七节完成定义仍未满足。后续语言与 X01–X04 保持独立扩展；本阶段没有启用这些能力。

## M1 已实现的基础与剩余门槛

`rutis-protocol::{contract,identity,broker,imports,exports,graph,json,codegen,sdk,memory}` 与对应 TS 模块：

- 完整 bundle 原始字节 SHA-256、精确版本、可达接口检查；scope/borrow、record/list/optional 的独立 tagged value；回调按完整签名哈希匹配。
- 不支持的 delegate、持久回调、waterfall、stream 在描述符准入时拒绝。JSON 数据中类似对象 id 的字段不会被当成引用。
- broker 为每次交付产生独立 id 与随机 token，按调用者 activation/scope 和接口视图校验；接收、释放、撤销幂等。
- 两端代理缓存按 scope/对象/权限来源区分；同 scope release 关闭全部别名；新交付只创建/复用当前有效包装，旧包装不复活。
- 子 scope 递归关闭；delivery pin 与 execution pin 分离，等待者释放不能提前释放仍执行的引用；旧 call/activation/scope/epoch 标识不复用。
- 回收必须是已接收、终态的连续前缀；缺口或活 token 阻止回收。低于已确认水位的迟到交付拒绝，迟到控制帧幂等，不按时钟推断。
- 双端严格解码 UTF-8/JSON，拒绝重复字段（含转义重名）、BOM、孤立 surrogate、非有限/不安全整数；16 MiB 字节与 64 层子值深度是解码器边界。回调签名采用一致的 UTF-8/IEEE-754 编码，避免 JSON 打印和 UTF-16 排序差异。
- owner 表按真实 Arc/JS 身份建 weak 映射，delivery/execution 独立强持有；独占 disposer 执行一次并等待，失败可观察，shared 对象保留本地所有权。runtime epoch 的 id 分配跨 activation 共用；broker 重复登记不可修改现有视图；宿主可添加独立视图，旧 grant 的 whitelist 不改变。
- 表接入真实 native ctx effect；失效的 gate/uid 同步拒绝新 pin，卸载等待执行和慢 disposer。整批导入先校验再附加；失败只释放新增交付，保留以前的成功别名。

- 对象图按完整表校验后两遍建代理和关系；每个表项必须可达，属性关系继承包含它的 scope。循环边只保存身份，不互持 Arc。非法新快照只拒绝新增交付；旧包装保持有效。显式释放子包装后导航失败，新交付建立新包装，旧别名永不复活。
- 已准入描述符不可修改，原始摘要和验证的 schema 保持绑定；Rust 同一真实 Arc 的不同 trait 视图保持同一身份，执行适配器不要求业务 trait 继承 Any。

- 已生成 Rust 客户端/业务 trait 和 TS 客户端/业务接口及两端原生导出适配器；两个固定原字节 bundle 编译并核对生成文件，覆盖 DTO、字符串 enum、判别 union、嵌套 record/list/optional、callback 和对象字段。Rust number 使用有限的 `serde_json::Number`；可选 DTO 字段保留缺失与显式 null 的区别。生成顺序不受 workspace 的 serde_json `preserve_order` 特性影响。
- Rust `memory::Network` 接入同一 broker、真实导出表、完整图编码和接收准入。生成客户端运行 stateful connect/query、owner pass-back facade、循环 agent/session 导航及 A→B→A 借用回调。回传必须证明原 accepted grant；第三方转发拒绝，旧引用保持有效。
- handler 不在 broker 锁内运行；丢弃等待者不丢弃 owner 执行任务。登记子任务可继续登记后代，全部完成后再闭合借用 scope 和释放执行 pin；旧 callback 包装拒绝新调用。四个真实线程上的并发调用在 broker 内分配 call/scope 编号，避免分配与准入乱序。
- Rust 生成调度已接入实际 managed rutis 插件的原始 Ctx 与注入服务，native effect 关闭 endpoint 并等待 exports。TS 生成适配器也用锁定 Cordis 原生 Ctx 验证注入与失效准入，但该测试是适配器单元测试，没有替代 Rust 权威 broker 的 TS 互通证据。
- import scope 继承原生 gate，缓存属性同步检查 gate，终态不重开。owner 撤销包括尚未接收的迟到 handoff；关闭 owner 后的纯值迟到结果也拒绝，已执行的 handler 仍完成。关闭图后即使作者继续保存旧代理，真实 Arc 对象图也不被保留。

可执行证据：Rust bindings 8、contracts 5、exports 9、graphs 7、objects 12、managed 11 项（合计 52）；TS bindings 7、contracts 5、exports 8、graphs 5、imports 5、managed 8 项（合计 38）。共享语料包含 JSON 35、描述符 50、wire value 32、回调签名 9、对象图 31 个用例，覆盖合法/非法输入、边界、Unicode/数字语义和不支持扩展。Rust 使用两个真实线程同时交付/释放；两端另验证原生失效时的执行 pin 和慢对象清理。
这些测试证明账本、真实对象持有、原生清理和编码规则，还覆盖循环图物化、快照一致性与图关闭；Rust 内存链路已覆盖真实状态对象返回、生成接口、回调 handler 和登记子任务；尚未证明跨语言 dispatch 或 IPC。当前约定见 [protocol README](../protocol/README.md)，水位控制帧顺序已写明，尚未接入实际消息。

本阶段工作区回归：`cargo test --workspace` 418 passed / 0 failed / 2 ignored，未设置 Node 跳过变量，旧桥真实 Node TCP e2e 通过。两个 ignored 是依赖外部 min-cordis/dsh 检出的 host e2e 与需要真实模型后端的 agent e2e，不能计入通过。`cargo check --workspace --all-targets`、全仓 fmt、协议 crate clippy `-D warnings`、TS check/test 及旧 host 的断连测试通过；没有执行外部 min-cordis/dsh 整体迁移验收。

M1 后续必须补齐以下内容，之后才能称为 M1 完成或冻结该实验协议：

1. 完成 TS 对象图编码、真实调用与 Rust broker 互通；把两端已生成绑定及原生适配接入同一实际调用链。当前 TS binding 测试不包含实际 broker 或 transport。
2. 补齐双端完整结果丢弃、scope 回收、独占资源 disposer、handler/子任务失败收敛及原生依赖失效交错；Rust 已有状态返回、owner facade、循环图与嵌套 borrow 内存证据，不能等同于跨语言验收。
3. 将严格解码和非法语料接入帧/握手，形成受认证的入站调度与控制消息；当前 memory helper 使用一个 admitted bundle，宿主 prepare 的多 bundle/路由/权限计划仍待 M2。
4. 将回收水位 ACK 与 epoch 记录回收接入真实控制帧，重跑迟到消息和取消/finished 交错；当前直接调用账本 API 的证据不能代替协议控制流。
5. 检查生成绑定在实际迁移插件中的可用性，完成成熟实现复用评估及完整 M1 门槛记录，之后再冻结实验协议。

参考核对：[Cap’n Proto RPC](https://capnproto.org/rpc.html)及其[rpc.capnp](https://github.com/capnproto/capnproto/blob/master/c%2B%2B/src/capnp/rpc.capnp)把 capability、释放和路径顺序作为协议机制。当前用例对照这些时序，但不声称继承它的实现正确性。继续采用设计要求的单 broker 路径；成熟实现复用与生成接口/原生生命周期的集成成本评估尚未完成，pipelining/直连由 T24 测量后另议。
