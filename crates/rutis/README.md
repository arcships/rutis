# rutis

Cordis 核心范式的 Rust 惯用实现(自 [min-cordis](https://github.com/eric8810/min-cordis) 独立成库)。

## 五支柱

1. **插件 = 装配单元**:一次 `apply`,提供服务 / 监听 / 清理
2. **fiber = 生命周期容器**:六态状态机 + 依赖门控 + 子树永久关闭 + 恰好一次清理
3. **服务 = 类型键注册表 + 实例子树可见性 + isolate 作用域**
4. **事件总线 = 四分发语义**(emit / parallel / serial / waterfall),实例事件独立派发与保序
5. **依赖驱动重载**:provider 卸载 → 消费者驱逐并自动重载

## 使用

```toml
[dependencies]
rutis = "0.3.0"
```

内核零 serde、零 unsafe,依赖仅 tokio / tokio-util / thiserror。设计与对拍文档见[仓库 docs](https://github.com/arcships/rutis/tree/main/docs)。

## 运行时诊断

`Ctx::diagnostics()` 返回只读的全树快照。`Ctx::subscribe_diagnostics()` 先订阅 root 的变化流，再扫描一次快照，返回 `initial`、订阅时的 `cursor` 和 `changes` 接收端。快照不是原子事务，可能与序号大于 `cursor` 的事件重叠；消费方应按插件、绑定身份和序号幂等合并。

变化流报告插件登记与终止、状态转换、服务绑定登记与移除、严格读取失败。每条记录只有身份和原因，不含服务值、配置或业务事件。序号表示提交点的入队顺序；不同 fiber 之间不承诺因果顺序。依赖检查缓存和读取历史等快照字段并非完整事件增量，需要时应重新取快照。

root fiber 在订阅前已存在，因此不会发出 `PluginRegistered`；它仍会报告状态变化，并在最终关闭时发出 `PluginTerminated`。

接收端是有界的 Tokio broadcast receiver。`recv()` 返回 `Lagged` 时已丢失记录，须重新调用 `subscribe_diagnostics()` 取得快照和新游标。root 最终 `shutdown()` 完成后，接收端读完剩余记录会返回 `Closed`。

## License

MIT(继承自 [Cordis](https://github.com/shigma/cordis) © Shigma)。
