# 事件关闭接纳：两阶段 marker 发布（TLA+）

[English](README.en.md)

用 TLC 穷举关闭一个 fiber 子树与分发方接纳 listener 调用的交错。对应 issue #1025（设计）、#613（模型范围）；质量规范 Q11.2；设计见[不遍历祖先链的事件监听器关闭接纳](../../docs/design-event-close-admission-2026-11-08.md) §2、§3。这是 `specs/event-close/` 的第一块：只回答关闭接纳，不建统一事件的登记表、emit 队列、节点连接和行生命周期；那些是 #613 的后续工作。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 关闭方：在 admission 下固定子树，postorder 为全部成员写 `lineage_closed`，然后发布 `closing`，最后排入 cleanup | 设计 §2 D1、D2；§3 规则 1–3 | `crates/rutis/src/fiber.rs` `mark_subtree_lineage`、`publish_subtree_closing`、`FiberInner::shutdown`、`begin_subtree_shutdown_locked`；`crates/rutis/src/ctx.rs` `CtxInner::shutdown` |
| 分发方：选中 entry 后做最终 `Entry::live`，只读 owner 自己的 marker，不遍历 parent 链 | 设计 §2 D3；§3 规则 4–5 | `crates/rutis/src/bus/table.rs` `Entry::live`、`Entry::start`、`CallFlight`；`crates/rutis/src/bus.rs` `start` |
| 已关闭的 root 下新建 fiber 以 `lineage_closed=true` 开始 | 设计 §2 D1；§3 规则 3 | `crates/rutis/src/fiber.rs` `spawn_fiber_inner`（`is_closed`、`AtomicBool::new(closed)`） |
| 已接纳调用等它结束，cleanup 不越过 call flight | 既有 §7.1；设计 §3 规则 4–5 | `crates/rutis/src/fiber.rs` `begin_event`、`finish_event`、`wait_events` |

设计文档的 Status 是“核心已实现，验证中”（2026-11-09 更新）：代码里已有 `lineage_closed`、两阶段发布、确定性竞态测试和 shuttle 模型。这个模型按当前代码建。

## 模型

- **参与者**：关闭方、分发方、cleanup、创建。关闭方一次只做一个 close pass，和创建共用 `Shared.admission`：pass 从收集子树开始，到排入 cleanup 结束，期间 `Spawn` 和下一次 close 不能发生；分发方不拿这把锁。
- **树**：`r → a → {b, c}`、`b → {d, n}`、`r → m`。初始存在 `r、a、b、c、d`；`n`、`m` 由 `Spawn` 创建。一个 fiber 一个 entry，代表“这个 fiber 上的 listener”；模型不关心一个 fiber 上挂了几个 listener。
- **关闭方**（`Collect`、`Mark`、`MarkingDone`、`Publish`、`Finish`）：
  - `Collect(f)` 在 `f ∈ CloseRoots`、`phase = "idle"` 时固定 `opMembers = Below(f) ∩ exists \ cleaned`；
  - `Mark(f)` 只允许在 `opMembers` 里、且 `f` 的子节点（同样在 `opMembers` 里）都标记之后，写 `lineage_closed`；
  - `MarkingDone` 要求全部成员已标记，记下 `committed`，root 关闭在这里发布 `Shared.closing`；这是 shutdown 的线性化点，此时任何 per-fiber `closing` 都还没发布；
  - `Publish(f)` 只在 `phase = "publishing"` 发布 `closing`（变异打开时可以在 marker pass 中途发布）；
  - `Finish` 要求全部成员已发布，按关闭任务槽排入 cleanup；重复关闭不会给已排过的成员再加一次。
- **分发方**（`Select`、`Start`、`Complete`）：`Select` 取一个 entry，可以是关闭前的旧快照；`Start` 先开始 `CallFlight`，再读 owner 的 `alive`、`cleaned`、`closing`、`lineage_closed`；不 live 就丢掉 flight、不调用 listener；live 就进入 `running`，由 `Complete` 结束。已接纳的调用在 close 提交后仍可继续，cleanup 等它结束。
- **cleanup**（`Cleanup`）：只处理队列里的 fiber，且要求该 fiber 没有 `running` 的调用；重复关闭不能让它跑第二次。
- **创建**（`Spawn`）：在 `phase = "idle"` 时把 `n` 或 `m` 加进 `exists`；如果 `Shared.closing` 已发布，或父 fiber 已标记，新 fiber 直接带 `lineage_closed` 开始。

