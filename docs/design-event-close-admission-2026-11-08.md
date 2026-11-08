# 不遍历祖先链的事件监听器关闭接纳

[English](design-event-close-admission-2026-11-08.en.md)

Status: 草案，尚未实现。日期：2026-11-08。
Issue：[ #1025 ](https://github.com/arcships/rutis/issues/1025)。实施 issue：#1025-core、#1025-tests、#1025-benchmark（将在 0.22 计划中拆出）。

## 1. 现状与要解决的问题

每个被选中的事件监听器现在都要检查 owner 是否仍开放：读取 owner 状态，然后调用 `Ctx::registration_open()`。后者读取全局关闭状态并遍历每个祖先（`crates/rutis/src/ctx.rs:191-207`）；`Entry::live()` 在 `crates/rutis/src/bus/table.rs:77-90` 的热路径执行此操作。在 `close_cpu` 工作负载（1000 个 instance × 每个 8 个 row × 8000 个 listener）中，2026-11-02 release 样本创建 instance 约 38.2 秒，其中 registration-open 约 9.7 秒（24%），每次 listener 调用还会读取两三条冷的祖先 cache line。#1025 的 issue 目标是该工作负载的创建耗时 ≤30 秒；实现验收还必须满足现有 CREATE_RATIO、CLOSE_RATIO 和 IDLE_SHARE 阈值，且不改变事件/关闭语义。

shutdown 已在标记子树并开始关闭时持有 `Shared.admission`（`crates/rutis/src/fiber.rs:1756-1780`）；root shutdown 在设置 `Shared.closing` 时持有同一锁（`crates/rutis/src/ctx.rs:1318-1338`）。事件 snapshot/selection 不持有该锁，`Entry::start()` 刻意在最终 liveness 检查前开始 call flight。没有发布协议的 marker 可能让 ancestor 已关闭而 descendant 仍被看作 live。

## 2. 决定

| 决定 | 选择 | 原因；放弃的方案 |
| --- | --- | --- |
| D1 | 在 `FiberInner` 增加永久的 `lineage_closed: AtomicBool`。永久关闭的 fiber 以及关闭提交时子树中的每个成员都为 true；root 已关闭后新建的 fiber 初始化为 true；generation reload 永不清除。 | 不使用 per-entry marker 或共享继承 marker：entry 是动态的；共享 marker 会在只关闭一个子树时误关 sibling。 |
| D2 | 在 `Shared.admission` 下两阶段发布：收集固定子树；先为所有成员写入 `lineage_closed=true`；全部写完后再发布现有 per-fiber `closing=true`（root 另发布 `Shared.closing=true`）；最后排入 cleanup。shutdown 线性化点是 marker pass 完成，而不是第一个 ancestor flag。 | 放弃简单的 ancestor→child 传播，因为会产生 ancestor closed/child live 窗口；也放弃仅 child-first 的公开关闭变更，因为会改变既有关闭边界。 |
| D3 | `Entry::live()` 读取 `withdrawn`、owner、`alive`、`owner.closing`、`owner.lineage_closed` 和 `live_generation`，不再遍历 context parent 链。`Entry::start()` 保留 `CallFlight::begin(owner)` 后再做最终 `live()` 检查。 | 放弃移除最终检查或 event flight：已接纳调用必须 drain；marker 提交后不得再接纳调用，但已接纳调用可以在提交后开始并按既有规则完成。 |
| D4 | `Ctx::registration_open()` 及其祖先遍历继续用于 registration/service API。本优化只改变 listener liveness，不改变 public registration error 或 instance-event admission。 | 放弃用 listener marker 取代所有 lifecycle 检查，因为 registration 有独立 API 合约。 |
| D5 | 第一版使用 `SeqCst`。事件 snapshot 热路径不加锁，也不持有会阻塞的 admission lock。两阶段 marker store 提供稳定的提交边界；`Entry::start` 观察到 marker 后拒绝新的接纳。 | 放弃每次 dispatch 都拿 `Shared.admission`，因为会增加锁开销并可能无法达到 CPU 目标；新的内存序证明另行处理。 |

## 3. 精确规则

1. 非 root 子树关闭前，shutdown 在 `Shared.admission` 下收集当前所有 descendant，为全部成员写 `lineage_closed=true`，然后写现有 `closing=true` 并调用 `pre_cancel`，最后调用 `begin_subtree_shutdown_locked`。collector 和 child links 使用与 fiber 创建/脱离相同的锁，集合因此固定。
2. root shutdown 对 root 加全部 descendant 执行相同 marker pass，再写 `Shared.closing=true`，对 root/descendant 执行 pre-cancel 并提交现有关闭意图。即使旧 root shutdown 只写 `Shared.closing`，root descendant 也必须被标记。
3. 在 shared root 已关闭时创建的 fiber 以 disposed 和 `lineage_closed=true` 开始。永久关闭 marker 不会在 restart/update 时清除；新 fiber 获取新的 marker。
4. marker pass 是关闭接纳边界：最终 `Entry::live` marker load 在完成前的调用可以接纳并正常等待；完成后的检查不得再接纳。snapshot 可能持有 stale entry，但 `Entry::start` 是权威的最终检查。已接纳的 callback 可能在 owner marker 提交后开始，但必须按既有规则 drain；提交后不得有新的 callback 被接纳。
5. `Entry::start` 先开始 `CallFlight` 再调用 `live`；closed 结果立即 drop flight，不调用 listener。已接纳调用保留现有 event-flight 等待和取消行为。`once` 只在 `start` 后 claim，因此被跳过 listener 不会被消耗。
6. marker 对子树关闭是永久的。普通 generation unload/reload 只要没有永久 shutdown，fiber 仍可供后续 generation 使用；reload 不会重置 `lineage_closed`。`withdrawn`、`alive`、`closing`、`live_generation` 含义不变。
7. registration 仍拿现有 admission 锁，并在 commit 时再次检查生命周期。marker pass 期间 preflight 读到 open 不能通过该锁提交；关闭子树不会接纳新 registration。
8. 既有 instance-event 规则不变：instance dispatch 停止接纳，等待相关的已接纳 dispatch，关闭后返回现有 `Closed`。本优化只涉及非 instance listener 的 `Entry::live`，不改变 instance permit 合约。
9. listener callback、事件顺序、dispatch mode、跨进程协议、取消 deadline 和 shutdown 输出不变。其他 process/runtime 不会看到新消息。
10. 如果实现改用重试 sequence guard，而不是仅用两阶段 marker，也必须保持同一规则：事件路径不能从混合发布中返回 live。可以使用 `SeqCst` 的 odd/even `shutdown_sequence`，但必须测量 spin 成本并证明不会无限等待；默认实现仍为 marker-first 两阶段 pass。

## 4. 开发者看到什么

### 插件作者

API 和输出没有变化。运行中的 row 注册的 listener 收到完全相同的事件和错误。ancestor 在 marker 提交后永久关闭时，之后发出的事件不会调用该 listener；instance 在关闭后 emit 仍返回 `ErrorKind::Closed`，文字为 `root has been shut down`，且没有新的 `code`/`data`。在途 callback 按现有 shutdown deadline 完成或取消。

新增概念：无。`lineage_closed` 是内部性能 marker。

### 使用 `rutis-host` 组装插件的人

没有配置变化，也没有新的 status 行。`rutis-host status` 仍打印现有 row 状态（`running`、`stopping`、`stopped` 或既有 failure headline）。事件关闭优化没有 host error code 或迁移设置。

新增概念：无。

### 在 Rust 中嵌入 rutis 的宿主作者

public Rust API、事件协议和 shutdown result 不变。既有保证继续成立：子树接纳关闭后，不再接纳新的 listener callback；已经接纳的调用即使在提交后开始，也按现有等待/取消规则 drain。没有新增 error code 或 data 字段。

新增概念：无；维护者只增加内部 owner marker 和关闭线性化规则。

### 部署者

没有部署设置、wire version 或 process boundary 变化。release artifact 行为不变，只在 stress benchmark 中降低 instance 创建 CPU。没有可见成功或错误输出变化。

新增概念：无。

## 5. 兼容与迁移

- public API、事件协议、listener 顺序、shutdown 状态/输出和 error code 不变。
- 现有 listener 无需迁移。唯一兼容要求是关闭后事件仍被排除，已接纳调用仍 drain。
- `close_cpu` 仍是 release benchmark。对比现有 `CREATE_RATIO=300`、`CLOSE_RATIO=200`、`IDLE_SHARE=5%` 阈值；不能用牺牲关闭语义换取创建速度。
- 该内部变更不新增 package version 或 runtime protocol field。0.22 release note 记录性能修复，并保留现有事件关闭保证。

## 6. 实施

1. **Core marker 与发布（#1025-core）。** 在 `FiberInner` 增加 `lineage_closed`，从初始 closed 状态初始化；在 `Shared.admission` 下集中处理 non-root/root shutdown marker 发布，在现有 `closing`/`Shared.closing` 发布前标记固定子树，保持 cleanup 顺序不变。仅从 `Entry::live` 移除 `owner.ctx.registration_open()`。完成标准：源码级锁/顺序审查证明关闭边界前所有子树成员均已标记；generation reload 永不清除 marker。
2. **事件竞态测试（#1025-tests）。** 增加确定性 parent→child listener 测试：完成 parent shutdown 的 marker pass 后，立即从不相关的 open context emit，断言 child 调用次数为零；增加 root shutdown、嵌套 subtree、sibling、Loading-owner、重复 shutdown 和 remote-entry 覆盖。利用测试 selection hook 选中 entry，在 `Entry::start` 前 shutdown，断言最终 marker check 跳过；验证已在途 callback 仍 drain。完成标准：sync、`emit`、`serial`、`parallel`、`bail`、waterfall 均不会在 commit 后接纳新的 callback；已接纳 callback 即使在 commit 后开始，也按既有规则 drain。
3. **Benchmark 与 CI（#1025-benchmark）。** 运行 `cargo test -p rutis-loader --release --test close_cpu -- --nocapture`；对比 creation/close CPU 和 queued-emits memory。验收 job 是 `.github/workflows/stress.yml:381-417` 的 Ubuntu 24.04 release job，依次运行 `close_cpu` 和 `queued_emits_memory`；summary 必须保留两项结果。只用实测值更新 0.22 performance record/CI summary。完成标准：记录 issue 工作负载的创建耗时目标（≤30 秒）；自动测试继续比较既有 `CREATE_RATIO`、`CLOSE_RATIO`、`IDLE_SHARE` 阈值，close CPU 不回归，并保持 queued-emits memory 结果。
4. **文档/release（#1025-docs）。** 保持本文档与英文对应稿同步；加入 0.22 release-note 条目，并链接不变的事件关闭保证。完成标准：双语审查确认没有暗示新的开发者可见行为。

## 7. 设计哲学检查

优化只在 Rust core 实现一次，不开放宽 hook；通过测试固定关闭边界和内存序，并且不对跨进程作出超出现有合约的承诺。唯一不能为性能而冒险改变的是精确关闭接纳边界，文档明确写出而不是静默改变。符合设计哲学 §7 的原则 1、3、4、7、10。
