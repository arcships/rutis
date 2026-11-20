# 单行卸载预算与污染状态：路线 B 形式化模型（TLA+）

[English](README.en.md)

用 TLC 穷举共享运行时进程（Node / Bun / Python）中多行、多代次卸载超时、残余后台清理、框架撤回与运行时重启的全部交错，验证路线 B（超时即污染，拒绝同进程静默重载直至运行时重启）的安全性，并给出路线 A（允许同进程静默重载）违反同进程单代隔离的反例。对应 issue #1323；质量规范 Q11.2；设计见[单行卸载预算与诊断](../../docs/design-row-unload-budget-2026-11-20.md) §2、§3，与[设计原则](../../docs/design-philosophy.md) §7、[卸载顺序](../../docs/design-unload-order-2026-11-01.md) D8 对齐。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 叶子卸载栈：外层用户清理 `c2` → 框架服务撤回 `wd` → 内层用户清理 `c1`；超时后跳过后续清理、照跑全部撤回，拒绝对已卸行的晚到 `provide` | 设计 §2 D2、D3、D5；§3.2、§3.3 | `node/rutis-runtime/src/leaf.mjs` `toCordis`、`drain`、`ctx.provide`；`bun/rutis-bun/src/runner.ts` `Runtime.unload`、`Runtime.provide`；`python/rutis/rutis/runner.py` `Runtime.unload`、`Context.provide` |
| Node 原生 Cordis fiber：`Promise.all` 并发处置，服务撤回在首微任务移出 store，超时后停止等待并完成 `finally` 收尾 | 设计 §2 D6；§3.3 | `node/rutis-runtime/src/runner.mjs` `disposeRowFiber`、`unloadRow`、`dispose` |
| 路线 B 污染表：超时行标记 `tainted`，同进程再次 `rows.load` 显式报错拒绝；无故障兄弟行不受影响；重启运行时进程后清零 | 设计 §2 D4、D5；§3.3、§4 | `node/rutis-runtime/src/runner.mjs` `markTainted`、`loadRow`；`bun/rutis-bun/src/runner.ts` `Runtime.load`、`Runtime.unload`；`python/rutis/rutis/runner.py` `Runtime.load`、`Runtime.unload` |
| 残余后台清理的晚到动作与未处理异常隔离（不污染兄弟行或新代次） | 设计 §2 D3、D5；§3.3 | `node/rutis-runtime/src/leaf.mjs` `drain`、`ctx.provide`；`node/rutis-runtime/src/runner.mjs` `disposeRowFiber`；`bun/rutis-bun/src/runner.ts` `Runtime.unload`；`python/rutis/rutis/runner.py` `_consume_task_result`、`Runtime.unload` |

设计文档当前状态为“草稿，PoC 已实现”（2026-11-20）：Node、Bun、Python 均已落地路线 B 污染逻辑与单测，本模型按当前设计稿与 PoC 同步构建。

## 模型

- **参与者**：宿主（`Load`、`LoadRefused`、`StartUnload`、`RestartRuntime`）、共享运行时进程（`epoch ∈ 1..MaxEpoch`）、多行（`Rows = {"w", "m"}`）的多代次实例（`gen ∈ 1..MaxGen`），以及超时后滞留在该进程后台的残余清理集合 `lingering`。
- **叶子模式（`Mode = "leaf"`）**：每行一次卸载按严格逆序经过 `c2`（外层用户清理）→ `wd`（框架服务撤回）→ `c1`（内层用户清理）→ `finish`（`finally` 收尾并回复宿主）：
  - `StartUnload(r)` 立即撤回监听器（§7.1）；
  - `RunC2Ok(r)` 表示 `c2` 在预算内完成；`TimeoutC2(r)` 表示预算到期，`c2` 成为 `<<epoch, r, gen[r]>> ∈ lingering`，按 `Policy` 决定是否把 `r` 加入 `tainted`；
  - `RunWithdraw(r)` 撤回该代服务（D8：不受预算限制）；超时后直接跳到 `finish`（不跑 `c1`）；
  - `FinishUnload(r)` 完成收尾并回复宿主（路线 B 下 `hostState[r]` 进入 `"tainted"`，路线 A 下回到 `"idle"`）。
