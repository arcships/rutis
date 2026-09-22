# 瞬态子插件的残留释放（0.2.1）

日期：2026-09-22。状态：已实现。类型：缺陷修复，patch 版本。

## 问题

rutis 0.2.0 的若干结构只在 fiber 自身卸载时清理。在长寿 root 下反复注册/销毁子插件（D32 工厂装载与 keyed 多实例的常规用法），以下残留随实例数无界增长：

| # | 残留 | 位置 | 增长单元 |
| --- | --- | --- | --- |
| 1 | mount 记录（child 级联 dispose 注册为 parent effect） | `FiberInner::effects` | 每插件一条，持有 child `FiberView`（`Arc<FiberInner>` 骨架） |
| 2 | 依赖声明条目 | `Registry::inject_index` | 每插件每键一条；keyed 声明每实例唯一限定名，条目不复用 |
| 3 | 空事件通道条目 | `EventBus::hooks/wf_hooks` | keyed 通道最后一个监听器摘除后空列表保留 |
| 4 | 派发尾链句柄 | `EventBus::dispatch_tail` | 同键最后一次 emit 的任务句柄保留 |
| 5 | root 级 provide 的 evict 清理记录 | `FiberInner::effects` | 每条 provide 一条（`Disposer::dispose` 后仍留 Done 记录） |
| 6 | root 级 provide 的记账 | `FiberInner::provided` | 每条 provide 一条，`Disposer` 释放后不摘 |

绑定本身经 `finalize_binding_if` 正常摘除，不在本表。

## 修复

核心机制两条：

1. **EffectRecord 自摘与错误寄存**（#1、#5）：记录携带宿主 fiber 的 `Weak` 引用；drain 进 `Done` 后，若记录仍在宿主 effects 列表（收集者是提前 drain 的调用方，不是 fiber 级卸载），则错误寄存宿主 `drained_errors` 并从列表移除；已被 `drain_effects` 整表取走的记录不寄存（其错误由 drain 的 join 直接收集，不重复）。寄存的错误在 fiber 卸载/重启时并入聚合——单一错误保持 `Arc` 同一性，与记录留在列表中被再次 join 的旧语义等价（对拍 reentrant.spec.ts:437）。
2. **子终态释放**（#1、#2）：非 root fiber 驱动在 Dispose 终态退出前执行 `release_transient`：按注册时捕获的声明快照注销 `inject_index` 条目；drain 自身 mount 记录（终态后清理幂等，`dispose()` 返回缓存终态即刻完成），记录自摘后 parent 不再持有 child 引用。

配套修复：

- **#3**：监听器摘除后删除空通道条目（`take_hooks` 对全 once 领走后的空列表同样处理）；再次注册经 `or_default` 重建，行为不变。
- **#4**：`dispatch_tail` 值为 (代次, 任务)；派发任务完成后按代次比较自摘——监听器内重入 emit 同键已插入新一代时不误删，保序链不断。
- **#6**：`evict_and_finalize` 末尾摘除 provider 的 `provided` 记账（仅本键本作用域一条；同键新 provide 的条目保留）。

语义保持：

- mount 清理闭包仅在「子错误尚未经 dispose() 交付」时送 ErrorSink——子已 Disposed（调用方已收到同一错误）不再重复上报；parent 卸载级联处置 Active/Loading/Pending/Failed 子时行为不变。
- `Disposer` drop 不触发清理的契约不变；`Ctx::effect`/`FiberView`/`EventBus` 公共签名不变（semver patch）。
- parity 全套（含 shared-promise Arc 同一性、restart 错误路由 sink、聚合不压平）不改动断言即通过。

## 验证

- 单元（`src/*/transient_tests.rs`）：churn 25 轮后 mount 记录、`inject_index`、`provided`、effects 列表回到基线；keyed 通道与尾链摘除后可重建复用。
- 黑盒（`tests/transient_release.rs`）：单 root 上 churn 50 插件（keyed 服务 + keyed 监听），实例全部析构（Drop 计数），已销毁通道再派发不 panic，root 持续可复用。
- 既有 contract/parity/config_update/event_keys/dispatch_chain_probe 全部通过；clippy `-D warnings` 与 fmt 干净。

## 已知边界

- 非 root 终态释放尾随 dispose 的 TaskDone 完成点，有界但不零延迟；需要确定性断言时按有界等待处理。
- root fiber 自身不退出：单 root 场景下 root driver 随进程存活是有界残留（每 root 一个任务），不在本修复范围。
