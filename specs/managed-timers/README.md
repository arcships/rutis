# 托管定时器与卸载栈（TLA+）

[English](README.en.md)

用 TLC 穷举插件行中托管定时器（`ctx.timeout` / `ctx.interval`）的触发、同步自取消、外部取消与 LIFO 卸载栈排干之间的全部交错，覆盖可中止运行时（Rust / Python）与不可中止 Promise 运行时（Node 叶子 / Bun）。对应 issue #1325；质量规范 Q11.2；设计见[托管定时器与裸资源边界](../../docs/design-managed-timers-2026-11-20.md) §二、§三。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| Rust 可中止运行时：通过 `effect`（`register_effect_named`）挂入卸载栈、子 `CancellationToken`、`tokio::select!` 取消并经 `TimerExitSignal` 等待任务退出，一次性 `timeout` 结束后立即 `claim` + `finish`（经 `FiberInner::detach_effect` 摘除） | 设计 §二 D1–D6；§三 3.2、3.4 | `crates/rutis/src/ctx.rs` `Ctx::timeout`、`Ctx::interval`、`TimerExitSignal`；`crates/rutis/src/fiber.rs` `FiberInner::detach_effect` |
| Node 叶子不可中止 Promise 运行时：在 `toCordis` 维护 `invoking` 与 `running`，同步序言内自取消不提前移出 `steps`，卸载在更早 LIFO 清理之前 `await running` | 设计 §二 D1–D8；§三 3.2 | `node/rutis-runtime/src/leaf.mjs` `toCordis`（`ctx.timeout`、`ctx.interval`） |
| Bun 不可中止 Promise 运行时：与 Node 叶子相同的 `invoking` / `running` 护栏，挂入 `row.steps` 逆序卸载栈，卸载移出行表后立即阻止新拍启动 | 设计 §二 D1–D7；§三 3.2 | `bun/rutis-bun/src/runner.ts` `Runtime.load`（`ctx.timeout`、`ctx.interval`） |
| Python 可中止运行时：`asyncio.Task` 经 `self.effect(cleanup)` 挂入 `row.steps`；任务未结束前自取消不移出清理，卸载 `task.cancel()` + `await task` 并在 `done_callback` 摘除 | 设计 §二 D1–D7；§三 3.2 | `python/rutis/rutis/runner.py` `Context.timeout`、`Context.interval`、`Context._timer` |

设计文档的状态是“草稿（Rust / Node 叶子 / Bun / Python 最小 PoC 已实现）”（2026-11-20）。这个模型按当前设计与四语言 PoC 代码建立；`mut-self-cancel-drops-step` 复现并钉住了评审中发现的 Node 叶子 / Bun 同步自取消提前摘除 `steps` 的竞态缺陷，`mut-timeout-leaks-stack` 钉住了设计 D3 要求一次性 `timeout` 完成后必须从卸载栈摘除的约束。

## 模型

- **资源与卸载栈**：模型固定一个先于定时器登记的资源 `pool`（`"open"` / `"closed"`），卸载栈 LIFO 顺序为先排定时器清理（`unloadStage = "timer"`），再排 `pool.close()`（`unloadStage = "pool"`）。
- **定时器状态**（`timerState`）：
  - `"scheduled"`：等待延时/周期到达；
  - `"invoking_sync"`：进入回调的同步序言（尚未返回 Promise / Future），此时允许 `SyncSelfCancel`（回调同步调用自己的取消句柄，可重复调用）；
  - `"in_flight_async"`：回调返回 Promise / Future 并挂起在 `await`；
  - `"idle"`：仅 `interval` 使用，表示上一拍刚结算完毕、尚未重新安排下一次等待的边界；
  - `"done"`：终态（一次性 `timeout` 完成，或取消/卸载且全部执行退出）。
- **两种运行时模式**（`Mode`）：
  - `"abortable"`（Rust / Python）：`StartUnload` 可在任意状态发生（包含多线程 Rust 并发于 `invoking_sync`）；进入 `"in_flight_async"` 后，一旦 `cancelled \/ rowState /= "active"`，既可由 `AsyncAbort` 在 `await` 处中止并运行 `Drop`/`finally`，也可由 `AsyncComplete` 正常收尾。
  - `"nonAbortable"`（Node 叶子 / Bun）：单线程 JS 不会在 `invoking_sync` 中途抢占 `StartUnload`；一旦进入 `"in_flight_async"`，Promise 无法被外部强杀，必须经 `AsyncComplete` 结算（访问 `pool`），而卸载栈在 `UnloadTimerStep` 等待其退出后才进入 `UnloadPoolStep`。
