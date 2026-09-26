# 实验 runtime 生命周期控制

Rust `lifecycle::{Runner,NativeDriver,Hello,RuntimeReady}` 与 TS
`lifecycle::{Runner,NativeModuleDriver}` 在真实 native fiber 外记录宿主意图。
两端默认服务 driver 已能在私有帧连接中工作；完整宿主业务图装配与
supervisor 仍在开发，协议尚未冻结。

## 引导

宿主从 `PreparedDeployment` 和对应的 `SnapshotGroup` 构造 `Hello::prepared`，
显式提供 runtime 名与新 epoch。快照的成员集合和代码摘要必须与计划一致。
hello 包括 family/version、框架/环境/代码身份、所需 capabilities，以及冻结的成员
entry、配置、配置 schema 原字节字符串和静态服务契约。schema 保持原字节 SHA。

runner 在 hello 时只验证这些声明。Rust `NativeDriver` 核对静态 factory 注册表、
framework/environment 与配置契约，不运行 factory。一个连接只能 hello 一次，不能
重新绑定 epoch。ACK 必须与宿主的冻结身份完全一致；两端拒绝重复或未知 capability。

hello 完成后，宿主通过 `publish_ready` 提供原生 `RuntimeReady`，key 由 runtime/epoch
完整身份构造。该服务不依赖任何成员 apply。宿主代理必须持有
`RuntimeReady::bind_consumer` 返回的租约：断连时同步关闭该代理的 gate，并调用 native
shutdown 预取消其 Ctx；单纯 `ctx.refresh()` 是异步重查，不能替代这个失效屏障。

嵌入 Rust runner 的服务入口调用
`serve(private_stream, NativeDriver::with_services(root, factories, bundles, ports)?)`。
`NativeDriver::new` 保留无服务控制模式，拒绝对象/事件 capability 与命名服务。
stream 必须来自宿主私有连接；不使用 stdout/stderr 承载协议。`serve` 在断连后等待
成员清理，嵌入程序随后 shutdown 自己的 native root。当前通用 API 不负责创建或回收
进程树，不能替代 Linux supervisor。

Node 的通用入口由 `npm --prefix protocol/ts run build` 生成
`dist/src/node-runner.js`。包的 runner 导入该入口，宿主在 `SnapshotGroup::argv()` 后
添加 `node_catalog()` 路径，并继承私有 stream fd 3。这个只读 catalog 由 prepare
快照生成，包含绝对 module entry 和契约、framework/environment/code 摘要及精确
bundle 原字节字符串，不含
实例配置；配置只通过私有 hello 帧交付。不同实例可以共享一个 module entry。
`NativeModuleDriver` 验证实际加载的 Cordis package 版本与锁定 adapter 一致，并
核对启动 catalog 与 hello；只有 start 才动态导入业务模块。

服务模式使用 `ObjectSession` 在成功私有 hello 后绑定唯一 runtime/epoch，不能重绑；
没有在 argv 或配置里伪造 activation。`serve` 自动把默认 driver 的对象和控制流接到
同一泵。编译后的 Node runner 从 catalog 准入全部原始 bundle；业务模块用同一 SDK
导出的 `protocolPorts: NativePorts` 声明本地键和生成适配器。无服务模块可省略它。

## 成员控制

| 方法 | 入参 | 当前行为 |
| --- | --- | --- |
| `runtime/hello` | `Hello` | 无业务代码执行的声明准入，返回精确身份 ACK |
| `plugin/start` | `{instance, activation, required?}` | 接收完整 required 根表，选择冻结成员，运行原生装载，返回 `{activation, services}` |
| `plugin/activate` | `{activation}` | 仅允许 Staged 且 native gate 仍打开的代发布 |
| `plugin/state` | `{activation}` | 返回 instance、activation 与宿主意图 phase |
| `plugin/stop` | `{activation}` | 同步撤销准入，独立推进并等待 native 清理 |
| `runtime/stop` | `{}` | 撤销全组成员并等待所有清理；ACK 后宿主关闭连接并回收进程 |

activation 使用现有十进制字符串编号。runtime/epoch 必须对应本次连接，每个 id 在
该 epoch 只登记一次；同一实例的新 id 必须晚于它以前的代，并等待以前的清理确认。
不同成员的独立 id 可以乱序到达，不能因编号分配与发送交错而拒绝合法 sibling。
旧 id、旧 epoch、未知实例、第二次 hello 和重新绑定 peer 都拒绝。

phase 为 Starting、Staged、Published、Closing、Stopped、Failed。原生装载完成不等于
发布：宿主登记服务并确认代理 native Active 后才发 activate。对象和事件适配器必须
在每次准入调用 `require_published`，同时检查 native gate。完整服务表的 key 必须与
provides 精确一致，缺少或额外服务触发回滚。默认服务 driver 的图、接口、grant 和
真实 pin 已接 session/broker；自定义 Driver 的 opaque table 本身不构成这些证据。

