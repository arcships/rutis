# 实验命名原生服务绑定

Rust `services::{Bundles,NativePorts,ServiceTable,StagedServices}` 与 TS `services.ts`
把精确 bundle、wire 服务名与本地原生服务键绑定。当前实现是生产装配所需的 SDK
基础；默认生命周期 driver 与首代 [HostProxy 原生图](host.md) 已通过实际冻结
子进程验证。监督恢复和完整迁移仍待验收，不能用这些 API 或单元测试宣布 M2 完成。

## 本地绑定与装载

`Bundles` 从完整原字节准入，拒绝同 id/version 不同 SHA 的两份 bundle。端口声明
必须绑定已准入 SHA、精确版本和已有 interface；本地 Rust TypeKey/TS 原生服务名
不进入 wire。`NativePorts.check` 在 Rust factory 构造前核对完整 provides/requires；
Node 从授权的模块读取 `protocolPorts`，在 native plugin mount/apply 前核对。
同一组端口不得重复本地键，避免两个 wire 服务抢占同一个 native slot。

Rust 的 provide 使用生成客户端作为契约类型见证，读取真实 `Arc<T>` 并调用生成
export adapter；require 把已接收对象绑定成生成客户端。`scope` 为每个 activation
隔离全部端口键，`install` 先检查整张 required 表再安装 native providers。
客户端必须属于本代的根 scope 1。SDK 收集 provides 时读取这个私有作用域，业务
代码仍通过原始 Ctx 的原生 inject/require 工作。

`StaticFactories.mount_bound` 在 native build 前检查 factory.injects 与 required
端口键完全一致；作者元数据只读取一次，检查和 native graph 使用同一份声明。
`NativeBindings.adopt` 把 import provider disposers 交给 ManagedActivation 的 stop
确认。装载/采用失败时，调用者必须等待 `rollback`，不能把 Drop 的后台兜底当作
清理确认；迟到采用失败会把 disposers 留给调用者继续回滚。

Rust `guard_exports` 需要本代 ManagedActivation 的原始业务 Ctx，把必要服务 guards
装载为该成员的原生子插件。发布前等待 guards Active；关闭成员自动等待子树，
不会在 runtime root 每代留下 Pending guard。必要服务单独移除也能撤销准入。

TS `NativePorts.mount` 检查全部生成 facade 及 inject 声明后，使用实际 Cordis
`ManagedActivation` 安装隔离依赖。保留 native inject 的 intercept 配置，生成
facade 兼容公开 `Service.tracker` 探测。依赖 provider 使用 Cordis availability
check；SDK 收到 import 撤销后调用 `refreshImports`，通过原生 notify 闭锁旧代。
imports 的根 scope 同时必须绑定本代 gate。默认 driver 已接这些调用顺序。
该 mount 是异步确认入口：部分 provider/hook 注册失败时先等待已登记资源回滚，
再返回错误；native driver 同样使用 `ManagedActivation.mount`。直接同步构造失败
会携带 `NativeMountError.cleanup`，嵌入者必须等待该任务；回滚失败保留原始异常
及清理异常，不得计作成功。

两端管理器记录传入 apply 的原始 Ctx/Context。Rust gate 使用 native view 的弱引用，
close 在调用点同步调用公开 `FiberView.shutdown`，而非仅取消 token 或排队 refresh。
Loading 时先调用本仓库 rutis 的公开 `FiberView.seal_effects`，普通 shutdown 的
协作清理语义保持原样；详见 [M0 补查与维护责任](../docs/protocol-plugin-implementation.md#m0-补查loading-子树的公开-effect-闭锁扩展)。
业务 apply 在 native view 完成绑定后才取得 Ctx，避免多 worker 装载/stop 竞争窗口。
已经进入且失效的成员终态为 Disposed；尚未进入的缺依赖成员仍可 Pending 等待首次
宿主授权装载。stop join native 清理及全部 import providers，丢弃 waiter 不放弃清理。
native 终态错误或 disposer 错误仍报告失败，不推断清理成功。

## 整表暂存与授权

每代只 stage 一份完整 provides 表，各服务包含正数且不重复的 stage 编号和完整
`DraftGraph`。根必须是真实本代 native-owned 对象，不能以 foreign facade 替代。
每个 own 引用绑定同一 activation 与该服务独立 source；source 是下面元组按
[契约规范编码](README.md#json-与描述符)生成 UTF-8 字节后的 SHA-256，使用与
callback signature 一致的长度/类型标记编码，不使用 JSON 打印形式：

```text
["protocol-service", activation, wireServiceName]
```

所有接口、快照关系、ownership、可达性和 foreign proof 使用既有 graph/draft 校验。
foreign 属性只带原接收证明，不能增加第三方转发能力；最终授权仍由 broker 决定。
`validate_table` / `validateTable` 是 shape/契约检查，本身不签发任何 grant。

Rust `offer_table` 校验整张表，再在单次外层 broker 事务中准入所有服务，包括多个
bundle。最后一图被拒绝时，前面图的对象视图、delivery id 和授权都不提交。
当前事务通过账本复制实现，开销仍需 T24 测量，不能推断达到性能目标。

owner 的 `StagedServices.commit` 保留私有 native manifest，先检查全部 proposed
graphs 的 root、对象、精确 view、快照和 scope，再转换第一个 staging pin。
转换中途失败释放全表 delivery pin 和剩余 staging pin，并把 handoff 置为终态。
重复成功提交必须完全一致；改变 token、对象、view 或快照的重传拒绝。
空 provides 表仍检查 broker activation/scope 和 native owner 准入；原生关闭后不能
用没有对象引用的 commit 获得成功确认。
Host 必须在 owner 确认 pin 后才暴露 grants，失败时撤销 broker 的完整新 grant 表，
并使 recipient SDK 记录完整拒绝 envelope，保留连续 receipt 前缀。

同一原生对象表、同一 bundle 的已登记 dispatchers 可以合并到调度端；合并不签发
grants，也不能扩展 view。跨 bundle 的生产 dispatcher registry 和 prepared route
source 的宿主授权绑定尚需接入，不能忽略 source 来绕过授权。

## 当前验证边界

Rust `tests/services.rs` 运行真实 native provides、两个独立消费者的 generated client
注入及 stateful connect/query、循环导航和 owner passback。内存链路使用既有权威
broker、真实对象表及 decoder；多 bundle 后半图伪造证明不留下前半图授权。
另覆盖完整 native manifest、pin 失败全表回收、重复提交和端口/契约拒绝。

TS `tests/services.test.ts` 使用发行 Cordis、真实 JS 对象和生成 adapters，验证隔离
注入、循环 facade、原始 apply context、必要子服务失效、整表 pin 回收和 dispatch
合并。其 owner commit manifest 来自明确标注的本地 fixture，没有实现或替代
Host broker；不能把这些测试计作私有 IPC 的完整命名服务发布验收。

两端消费同一 `services-corpus.json` 的 12 个合法/非法服务表用例。
既有 `objects_ipc` 继续提供真实私有 fd 的跨语言对象调用证据。完整 HostActive
发布、原生 Pending 业务图、internal child 对象精确 creator Ctx、迁移/恢复和
T01–T24 全部运行时证据仍未完成，原设计范围保持不变。