每个动作原子；模型只保留协议需要的状态。`Select` 允许在入口已被标记之后取到 entry，代表分发方持有旧快照；最终检查才是权威的。

## 简化

- 一个 fiber 一个 entry；登记、撤下、`once`、generation、`withdrawn`、`live_generation` 都不建模。entry 永不撤下，`withdrawn` 恒为 false。
- 不建 event 的 `emit` / `serial` / `parallel` / `bail` / `waterfall` 分发方式、snapshot 列表与缓存、filter、observer；只建“选中一个 entry 然后开始它的一个调用”。
- 每个动作原子，`Shared.admission` 只体现为“一个 close pass 期间不能创建或开始另一个 pass”；真实锁的临界区、`pre_cancel` 和任务树不建。
- 内存序不建模：设计第一版用 `SeqCst`，模型假定 marker 写与最终读的先后如动作顺序。Rust 的实际交错、`crate::sync::AtomicBool` 的切换点留给 #622 的 shuttle 模型。
- 关闭截止时间不建模：真实 `wait_events` 等 `RUTIS_SHUTDOWN_TIMEOUT`，超时后记录错误并继续卸载；模型把等待当成无限，因此 `NoCleanupWithFlight` 和 `AdmittedComplete` 描述的是“截止时间之前”的规则。超时后 callback 是否被取消按现有规则，不在本模型范围。
- `release_transient` 现在在 `Shared.admission` 下摘除 child（`fiber.rs`，2026-11-09）：模型把摘除算作锁内的一部分，不单独建模；子树在 pass 期间固定这个前提由实现满足，锁内细节（drain、锁顺序）不建。
- 时间、故障、远端 listener、进程边界、事件 payload、错误内容都不建。

## 不变式与活性

| 名字 | 内容 |
| --- | --- |
| `ClosedDownward` | 已标记的 fiber 的所有已存在后代都已标记：不会出现祖先已关闭而子树还没标记 |
| `ClosingImpliesMarked` | 任何已发布的 `closing` 都对应一个已标记的 fiber：marker pass 先于公开关闭 |
| `CleanupAfterMarkers` / `CleanupAfterPublish` | cleanup 只在 marker 和 `closing` 之后 |
| `NoCleanupWithFlight` | 一个 fiber 被 cleanup 时没有它的调用还在 `running`：已接纳调用不会被 cleanup 越过（截止时间内） |
| `CleanupOnce` | 一个 fiber 的 cleanup 最多执行一次：重复关闭、父/子重复关闭都复用已排的任务 |
| `NoAdmitMarkedOwner` | 最终检查通过的调用，其 owner 在那一刻还没有 `lineage_closed` |
| `CommittedStaysOut` | 一个 close pass 提交后，它的固定子树里不再有新的调用被接纳，即使 `closing` 还没发布 |
| `SharedClosedImpliesMarked` | `Shared.closing` 只在 root 的 marker pass 完成后发布 |
| `AdmittedComplete` | 活性：已接纳的调用最终都会结束（只看无截止时间、无故障的运行） |

## 结果

TLC 2.19（tla2tools v1.7.4），Java 21.0.12（Temurin），32 核，`-workers auto`。`通过` 指穷举完成、所列性质全部成立；被违反的配置找到第一个反例就停，状态数取决于搜索顺序，每次可能不同。整个目录约 20 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `close-healthy` | 只关 `b` 子树；可创建 `n`/`m`；可重复关闭；安全不变式 | 通过 | 2,263,040 | 10 s |
| `close-tree` | 只关 root（整棵树），不创建；重复关闭；安全不变式 | 通过 | 688,580 | 5 s |
| `close-drain` | 只关 `b` 子树，不创建；只检查 `AdmittedComplete` | 通过 | 31,488 | 1 s |
| `close-mut-early-closing` | marker pass 中途就发布 `closing` | 违反 `ClosingImpliesMarked` | 204（找到反例即停） | <1 s |
| `close-mut-missed-member` | 收集 `b` 的子树时漏掉 `d` | 违反 `CommittedStaysOut` | 824（找到反例即停） | <1 s |
| `close-mut-ignores-marker` | 最终检查只读 `closing`，不读 `lineage_closed` | 违反 `CommittedStaysOut` | 718（找到反例即停） | <1 s |
| `close-mut-reclose` | 重复关闭时把成员再次排入 cleanup | 违反 `CleanupOnce` | 19,780（找到反例即停） | <1 s |

