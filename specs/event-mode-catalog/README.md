# 可选事件 mode 目录与运行时检查（TLA+）

[English](README.en.md)

用 TLC 穷举宿主更新事件 mode 目录（合法更新、清空与非法更新）、多个并发分发方（`emit` / `serial` / `bail`、未列出事件、框架事件 `rutis/*`）、`Warn` 按 epoch 去重，以及两阶段监听器关闭接纳与 drain 的全部交错。对应 issue [#1324](https://github.com/arcships/rutis/issues/1324)；质量规范 Q11.2；设计见[可选事件 mode 目录与比对](../../docs/design-event-mode-catalog-2026-11-20.md) §2、§3。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 宿主更新与校验目录：先校验（空名、`rutis/*`、`internal/*`），再在 `catalog` 写锁内原子递增 `epoch`、替换目录、按 `catalog -> warned` 锁序重置 `Warn` 去重 epoch 并更新 `has_catalog` | 设计 §2 D1、D4、D6；§3.1 | `crates/rutis/src/events/catalog.rs` `EventModeCatalog::validate`；`crates/rutis/src/bus.rs` `EventBus::set_event_mode_catalog`、`EventBus::event_mode_catalog`；`crates/rutis/src/ctx.rs` `CtxHost::set_event_mode_catalog` |
| 并发分发前检查：`snapshot_with` 在选监听器前执行 `check_mode`；先读 `has_catalog` 快路径，再在 `catalog` 读锁下读取 `(epoch, expected, policy)` 并释放读锁；`opts.framework` 跳过用户目录；`Error` 拒绝不启动任何监听器；`Warn` 在 `warned` 锁内核对 `epoch` 后按 `(emitter, event, actual, expected)` 去重且照常投递 | 设计 §2 D3–D5；§3.2 | `crates/rutis/src/bus.rs` `EventBus::check_mode`、`EventBus::snapshot_with`、`WarnedMismatches`；`crates/rutis/src/ctx.rs` `Ctx::emitter_label`；`crates/rutis/src/error.rs` `ErrorKind::EventModeMismatch` |
| 两阶段关闭接纳与在途调用 drain：目录检查与替换不破坏 `lineage_closed` 边界，也不跳过在途调用 | 设计 §2 D5；§3.2；既有事件关闭接纳 | `crates/rutis/src/bus.rs` `EventBus::start`、`EventBus::start_alone`；`crates/rutis/src/bus/table.rs` `Entry::live` |

设计文档处于草稿与原型阶段（2026-11-20）：`crates/rutis` 核心目录与运行时检查已实现，本模型按当前原型代码建模。

## 模型

- **参与者**：宿主目录更新（`SetValidCatalog`、`SetInvalidCatalog`，以及 `nonAtomicSwap` 变异下的分步拆锁动作）、两个并发分发方 `d1` / `d2`（`BeginDispatch`、`CheckCatalog`、`RecordWarn`、`AdmitListener`、`FinishDelivery`、`CompleteCall`、`ResetDispatcher`）、可关闭监听器 `l2` 的两阶段关闭（`CloseMark`、`ClosePublish`、`CloseCleanup`）。
- **事件与分发形态**：`Jobs` 覆盖五类代表性派发：目录内不匹配的 `("msg", "emit", FALSE)` 与 `("msg", "serial", FALSE)`、目录内匹配的 `("msg", "bail", FALSE)`、未列出事件 `("other", "emit", FALSE)`，以及框架事件 `("rutis/row", "emit", TRUE)`。
- **目录、快路径标志与 epoch**：`CatalogConfigs` 在空目录 `("none", "warn")`、告警目录 `("bail", "warn")` 与严格目录 `("bail", "error")` 之间切换（最多 `MaxEpoch` 次）。正确实现（`SetValidCatalog`）在 `catalog` 写锁内原子递增 `epoch`、替换目录、将 `warnedEpoch` 同步到新 `epoch` 并清空 `warned` / `epochWarnCount`，同时更新快路径布尔标志 `hasCatalog = (catalogMode # "none")`；`SetInvalidCatalog` 模拟传入空名、`rutis/*` 或 `internal/*`，正确实现在校验阶段返回错误、完全不改动目录、`hasCatalog` 与 `epoch`。
- **分发管线**：
  - `BeginDispatch` 启动一次分发（每分发方最多 `MaxDispatches` 次，便于覆盖同 epoch 去重与跨 epoch 重置）；
  - `CheckCatalog` 先检查快路径 `hasCatalog`，再读取当前目录与 `epoch`：框架事件跳过用户目录；`Error` 策略命中不匹配时直接进入 `"rejected"`；`Warn` 策略命中不匹配时记录快照 `dSnapEpoch[d] = epoch` 并释放读锁进入 `"recordingWarn"`（允许并发目录切换在获取 `warned` 锁之前插队）；未命中或方式匹配时进入 `"delivering"`；
  - `RecordWarn` 在 `warned` 锁内核对 `warnedEpoch = dSnapEpoch[d]`：若期间已切换到新 `epoch`，忽略过期检查而不污染新 epoch 的去重表；否则按 `<<d, actualMode>>` 记入当前 epoch 去重集合并继续进入 `"delivering"`；
  - `AdmitListener` 逐监听器检查两阶段关闭标记（`marked` / `closing` / `cleaned`），打开则加入 `inFlight` 与 `dInvoked`，已关闭则记入 `dSkippedClosed`；
  - `CompleteCall` 结束在途调用；`CloseCleanup` 仅在 `l2` 已发布 `closing` 且无在途调用时执行。

## 简化

- 两个分发方（`d1`、`d2`）和两个监听器（常活 `l1` 与可关闭 `l2`）；目录只为代表性用户事件 `"msg"` 切换期望方式（`"none"` / `"bail"`），其余用户事件用 `"other"` 代表，框架事件用 `"rutis/row"` 代表。
- 不建 `parallel` / `waterfall` 各自的执行器细节、过滤器闭包、动态登记/撤下、跨节点 `invoke_entry` 与 4096 项 FIFO 淘汰边界（模型通过 `MaxEpoch = 2`、`MaxDispatches = 2` 穷举同 epoch 去重、跨 epoch 清空以及 `CheckCatalog` 与 `RecordWarn` 之间的并发切换交错；有界 FIFO 淘汰由代码与单元测试覆住）。
- 关闭接纳只保留单个可关闭监听器 `l2` 的 `marked -> closing -> cleaned` 两阶段边界；完整子树 postorder 标记与并发创建已在 [`specs/event-close/`](../event-close/README.md) 单独证明。
- `Warn` 的 `ErrorSink` 回调假设不阻塞且不抛 panic（代码里在释放锁后的 `catch_unwind` 中调用）。
- 不建 Rust 原子内存序细节（`Acquire`/`Release`）与 `rutis-bridge` / `rutis.json` 外层路径。

## 不变式与活性

| 名字 | 内容 |
| --- | --- |
| `TypeOK` | 目录状态、`hasCatalog`、`epoch`、`warnedEpoch`、去重集合、在途调用与关闭标记集合始终处于合法域 |
| `NoPartialExecOnError` | `Error` 策略下被拒绝的分发不调用任何监听器（`dInvoked[d] = {}`），也不留下任何在途调用 |
| `AtomicCatalogValidation` | 非法目录更新（`""`、`rutis/*`、`internal/*`）绝不覆写或清空此前的合法目录与 `epoch` |
| `CatalogFlagInSync` | 快路径 `hasCatalog` 始终与是否安装了非空目录同步（`(catalogMode # "none") <=> hasCatalog`），`warnedEpoch = epoch`，且每个键满足 `k ∈ warned <=> epochWarnCount[k] = 1`（同 epoch 不丢告警也不重复告警） |
| `WarnPreservesDelivery` | `Warn` 策略下，不匹配分发不会丢弃投递，所有未因关闭而跳过的合格监听器都被调用（与空目录一致），且每个 epoch 内每组 `(emitter, event, actual, expected)` 至多告警一次 |
| `FrameworkNeverBlocked` | 框架派发（`rutis/*`，`opts.framework = TRUE`）绝不被用户目录拒绝 |
| `CloseDrainPreserved` | 目录检查与替换不会在关闭提交（`closeCommitted`）之后接纳监听器，不会在有在途调用时运行 cleanup，且保持 `cleaned ⊆ closing ⊆ marked` |
| `AllInFlightDrain` | 活性（`catalog-drain`）：并发目录替换与关闭接纳下，所有已接纳的在途调用最终都会结束，不会泄漏 flight |

## 结果

TLC 2.19（tla2tools v1.7.4），OpenJDK 17.0.18，16 核，`-workers auto`。`通过` 指穷举完成、所列性质全部成立；变异配置找到第一个反例即停，状态数取决于搜索顺序。整个目录约 36 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `catalog-healthy` | `MaxEpoch = 2`、`MaxDispatches = 2`；并发目录切换、非法更新、`CheckCatalog`/`RecordWarn` 分锁交错、全部五类派发与关闭接纳；全部安全不变式（含 `CatalogFlagInSync`） | 通过 | 3,780,320 | 24 s |
| `catalog-drain` | `MaxEpoch = 1`、`MaxDispatches = 1`；活性 `AllInFlightDrain` | 通过 | 227,788 | 8 s |
| `mut-check-after-start` | 变异：在 `CheckCatalog` 之前启动监听器 `l1` | 违反 `NoPartialExecOnError` | 805（找到反例即停） | 1 s |
| `mut-clear-on-invalid` | 变异：非法更新在校验失败前清空原目录 | 违反 `AtomicCatalogValidation` | 128（找到反例即停） | <1 s |
| `mut-block-framework` | 变异：框架派发未绕过用户目录检查 | 违反 `FrameworkNeverBlocked` | 833（找到反例即停） | 1 s |
| `mut-warn-drops-delivery` | 变异：`Warn` 告警后直接丢弃投递 | 违反 `WarnPreservesDelivery` | 580（找到反例即停） | 1 s |
| `mut-nonatomic-swap` | 变异：目录切换在更新 `catalogMode` 后释放写锁，再分步更新 `hasCatalog` 与清空 `warned`（允许第二次切换或 `CheckCatalog` 插队） | 违反 `CatalogFlagInSync` | 2（找到反例即停） | 1 s |

## 变异与反例

| 配置 | 变异 | 违反 | 反例 |
| --- | --- | --- | --- |
| `mut-check-after-start` | 允许 `StartBeforeCheck` 在 `CheckCatalog` 之前启动监听器 | `NoPartialExecOnError` | `SetValidCatalog(<<"bail","error">>)` → `BeginDispatch(d, <<"msg","emit",FALSE>>)` → `StartBeforeCheck(d, l1)` 将 `l1` 加入 `dInvoked[d]` 与 `inFlight` → `CheckCatalog(d)` 拒绝分发，导致 `dState[d] = "rejected"` 而 `dInvoked[d] = {"l1"}` |
| `mut-clear-on-invalid` | `SetInvalidCatalog` 在校验拒绝前清空 `catalogMode` | `AtomicCatalogValidation` | `SetValidCatalog(<<"bail","error">>)` 后调用 `SetInvalidCatalog`：`catalogMode` 变为 `"none"`，而 `lastValidMode` 仍为 `"bail"` |
| `mut-block-framework` | `CheckCatalog` 忽略 `dFw[d]`，把严格用户目录套到框架事件上 | `FrameworkNeverBlocked` | `SetValidCatalog(<<"bail","error">>)` → `BeginDispatch(d, <<"rutis/row","emit",TRUE>>)` → `CheckCatalog(d)` 把框架事件判为 `"rejected"` |
| `mut-warn-drops-delivery` | `RecordWarn` 在 `Warn` 下记录告警后直接结束而不进入 `"delivering"` | `WarnPreservesDelivery` | `SetValidCatalog(<<"bail","warn">>)` → `BeginDispatch(d, <<"msg","emit",FALSE>>)` → `CheckCatalog(d)` → `RecordWarn(d)` 将 `dState[d]` 置为 `"done"`、`dResult[d] = "dropped"`，合格监听器 `l1`/`l2` 一个都未被调用 |
| `mut-nonatomic-swap` | 拆开 `set_event_mode_catalog`：写入 `catalogMode` 后即释放写锁，延后更新 `hasCatalog` 与清空 `warned`，允许并发切换与 `CheckCatalog`/`RecordWarn` 插队 | `CatalogFlagInSync` | `NonAtomicWriteCatalog(<<"bail","warn">>)` 释放写锁后，`catalogMode = "bail"`、`epoch = 1`，而 `hasCatalog = FALSE`、`warnedEpoch = 0`（紧接着并发切换可把 `hasCatalog` 永久写反，插队的 `RecordWarn` 也会在随后被 `NonAtomicClearWarned` 清掉导致同 epoch 重复告警） |

## 结论

- 正确协议在 3,780,320 个安全状态与 227,788 个活性状态上穷举成立：目录校验在选表和启动任何监听器之前完成，保证 `Error` 策略零部分执行；`validate()` 先于状态写入，保证非法目录永不破坏现有生效目录；在 `catalog` 写锁内同步更新 `epoch`、`warned` 与 `has_catalog`，加上 `RecordWarn` 核对 `warnedEpoch = dSnapEpoch[d]`，保证快路径标志永不失步且同 epoch 不丢不重；框架事件绕过用户目录；`Warn` 策略保持与空目录一致投递；目录操作与两阶段关闭接纳互不干扰且不泄漏在途调用。
- 五个变异全部被 TLC 在 1 秒内以最短反例抓出，证明五条核心安全性质均非空洞性质。
- 模型未覆盖的：Rust 原子内存序、4096 项有界 FIFO 淘汰、`rutis-bridge` 的跨进程 `BindingError` 转发与 `rutis.json` 解析（由 `crates/rutis/src/bus.rs` 并发单元测试、`crates/rutis/tests/event_mode_catalog.rs` 及步骤 2–3 的集成测试覆住）。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/event-mode-catalog/run.sh                    # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/event-mode-catalog/run.sh catalog-healthy    # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/event-mode-catalog/run.sh      # 限定 TLC 线程数
```

`run.sh` 把每个配置的结果与 `expected.txt` 比对，全部匹配时退出码为 0；日志与反例写在 `TLC_OUT`（默认临时目录）。CI 的 `specs` 矩阵已包含 `event-mode-catalog`（见 [CI](../../docs/ci.md) §4）。

## 维护

设计 §2 D1–D9、§3.1–§3.2 修改时，或 `crates/rutis/src/bus.rs` 的 `set_event_mode_catalog` / `check_mode` / `snapshot_with`、`crates/rutis/src/events/catalog.rs` 的 `validate`、`crates/rutis/src/ctx.rs` 的 `emitter_label` 改变时，同步更新本模型与 `expected.txt`（规范 Q11.2.3）。
