# 验收记录：cordis-effect-02（PR #54 带标签 effect 清理树）

## 被审版本与 diff 范围

- 被审提交：`c9d4caf`（`feat(core): expose labeled effect ownership tree`）
- diff 范围：`5e107bf..c9d4caf`（叠放在 PR #53 `5e107bf` 之上，本层仅验收新增改动）
- worktree：`/tmp/rutis-rev54`
- 变更文件（8 个，+314 / -22）：
  - `crates/rutis/src/effect.rs`（EffectMeta / EffectPhase / into_cleanups / snapshot / index 清理）
  - `crates/rutis/src/fiber.rs`（effect_index、push_effect、FiberView::effects）
  - `crates/rutis/src/ctx.rs`（effect_named、register_*_named、各标签）
  - `crates/rutis/src/bus.rs`（内部 effect 改名 + 标签）
  - `crates/rutis/src/lib.rs`（导出 EffectMeta / EffectPhase）
  - `crates/rutis/src/fiber/transient_tests.rs`（1000 轮断言 effect_index）
  - `crates/rutis/tests/effect_tree.rs`（新增 4 个验收测试）
  - `README.md`（清理树说明）

## 结论

**pass**

实现完整覆盖设计文档 §2 的验收场景，测试、clippy、fmt 全部干净，未发现阻塞问题。

## 问题列表

无阻塞问题。详见「观察项」。

## 验收标准逐条核对

| 验收标准 | 覆盖证据 | 结论 |
| --- | --- | --- |
| 自动标签（`anonymous` / 插件名）与显式标签（`effect_named`） | `Ctx::effect` 委托 `effect_named("anonymous", f)`（[ctx.rs](crates/rutis/src/ctx.rs#L826-L828)）；`plugin apply: {name}`（[fiber.rs](crates/rutis/src/fiber.rs#L552-L557)）；测试 `named_effect_...`、`framework_labels_...` 断言 `anonymous`、`plugin apply:` | 通过 |
| `Effect::Many` 的真实嵌套生成 `children`；LIFO 清理顺序不变 | `into_cleanups` 递归建树（[effect.rs](crates/rutis/src/effect.rs#L57-L76)），`out` 仍按声明序压入、`run_cleanups` 用 `pop()` 取 LIFO；测试断言 children 结构 `0: disposer`/`1: many` 且 dispose 后顺序 `[3,2,1]` | 通过 |
| 并列 `ctx.effect()` 登记为兄弟项，不臆造父子关系 | 每个 `effect_named` 独立建 `EffectRecord`（单一顶层 `EffectMeta`），无调用栈父子的机制；`framework_labels_...` 展示多顶层兄弟项共存 | 通过（见观察项 1） |
| 提前 dispose 后元数据消失；清理期间可见 `Draining`；记录完成后索引删除 | `snapshot()` 按 `Live/Draining/Done` 返回；`drain` 任务在 `Done` 后从 `effect_index` retain 删除（[effect.rs](crates/rutis/src/effect.rs#L173-L180)）；测试 `draining_record_remains_visible_until_cleanup_finishes`、`named_effect_...` | 通过 |
| `EffectMeta` 不捕获清理闭包、不执行用户代码、不持子 fiber/服务值 | `EffectMeta` 仅含 `label/phase/children`（[effect.rs](crates/rutis/src/effect.rs#L32-L36)），cleanups 单独存入 `EffectState::Live`；`effects()` 仅 clone 元数据、upgrade weak、`snapshot`，不调用户代码（[fiber.rs](crates/rutis/src/fiber.rs#L1130-L1135)） | 通过 |
| 现有 `Ctx::effect()` / `Plugin::apply()` 返回类型不变；错误聚合不变 | `effect` 仍返回 `Result<Disposer, CordisError>`，`apply` 仍返回 `BoxFuture<Result<Effect,..>>`；清理顺序/聚合逻辑未改，既有 `contract.rs::aggregate_no_flatten`、`exactly_once_same_error`、`parity.rs` 聚合测试均通过 | 通过 |
| 1000 轮子树关闭后父 fiber 元数据与 effect 记录回到基线 | `thousand_shutdowns_reclaim_all_root_side_records` 每轮断言 `root.effects.len()==1` 且新增 `root.effect_index.len()==1`、子 fiber `effect_index` 空（[transient_tests.rs](crates/rutis/src/fiber/transient_tests.rs#L152-L154)） | 通过 |
| `FiberView::effects()` 入口满足 #27「由 fiber 获取」 | `pub fn effects(&self) -> Vec<EffectMeta>`（[fiber.rs](crates/rutis/src/fiber.rs#L1130)），lib.rs 导出 `EffectMeta`/`EffectPhase` | 通过 |

## 验证命令与实际结果

在 `/tmp/rutis-rev54` 内执行（toolchain `1.98.1` 已安装，worktree 无 `rust-toolchain.toml`，按要求用 `+1.98.1` 显式指定）：

```
cargo +1.98.1 test -p rutis
```

- **213 passed；0 failed**。明细：unittests 14、cleanup_errors 3、config_update 15、contract 69、dispatch_chain_probe 1、dispatch_observation 6、effect_tree 4、event_keys 11、instance_subtrees 23、lifecycle_diagnostics 4、parity 57、strict_reads 4、transient_release 1、doc-tests 1。

```
cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings
```

- 干净（`Finished`，无 warning）。

```
cargo +1.98.1 fmt -p rutis -- --check
```

- 干净（退出码 0）。

## 观察项（非阻塞）

1. **「并列 sibling 不臆造父子」无专属测试**：§2 验收清单（设计文档 L91）未把它列为验收项，只在正文（L85）作为设计约束表述；实现结构上天然满足（每个 `effect_named` 独立顶层记录），且 `framework_labels_and_child_ownership_follow_lifecycle` 已展示多顶层兄弟项共存。可作为后续补充覆盖，不构成阻塞。

2. **元数据为每次读取深拷贝整树**：`FiberView::effects()` 对每个记录 `metadata.clone()`（递归 clone 标签与 children），大 effect 树下为 O(n) 分配。设计明确「第一步先在单 fiber 上读取」且「读取只复制标签/阶段/树结构」，符合草案，无正确性问题，仅提示未来纳入全树 DTO 时注意开销。

3. **`EffectMeta` 是纯值快照，非引用视图**：`snapshot()` 返回 `Option<EffectMeta>`（owned），`children` 内嵌 owned `EffectMeta`，符合「不持子 fiber/服务值」要求；代价是读者拿不到实时引用（设计已说明「读取只复制」），符合预期。

4. **`effect_index` 的弱引用 GC 依赖 drain 完成时触发**：`index.retain(...)` 只在某条记录 drain 完成时清理自身及 `strong_count()==0` 的陈旧弱项（[effect.rs](crates/rutis/src/effect.rs#L176)）。所有记录最终都会经 `drain`（提前 dispose 或 fiber 卸载 `drain_effects`），故无泄漏；1000 轮测试已验证容量有界。这是与既有 `parent.children` 相同的清理模式（[fiber.rs](crates/rutis/src/fiber.rs#L914)），一致且安全。

5. **`register_effect_named` 侧效应回滚路径**（[ctx.rs](crates/rutis/src/ctx.rs#L932-L945)）：`f()` 已执行但生命周期越过后，`record.drain()` 排干清理，因该记录未 `push_effect` 故 index 中本无条目，drain 的 index retain 为无害 no-op。行为与改名前一致。
