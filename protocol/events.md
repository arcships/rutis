# 基础事件

`events::HostEvents` 管理本次交付的唯一监听列表。每个本地或远端监听都是其中的一条注册；远端监听复用对象 `Caller`，不向原生事件总线再次广播。

Host 选择 `EventKey { scope, bundle, event }`，并把发布权限绑定到实际原生上下文和该装载的 `Exports`。运行器提交的载荷不能选择发布身份或扩大 scope。实际接入示例见[设置服务](settings-example.md)。

## 注册、调用与关闭

- `subscribe(ctx, key, once, listener)` 同步返回即 ready。跨进程端点先完成对象交付与 Accept，再进入 Host 列表；作者不维护 delivery token。
- `parallel` 并发执行有效监听，等待全部结束，在 `DispatchResult.errors` 汇总错误。
- `serial` 按 Host 注册顺序执行。`ListenerResult::Continue` 继续，`Return(value)` 短路；`0`、`false`、`null` 均是有效返回。
- once 在调用前原子认领并从列表移除。重入及并发发布不能重复执行。
- `Subscription::unsubscribe()` 调用时同步关闭准入并移除注册；返回的 future 等待在途监听。丢弃句柄也关闭准入，原生 effect 仍负责等待实际执行。
- 创建者原生卸载时关闭并移除注册、等待在途监听、释放闭包。执行前再次检查原生关闭状态。丢弃发布等待者不会取消已经开始的工作。

注销等待需要在监听执行之外完成；监听内部可以关闭准入，但等待自身退出会造成循环等待。

载荷沿用接口 bundle 的类型校验，支持 JSON、record/list/optional 和对象引用。自有对象使用原生导出表；foreign 必须是发布端实际持有的同 bundle grant。远端交付继续经过 broker：可以交付自有对象，或传回接收者拥有的对象，第三方转交拒绝。事件结果目前限于声明的 JSON 值。

## 原生接入

Rust Host 用 `subscribe` 注册本地函数，或注册调用生成 `EventListener` 对象的适配器。TS 的 `onProtocolEvent(ctx, listener)` 返回一条实际 Cordis 上下文拥有的端点；Host 完成交付后注册它。每条端点对应一个监听，没有第二份跨进程广播列表。

Node 发布时使用 Host 授予的 `EventPublisher` 服务和 `emitProtocolEvent`。端点固定事件与 scope，内部调用同一个 Host 列表；对象沿现有 SDK 通道交付。适配器与生成接口属于本仓库实验源码 API，尚未作为独立稳定 npm 接口发布。

纯本地 `ctx.on/emit/parallel/serial` 保持原生行为。跨进程调用点显式改用适配器和 `await`。Cordis 原生 serial 把原始 false/null 视为继续；适配器用明确的 `{ returned, value }` 保留跨进程 Return(false)/Return(null)。异常成为监听错误，不通过同步 emit 隐藏。

本实现拒绝 emit/bail/waterfall 和非 JSON 事件结果；持久业务回调、流与任意原生 scope 过滤器不属于当前支持范围。默认通用 driver 未安装这些事件端点，仍在 hello 拒绝事件 capability；应用显式接线后才能使用。当前固定 Rust/Node 示例已经完成双向接线，不宣称任意插件自动获得事件传输。

验证入口：`crates/rutis-protocol/tests/events.rs` 与 `settings_ipc.rs`。它们覆盖并行等待、错误、短路值、重入 once、scope 拒绝、错误载荷、注销、原生卸载、丢弃等待者及真实双向对象载荷。
