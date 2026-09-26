# 实验 runtime 生命周期控制

Rust `lifecycle::{Runner,NativeDriver,Hello,RuntimeReady}` 在真实 native fiber 外记录
宿主意图，不实现第二套插件内核。当前控制入口已能在私有帧连接中工作；TS 对应控制
入口、完整对象服务适配、宿主业务图装配与 supervisor 仍在开发，协议尚未冻结。

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

嵌入 Rust runner 的入口调用 `serve(private_stream, NativeDriver::new(root, factories))`。
stream 必须来自宿主私有连接；不使用 stdout/stderr 承载协议。`serve` 在断连后等待
成员清理，嵌入程序随后 shutdown 自己的 native root。当前通用 API 不负责创建或回收
进程树，不能替代 Linux supervisor。

## 成员控制

| 方法 | 入参 | 当前行为 |
| --- | --- | --- |
| `runtime/hello` | `Hello` | 无业务代码执行的声明准入，返回精确身份 ACK |
| `plugin/start` | `{instance, activation}` | 选择冻结成员，运行原生装载，返回 `{activation, services}` |
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
provides 精确一致，缺少或额外服务触发回滚。表项的对象图、接口、grant 和真实 pin
验证还需要后续 broker 适配，当前 opaque table API 不构成这些属性的验收证据。

start 在流的 admission 顺序中保留实例/代，真实构造和装载离开管理锁运行。driver
必须把同一个 admission gate 交给 `mount_gated` 或 `mount_factory_gated`；runner
拒绝返回另一个 gate 的 adapter。stop 即使发生在构造尚未返回时也关闭该 gate，
阻止 native apply 迟到启动。services 的 staging future 在 native Active 后执行。

控制泵不等待整个组：一个成员 Loading 时，其他成员可以 ready/activate。停止 waiter
被丢弃不取消清理；重复 stop join 同一确认。清理未完成或失败时，不允许替换该实例。
原生 gate 失效会关闭协议入口并推进 stop，activate 不能重新打开旧代。断连的 close
hook 同步撤销全部成员，再由独立任务清理；frame 锁不包住 native shutdown。

## 当前能力边界与证据

默认 `NativeDriver` 目前实现原生生命周期，不安装 object/event transport adapter，
因此在 hello 阶段拒绝相应 capability 和命名服务声明。它不能启动依赖这些能力的
最终协议插件。已有对象协议 `objects_ipc` fixture 尚需接入这一控制流，不能通过
宣称能力已实现来绕过这个限制。

`tests/lifecycle.rs` 使用真实 rutis Ctx、effect 与 native fiber 验证独立 RuntimeReady、
一个成员 Loading 时另一个发布、构造/stop 交错、发布前拒绝、失效后旧 Ctx 闭锁、
丢弃 stop waiter 后继续清理、失败回滚、编号/epoch/hello 拒绝和乱序 sibling id。
另从当前不可变 Rust test executable 启动独立子进程，通过继承 Unix stream fd 3
验证 hello、两个不同 native Ctx、activate、独立 stop 和全组 stop，诊断仍走 stdout。
其中 child-entry 测试是该进程入口，不单独计作运行时验收。

这尚未证明同组业务依赖的完整启动图、named service 发布/对象调用、TS 生命周期、
StopUnconfirmed 管理等待、配置更新、故障/后代回收双屏障或真实旧插件迁移。
