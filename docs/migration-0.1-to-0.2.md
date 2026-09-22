# 从 0.1.0 升级到 0.2.0

## 概述

**0.2.0 对 0.1.0 的 API 完全向后兼容。** 不需要改任何现有代码即可升级。

新增两个内核能力（配置热更新、动态事件键）+ 一轮 cordis 契约审计修正，全是增量。

## 不兼容变更

**只有一处**：`TypeKey` 不再实现 `Copy`。

0.2.0 引入了动态限定名变体 `Qualifier::Dynamic(Arc<str>)`，使 `TypeKey` 无法再 `Copy`。如果你在代码中依赖了 `TypeKey: Copy`（比如在多个地方传值而不 clone），需要显式 `.clone()`。

影响范围极小：`TypeKey` 只在服务注册/事件注册/装载路径使用，不经过热路径。

```rust
// 0.1.0
let k = TypeKey::of::<MyService>();
do_something(k);
do_other(k);  // Copy, 没问题

// 0.2.0
let k = TypeKey::of::<MyService>();
do_something(k.clone());
do_other(k);
```

## 新增能力

### 配置热更新 (D32)

`Plugin` trait 的静态模式不变。新增 `PluginFactory` trait 支持运行中改配置：

```rust
use rutis::{Plugin, PluginFactory, Ctx, CordisError, Effect, BoxFuture};

// 静态模式（0.1.0 已有，0.2.0 不变）
struct MyPlugin { config: MyConfig }
impl Plugin for MyPlugin { /* ... */ }
let view = ctx.plugin(MyPlugin { config });

// 工厂模式（0.2.0 新增）
struct MyFactory;
impl PluginFactory<MyConfig> for MyFactory {
    fn build(&self, config: &MyConfig) -> Result<Box<dyn Plugin>, CordisError> {
        Ok(Box::new(MyPlugin { config: config.clone() }))
    }
}
let view = ctx.plugin_with(MyFactory, config_v1);
view.update(config_v2).await?; // 热更新: dry-run → 存 → restart
```

关键约束：

- **`build` 必须是纯构造，无副作用。** dry-run 与实际装载各调一次，两次产物不要求同一实例但要求等价。
- **`injects` 是静态声明（`&[TypeKey]`），与 `Plugin::injects` 完全同形。** 不要试图从 config 派生依赖——那是 D32f 修订明确拒绝的路径。按配置选依赖的标准做法是拆成多个插件、配置决定装哪个。
- **`update()` 适用全部六态。** Pending 存着等门控、Failed 可热修复、Loading 协作取消——复用现有 `Intent::Restart` 路径，零新事务。

### 动态事件键 (D33)

同事件类型可以有多个独立通道，运行时字符串作为限定名：

```rust
use rutis::{Ctx, Event, TypeKey};

// 静态限定名（零分配）
let key = TypeKey::keyed::<MyEvent>("primary");

// 动态限定名（运行时字符串，用于桥事件等）
let key = TypeKey::keyed_dynamic::<MyEvent>(format!("session/{}", id));

// 事件总线新增 keyed 面
ctx.events().on_keyed::<MyEvent>(&ctx, "channel_a", listener)?;
ctx.events().emit_keyed(&ctx, "channel_a", Arc::new(event));
ctx.events().serial_keyed::<MyEvent>(&ctx, "channel_a", &event).await?;
ctx.events().parallel_keyed::<MyEvent>(&ctx, "channel_a", Arc::new(event)).await?;
ctx.events().waterfall_keyed::<MyEvent>(&ctx, "channel_a", &event, terminal).await?;
```

同类型不同名互不串扰（各自独立的 hook 列表和 dispatch 尾链），四分发语义免费继承。

`on_keyed` / `once_keyed` / `on_waterfall_keyed` 覆盖全部注册形态（带 prepend 选项的 `*_opt` 版本同样可用）。

## 契约修正（0.1.0 → 0.2.0 修的问题）

这些修正改变了框架内部行为但不影响 API 签名：

| 修正 | 0.1.0 行为 | 0.2.0 行为 |
|------|-----------|-----------|
| FAILED 粘性（依赖消失时） | Failed 被降级 Pending，错误隐入 settle 通道 | 保持 Failed，错误持续可见，依赖恢复后重试 |
| dispose × restart 竞态 | 并发调用可能让 join 永等 | dispose 先登记则 restart 立即拒绝 `InactiveEffect` |
| 驱动退出后迟到投递 | 伪装 `Ok` 完成 | 携带 fiber 终态错误完成 |
| 工厂 build panic | 杀驱动任务 | 转为 `fail_load`，不崩驱动 |
| EffectRecord 清理 panic | 可能卡 Draining | 全程 panic 边界，必进 Done |

## 迁移清单

- [ ] Cargo.toml: `rutis = "0.2"`
- [ ] 搜索 `TypeKey` 的 `Copy` 使用，改为显式 `.clone()`（通常 0 处改动）
- [ ] 需要热更新的插件：实现 `PluginFactory<C>` 替代直接持有 config
- [ ] 需要多通道事件的场景：使用 `on_keyed` / `emit_keyed` 替代 `on` / `emit`
- [ ] 运行现有测试套件确认无回归

现有 `Plugin` impl 和 `ctx.plugin()` 调用零改动。`cargo test` 全部通过即可。