# 验收记录：cordis-dispatch-01（PR #53 事件投递前观察）

## 被审版本与 diff 范围

- 被审顶点：`5e107bf`（`feat(core): observe event dispatch before listener selection`）
- diff 范围：`7d7402d..5e107bf`
- worktree：`/tmp/rutis-rev53`
- 改动文件：
  - `crates/rutis/src/bus.rs`（+177，核心实现）
  - `crates/rutis/src/ctx.rs`（+4，`plugin_id()` 访问器）
  - `crates/rutis/src/lib.rs`（+1，导出 `DispatchAttempt`/`DispatchMode`）
  - `crates/rutis/src/bus/transient_tests.rs`（+12，1000 轮注册/卸载）
  - `crates/rutis/tests/dispatch_observation.rs`（+280，6 个验收测试）
  - `README.md`（+2，文档段落）

## 结论：pass

实现完整、正确，验收标准逐条覆盖。发现 1 项非阻塞覆盖缺口（重入投递无显式测试）与若干观察项，均不影响结论。

## 问题列表

1. **重入投递缺少显式测试**（覆盖缺口，非阻塞）
   - 位置：`crates/rutis/tests/dispatch_observation.rs`（整体），对照 `bus.rs:366-371`（观察器回调在锁外执行）
   - 触发条件：观察器内部再次 `emit`/`serial`/`parallel`/`waterfall` 同键或异键事件
   - 证据：设计 §1 验收清单明确列出「重入」；实现中 `observe_attempt` 在收集观察器后已释放 admission 与 bus 表锁（`bus.rs:313-357` 的块内持锁，`bus.rs:366` 起回调在锁外），代码审查确认重入不会死锁、按普通嵌套处理。但测试套件中无任何测试让观察器回投事件（`observer_can_register_listener_before_snapshot` 只验证观察器注册监听器，非重入投递）
   - 是否阻塞：否。实现逻辑正确，仅缺回归测试。

2. （无阻塞问题）

## 验收标准逐条核对

| 标准（设计 §1 / 任务 verification） | 覆盖证据 | 结论 |
| --- | --- | --- |
| 四种分发模式各触发观察器 | `dispatch_observation.rs:64-123`（Emit/Serial/Parallel/Waterfall 依次断言 mode）；实现 `bus.rs:671,747,782,842,869,920` | 通过 |
| 动态限定名、实例键正确进入 `DispatchAttempt` | 测试 `:99-121`（`emit_keyed` 校验 `TypeKey::keyed_dynamic`；`emit_instance` 校验 `TypeKey::instance` 及 `InstanceOutOfScope` 拒绝）；`key` 字段 `bus.rs:359` | 通过 |
| 零业务监听器时观察器仍被调用 | 测试 `:83-84`（emit 无监听器仍观察到）、`:86-89`（serial/parallel/waterfall 无监听器仍观察）；`observe_attempt` 先于 `take_hooks` 的 `hooks.is_empty()` 判定（`bus.rs:671` 在 `bus.rs:679-682` 之前） | 通过 |
| 观察器先于业务监听器快照（观察器内注册/移除可影响随后投递） | 测试 `:126-143`（观察器内 `on` 注册监听器，随后的 serial 快照选中它）；`observe_attempt` 在 `take_hooks`/`take_wf_hooks` 之前（`bus.rs:671→679`、`869→870`、`920→921`） | 通过 |
| 注册/卸载竞态 | 测试 `:244-280`（`early_disposal...`）、`:208-241`（`shutdown_waits...`）；观察器移除在 admission+inner 双锁下与选择互斥（`bus.rs:281-283`），随后 `wait_events` 等在途回调（`bus.rs:290-294`） | 通过 |
| 重入投递 | 实现 `bus.rs:366-371`（回调无锁）；**无显式测试**（见问题 1） | 实现通过，覆盖不足 |
| 观察器 panic 隔离（ErrorSink 继续，sink 自身 panic 也隔离） | 测试 `:187-205`（观察器 panic + sink panic 均不中断业务投递，业务监听器仍执行）；实现 `bus.rs:366-370`（双层 `catch_unwind`） | 通过 |
| 不同实例子树隔离（发射方祖先链筛选，兄弟互不可见） | 测试 `:146-184`（root/a/b 三观察器，a 发射仅 root+a 可见，b 不可见；a shutdown 后 b 发射仅 b 可见）；实现 `bus.rs:323-349`（发射 fiber 祖先链 + `Arc::ptr_eq` 匹配） | 通过 |
| 1000 轮注册/卸载无泄漏/无崩溃 | `bus/transient_tests.rs:86-97`（`dispatch_observers_prune_after_repeated_registration`，每轮 count 1→0）；`shrink_to_fit` 逻辑 `bus.rs:284-288` | 通过 |
| 无观察器时原有事件行为对拍不变 | 快速路径 `bus.rs:310-312`（观察器空则立即返回，不触发 admission/祖先扫描）；整套既有事件测试（parity.rs 57、contract.rs 69 等）在无观察器下全部通过 | 通过 |
| 观察器不持锁运行 | `bus.rs:313-357`（admission + inner 锁在块内，回调前释放）；测试 `:208-241`（观察器阻塞时 shutdown 仍推进等待而非死锁）间接验证 | 通过 |
| 注册时校验 owner 属于该总线 root | `bus.rs:264-268`（`Arc::ptr_eq(&self.inner, &owner.events().inner)`）；`bus.rs:263` `registration_preflight` | 通过 |
| 观察器按 effect 卸载 | `bus.rs:275-296`（`register_internal_effect` + `AsyncDisposer` 内 retain 移除并 wait_events）；测试 `:244-280` | 通过 |