- **外部手动取消**（`ManualCancel`）与**同步自取消**（`SyncSelfCancel`）：最多调用 2 次以验证幂等性；在 `scheduled` / `idle` 时直接进入 `done` 并摘除栈条目，在 `invoking_sync` / `in_flight_async` 时保留 `onStack = TRUE`，直到当前拍结束才摘除。
- **固定延迟与周期上界**（`MaxTicks`）：`timeout` 取 `MaxTicks = 1`，`interval` 取 `MaxTicks = 2`，足以穷举“上一拍完成 → 重排下一拍 → 第二拍期间自取消/卸载”的全部状态转移。

## 简化

- 只建一个先登记的外部资源 `pool` 与一个后登记的托管定时器；不建多个同级定时器之间的相互交错（各定时器在卸载栈上互相独立，按各自 `onStack` 与 LIFO 顺序逐个排干）。
- 不建具体时钟数值、错误内容格式化、参数校验（同步前置检查）或 `ErrorSink` / stderr 日志输出：回调正常返回与抛错（被就地捕获）在生命周期状态机上等价，统一落到 `SyncComplete` / `AsyncComplete`。
- 不建卸载超时预算（`RUTIS_SHUTDOWN_TIMEOUT` / #1323）：在 `nonAbortable` 模式下，如果 JS 回调返回永不 resolve 的 Promise，模型里的安全不变式依然成立（`pool` 不会被提前关闭），而卸载活性依赖该 Promise 最终结算或由宿主/运行时卸载预算放弃，见设计稿 §3.2 跨语言取消语义矩阵。
- Go / Swift / Kotlin 尚未落地代码，其可中止取消语义与模型中的 `Mode = "abortable"` 一致。

## 不变式

| 名字 | 内容 |
| --- | --- |
| `TypeOK` | 所有状态变量始终落在合法值域内 |
| `NoUseAfterPoolClose` | `~usedPoolAfterClose /\ (pool = "closed" => inFlight = 0)`：在途回调绝不会在 `pool` 关闭之后继续执行或访问 `pool`（包括异步回调在第一个 `await` 前同步自取消的场景） |
| `NoFireAfterUnload` | `~firedAfterUnload`：行进入 `unloading` 或 `disposed` 之后，绝不再启动任何新的定时回调 |
| `NoOverlappingTicks` | `inFlight <= 1`：同一时刻至多有一个定时回调在执行（固定延迟不重叠） |
| `CleanDetach` | `(timerState = "done" => ~onStack)`、`(rowState = "disposed" => ~onStack /\ timerState = "done")`、`cleanupRuns <= 1`：已完成或已取消的定时器干净摘除卸载栈，卸载完成后无残留，且定时器清理最多执行一次 |

## 结果

TLC 2.19（tla2tools v1.7.4），OpenJDK 17.0.18，`-workers auto`。`通过` 表示全状态空间穷举完成且全部安全不变式成立；变异配置在找到第一个反例时即停。整个目录约 6 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `timers-timeout-abortable` | 一次性 `timeout`，可中止模式（Rust / Python） | 通过 | 55 | <1 s |
| `timers-interval-abortable` | 固定延迟 `interval`（`MaxTicks = 2`），可中止模式（Rust / Python） | 通过 | 100 | 1 s |
| `timers-timeout-nonabortable` | 一次性 `timeout`，不可中止 Promise 模式（Node 叶子 / Bun） | 通过 | 49 | 1 s |
| `timers-interval-nonabortable` | 固定延迟 `interval`（`MaxTicks = 2`），不可中止 Promise 模式（Node 叶子 / Bun） | 通过 | 88 | 1 s |
| `mut-self-cancel-drops-step` | 变异：同步自取消时仅检查 `!running` 提前移出卸载栈 | 违反 `NoUseAfterPoolClose` | 104（找到反例即停） | <1 s |
| `mut-no-await-inflight` | 变异：卸载定时器清理不等在途 Promise 即进入 `pool.close()` | 违反 `NoUseAfterPoolClose` | 100（找到反例即停） | 1 s |
| `mut-overlap-tick` | 变异：固定速率 `interval` 在上一拍未完成时触发第二拍 | 违反 `NoOverlappingTicks` | 86（找到反例即停） | 1 s |
| `mut-fire-after-unload` | 变异：卸载开始后、清理执行前触发回调时不检查行活跃状态 | 违反 `NoFireAfterUnload` | 44（找到反例即停） | <1 s |
| `mut-timeout-leaks-stack` | 变异：一次性 `timeout` 正常完成后进入 `done` 但未从卸载栈摘除（`onStack` 仍为 `TRUE`） | 违反 `CleanDetach` | 44（找到反例即停） | 1 s |