`close-healthy` 原来把活性也放在同一配置里，状态数相同但多花约 40 秒；现在活性单独放在 `close-drain`，PR 上的总时间更可控。

## 变异与反例

每个被检查的性质都配一个故意引入的错误，TLC 必须找到反例（`expected.txt` 的 `violated:<名字>`）：

| 配置 | 变异 | 违反 | 反例 |
| --- | --- | --- | --- |
| `close-mut-early-closing` | `Publish` 在 `phase = "marking"` 就允许 | `ClosingImpliesMarked` | `Collect(a)` 后立刻 `Publish(a)`：`closing = {a}`，`marked = {}`。公开关闭提前到 marker pass 之前，旧的关闭边界被改变 |
| `close-mut-missed-member` | 收集 `b` 时把 `d` 排除出 `opMembers` | `CommittedStaysOut` | `Collect(b)`、`Mark(b)`、`MarkingDone` 提交 `{b}`；`d` 既没 marker 也没 closing，`Select(d)` 后的 `Start(d)` 通过最终检查，`afterCommit[d] = TRUE` |
| `close-mut-ignores-marker` | `Start` 的最终检查不读 `lineage_closed` | `CommittedStaysOut` | `Collect(b)` 标记 `{b,d}` 并 `MarkingDone`，`closing` 还没发布；`Select(b)` 后 `Start(b)` 被接纳，尽管 `b` 已标记、`b` 的 close 已提交 |
| `close-mut-reclose` | `Finish` 不检查关闭任务槽，重复排队 | `CleanupOnce` | 关闭 `b` 两次，`d` 的队列计数变成 2；`Cleanup(d)` 执行两次，`cleanupCount[d] = 2` |

`close-mut-missed-member` 同时也会让 `ClosedDownward` 失败（`b` 已标记、`d` 没有），这个配置只列出预期的那一条；`close-mut-ignores-marker` 同样会让 `NoAdmitMarkedOwner` 失败。分开列是为了让每个反例只对应一个结论。

## 结论

- 按设计 D2 的两阶段发布，正确协议在穷举范围内成立：postorder 标记保证没有“祖先已关闭、后代还没标记”的窗口；marker pass 完成是接纳边界，`closing` 可以晚于它发布；`Entry::live` 只读 owner 自己的 marker 与 `closing`，不需要遍历 parent 链；重复关闭复用关闭任务，cleanup 不会重复执行。
- `CommittedStaysOut` 的两个反例分别锁定设计的两个前提：固定子树必须包含全部成员（`release_transient` 已在 admission 下摘除），最终检查必须读 `lineage_closed`（不能只依赖尚未发布的 `closing`）。`ClosingImpliesMarked` 锁定发布顺序；`CleanupOnce` 锁定重复关闭的幂等性。四条性质都不是空的。
- 模型没有证明的：`SeqCst` 之外的内存序、真实调度的切换点（#622 的 shuttle）、shutdown deadline 之后的行为、动态登记/撤下/generation、事件分发的其它方式，以及 #613 的事件表、emit 队列、节点连接、行生命周期和调用生命周期。本目录只是 #613 的第一块，不能当作完整事件模型的证据。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/event-close/run.sh                    # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/event-close/run.sh close-healthy      # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/event-close/run.sh      # 限定 TLC 的线程数
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；日志和反例写在 `TLC_OUT`（默认一个临时目录）。CI 的 `specs` 任务矩阵要加入 `event-close`（主线程处理，见 [CI](../../docs/ci.md) §4）。

## 维护

设计 §2 的 D1–D5、§3 的规则 1–5 修改时，或 `fiber.rs` 的 `mark_subtree_lineage` / `publish_subtree_closing` / `shutdown`、`bus/table.rs` 的 `Entry::live` / `Entry::start`、`release_transient` 的锁改变时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。
