# #59 对象协议实施与验收记录

实现分支：`feat/protocol-plugins-59`，基线是 #59 的设计提交 `0d8a5ac`。
目标保持[设计](design-protocol-plugins-2026-09-25.md)的 M0–M5 和 T01–T24 全范围；以下分阶段证据不能代替首版完成验收。

## M0：Go（2026-09-27）

实际依赖：本仓库 `rutis` 0.3.0；npm 发布包 `@deepseek-ai/cordis` **4.0.1**。
TS 独立包的 `package-lock.json` 绑定 npm tarball 与完整性摘要：
`sha512-YBdskTU2Po1kru3GgcUWUbkTsPMA9LkSQDAY8rBkFJeajdgcQad3QPJZE26JyK99Xb6HaASvoXg2DSUTeN/0Nw==`。
运行测试从该包的公开入口导入，没有用参考源码快照替代发行包。

本阶段使用本仓库 rutis 的公开托管扩展 `FiberView.seal_effects`，后续 child 创建者校验还使用只读 `Ctx.is_within`。这些扩展尚需包含在正式 rutis 发行版本中，不宣称未修改的外部 0.3.0 已有这些 API；不修改 Cordis 或维护 Cordis fork。适配层及 rutis 扩展由本仓库维护；锁版本更新必须重跑本阶段契约和后续互通测试。
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

M0 门槛用例：Rust managed 14 项、TS managed 8 项通过；这些是 M0 原生框架机制证据，不是全协议验收。
CI 增加锁定依赖的 TS 检查；Rust crate 纳入现有 workspace 测试。

## 后续阶段与最终验收

| 阶段 | 状态 | 尚需取得的证据 |
| --- | --- | --- |
| M1 | 开发中 | 两端草稿编码、事务授权、精确 native child 创建者、单对象远端撤销及生成 dispatch 已接入私有 socket；完整失败交错、双向独占资源跨语言清理、生产连接的控制流与复用评估仍需补齐 |
| M2 | 开发中 | prepare、冻结快照、默认 Rust/Node 服务 driver、首代 HostProxy 与 Host native export adapter 的真实 slot 发布/关闭及子进程双向 DI 已有证据；快照监督租约、共享/单独完整拓扑、真实旧桥迁移和 T24 测量尚未完成 |
| M3 | 开发中 | Linux 冻结进程和脱离会话后代的独立回收已有实际证据；调用取消/完成、更新/失败回滚、全组消费者/OS 恢复屏障及管理状态仍需完成 |
| M4 | 待完成 | broker 权威事件列表、parallel/serial、scope、ready、once 与扩展拒绝 |
| M5 | 待完成 | T01–T24 逐项运行时证据、旧桥回归、迁移/回退指南、基础协议冻结 |

设计的第十七节完成定义仍未满足。后续语言与 X01–X04 保持独立扩展；本阶段没有启用这些能力。

## M1 已实现的基础与剩余门槛

`rutis-protocol::{contract,identity,broker,imports,exports,graph,draft,frame,json,codegen,sdk,memory}` 与对应 TS 模块：

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

- 已生成 Rust 客户端/业务 trait 和 TS 客户端/业务接口及两端原生导出适配器；三个固定原字节 bundle 编译并核对生成文件，覆盖 DTO、字符串 enum、判别 union、嵌套 record/list/optional、callback 和对象字段。Rust number 使用有限的 `serde_json::Number`；可选 DTO 字段保留缺失与显式 null 的区别。生成顺序不受 workspace 的 serde_json `preserve_order` 特性影响。
- Rust `memory::Network` 接入同一 broker、真实导出表、完整图编码和接收准入。生成客户端运行 stateful connect/query、owner pass-back facade、循环 agent/session 导航及 A→B→A 借用回调。回传必须证明原 accepted grant；第三方转发拒绝，旧引用保持有效。
- handler 不在 broker 锁内运行；丢弃等待者不丢弃 owner 执行任务。登记子任务可继续登记后代，全部完成后再闭合借用 scope 和释放执行 pin；旧 callback 包装拒绝新调用。四个真实线程上的并发调用在 broker 内分配 call/scope 编号，避免分配与准入乱序。
- Rust 生成调度已接入实际 managed rutis 插件的原始 Ctx 与注入服务，native effect 关闭 endpoint 并等待 exports。TS facade 兼容锁定 Cordis 公开的 `Service.tracker` 元数据探测，不开放额外字符串 selector；实际 native 注入在私有 socket fixture 中验证。
- import scope 继承原生 gate，缓存属性同步检查 gate，终态不重开。owner 撤销包括尚未接收的迟到 handoff；关闭 owner 后的纯值迟到结果也拒绝，已执行的 handler 仍完成。关闭图后即使作者继续保存旧代理，真实 Arc 对象图也不被保留。

- 双端 `GraphExporter` 先暂存真实对象，完整验证草稿，再向 broker 请求授权。broker 整图事务拒绝伪造 owner/source/foreign proof，不留下交付 id 缺口；SDK commit 只确认对应的真实对象与视图。Rust snapshot panic 与两端验证失败释放 staging；原生闭锁后纯值结果也不能编码。同一 owner 的多个 bundle 编码器共用 staging 编号空间；Rust 内存链路复用同一编码器，owner pin 失败通过完整拒绝 manifest 记录接收前缀。
- 双端帧接收泵支持重入和并发，拒绝重复/倒序请求 id；Rust 独立 writer 在等待者被丢弃时继续完成已排队帧，关闭时可打断阻塞写并释放 stream。TS 编码前拒绝 undefined、NaN、Date、稀疏数组和自定义原型，防止 stringify 改变数据含义。