## 验证命令与实际结果

```
cargo +1.98.1 test -p rutis         → 209 passed; 0 failed; 0 ignored
cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings → 干净（Finished，无警告）
cargo +1.98.1 fmt -p rutis -- --check → 干净（exit 0）
```

测试分布（`dispatch_observation.rs` 6 项；`bus/transient_tests.rs` 含 `dispatch_observers_prune_after_repeated_registration` 1 项）：
- unittests(src/lib.rs) 14、cleanup_errors 3、config_update 15、contract 69、dispatch_chain_probe 1、dispatch_observation 6、event_keys 11、instance_subtrees 23、lifecycle_diagnostics 4、parity 57、strict_reads 4、transient_release 1、doc-tests 1。

## 观察项（非阻塞）

1. **观察器平铺线性扫描**：`BusInner.observers` 为 `Vec`，每次被观察投递时按注册顺序全量扫描 + `owner.upgrade()` + 祖先链匹配（`bus.rs:338-355`）。观察器数量大时每投递 O(n)。设计未提扩展性要求，属可接受实现选择，未来可考虑按发射 fiber 建立索引。
2. **`emitter`/`emitter_instance` 取自发射 `Ctx` 本身**：`bus.rs:361-362` 用 `ctx.plugin_id()`/`ctx.instance()`。对 isolate 派生的 `Ctx`，`instance()` 保留拥有 fiber 的实例号（`ctx.rs:117-119`），与「发射方」语义一致；但若未来 isolate 上下文语义变化需重新审视该字段来源。
3. **`observe_dispatch` 双重点检**：`bus.rs:263` 先 `registration_preflight`，随后 `register_internal_effect` 内部再次 `registration_open`（`ctx.rs:842-843`）。与 `on_instance` 的既有模式一致，仅冗余，无正确性问题。
4. **`DispatchMode` 未标 `#[non_exhaustive]`**：公开枚举新增变体将是破坏性变更；当前四变体即设计全量，无需处理，仅作未来演进提示。
5. **README 措辞**：`README.md:168` 描述与实现一致（零监听器调用、按 fiber 清理、无拒绝返回值），无偏差。

## 剩余不确定性

- 无。实现与设计 §1 逐条吻合，验证命令全绿。