- **原生 Cordis 模式（`Mode = "cordis"`）**：`CordisWithdraw(r)` 模拟 `ctx.provide` disposer 在首微任务删掉 store 条目，`CordisFinishOk(r)` / `CordisTimeout(r)` 模拟 `Promise.all` 在预算内结束或超时后停止等待并走 `FinishUnload(r)`。
- **残余旧清理**：对于当前进程里仍活着的 `item ∈ lingering`，随时可能通过 `LingeringFinish(item)` 自行结束，或通过 `LingeringLateAction(item, target)` 尝试对同进程正在运行的兄弟行 / 新代次发起晚到 `provide` / `emit` / `withdraw` / 异常。
- **重启运行时（`RestartRuntime`）**：当 `tainted ≠ {}` 且 `epoch < MaxEpoch` 时，宿主重启运行时进程（`epoch' = epoch + 1`），旧进程的 `lingering` 与 `tainted` 清零，所有行回到 `"idle"` 可干净装载。

## 简化

- 每行简化为 1 个服务、1 组监听器、2 条用户清理（`c2` 在 `provide` 后，`c1` 在 `provide` 前）；多条清理的任意前缀超时在语义上归约为“第一条卡住的清理与它之后的至少一条清理”。
- 卸载第 4 步等待正在运行的监听器（沿用 `RUTIS_SHUTDOWN_TIMEOUT`）不在本模型展开，模型从卸载栈排干开始。
- 同步死循环占住事件循环线程导致定时器无法触发的情形（由 5 秒阻塞检测与关闭看门狗处理）不建入本模型。
- 时间抽象为非确定性的“预算内完成”与“预算到期”两个分支，不建具体毫秒值；错误文本、跨进程帧编码与远程节点连接不建。

## 不变式

| 名字 | 内容 |
| --- | --- |
| `TypeOK` | 所有状态变量满足类型与值域约束 |
| `NoOrphanWithdrawal` | D8 不变量：任意行在正常卸载或超时卸载完成后（`hostState ∈ {"idle", "tainted"}`），该行不再遗留任何服务或监听器注册 |
| `SerialCleanupsWithinGen` | D3 不变量：同一代次内，只要外层用户清理 `c2` 已超时仍在 `lingering`，内层用户清理 `c1` 绝不会开始执行 |
| `NoCrossGenCorruption` | D5 / 隔离不变量：残余旧清理的任何晚到动作或未处理异常都不会破坏同进程中其他健康行或新代次 |
| `NoConcurrentSameRowInSharedProcess` | 路线 B 核心性质：在同一个共享运行时进程（同一 `epoch`）中，绝不会出现同一行 `r` 的新代次处于 `"running"` 同时其旧代次的清理仍在 `lingering` 中执行 |

## 结果

TLC 2.19（tla2tools v1.7.4），OpenJDK 17.0.18，16 核，`-workers auto`。`通过` 指穷举完成且所列性质全部成立；被违反的配置找到第一个反例即停。整个目录约 5 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `budget-route-b` | 路线 B（`Policy = "taint"`），叶子模式，2 行 / 2 代 / 2 进程周期，全部 5 条不变式 | 通过 | 1,185 | 1 s |
| `budget-route-b-cordis` | 路线 B（`Policy = "taint"`），Node 原生 Cordis 模式，2 行 / 2 代 / 2 进程周期，全部 5 条不变式 | 通过 | 702 | <1 s |
| `budget-route-a` | 路线 A（`Policy = "allowReload"`），叶子模式，检查 `NoConcurrentSameRowInSharedProcess` | 违反 `NoConcurrentSameRowInSharedProcess` | 198（找到反例即停） | 1 s |
| `budget-mut-skip-withdraw` | 变异：超时时跳过框架撤回 `wd` | 违反 `NoOrphanWithdrawal` | 9（找到反例即停） | 1 s |
| `budget-mut-continue-serial` | 变异：超时后继续执行后续用户清理 `c1` | 违反 `SerialCleanupsWithinGen` | 14（找到反例即停） | 1 s |
| `budget-mut-corrupt-new-gen` | 变异：残余清理缺少隔离/代次守卫而污染运行中的行 | 违反 `NoCrossGenCorruption` | 70（找到反例即停） | 1 s |