可执行证据：Rust imports 单元 1、child exports 单元 2、bindings 8、contracts 5、drafts 5、exports 10、factories 6、frames 5、graphs 7、objects 15、managed 14、objects_ipc 1、prepare 21、lifecycle 12、services 8、session_ipc 2、native_runner_ipc 10 项（合计 132，其中 lifecycle 与 native_runner_ipc 各一项是子进程入口）；TS bindings 7、contracts 5、drafts 4、exports 11、frames 5、graphs 5、imports 7、managed 8、lifecycle 12、services 10、session 1 项（合计 75）。共享语料包含 JSON 35、描述符 50、wire value 32、回调签名 9、对象图 31、帧 10、lifecycle hello 17、named services 12 个用例。Rust 使用两个真实线程同时交付/释放；两端另验证原生失效时的执行 pin 和慢对象清理。

Linux `objects_ipc` 运行独立 Node 进程与真实 managed rutis/Cordis Ctx，使用继承的私有 Unix stream fd 3，stdout 诊断另行读取。Rust 权威 broker 从固定连接取得 Node activation，校验 family/version/raw SHA/capabilities；两端生成客户端实际 connect/query，保留同一状态对象和 session→agent→session 循环，owner pass-back 仍走 broker。两端借用 callback 重入第三层调用并等待登记子任务；保存的 callback 到期后拒绝调用。Node 在业务 dispatch 前通过控制帧确认参数 grant，避免回调/pass-back 读取尚未 accepted 的引用。丢弃 Rust waiter 后 Node 实际执行仍持有 execution pin，完成后才释放。

同一私有 socket fixture 已完成 Node 接收 SDK 的前缀提议、broker 独立检查、owner 回收通知、ACK 后 SDK 清理；Rust 接收端也走同一 broker 和 Node owner 通知。活的首份 grant 阻止后续已释放 borrow 的回收，旧 id 的控制重传不能恢复授权，低于水位的接收和 pin 拒绝。后续 `session_ipc` 又覆盖多个 activation 共用前缀和全部成员关闭后的 epoch 回收。当前约定见 [protocol README](../protocol/README.md)。这些是 conformance 证据；完整断连/取消/finished 故障交错与 supervisor 尚未验收。

本阶段工作区回归：修复下文记录的并发 Accept 竞态、增加 exec `ETXTBSY` 有界重试、接入单对象撤销与独立 Linux 后代回收后，`cargo test --workspace` 499 passed / 0 failed / 2 ignored；新对象协议私有 Node socket、Rust 生命周期子进程、冻结目录的默认 Rust/Node 服务 driver、首代 Host instance/native 发布与关闭、实际 child 创建者、受管后代回收及旧桥真实 Node TCP e2e 均通过。两个 ignored 是依赖外部 min-cordis/dsh 检出的 host e2e 与需要真实模型后端的 agent e2e，不能计入通过。`cargo check --workspace --all-targets`、全仓 fmt、协议 crate clippy `-D warnings`、TS check/build/test 通过；没有执行外部 min-cordis/dsh 整体迁移验收。Linux Rust CI 安装锁定 Node/TS 依赖并运行该新互通测试，旧桥的跳过变量不跳过它。

### 普通原生 child 的单对象撤销

`Exports` 观察原始创建者代结束并关闭其对象；新登记也检查原始 token/state，
避免取消观察已完成后又登记新对象。Rust child 的 observer/effect 和 TS 托管根的
公开 status listener/child effect 均由实际原生子树拥有。`RuntimeObjects.bind`
接入 `object/closed`，Host 按固定连接与 owner 校验整批身份后同步关闭 broker
grants，并发给全部接收 SDK `object/revoke-objects`；ACK 包括真实 Release 和
owner pin 确认。重复撤销、迟到首次导出及跨 activation 重用同 epoch id 均拒绝
复活授权。required 根失去时关闭已捕获的消费者；普通返回对象不会关闭根。

Rust/TS 的独立发送与清理任务不随 waiter 丢弃取消；child stop 保留真实 execution
pin，等待执行、登记后代、独占 disposer 和远端确认。失败 ACK/panic 被保留，
重复 join 仍报告失败；Cordis 原生 dispose 记录错误但不拒绝 Promise，SDK 将该
错误保留至 join/托管根 stop。整根关闭仍使用既有整代撤销屏障；不以单对象通知
取代 M3 的 StopUnconfirmed 与 supervisor。

实际 `session_ipc` 的 Node child 向 Rust 原生消费者交付独占 Connection 和循环
属性。暂停真实 Release ACK 后，旧缓存已关闭而 child stop 未完成；恢复 ACK 后
其他根对象继续调用，在途登记后代仍持有 execution pin。后代完成使 pin 归零，
慢 disposer 继续阻止 child stop，放行后只清理一次，两个根保持开放。ACK 暂停
会阻塞同一接收端的控制队列，本用例不声称该连接不受控制流背压影响。

Rust `objects` 覆盖整批闭合回滚、独立 source/对象、新调用拒绝、保留在途 pin
与迟到交付；`services` 覆盖实际 child stop 后旧 proxy/属性及 broker pins 归零。
两端 exports 原生测试分别控制执行、慢 disposer 和 ACK；Rust 丢弃 stop waiter
并注入撤销任务 panic，TS 注入失败 ACK 后重复观察导出表错误。完整双向故障
交错、生产监督与最终 T01–T24 仍待后续验收。

M1 后续必须补齐以下内容，之后才能称为 M1 完成或冻结该实验协议：

1. 扩展首代 frozen runner/HostProxy/native export adapter 原生图到多 bundle 和共享/单独完整拓扑；早期私有 session 的 fixture 管理入口不能代替这些装配证据。
2. 补齐双端完整结果丢弃、scope 回收、独占资源 disposer、handler/子任务失败收敛及原生依赖失效交错；实际 child 对象和 borrow callback 已使用精确创建者 Ctx，选择性撤销、在飞清理与 Node 独占对象向 Rust 交付的延迟 ACK 已有证据，双向完整故障矩阵仍需验收。
3. 将已有严格帧/握手检查、只读 prepare 与首代 HostActive 装配接入生产监督状态机；两端 instance 原生路由与 Host native export adapter 已有证据，更新恢复仍待完成。
4. 补齐 epoch 记录回收、断连/迟到消息与取消/finished 交错；多 activation 的正常终态回收 ACK 已有真实 session 证据，但不代替故障控制流和监督恢复。
5. 检查生成绑定在实际迁移插件中的可用性，完成成熟实现复用评估及完整 M1 门槛记录，之后再冻结实验协议。

