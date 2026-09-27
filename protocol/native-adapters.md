# Host 原生服务适配器

`HostGraph::mount_native(parent, ports, plugin)` 挂载实际 rutis 插件，并通过同一个
权威 broker 和 Host SDK 发布 frozen `native_services`。它返回 `HostNative`，拥有
原始本代 native fiber；业务 apply 与对象 dispatch 使用同一个实际 Ctx。

`ports` 是 export-only `NativePorts`，用生成客户端作为精确 bundle/interface 见证，
本地 native TypeKey 和显式导出函数从真实业务服务取得对象。声明必须是 frozen
catalog 的非空子集；空表、协议 imports、未声明名称或精确契约不匹配在 native
mount/validate/apply 前拒绝。其他本地依赖由实际插件自己的 `injects()` 管理。
调用者提供的插件值仍由调用者构造，这个入口不执行远端 package/factory 发现。

```rust,ignore
let mut ports = NativePorts::default();
ports.provide::<dyn InterfaceDatabaseService, InterfaceDatabaseClient>(
    "host-database",
    TypeKey::of::<dyn InterfaceDatabaseService>(),
    &bundles,
    exportInterfaceDatabase,
)?;
let native = graph.mount_native(&host_root, Arc::new(ports), DatabasePlugin)?;
native.ready().await?;
```

## 原生装载与发布

服务名在挂载时占用，不能 unchecked rebind。适配器取得两个独立的 Host SDK
activation：实际 native owner 和 Host mirror recipient，编号与其他 HostGraph
成员共用同一单调分配器。每个声明的 typed native port 进入自己的隔离 scope，
实际插件继承该 scope，并保留自己的本地注入；缺依赖时保持真实 Pending。

managed enter 在原始 native Ctx 上先登记协议 rollback effect，再创建
`Exports.managed` 并 bind owner，随后才进入业务 apply。原生 Active 后读取完整
声明服务表；缺服务直接失败并 join cleanup，早于挂载 export guards，避免永远
等待缺失服务的 Pending guard。有效 guards 是实际业务插件的原生 children。

完整表注册到同一个 SDK dispatcher registry，再保存为 frozen native provider。
Host 按独立 `host-native-mirror` source 从保留清单取得真实 route stage，经 broker
签发和 SDK Accept；原始 object/dispatcher/bundle/snapshot 均保持一致。普通调用
仍由 native Published 屏障控制。导入整表之后，mirror recipient 才 bind 同一
原始 Ctx；`PreparedDeployment::native_key(service)` 下安装真实 ObjectProxy，
availability 保持关闭，随后确认 gate 和 native Active 才发布并 refresh。

消费者实际捕获这份原生 Arc，核对其 owner、root object、完整 interface/bundle 和
mirror source，再从原始保留清单取得自己 prepared route 的独立 source/grant。
一个 mirror recipient 不能成为第三方转交通道，原始完整 commit 检查没有放宽。
旧的低层 `stage_native` 继续保留原始 source，不能冒充这条安装路径。

## 关闭与确认

必要 typed 服务移除后，native guard 的取消在准入检查中关闭 gate。弱 gate hook
在 gate 与原生 admission 锁之外同步关闭 Host owner、mirror 与捕获它的消费者，
封闭旧原始 Ctx 和 availability。hook 不等待 native effect drain 或远端 ACK。
epoch 断连同样撤销尚未进入 apply 的适配器。

SDK scope 排干在独立任务中取得 actor 锁：gate hook 可以从 SDK metadata 或
import scope 检查触发，不能在同一调用栈再次取得这些锁。HostProxy 关闭采用相同
顺序；捕获 proof 和最终发布也不持有 pairing 锁调用 SDK gate。关闭后的 broker /
SDK 成员不可再次 publish，availability 还检查单次 gate，迟到发布不能重开。

`HostNative::stop()` 在调用点封闭，重复/丢弃 waiter 不取消实际清理；结果 join
broker revoke、SDK Release ACK 与实际 native shutdown。`HostGraph::shutdown()`
在关闭 Host SDK 连接前，也等待去重后的 native adapters。失败保持可观察；没有
新代、retry 或绕过清理的 slot 重用入口。

## 实际证据与范围

Linux `native_runner_ipc` 使用删除源包后的冻结快照，启动实际 Rust image 与通用
Node runner。两位消费者在 RuntimeReady 后仍 Pending，直到实际 Host native
插件满足自己的本地依赖并发布服务。两端捕获 Arc 与实际 native key 下的 Arc
相同，owner 为真实 Host activation；业务运行对象返回、循环属性、owner passback、
borrow callback 与登记 child，native query 核对原始 Ctx 的 typed 服务身份。

用例拒绝空/不匹配 ports 和重复挂载。另一位故意缺少声明服务的实际 native 插件
ready 失败，真实 cleanup 一次，native 终态 Disposed，没有残留的 ObjectProxy。
移除有效 typed 服务后，原生 gate 准入检查同步撤销两个 runtime 的 Host 消费者，
旧 Ctx/客户端拒绝新操作，其他 remote providers 独立保持可用。最后核对真实 native
清理、两个子进程正常退出，再释放 snapshot。

这些用例的 provider 与 borrow callback 已由实际内部 child 创建，并核对精确
[创建者 Ctx](native-context.md)。这是首代 native export adapter 的安装与关闭证据。
[现有设置服务适配](settings-example.md)与[基础事件](events.md)已完成真实私有 IPC
验证；不承诺自动适配任意 Host 服务。
验收范围见[简化设计](../docs/design-protocol-plugins-2026-09-25.md)。