## 变异与反例

每个核心性质与路线选择都有对应的反例配置，TLC 必须捕获（`expected.txt` 的 `violated:<名字>`）：

| 配置 | 策略 / 变异 | 违反 | 反例路径 |
| --- | --- | --- | --- |
| `budget-route-a` | `Policy = "allowReload"`（路线 A：超时后允许在同进程原位重载） | `NoConcurrentSameRowInSharedProcess` | `Load("w")`（代 1）→ `StartUnload("w")` → `TimeoutC2("w")`（`<<1,"w",1>> ∈ lingering`）→ `RunWithdraw("w")` → `FinishUnload("w")`（`hostState["w"] = "idle"`）→ `Load("w")`（代 2 进入 `"running"`，而代 1 清理仍在同一进程 `epoch = 1` 后台运行） |
| `budget-mut-skip-withdraw` | `Mutation = "skipWithdraw"`（`TimeoutC2` 跳过 `wd` 直接进 `finish`） | `NoOrphanWithdrawal` | `Load("w")` → `StartUnload("w")` → `TimeoutC2("w")` → `FinishUnload("w")`：`hostState["w"] = "tainted"`，但 `svcOwner["w"] = <<1, 1>>` 未被撤回 |
| `budget-mut-continue-serial` | `Mutation = "continueSerial"`（`RunWithdraw` 在超时后仍进入 `c1`） | `SerialCleanupsWithinGen` | `Load("w")` → `StartUnload("w")` → `TimeoutC2("w")` → `RunWithdraw("w")` → `RunC1("w")`：`c2` 仍在 `lingering` 时 `c1` 已启动，`c1StartedWhileHung["w"] = TRUE` |
| `budget-mut-corrupt-new-gen` | `Mutation = "corruptNewGen"`（残余清理未隔离，改写运行中兄弟行） | `NoCrossGenCorruption` | `Load("w")`、`Load("m")` → `StartUnload("w")` → `TimeoutC2("w")` → `LingeringLateAction(<<1,"w",1>>, "m")`：`corrupted["m"] = TRUE` |

## 结论

- 在穷举范围内，**路线 B**（超时后跑完框架撤回 + 跳过后续用户清理 + 在本运行时进程内污染该行直至重启）同时满足全部四条安全不变式，覆盖叶子栈模式与 Node 原生 Cordis 模式；健康兄弟行可继续无影响运行，重启运行时进程后被污染行干净恢复。
- **路线 A** 的反例直接证明：在 JS / Python 无法强杀残余清理的前提下，允许同进程静默原位重载必然允许同一行的两个代次在共享进程内并发执行，违背[设计原则](../../docs/design-philosophy.md) §7 的“不静默降级、显式优于隐式”。
- 三个变异配置分别锁死了 D8 撤回不变量、D3 跳过后续用户清理、以及残余清理晚到动作隔离；没有任何一条不变式是空性质。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
TLA2TOOLS=/tmp/tla2tools.jar specs/row-unload-budget/run.sh                    # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/row-unload-budget/run.sh budget-route-b     # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/row-unload-budget/run.sh      # 限定 TLC 线程数
```

`run.sh` 把每个配置的结果与 `expected.txt` 比较，全部匹配时退出码为 0；日志和反例输出到 `TLC_OUT`（默认临时目录）。本目录已加入 CI 的 `specs` 矩阵（见 [CI](../../docs/ci.md) §4）。

## 维护

[单行卸载预算与诊断](../../docs/design-row-unload-budget-2026-11-20.md) §2–§3 修改，或 Node（`leaf.mjs` / `runner.mjs`）、Bun（`runner.ts`）、Python（`runner.py`）的单行卸载预算与污染逻辑改动时，同步更新本模型与 `expected.txt`（规范 Q11.2.3）。