## 变异与反例

四条核心安全性质（含 `CleanDetach`）都配了故意引入的变异，共 5 组，TLC 必须逐一捕获（`expected.txt` 中的 `violated:<名字>`）：

| 配置 | 变异 | 违反 | 反例路径 |
| --- | --- | --- | --- |
| `mut-self-cancel-drops-step` | `SyncSelfCancel` 在 `invoking_sync` 期间把 `onStack` 置 `FALSE`（复现未加 `invoking` 守卫前 `if (!running) removeStep(cleanup)` 的缺陷） | `NoUseAfterPoolClose` | `TimerFire` → `SyncSelfCancel`（`onStack=FALSE`）→ `ReturnAsync`（`in_flight_async`, `inFlight=1`）→ `StartUnload` → `UnloadTimerStep`（因 `~onStack` 跳过等待）→ `UnloadPoolStep`（`pool="closed"` 而 `inFlight=1`） |
| `mut-no-await-inflight` | `UnloadTimerStep` 在 `in_flight_async` 时不等回调结算直接推进到 `pool` | `NoUseAfterPoolClose` | `TimerFire` → `ReturnAsync` → `StartUnload` → `UnloadTimerStep`（不等待即进 `pool`）→ `UnloadPoolStep`（`pool="closed"` 而 `inFlight=1`） |
| `mut-overlap-tick` | 允许 `OverlapFire` 在 `in_flight_async` 时再触发一拍 | `NoOverlappingTicks` | `TimerFire` → `ReturnAsync` → `OverlapFire`（`inFlight=2`） |
| `mut-fire-after-unload` | `TimerFire` 只检查 `~cancelled`，不检查 `rowState = "active"` | `NoFireAfterUnload` | `StartUnload`（`rowState="unloading"`，此时尚未执行 `UnloadTimerStep`）→ `TimerFire`（`firedAfterUnload=TRUE`） |
| `mut-timeout-leaks-stack` | 一次性 `timeout` 在 `SyncComplete` / `AsyncComplete` 正常进入 `done` 时保留 `onStack = TRUE` 而不摘除 | `CleanDetach` | `TimerFire`（`invoking_sync`, `onStack=TRUE`）→ `SyncComplete`（`timerState="done"`, `onStack=TRUE`） |

## 结论

- 在可中止（Rust / Python）和不可中止 Promise（Node 叶子 / Bun）两种语义模式下，修正后的托管定时器状态机均在全状态空间内满足 `NoUseAfterPoolClose`、`NoFireAfterUnload`、`NoOverlappingTicks` 与 `CleanDetach`。
- `mut-self-cancel-drops-step` 的 7 步反例直接证实了 Node 叶子 / Bun 必须维护 `invoking` 守卫（或在回调结算前不得提前把清理移出卸载栈）；其余四条变异分别钉住了“卸载必须等待在途回调”“`interval` 必须不重叠”“触发前必须检查行活跃状态”以及“一次性 `timeout` 正常完成后必须立刻从卸载栈摘除（D3）”。
- 模型没有证明的内容：JS Promise 永不结算时的有界超时放弃（属 #1323 卸载预算）、Tokio/操作系统底层调度器的精确线程切换点、多定时器间共享可变状态、以及尚未实现的 Go / Swift / Kotlin 代码细节。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/managed-timers/run.sh                           # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/managed-timers/run.sh timers-interval-abortable # 单个配置
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0。

## 维护

修改设计稿 §二 D1–D8、§三 3.2–3.4，或修改 `crates/rutis/src/ctx.rs`、`node/rutis-runtime/src/leaf.mjs`、`bun/rutis-bun/src/runner.ts`、`python/rutis/rutis/runner.py` 中的托管定时器取消与卸载逻辑时，同步更新本模型与 `expected.txt`（规范 Q11.2.3）。