参考核对：[Cap’n Proto RPC](https://capnproto.org/rpc.html)及其[rpc.capnp](https://github.com/capnproto/capnproto/blob/master/c%2B%2B/src/capnp/rpc.capnp)把 capability、释放和路径顺序作为协议机制。当前用例对照这些时序，但不声称继承它的实现正确性。继续采用设计要求的单 broker 路径；成熟实现复用与生成接口/原生生命周期的集成成本评估尚未完成，pipelining/直连由 T24 测量后另议。

## M2 已实现的 prepare 与静态注册边界

`prepare::{PreparedPackage,PreparedDeployment}` 冻结版本目录的 manifest 原字节、全部
artifact 字节、配置 schema、每实例配置、精确 bundle 及 deployment 路由。完整 required
图拒绝同组和跨组循环；漏配路由留为 missing/Pending。组兼容性检查绑定 image、Node
runner、framework、完整 dependency environment 与 capabilities。原生 TypeKey 使用
完整身份元组，不把 Rust TypeId 放入 wire。配置变化不改变组代码摘要。

Rust runner 的 `.rutis.protocol.catalog` 从实际 ELF section 读取，没有执行插件来
探测 factory。`StaticFactories` 核对嵌入 catalog 与实际链接的延迟构造器表，拒绝缺失、
额外、重复或契约不同的 factory。`mount_prepared` 只选择冻结实例，使用冻结配置进行
schema、原生类型及 native config 校验，再挂载真实 ManagedActivation；原生失效不能
重启旧代。注册期捕获 factory 元数据先于 permit，panic 不留下部分挂载。

prepare 前 17 项覆盖实际 test ELF、Node 模块不执行、配置冻结、完整文件库存、路径与
symlink、共享组兼容、原字节 route mismatch、required cycle、event 权限与 CLI 不泄露
配置，以及冻结计划到真实 native mount 的边界。factories 6 项验证延迟构造、两个独立
原生配置/服务范围、native Pending 与旧代失效、schema/类型/config 拒绝、catalog
不一致及构造器/元数据 panic 回滚。prepare 的 Node 声明测试用不执行的 ELF artifact，
不是实际 Node runner 启动证据；真实对象调用证据来自 `objects_ipc`，实际多成员装载见后续冻结启动测试。
另以 release profile 运行实际 ELF catalog 准入测试，确认优化和链接后仍保留可读取的
嵌入元数据。

具体包与部署格式和 CLI 见 [prepare 约定](../protocol/package-format.md)。
`verify_unchanged()` 能检查磁盘包变化。Linux `snapshot::Snapshot` 从冻结字节建立私有
启动树，保留内部 alias 和执行标志；环境摘要同时绑定这些元数据。相同包快照只保留
一个代码目录；各组的 dependency canonical tree 供成员共用，避免复制 Cordis/SDK
导致框架类型和模块缓存分裂。组和成员租约持有整个树，显式 cleanup 拒绝删除仍被
租用的路径。监督者与进程/consumer 双屏障的实际接入仍待 M3，不能用租约 API 代替。

快照测试从原包删除后的冻结路径启动实际 Node executable，加载实际 Cordis 4.0.1
和经 `tsconfig.build.json` 编译的 SDK。两个不同代码包和 SDK alias 保持同一框架类，
各自装载 native Ctx、配置和 isolated service；关闭其一不会关闭另一个。另验证原
manifest 字节、私有目录、执行文件/依赖 helper 权限、group/member 租约与 cleanup、
依赖 alias/执行标志不同时的 shared-group 拒绝。它是固定启动 conformance fixture，
不是多 member private IPC 生命周期验收。

Rust `lifecycle` 控制流已接入实际 native fiber 和私有帧连接。hello 验证冻结身份和
factory 表且不构造插件；原生 RuntimeReady 在成员 apply 前独立发布。start/ready
之后仍需 activate，native 失效或 stop 不得重开。gate 在构造前保留并传入原生
mounter，stop 可以阻止迟到 apply。管理锁不跨 native 业务或 cleanup await，丢弃
stop waiter 不丢弃清理。不同成员的 id 可乱序到达，同一实例的旧 id 不能替换新代。

RuntimeReady 的 check 刷新不足以同步取消消费者；新增明确的 consumer 租约在 peer
close 返回前关闭 gate 并触发 native shutdown，旧 Ctx 随即拒绝 effect。frame close
hook 在帧锁外执行，断连先撤销、后独立推进 cleanup。实际 Rust 子进程继承私有 fd 3，
完成 hello、两个不同 native Ctx、activate、独立 stop 和整组清理；stdout 只读诊断。
具体行为和边界见[生命周期控制](../protocol/lifecycle.md)。

默认 native driver 的命名对象服务装配现已接入，见后面的冻结默认 driver 证据；
hello 继续明确拒绝事件能力。后续 HostProxy 首代发布证据见末节。
完整宿主业务图、StopUnconfirmed 管理 API、配置更新和 supervisor 同样尚未验收。

TS `lifecycle` 和编译后的通用 `node-runner` 接入同一控制形状，hello 保持模块惰性，
start 才导入 frozen entry；异步导入期间 stop 会撤销传入 native mounter 的同一个
gate。启动 catalog 由快照生成，不含实例配置，绑定精确 module 契约和代码/环境
身份；还检查实际 Cordis package 版本。真实 Rust Host 从原包删除后的冻结路径
启动 Node，通过私有 fd 3 装载两个不同代码包、两个独立 native Ctx。慢成员仍
Loading 时快成员可以 activate，停止慢成员后旧 Ctx 拒绝迟到 effect，快成员保持
Published，最终全组清理并正常退出。该用例使用通用 SDK 入口，但没有命名对象服务
或完整 Host 业务图，因此不代表整个 T01/T13 已验收。

TS 管理器通过公开 logger exporter 记录成员子树在关闭阶段的 error 诊断，避免
Cordis native unload 记录异常却成功返回 dispose 等待时产生错误 stop ACK。
child disposer 失败的实际测试验证 Closing 保留、全部清理尝试、错误可观察和拒绝
替换；作者在关闭阶段的 error 日志也保守地视为未确认。两端共用 17 个 hello 准入
语料，严格验证 shape、entry、契约、raw schema、family/epoch/capabilities。

逻辑 event scope 准入、native adapter 元数据、原生 mount 都不等于完整 runtime 绑定。生产 RuntimeReady、
导出暂存/HostActive ACK、完整多成员服务连接、真实旧插件迁移与 T24 仍未验收。


## M2 命名原生服务与整表提交基础

两端新增 `services` SDK，绑定 wire 服务名、精确 raw bundle/版本/interface 与本地
原生键。Rust 导出真实 Arc trait，导入生成客户端；TS 使用生成 export/facade 和
发行 Cordis 的隔离 slot。所有 required 类型/owner/root-scope 检查先于 provider
登记，native inject 必须与端口一致；Rust 捕获一次 factory 元数据后用于同一
native graph，TS 保留 inject 的 intercept 配置。

Rust 两个独立消费者在共享 root 的独立 native scopes 使用生成客户端，实际运行
stateful connect/query、重复对象身份、循环属性与 owner passback。内存链路复用
既有权威 broker、owner pins 和 decoder，没有手工拼对象根。TS 测试覆盖发行
Cordis 的同名 facade 注入隔离、原始 apply context、必要子服务失效，以及部分
provider 登记失败后异步 mount 等待回滚。Rust import provider disposers 纳入
ManagedActivation 的独立 stop；迟到采用仍保留资源让调用者显式回滚。

`ServiceTable` 必须是完整 provides 表，根为本代真实 own 对象，各服务有独立
source 和 stage 身份。Rust `offer_table` 在同一事务中准入多 bundle；第二张图的
伪造 foreign proof 被拒绝后，第一张图没有残留 views/grants 或交付编号缺口。
owner 整表 commit 在第一个 pin 转换前核对所有原始 manifest；中途 pin 失败
释放全表 staging/delivery pins 并永久关闭该 handoff。改变成功提交内容的重传
拒绝。两端共用 12 个合法/非法服务表用例，实际发现并修正了 source 摘要编码差异。
TS 的 pin/commit manifest 测试明确使用本地 fixture，不能当作 Host broker 或 IPC
授权证明。

原生闭锁测试证明仅取消 Rust generation token 不足以阻止 `Ctx.effect`；gate
通过弱引用同步调用公开 native shutdown，业务 Ctx 在 view 绑定后才交给 apply。
stop handler 的确认 future 尚未 poll 时，原始 Ctx 已拒绝 effect/provider/child。
必要服务 guards 属于真实成员的 native 子树，避免 runtime root 每代残留 Pending
插件。失效代终态为 Disposed，尚未进入的缺依赖代仍可 Pending 等待首次装载。
Loading 原始 Ctx 的 effect 拒绝需要下面记录的本仓库 rutis 公开扩展；不维护 Cordis fork。

行为和调用顺序见[服务绑定](../protocol/services.md)。这仍是 SDK/事务基础证据：
默认 driver 的对象 transport 已接入，事件仍拒绝。冻结根选择与 source alias 已有
后续私有 session 证据，默认服务 driver 也已复用这份 registry 和私有命名服务交付。
首代 instance HostActive 图与 Host native export adapter 的实际 slot 安装已有后续
证据；internal child 对象精确 creator Ctx 的后续证据见下，单对象远端撤销及
迁移/恢复/T24 尚待完成。
M0–M5 和 T01–T24 的最终验收范围保持不变，目标没有标为完成。


## M0 补查：Loading 子树的公开 effect 闭锁扩展

真实 Loading 测试发现：generation token 取消和公开 `FiberView.shutdown()` 已经
关闭 provider/listener/child，但 `Ctx.effect` 在 Loading 保留子树 closing 豁免。
原生回归 `shutdown_keeps_cleanup_from_loading_apply` 证明普通 shutdown 有意允许
协作取消后的清理 effect 登记；直接移除豁免会破坏现有行为。原先的仅适配层 Go
结论在这个交错上证据不足，本轮通过明确扩展点补齐。

本仓库 rutis 新增公开 `FiberView.seal_effects()`，在 native admission 锁内永久
关闭该 fiber 子树的新 effect factory，包括 Loading、原始 Ctx 及其 isolate。
托管终态先 seal，再同步 shutdown；普通 shutdown 的协作清理语义保持原样。
已经进入的 factory 及 apply 返回的 Effect 仍由既有 native drain 清理，没有
替代 Ctx、第二套插件内核或 Cordis fork。调用者须持有 owning view 直到 shutdown
确认，托管适配器通过现有 native view 和独立清理任务保证这一点。

`generation_registration` 原生测试在真实 parent 的 Loading child 上验证继承
闭锁：新 factory 不执行，provider/listener/child 拒绝，root 仍可登记 effect；
解除 apply 后，既有和返回的两个 disposer 都被清理。原有普通 Loading shutdown
清理测试及 protocol managed 的托管 Loading 闭锁测试同时保留并通过。

维护责任在本仓库 rutis 原生生命周期模块；测试纳入常规 workspace/CI。
M0 Go 要求使用包含本扩展的源码构建，并锁定实际 runner image/code 摘要；不能仅
凭 `rutis` 0.3.0 semver 推断未包含扩展的外部发行包满足此行为。正式版本发布和
完整协议冻结仍由 M5 完成。Cordis 继续使用锁定发行包 4.0.1 的公开扩展点。


## M1/M2 私有 session 与多成员 named DI

对象传输从固定双端 fixture 提取为 Rust `HostObjects` 和两端 `RuntimeObjects`。
Host 依继承连接校验 runtime/epoch，actor 共用 epoch-wide ObjectIds/Imports，
保持独立 member native 表与原始 context。dispatcher registry 按精确 SHA 分别
合并，保留完整 source view；回调按嵌套完整签名匹配，prototype selector 拒绝。
生命周期组合入口把对象和控制消息放到同一泵。默认 frozen driver 的服务装配
见后面的冻结默认 driver 证据；事件能力继续拒绝。

真实 Unix 私有流用例运行两个 Rust、两个发行 Cordis native member，双向注入
两份精确 bundle 的生成客户端。业务在原始 apply 调用 stateful connect/query、
重复对象和循环属性、owner passback，以及借用回调和登记后代。handler throw /
Rust panic 后仍等待实际 child；丢弃 value 与 object-valued waiter 不释放运行中的
执行 pin，结果 envelope 最终被拒收并进入连续回收前缀。源连接伪造被提前拒绝。

两端控制队列共用串行 ACK 屏障，空队列 flush 必须等待已经发出的控制确认。
真实 broker 延迟 Release ACK 的测试验证第二次 flush 不提前完成；Rust 丢弃
第一位等待者后屏障仍有效。revoke/reject/end 也经过该屏障，不能取走另一调用的
Accept 队列并让它提前把参数交给作者。

完整服务交付拥有 RootDelivery receipt，owner pins 确认后仍须等待整表 Accept。
丢弃 receipt 关闭预留消费者并拒收全部新根；额外第三个 Node provider 的两枚
真实 native pins 降到零。该额外控制用例没有给预留 Rust 接收者构造业务成员。
两端另验证已准入但未 execute 的 pin 在断连关闭时不阻塞 native drain；运行中的
执行保留到实际完成。Host 丢失 owner ACK 不自动确认执行完成。

移除 Rust 必要服务的实际 Disposer 触发 native 终态，并通过 closing 控制通知 Host
撤销 Node 消费者，旧原始 Ctx 的 effect 被拒绝。closing 是意图，不是 stop ACK。
连接 epoch 的回收在所有成员关闭后仍可进行，Host 检查前缀并通知所有 native
owner；不为此伪造新业务 activation。用例核对所有 native 清理计数和 Node 退出。

约定、顺序和边界见[私有 session](../protocol/session.md)。管理/装载入口仍是标注
fixture，Rust runtime actor 在 Host 测试进程中，Node 使用实际子进程。尚不能把它
作为 frozen plan → RuntimeReady → HostProxy Active → activate 的完整装配证据。
独立包双语言共享/单独拓扑、非必要 child 单对象远端撤销、StopUnconfirmed
与 reaping、事件、真实旧桥迁移和 T24
继续保留在 M0–M5/T01–T24 最终验收中，目标未标为完成。

## M2 冻结 required 表与多消费者 source

`DeploymentObjects` 保留同一个 prepared plan，绑定首代成员和 runtime group，检查
完整 provides 表，并按 frozen exports allowlist、instance/native provider 和精确
bundle 选择消费者的整张 required 表。缺路由或 provider 未发布不关闭消费者；
签发之后禁止重复 required 交付或 unchecked rebind。

两端私有 `object/route` 从原始保留清单复制独立 stage，沿用真实对象和原 dispatcher。
Host 只允许 own view 改为已选择的 `PreparedRoute.source`，foreign proof 保持原样；
然后跨多个 owner 和 bundle 原子签发表，并等待每个实际 native commit。原整图与
整表 commit 对完整 view 的核对没有放宽，原 staging pins 保留到 native 关闭。

现有真实 `session_ipc` 扩展到额外一对消费者，运行两 owner / 两 bundle 的 native
注入，其中一项经 frozen native adapter 选择。同一 owner 根保持 object identity，
消费者 source 不同；各自原始 apply 仍执行 stateful query、循环属性和回调。
发布前、重复交付、错误 group 与旧代 rebind 均有实际拒绝证据。

故障用例篡改最后一份 owner 清单，拒绝后暂存 pins 回到原值；另一用例拒绝第二个
实际 commit，首个已转换的 delivery pin 和余下 stage 均清理。旧消费者继续运行，
有效 token 的 source 替换拒绝，失败消费者没有业务构造。最终连续前缀回收覆盖这
些拒收 envelopes，Node cleanup 4 / Rust cleanup 3 及真实 Node 退出均被检查。

路由计划来自实际 prepare，冻结原字节后删除源包；计划内 Node entry 是明确拒绝
装载的元数据 fixture，实际 Node/Cordis 使用 session 子进程。详情见[冻结根交付](../protocol/deployment.md)。
这完成了根选择与交付装配层的证据，默认 service-capable driver 与首代 HostProxy
发布链和内部 child creator context 的后续证据见下；runtime/snapshot 监督租约、事件、
真实旧桥迁移、共享/单独双语言完整拓扑与 T24 仍待完成。全目标保持开发中。

## M2 默认服务 driver 与冻结子进程

Rust `NativeDriver::with_services` 在纯声明阶段核对完整 factory/port/bundle 表。
两端 `ObjectSession` 从成功私有 hello 获取唯一 runtime/epoch，在构造前 reserve
同一个 host admission gate。`plugin/start.required` 接收整张 required 表，先验证
所有图和精确 bundle，再接收并等待 Accept ACK；随后才安装生成客户端、调用
Rust factory 或动态加载 Node 业务模块。`RootDelivery.start(instance)` 取得回复后
也检查权威 broker 中每个实际 grant 已 Accepted，不能用普通成功响应替代确认。

Rust 托管入口在原生 generation Ctx 进入 apply、绑定 native view 后创建实际
Exports 并绑定 actor，随后进入业务 apply。TS 保留原 plugin.apply 的返回形态及
this，在同一原始 Cordis Context 执行相同步骤；模块导出 canonical SDK 的
`protocolPorts: NativePorts`，声明检查在 native mount/apply 前完成。
Native Active 后收集完整 provides，复用同一 Exports，必要 guards 属于本代原生
子树。Rust 先读取完整表再等待 guards，声明服务缺失时失败并 join cleanup，
不会为缺失服务挂起 Pending guard。原无服务 driver 入口继续保持兼容。

冻结 Node catalog 加入 group 的 bundle 原字节字符串，以原 SHA 作为键。编译后的
node-runner 在业务 import 前校验摘要并准入 bundle；配置和 runtime/epoch 仍只走
私有 hello，不通过 argv 伪造成员身份。默认驱动只实现 object.scope/callback.borrow，
event.parallel/serial 在 hello 拒绝，没有宣称 broker 事件已实现。

`native_runner_ipc` 使用四个独立包：Rust provider/consumer 链接同一个静态 catalog，
Node provider/consumer 是不同 module 的实际包，共享一份 canonical Cordis/SDK。
原包在启动前删除，真实 Rust 子进程与编译后的通用 Node runner 全部从冻结路径
通过 fd 3 接线，没有 fixture start/activate handler 或手工 grant。
两端原始 native apply 执行双向 DI、状态 query、重复 Connection、session/agent
循环导航、owner passback、borrow callback 与登记 child，并核对原始 native Ctx。

真实 Host 分别延迟 Rust/Node required 表的 Accept ACK，期间无构造/module import；
放行后两端启动完成。另在相同 ACK 延迟窗口 stop，各端 late business code 都未
执行；stop 加入原生清理，最终每个 runtime 的两个已进入成员均清理且子进程
正常退出，然后才释放 snapshot。Rust 另验证缺失 provides 的回滚不会等待无服务
guard；TS 另验证 missing bundle、ports mismatch、catalog 原字节摘要，以及有/无
服务传输的 module load/stop 交错。

这是 frozen runner 与默认服务生命周期的证据。此用例直接操作控制层的 stage 和
activate；后续 Host 原生发布图证据见下。不计作整个 M2/T01–T24 完成。
内部 child 精确 creator Ctx 的后续证据见下；单对象远端撤销、共享/单独完整
拓扑、快照监督租约、Linux descendant reaping、更新恢复、broker 事件、真实
旧桥迁移与完整 T24 仍待验收。

## M2 首代 Host 原生发布与同步关闭

`host::{HostGraph,HostProxy}` 挂载真实 rutis 依赖 fiber，inject 精确 RuntimeReady
和 prepared route 的 ObjectProxy TypeKeys。缺依赖保持原生 Pending，无 activation
或远端构造。原始 Host Ctx 先登记 rollback effect，再捕获真实 Arc 和冻结的
owner/object/interface/bundle/source，按 Host 顺序保留 remote 与 Host SDK activation。
编号分配与 broker reserve 在同一 metadata 顺序内，避免并发 fiber 的准入倒序。

整张 required 表经真实 broker 和默认 driver Accept 后启动，完整 ready table 校验后
为启用的 exports 建独立 mirror source，复用保留的 native 清单和 dispatcher。
Host 的真实 SDK 接收/确认整表，再绑定原始 Ctx；实际原生键保存 ObjectProxy，
availability 为 false。独立发布任务观察原生 Active，再等待远端精确 activate ACK，
确认 gate 仍开才发布、设置 availability 和 refresh。原生初次 await 的 Pending
不作为失败；同组成员没有 ready 屏障。

可选 `get_as` 能读取尚未可用的注册值，因此 SDK facade 的 delivery/方法/缓存属性
另检查 publication，属性导航继承相同检查而不改变 wrapper 身份。单元用例验证
暂存根仍被 SDK 保留，循环属性返回的旧 wrapper 在关闭后拒绝访问。

stop 在调用点关闭 availability 和 gate、封闭原始 Ctx，并同步排队远端 stop，
早于 Host native effect drain。配对锁保证 start/stop 入队顺序，但不跨 await。
Loading start 的关闭结果属于 ready 失败，独立 cleanup effect 仍 join 真实 stop ACK；
配对清理还 join broker revoke 与本地 SDK Release ACK，cleanup 错误不丢弃。
已断连 epoch 已有撤销证据，不要求它回复新的 revoke；这不替代远端 native stop。
Host member 的弱关闭租约在 broker 锁外同步封闭 provider 和
实际捕获它的消费者；epoch 租约也封闭尚未进入 apply 的 Pending 代理。

`native_runner_ipc` 的 HostGraph 用例从同一四包冻结快照启动实际 Rust/Node 子进程，
不直接操作 stage/activate。Rust activate ACK 延迟时，Host provider 虽 Active，
SDK 与原生 availability 仍关闭，Node consumer 保持 Pending；同组另一位 Rust
成员独立发布。放行 ACK 后 refresh 启动消费者，捕获 Arc 与真实原生 slot 相同。
撤销 provider 的 future 尚未 poll，两个旧 Ctx 与返回对象已拒绝新操作。

取消用例分别持有两端真实 Accept ACK，在 Host Loading 时发 stop，查询远端 Closing
并检查无构造/import；丢弃首个 stop waiter 后，重复 stop 仍 join 清理。
Node activate ACK 延迟时仍完成 stop，旧 ACK 到达不恢复
availability 或旧 Ctx。已断连 epoch 封闭 missing-route Pending，拒绝新挂载。
全部实际 cleanup 计数与子进程正常退出确认后才释放 snapshot。

API 顺序和边界见 [Host 原生图](../protocol/host.md)。这完成首代 instance 路由的
发布/关闭纵向证据，尚不拥有 snapshot 监督租约、Linux descendant reaping 或恢复
换代。Host native export adapter 的实际原生 slot 安装与内部 child 精确 creator
Ctx 证据见下；单对象远端撤销、共享/单独完整拓扑、broker 事件、真实旧桥迁移与 T24 仍需完成；
全目标保持开发中。

## M2 Host native export adapter 的真实安装与失效

`HostGraph::mount_native` 使用纯 `NativePorts` 元数据，在实际 native mount、validate
与 apply 前核对 frozen `native_services` 的非空子集和精确契约。此入口只接受
exports，插件自己的其他本地依赖仍由原生 injects 管理；它不执行远端 factory 发现。
同名 native slots 挂载时占用，关闭后也不能 unchecked rebind。

实际业务插件进入自己的原始本代 Ctx 时，先登记协议 rollback effect，再创建
Host SDK owner Exports 和 dispatcher binding，随后进入业务 apply。缺本地依赖
保持真实 Pending；Active 后读取完整声明服务，再安装实际原生 children guards。
先读取服务表保证缺少声明服务时直接失败并 join cleanup，不挂起缺服务 guard。

Host SDK owner 与 mirror recipient 使用 HostGraph 的同一 activation 分配器。
完整表保存到 frozen native provider，保留原 object/dispatcher/bundle 清单，经
独立 mirror source、真实 broker 和 SDK Accept 后，在 prepared native key 下
安装实际 ObjectProxy。消费者捕获这份原生 Arc，再核对并签发各自冻结 route 的
独立 source/grant；没有放宽第三方转交或完整原始 commit 检查。

Gate 的弱关闭 hook 在原生 admission 锁与 gate 锁之外同步撤销 Host 权威和实际
捕获它的消费者、关闭 availability 并封闭旧 Ctx。SDK scope 排干由独立任务取得
actor 锁，避免 gate 准入检查在 SDK metadata/import scope 调用栈中重入同一锁。
HostProxy 使用相同关闭顺序，proof 读取和最终 SDK 发布也不持有 pairing 锁。
单次 gate 与 broker/SDK 的终态检查使迟到 publish 不能重开旧代。

新增真实 `native_runner_ipc` 用例启动冻结目录里的 Rust image 和通用 Node runner。
两个消费者在 RuntimeReady 后仍 Pending，Host native 插件满足本地依赖后才刷新
并启动。两端捕获的 Arc 与实际 native key 完全相同，owner 为真实 Host activation；
原始 native dispatch 执行 stateful query、返回对象、循环属性、owner passback、
borrow callback 与登记 child，query 检查原始 Ctx 的 typed 服务身份。

空 ports、契约不匹配与重复挂载实际拒绝；另一位故意缺少声明服务的 native 插件
ready 失败，真实 cleanup 一次，native 为 Disposed，没有残留 ObjectProxy。
移除有效 typed 服务后，准入检查在 adapter stop waiter 被 await 前同步关闭两个
runtime 的 Host 消费者，旧 Ctx/返回客户端拒绝操作；其他 remote providers 独立
保持可用。最终 join broker revoke、SDK Release ACK 与 native shutdown，确认两个
实际子进程清理并正常退出后才释放 snapshot。API 见[原生适配器](../protocol/native-adapters.md)。

本轮首次工作区回归发现内存传输的并发 Accept 竞态：一条 flush 取走控制队列、
尚未完成 broker 接收时，另一条空 flush 可以提前返回，生成客户端取得对象后
立即调用被拒绝为 `grant not active`。`memory::Endpoint` 现在将取队列、broker
确认与 native pin 更新放在同一 receiver 屏障内；现有四线程并发用例扩展到
8 轮 × 16 次 connect/query，共 128 次实际状态调用。该修复不改变 wire 格式。

这些是首代 native export adapter 和并发接收确认的证据，内部 child creator Ctx
的后续证据见下；不代替监督快照租约、Linux descendant reaping、恢复、broker 事件、真实
旧桥迁移、共享/单独完整拓扑或 T24。全 M0–M5 / T01–T24 目标仍在开发中。

## M1/M2 内部 child 的实际创建者 Ctx

两端 Exports 现在在首次 native 对象登记时保存精确创建者 Ctx/Context，identity
的 owner 仍是原托管根。`with_native` / `withNative` 显式关联 child 的 own 对象，
必须位于该 managed root 的真实原生子树；Rust 使用本仓库新增的公开只读
`Ctx::is_within`，TS 使用发行 Cordis 的公开 fiber/parent 链，没有 child 全局代理。
重复对象、其他接口或 source 的登记不修改第一次创建者，root reexport 也不能
复活已取消的旧对象。SDK facade 与 owner passback 仍经过 broker。

实际 pin 后的调度从同一 object entry 取得原始 native Ctx，而不是统一使用
`RuntimeObjects.bind` 的根 Ctx。handler 和登记后代结束后，返回的 own 对象继承
该创建者；纯值结果也检查创建者仍开放。关系快照的新 own 属性继承对象的第一
创建者，后来的包装不能修改它；foreign proof 保持原样。

生成客户端的 borrow callback 可通过其 Caller 的 `bind_native` / `bindNative`
在编码前关联到实际 child Ctx。关联只登记本地 weak 身份，不签发 grant 或 pin；
参数仍走生成编码、broker 整图授权与真实 Accept ACK。Rust 保存原始代的取消
token 所属 Ctx；TS 在实际创建者自己的 effect 中登记代关闭标记，并检查公开
state/uid，普通 child 重载不能重开旧对象。

真实 frozen `native_runner_ipc` 的两端 providers 现在在原生内部 child 里提供
Database，connect/query 检查实际 child Ctx/Context；两端消费者的 borrow callback
也由另一位内部 child 创建、关联并执行，校验其实际创建者。返回 Connection、
循环关系、owner passback、双向重入与登记后代继续使用同一个 SDK/broker/private
fd。Host native adapter 的业务 provider 同样在真实 child 中运行，必要 typed
服务移除继续同步封闭托管根与两个 remote consumers。

新增 Rust services 用例验证跨树 Ctx 拒绝、child 停止后根仍 Active、旧 Connection
的新 pin 拒绝，以及 root reexport 不能复活。新增 TS exports 用例验证第一次
创建者不可改写、跨树拒绝、child 停止后的新 pin/dispatch/纯值结果拒绝，已有
execution pin 仍持有真实对象。

本轮工作区的两个运行曾在 Linux 子进程 exec 点返回 `ETXTBSY`；失败发生在业务
进入前，未归为协议通过。快照写句柄在 materialize 返回前关闭；并发 fork 短暂
继承其他线程写句柄是候选原因，具体持有者尚未捕获。最初 fixture 中只对该内核
失败执行最多 8 次有界重试（总退避 255 ms）；现在由下述 SDK helper 执行相同
有界 exec 重试。其他错误或耗尽仍失败，没有业务重试或忽略用例。原有真实
native_runner_ipc 的 6 项随后通过；完整更新/恢复启动策略仍属 M3。

API 及边界见[创建者上下文](../protocol/native-context.md)。必要 child 服务失效已有
托管根撤销证据；非必要 child 对象在 owner 拒绝新 pin 与调度，单对象远端撤销及
执行/慢 disposer/ACK 交错已有上方新增证据，完整双向故障矩阵仍需补齐。本阶段不计作
T15 或全 M1/M2 完成，M0–M5 / T01–T24 的最终目标不变。

## M3 冻结 runtime 与独立 Linux 回收

新增 `process::FrozenProcess` 和 SDK `rutis-protocol-reaper` 二进制，从实际
`SnapshotGroup` 的路径启动 Rust/Node。Host 私有 fd 3 承载既有 SDK/control；
helper 的私有 fd 4 只传启动与 OS 回收证据，runtime exec 时关闭它。stdout/stderr
仍只作诊断。helper 是单线程 subreaper，Host 不修改全局 subreaper 或争抢其他
模块的 waitpid。API、Linux 前提与证明边界见[进程回收](../protocol/process.md)。

显式 terminate 在调用点关闭真实 peer，原生 RuntimeReady、HostGraph 和已捕获
provider 的消费者同步封闭准入；独立 OS 任务不等待 native effect/disposer。正常
断连给 runtime 默认 200 ms 宽限，显式 terminate/最后一个 handle 丢弃直接请求
终止。helper 逐轮终止、wait 自己的子进程与孤儿后代，只有实际 ECHILD、原始
runtime exit status 以及 Host 对 helper 正常退出的 wait 共同形成缓存 `Reaped`。
丢弃 receipt waiter 不取消回收，也不保持 runtime 存活。

原有 frozen 默认 driver、HostProxy/Host native adapter 用例现在使用同一组件，
继续检查实际正常退出与 native cleanup。新增用例由实际 Node provider 在两个
脱离会话中派生子、孙进程；原生 Host 消费者的实际 disposer 被受控等待阻塞。
终止后旧 Ctx 与连接拒绝操作，主进程/子孙/helper 四个 PID 实际消失且取得 OS
receipt，但消费者仍持有快照，目录保持存在。放行消费者实际清理后才删除目录；
HostProxy 丢失 native stop ACK 仍是错误，旧 instance 不能借 OS receipt 重挂。

另外三项验证：丢弃 waiter 后仍 Running，最后一个进程 lease 丢弃仍独立回收并
缓存同一证明；无可信 helper receipt 时隔离并保留快照；启动大描述符过程中丢弃
future 时，helper 确认未创建 runtime 或完成回收，快照释放。回收与取消启动各有
独立 5 秒测试期限。缺少证明测试使用已知不会 fork 的 `/usr/bin/true`，不是实际
kill 权限失败的证据；该 fixture 才能显式清理自己的文件。生产 API 没有清除
quarantine、强行成功或自动新 epoch 的入口。

增加用例后，两个回归曾在多份大镜像同时写入时遇到 `Disk quota exceeded`，发生
在 `native_plan` 准备包文件阶段，未归为协议通过。大型 fixture 现在由异步
semaphore 限制为最多两个并行，等待发生在各用例期限前；用例内部的多个真实
runtime、双向调用与停止交错仍并发。最终结果以下述完整重跑证据为准。

最终 `cargo test --workspace` 为 499 passed / 0 failed / 2 ignored，其中协议 crate
132 项、native runner 10 项全部通过，旧桥真实 Node TCP 回归继续通过。
`cargo check --workspace --all-targets`、fmt、协议 crate clippy `-D warnings` 与
TS check/build/test（75 passed）通过。最终 native runner 的丢弃 lease 用例还实际
核对 runtime 的全部可读 fd，确认业务 socket 存在但 helper 证据 socket 未继承；
异常 monitor 双向关闭通道，避免 helper 因证据写入背压而停在 OS 回收之前。
11 份协议/验收文档的 42 个相对链接全部存在。当前沙箱禁止本地 TCP/Unix listen，
完整 Rust 与 TS 测试已获准在沙箱外执行；其绑定失败未归为实现测试通过。

OS worker 自动持有冻结组到回收证明；未确认时保留到本 Host 生命周期。原生
消费者租约当前由调用方显式拥有。还需实现全组 epoch supervisor 自动捕获全部
旧成员/失效消费者、等待实际清理与 OS receipt、收敛死亡 owner 的 execution
pins、精确开新 epoch，并完成配置/代码/runner 更新与回滚、StopUnconfirmed、
RecoveryBlocked/Quarantined 的管理和真实 OS 失败注入。此阶段只提供 T20/T23
部分平台证据，不能记作完整 T20、T23 或 M3 完成。