start 在流的 admission 顺序中保留实例/代，真实构造和装载离开管理锁运行。driver
取得 `MountRequest` 中由宿主保留的 instance/activation/member 和完整 required 表，不能从业务配置
推断对象 owner。必须把同一个 admission gate 交给 Rust `mount_gated` /
`mount_factory_gated` 或 TS `ManagedActivation`；runner
拒绝返回另一个 gate 的 adapter。stop 即使发生在构造尚未返回时也关闭该 gate，
阻止 native apply 迟到启动。services 的 staging future 在 native Active 后执行。

控制泵不等待整个组：一个成员 Loading 时，其他成员可以 ready/activate。停止 waiter
被丢弃不取消清理；重复 stop join 同一确认。清理未完成或失败时，不允许替换该实例。
原生 gate 失效会关闭协议入口并推进 stop，activate 不能重新打开旧代。断连的 close
hook 同步撤销全部成员，再由独立任务清理；frame 锁不包住 native shutdown。

Rust gate 关闭同步调用公开 native shutdown，业务 Ctx 仅在 native view 绑定后
交给 apply；仅取消 generation token 不足以阻止 `Ctx.effect`。stop handler 在
返回尚未 poll 的确认 future 前已经关闭 native 登记。import providers 纳入相同
stop 任务，全部 removal 先启动再等待，迟到采用须显式回滚。整表服务 SDK 的
装载/暂存顺序见[服务绑定](services.md)，默认 driver 已使用它。

Cordis 4.0.1 的 native unload 会通过 logger 报告 disposer 错误，却可能成功返回
dispose 等待。TS adapter 使用公开 logger exporter 保留当前成员及其子树在关闭
阶段的 error 诊断，使 stop 失败并保持 Closing，禁止替换。作者在关闭阶段主动
记录 error 也保守地归为未确认清理；这个机制不修改 Cordis。管理 timeout 和操作员
处理仍需后续 StopUnconfirmed API，当前失败或卡住不会自动放行。

## 当前能力边界与证据

Rust `NativeDriver::with_services` 与配置 Bundles 的 TS `NativeModuleDriver` 支持
object.scope/callback.borrow；事件能力在 hello 拒绝。它们先 reserve 同代 gate，
校验完整 required 表并等待 Accept ACK，才安装生成客户端及运行构造/module loader。
真实 native apply 在业务前绑定同一 Exports 与原始 Ctx，支持启动调用中的回调重入。
native Active 后收集完整 provides，必要 export guards 成为本代 native 子树；随后
start 才返回 staged。缺少声明的 Rust export 直接失败并 join cleanup，不留下等待
缺服务的 Pending guard。activate ACK 才开放普通远端 execute。

`native_runner_ipc` 从四个独立包的冻结字节启动真实 Rust 子进程与编译后的 Node
runner，在原包删除后运行双向 DI、状态对象、循环属性、owner passback、两层回调。
真实 Host 延迟两端 Accept ACK，验证构造/import 未开始；期间 stop 阻止迟到业务。
每个实际 runtime 正常退出，native cleanup 都被核对。该用例直接操作控制层，仍不
构成 RuntimeReady → HostProxy Active → availability/refresh 的完整 Host 图证据。

`tests/lifecycle.rs` 使用真实 rutis Ctx、effect 与 native fiber 验证独立 RuntimeReady、
一个成员 Loading 时另一个发布、构造/stop 交错、发布前拒绝、失效后旧 Ctx 闭锁、
丢弃 stop waiter 后继续清理、失败回滚、编号/epoch/hello 拒绝和乱序 sibling id。
另从当前不可变 Rust test executable 启动独立子进程，通过继承 Unix stream fd 3
验证 hello、两个不同 native Ctx、activate、独立 stop 和全组 stop，诊断仍走 stdout。
其中 child-entry 测试是该进程入口，不单独计作运行时验收。

TS lifecycle 测试另外验证异步 module load/stop 交错、关闭阶段 child disposer 异常
不会产生成功 ACK，以及冻结声明和相同 hello 语料。共享 lifecycle corpus 有 17 个
准入用例，两端校验错误类别。Rust prepare 测试从原包删除后的冻结路径启动实际
Node executable、通用 SDK runner 与两个不同代码包，通过 fd 3 验证慢成员 Loading
期间快成员发布、独立 native Ctx、stop、旧 Ctx 拒绝迟到 effect 和全组 cleanup。
stdout 只承载诊断，fixture 的 stdin 仅控制慢 apply 何时返回。

这尚未证明同组业务依赖的完整启动图、named service 发布/对象调用、
StopUnconfirmed 管理等待、配置更新、故障/后代回收双屏障或真实旧插件迁移。
