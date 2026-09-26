# 原生对象的创建者上下文

一个托管根仍只占一个全局 activation。它的内部子插件由真实 rutis/Cordis 子树
拥有，对象 identity 的 owner 保持托管根；SDK 的 owner 表另保存对象首次登记时
的实际原生 Ctx/Context，`CallContext.native()` 在调度时返回这份创建者上下文。
不会为 child 新建全局代理或把原始 Ctx 放入 wire。

## 登记与继承

`Exports.managed` 的默认创建者是传入的原始托管根上下文。导出来自内部 child 的
服务时，用 Rust `sdk::with_native(&child_ctx, exportInterfaceDatabase(value))` 或
TS `withNative(childCtx, exportInterfaceDatabase(value))` 标注声明的 own 对象。
`NativePorts.provide` 的显式导出函数可以从业务对象或作者保存的本地元数据取得
这份 Ctx；SDK 不枚举对象字段或自动探测业务接口。foreign grants 保持原证明。

同一真实 Arc/JS 对象跨重复交付、接口视图、路由 source 和 owner passback，始终
复用第一次登记的创建者。以后从根上下文再次导出它也不能改写原上下文；已取消
的创建者不能借新包装复活旧对象。关系快照里的新 own 属性默认继承实际对象的
首次创建者；显式标注的另一位内部 child 仍可以提供自己的对象。

实际 handler 与登记后代完成后，runtime 用这份 CallContext 标注返回的 own
对象。纯值迟到结果也检查创建者仍开放，失活后拒绝编码。这样 `connect()` 返回
的 Connection 以及它的循环关系继续属于同一实际 child 上下文，而不是接线的
托管根。作者若在另一位 child 内创建对象，应明确标注那份本地创建者。

生成客户端接受的借用 callback 是业务对象，可以在编码前通过其 Caller 关联：

```rust,ignore
database.client().caller().bind_native(
    &child_ctx,
    exportBorrowCallback0(callback.clone()),
)?;
database.withCallback(callback).await?;
```

```typescript
bindNative(handle(database).caller, childCtx, exportBorrowCallback0(callback))
await database.withCallback(callback)
```

该关联只登记本地弱身份与创建者，不签发 grant、不增加 pin，也不建立绕过 broker
的调用路径。实际参数仍由生成客户端编码，经整图授权和 Accept 后才交给业务。
本地 metadata 的关联先于首次交付；已导出的对象不能更换创建者。

## 代边界与清理

创建者必须在该 `Exports.managed` 的真实 native 子树内。Rust 使用本仓库新增的
公开 `Ctx::is_within` 检查仍存在的 fiber 祖先身份，该 API 不改变隔离 scope 或授予服务
访问权。TS 使用锁定 Cordis 4.0.1 的公开 fiber/parent 链；不维护 Cordis fork。
应用根、另一位托管插件或其他 native 树的 Ctx 不能冒充这份 owner 的 child。

Rust 保存原始代的 cancellation token 所属 Ctx；新 pin 和 dispatch 入口同步拒绝
已取消的创建者。TS 在首次登记时通过创建者自己的 effect 保存代关闭标记，并
检查公开 fiber state/uid。普通内部 child 原生重载也不能重开原对象的关闭标记。
已有 execution pin 继续持有实际对象，真实执行及登记后代结束后才释放。

必要导出仍由托管根的原生 guard 管理：child 服务失效关闭根 gate，撤销 Host
权威及已捕获的消费者。非必要 child 对象目前同步拒绝 owner 新 pin 与调度，但
尚未实现只撤销该对象的远端通知；其旧 delivery pins 仍按现有 recipient release
或托管根撤销收敛。单对象撤销及 child 在飞 handler/慢 disposer 的完整交错仍需
验收，不能用精确创建者测试代替这些门槛。

## 可执行证据

`native_runner_ipc` 从删除源包后的 frozen snapshot 启动真实 Rust image 与通用
Node runner。两端 providers 在实际原生内部 child 中提供服务，返回 Connection
的方法核对 child Ctx/Context；消费者的 borrow callbacks 也在另一位真实内部
child 中创建、关联并执行。双向回调重入、登记后代、循环属性、owner passback
和 Host native adapter 路由继续走同一个实际 SDK、broker 和私有 fd。

Rust `services` 另验证不属于托管根的 Ctx 被拒绝，停止普通 child 后根仍 Active、
旧 Connection 不能取得新执行 pin、根上下文重新导出不能复活它。TS `exports`
验证第一次创建者不被重传改写、跨树拒绝、native stop 后新 pin/dispatch/纯值
结果拒绝，已有 execution pin 仍持有原对象。更新恢复、选择性撤销和最终 M0–M5 /
T01–T24 验收仍未完成。
