# 实际原生消费者清理

本仓库 rutis 的可选 `Ctx.track_dependency_cleanup()` 返回 `DependencyCleanup`。
它读取真实原生依赖和清理结果，用于补齐提供者 eviction 忽略消费者 cleanup
错误、普通 Pending 卸载把错误交给 ErrorSink 后清除依赖边的观察缺口。它不驱动
stop、不执行额外 disposer，也不构成 OS 回收或新代许可。

## 登记与等待

在原始 apply Ctx 中、发布任何服务前登记并持有 observation。registry 在 binding
对消费者可见前登记精确 provider id/generation；内部 child 提供的服务也属于这棵
实际子树。不同隔离 scope 的同名服务有各自的 provider 身份，不按类型名推测
消费者。原始代取消后不再登记新提供者代。

native 在消费者实际 drain 开始时检查本代捕获的依赖四元组，在清除依赖边前记录
`ConsumerCleanup`。每份 receipt 绑定不可修改的 id、generation 和名称，`result()`
返回尚未完成或缓存结果，`wait()` 等待实际 effect drain。失败保留真实清理错误；
apply 业务错误不会被冒充为 disposer 错误。下一代清理有独立 receipt，不能覆盖
上一代失败。内部 child 的清理归原生子树所有，不重复登记成外部消费者。

首先关闭提供者，然后等待 `DependencyCleanup.wait_provider()`，再读取并等待
`consumers()`。源提供者本代实际 drain 的 receipt 独立保留，不依赖管理句柄仍然
存在。提供者尚未排干时，空消费者列表不能证明没有待清理消费者。观察器和
receipt 不保留 native view 的强引用；调用方仍须按原生 owning view 契约发起关闭。

等待者取消不会取消真实清理，重复等待取得同一结果。实际 drain 被中断时保留
abandoned 错误，不生成成功结果。观察器存在期间保留历史 receipt；生产 supervisor
后续须在真实恢复屏障通过后退役这些记录，不能靠时间或列表为空清除失败。

## Host 接入与证据

HostProxy 和 Host native adapter 在发布服务前自动登记。[HostGraph](host.md)
保留观察器，`consumer_cleanup()` 合并相同原生 id/generation。shutdown 先关闭
全部成员并等待提供者实际清理，再等待消费者；消费者 disposer 失败使 shutdown
失败，即使提供者自己的 native stop 已成功。

rutis 的两个真实原生用例覆盖内部 child、两个隔离 scope、慢 disposer、取消已
开始的等待者、依赖边移除、失败 apply 的独立清理错误及下一代不能覆盖旧失败。
冻结 Rust/Node 互通用例另验证 OS 回收先完成而原生 consumer receipt 仍等待，
以及实际 Host native service 的消费者清理失败由 HostGraph 保留。

这是本仓库 rutis 的公开扩展，尚须随正式发行版本发布。远端 native stop ACK、
本地清理结果和 OS receipt 仍有各自的证据要求。自动消费者快照租约、全组
supervisor、显式新 epoch 与 StopUnconfirmed 管理尚待 M3，不能以本 API 重挂旧代。
