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
| M1 | 开发中 | 两端草稿编码、事务授权与生成 dispatch 已接入私有 socket；完整失败交错、独占资源跨语言清理、生产连接的控制流与复用评估仍需补齐 |
| M2 | 开发中 | prepare、冻结快照及 Rust/Node 多成员控制入口已有证据；完整服务图与发布装配、快照监督租约、真实旧桥迁移和 T24 测量尚未完成 |
| M3 | 待完成 | 调用取消/完成、更新/失败回滚、组恢复屏障、Linux 进程和受管后代回收 |
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

可执行证据：Rust bindings 8、contracts 5、drafts 5、exports 10、factories 6、frames 5、graphs 7、objects 13、managed 11、objects_ipc 1、prepare 21、lifecycle 11 项（合计 103，其中 lifecycle 一项是子进程入口）；TS bindings 7、contracts 5、drafts 4、exports 8、frames 5、graphs 5、imports 6、managed 8、lifecycle 9 项（合计 57）。共享语料包含 JSON 35、描述符 50、wire value 32、回调签名 9、对象图 31、帧 10、lifecycle hello 17 个用例。Rust 使用两个真实线程同时交付/释放；两端另验证原生失效时的执行 pin 和慢对象清理。

Linux `objects_ipc` 运行独立 Node 进程与真实 managed rutis/Cordis Ctx，使用继承的私有 Unix stream fd 3，stdout 诊断另行读取。Rust 权威 broker 从固定连接取得 Node activation，校验 family/version/raw SHA/capabilities；两端生成客户端实际 connect/query，保留同一状态对象和 session→agent→session 循环，owner pass-back 仍走 broker。两端借用 callback 重入第三层调用并等待登记子任务；保存的 callback 到期后拒绝调用。Node 在业务 dispatch 前通过控制帧确认参数 grant，避免回调/pass-back 读取尚未 accepted 的引用。丢弃 Rust waiter 后 Node 实际执行仍持有 execution pin，完成后才释放。

同一私有 socket fixture 已完成 Node 接收 SDK 的前缀提议、broker 独立检查、owner 回收通知、ACK 后 SDK 清理；Rust 接收端也走同一 broker 和 Node owner 通知。活的首份 grant 阻止后续已释放 borrow 的回收，旧 id 的控制重传不能恢复授权，低于水位的接收和 pin 拒绝。当前约定见 [protocol README](../protocol/README.md)。这是 conformance fixture，不是可部署 runner；多 activation 连接的前缀、断连/取消/finished 交错与 supervisor 尚未验收。

本阶段工作区回归：`cargo test --workspace` 469 passed / 0 failed / 2 ignored，新对象协议私有 Node socket、Rust 生命周期子进程、冻结目录的通用 Node runner 与旧桥真实 Node TCP e2e 均通过。两个 ignored 是依赖外部 min-cordis/dsh 检出的 host e2e 与需要真实模型后端的 agent e2e，不能计入通过。`cargo check --workspace --all-targets`、全仓 fmt、协议 crate clippy `-D warnings`、TS check/build/test 通过；没有执行外部 min-cordis/dsh 整体迁移验收。Linux Rust CI 安装锁定 Node/TS 依赖并运行该新互通测试，旧桥的跳过变量不跳过它。

M1 后续必须补齐以下内容，之后才能称为 M1 完成或冻结该实验协议：

1. 将已验证的两端草稿编码、broker 及 native dispatch 接入生产 runner；当前私有 fd 测试宿主/Node fixture 的固定单 bundle、单 member 初始化不能代替 prepare/生命周期协议。
2. 补齐双端完整结果丢弃、scope 回收、独占资源 disposer、handler/子任务失败收敛及原生依赖失效交错；Rust 已有状态返回、owner facade、循环图与嵌套 borrow 内存证据，不能等同于跨语言验收。
3. 将已有严格帧/握手检查及只读 prepare 接入生产连接状态机；两端生命周期控制已有证据，实际 native 路由绑定和完整 HostActive 装配仍待 M2。
4. 补齐多 activation 连接的回收 ACK、epoch 记录回收、断连/迟到消息与取消/finished 交错；私有 fd 的正常终态回收证据不代替故障控制流和监督恢复。
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

默认 native driver 尚未安装命名对象服务/事件 transport，因此 hello 会明确拒绝
相关能力和服务声明；现有对象互通 fixture 与该控制流的集成仍未完成。
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
