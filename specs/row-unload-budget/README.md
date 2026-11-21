# 单行卸载预算与污染状态：路线 B 形式化模型（TLA+）

[English](README.en.md)

用 TLC 穷举共享运行时进程（Node / Bun / Python）中多作用域行实例（含同名不同 `scope` 的 `instances` 副本）、多代次卸载超时、残余后台清理对共享运行时表的晚到操作、框架撤回与运行时重启的全部交错，验证路线 B（超时即按作用域污染，拒绝同进程静默重载直至运行时重启）的安全性，并给出路线 A（允许同进程静默重载）与 6 项守卫变异的反例。对应 issue #1323；质量规范 Q11.2；设计见[单行卸载预算与诊断](../../docs/design-row-unload-budget-2026-11-20.md) §2、§3，与[设计原则](../../docs/design-philosophy.md) §7、[卸载顺序](../../docs/design-unload-order-2026-11-01.md) D8 对齐。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 叶子卸载栈：外层用户清理 `c2` → 框架服务撤回 `wd` → 内层用户清理 `c1`；超时后跳过后续清理、照跑全部撤回 | 设计 §2 D2、D3；§3.2、§3.3 | `node/rutis-runtime/src/leaf.mjs` `toCordis`、`drain`；`bun/rutis-bun/src/runner.ts` `Runtime.unload`；`python/rutis/rutis/runner.py` `Runtime.unload` |
| Node 原生 Cordis fiber：`Promise.all` 并发处置，服务撤回在首微任务移出 store，超时后停止等待并完成 `finally` 收尾 | 设计 §2 D6；§3.3 | `node/rutis-runtime/src/runner.mjs` `disposeRowFiber`、`unloadRow`、`dispose` |
| 路线 B 作用域污染表：按 `(rowName, scope)`（加精确行键 `key`）标记 `taintedSlots`，同进程重载该作用域实例显式报错拒绝；同名不同 `scope` 副本与无故障兄弟行不受影响；重启运行时进程后清零 | 设计 §2 D4；§3.3、§4 | `node/rutis-runtime/src/runner.mjs` `taintScopeKey`、`markTainted`、`findTaint`、`loadRow`；`bun/rutis-bun/src/runner.ts` `taintScopeKey`、`Runtime.load`、`Runtime.unload`、`Runtime.update`；`python/rutis/rutis/runner.py` `_taint_scope_key`、`Runtime.load`、`Runtime.unload`、`Runtime.update` |
| 共享表守卫与残余清理晚到动作隔离：`LateProvide`（活跃行+同代次守卫）、`LateWithdraw`（登记身份 `<<epoch, slot, gen, token>>` 比对）、`LateUnhandledError`（拒绝回收回调+活跃归属守卫） | 设计 §2 D5；§3.3 | `node/rutis-runtime/src/leaf.mjs` `ctx.provide`、`drain`；`node/rutis-runtime/src/runner.mjs` `disposeRowFiber`、`charge`；`bun/rutis-bun/src/runner.ts` `Runtime.provide`、`Runtime.unload`；`python/rutis/rutis/runner.py` `Context.provide`、`Runtime.provide`、`_consume_task_result`、`Runtime.charge` |

设计文档当前状态为“草稿，PoC 与 TLA+ 模型已实现”（2026-11-20）：Node、Bun、Python 均已落地按作用域键控的路线 B 污染逻辑与单测，本模型按当前设计稿与 PoC 同步构建。

## 模型

- **参与者与共享运行时表**：宿主（`Load`、`LoadRefused`、`StartUnload`、`RestartRuntime`）、共享运行时进程（`epoch ∈ 1..MaxEpoch`）、作用域行槽 `Slots = RowNames × Scopes`（默认 `RowNames = {"w"}`、`Scopes = {"s1", "s2"}`，直接建模同名行 `"w"` 在两个不同作用域中的副本），以及三张真实共享表：
  - `svcTable[svc]`：共享服务表，映射到 `NoOwner` 或归属身份 `<<epoch, slot, gen, token>>`（`token = 1` 为正常装载登记，`token = 2` 为无守卫晚到 `provide`）；
  - `rowStatus[s]`：运行时行表状态（`"none" | "active" | "draining" | "failed"`）；
  - `taintedSlots`：本进程路线 B 污染表（作用域槽集合）；另有 `lingering` 记录超时后滞留后台的旧代次清理及其原始登记身份与服务名。
- **叶子模式（`Mode = "leaf"`）**：每槽一次卸载按严格逆序经过 `c2`（外层用户清理）→ `wd`（框架服务撤回）→ `c1`（内层用户清理）→ `finish`（`finally` 收尾并回复宿主）：
  - `StartUnload(s)` 立即撤回监听器（§7.1）并将 `rowStatus[s]` 置为 `"draining"`；
  - `RunC2Ok(s)` 表示 `c2` 在预算内完成；`TimeoutC2(s)` 表示预算到期，将 `<<epoch, s, gen[s], 1, rowSvc[s]>>` 加入 `lingering`，并按 `Policy` 与 `Mutation` 将 `TaintFor(s)` 记入 `taintedSlots`；
  - `RunWithdraw(s)` 按登记身份撤回该代服务（D8：不受预算限制）；超时后直接跳到 `finish`（不跑 `c1`）；
  - `FinishUnload(s)` 完成收尾并回复宿主（路线 B 下 `hostState[s]` 进入 `"tainted"`，路线 A 下回到 `"idle"`）。
- **原生 Cordis 模式（`Mode = "cordis"`）**：`CordisWithdraw(s)` 模拟 `ctx.provide` disposer 在首微任务删掉 store 条目，`CordisFinishOk(s)` / `CordisTimeout(s)` 模拟 `Promise.all` 在预算内结束或超时后停止等待并走 `FinishUnload(s)`。
- **残余旧清理对共享表的真实晚到动作**：对当前 `epoch` 中仍活着的 `item ∈ lingering`，随时可能通过 `LingeringFinish(item)` 自行结束，或发起三类真实操作：
  - `LateProvide(item, svc)`：检查 `rowStatus[s] = "active" ∧ gen[s] = g` 守卫后再写 `svcTable`；变异 `"missingActiveGuard"` 去掉守卫直接改写 `svcTable`；
  - `LateWithdraw(item)`：检查 `svcTable[svc] = <<ep, s, g, tok>>` 登记身份后再删除；变异 `"withdrawByNameOnly"` 只按服务名删除；
  - `LateUnhandledError(item, target)`：检查活跃源归属与拒绝吸收守卫；变异 `"errorCorruptsLive"` 去掉守卫，把运行中的 `target` 在 `rowStatus` 中标成 `"failed"`。
- **重启运行时（`RestartRuntime`）**：当 `taintedSlots ≠ {}` 且 `epoch < MaxEpoch` 时，宿主重启运行时进程（`epoch' = epoch + 1`），旧进程的 `lingering`、`taintedSlots` 与共享表清零，所有槽回到 `"idle"` 可干净装载。

## 简化

- 每槽简化为最多提供 1 个共享服务（`Services = {"forecast"}`；空闲时认领，被占时仍可并行装载并在对方撤回后接管）、1 组监听器、2 条用户清理（`c2` 在 `provide` 后，`c1` 在 `provide` 前）；多条清理的任意前缀超时归约为“第一条卡住的清理与它之后的至少一条清理”。
- 卸载第 4 步等待正在运行的监听器（沿用 `RUTIS_SHUTDOWN_TIMEOUT`）不在本模型展开，模型从卸载栈排干开始。
- 同步死循环占住事件循环线程导致定时器无法触发的情形（由 5 秒阻塞检测与关闭看门狗处理）不建入本模型。
- 时间抽象为非确定性的“预算内完成”与“预算到期”两个分支，不建具体毫秒值；错误文本、跨进程帧编码与远程节点连接不建。

## 不变式

| 名字 | 内容 |
| --- | --- |
| `TypeOK` | 所有状态变量满足类型与值域约束 |
| `NoOrphanWithdrawal` | D8 不变量：任意槽在正常卸载或超时卸载完成后（`hostState ∈ {"idle", "tainted"}`），`svcTable` 与监听器中不再遗留归属于该槽的注册 |
| `SerialCleanupsWithinGen` | D3 不变量：同一代次内，只要外层用户清理 `c2` 已超时仍在 `lingering`，内层用户清理 `c1` 绝不会开始执行 |
| `ServiceOwnerValid` | 共享服务表有效性：`svcTable` 中每一条非空注册都属于当前 `epoch`、处于 `"active"` 或 `"draining"` 的槽、当前代次 `gen`、合法装载令牌 `1` 且与 `rowSvc` 一致 |
| `ActiveServiceIntact` | 活跃服务完整性：每一个处于 `"active"` 且登记了服务的槽，其在 `svcTable` 中的条目始终保持为自己的 `<<epoch, s, gen[s], 1>>`（不被晚到 `withdraw` 删掉或被晚到 `provide` 覆盖） |
| `NoSpuriousRowFailure` | 行表保护：没有任何运行中的槽因残余旧清理的晚到异常而被标记为 `rowStatus = "failed"` |
| `NoCrossGenCorruption` | D5 跨行/跨代隔离合取：`ServiceOwnerValid ∧ ActiveServiceIntact ∧ NoSpuriousRowFailure` |
| `HealthyInstanceNotBlocked` | 多实例作用域污染隔离：`taintedSlots ⊆ timedOutInEpoch`——未在当前进程超时的健康实例（包括同名不同 `scope` 的副本）绝不会被误污染或阻塞 |
| `NoConcurrentSameRowInSharedProcess` | 路线 B 核心性质：在同一个共享运行时进程（同一 `epoch`）中，绝不会出现同一作用域槽 `s` 处于 `"active"` 同时其旧代次的清理仍在 `lingering` 中执行 |

## 结果

TLC 2.19（tla2tools v1.7.4），OpenJDK 17.0.18，16 核，`-workers auto`。`通过` 指穷举完成且所列性质全部成立；被违反的配置找到第一个反例即停。整个目录约 6 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `budget-route-b` | 路线 B（`Policy = "taint"`），叶子模式，同名双作用域 `{<<"w","s1">>, <<"w","s2">>}` / 2 代 / 2 进程周期，全部 9 条不变式 | 通过 | 3,271 | 1 s |
| `budget-route-b-cordis` | 路线 B（`Policy = "taint"`），Node 原生 Cordis 模式，同名双作用域 / 2 代 / 2 进程周期，全部 9 条不变式 | 通过 | 1,369 | 1 s |
| `budget-route-a` | 路线 A（`Policy = "allowReload"`），叶子模式，检查 `NoConcurrentSameRowInSharedProcess` | 违反 `NoConcurrentSameRowInSharedProcess` | 187（找到反例即停） | 1 s |
| `budget-mut-skip-withdraw` | 变异：超时时跳过框架撤回 `wd` | 违反 `NoOrphanWithdrawal` | 9（找到反例即停） | 1 s |
| `budget-mut-continue-serial` | 变异：超时后继续执行后续用户清理 `c1` | 违反 `SerialCleanupsWithinGen` | 16（找到反例即停） | <1 s |
| `budget-mut-missing-active-guard` | 变异：`LateProvide` 缺少活跃/排干守卫，直接写 `svcTable` | 违反 `ServiceOwnerValid` | 119（找到反例即停） | <1 s |
| `budget-mut-withdraw-by-name` | 变异：`LateWithdraw` 仅按服务名删除 `svcTable`，不比对登记身份 | 违反 `ActiveServiceIntact` | 458（找到反例即停） | <1 s |
| `budget-mut-error-corrupts-live` | 变异：`LateUnhandledError` 缺少吸收/归属守卫，把运行中兄弟行标成 `"failed"` | 违反 `NoSpuriousRowFailure` | 195（找到反例即停） | 1 s |
| `budget-mut-taint-by-bare-name` | 变异：仅按裸 `rowName` 污染，忽略 `scope` | 违反 `HealthyInstanceNotBlocked` | 72（找到反例即停） | 1 s |

## 变异与反例

每条核心性质、真实共享表守卫以及路线选择都有对应的反例配置，TLC 必须捕获（`expected.txt` 的 `violated:<名字>`）：

| 配置 | 策略 / 变异 | 违反 | 反例路径 |
| --- | --- | --- | --- |
| `budget-route-a` | `Policy = "allowReload"`（路线 A：超时后允许同进程原位重载） | `NoConcurrentSameRowInSharedProcess` | `Load(<<"w","s1">>)`（代 1）→ `StartUnload` → `TimeoutC2`（`<<1,<<"w","s1">>,1,1,"forecast">> ∈ lingering`）→ `RunWithdraw` → `FinishUnload`（回到 `"idle"`）→ `Load(<<"w","s1">>)`（代 2 进入 `"active"`，而代 1 清理仍在 `epoch = 1` 后台运行） |
| `budget-mut-skip-withdraw` | `Mutation = "skipWithdraw"`（`TimeoutC2` 跳过 `wd` 直接进 `finish`） | `NoOrphanWithdrawal` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2` → `FinishUnload`：`hostState = "tainted"`，但 `svcTable["forecast"] = <<1, <<"w","s1">>, 1, 1>>` 未撤回 |
| `budget-mut-continue-serial` | `Mutation = "continueSerial"`（`RunWithdraw` 在超时后仍进入 `c1`） | `SerialCleanupsWithinGen` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2` → `RunWithdraw` → `RunC1`：`c2` 仍在 `lingering` 时 `c1` 启动，`c1StartedWhileHung[<<"w","s1">>] = TRUE` |
| `budget-mut-missing-active-guard` | `Mutation = "missingActiveGuard"`（`LateProvide` 省去活跃行守卫） | `ServiceOwnerValid` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2` → `LateProvide`：向 `svcTable["forecast"]` 写入无卸载栈追踪的 `<<1, <<"w","s1">>, 1, 2>>` |
| `budget-mut-withdraw-by-name` | `Mutation = "withdrawByNameOnly"`（`LateWithdraw` 省去登记身份比对） | `ActiveServiceIntact` | `Load(<<"w","s2">>)` 占 `"forecast"` → `StartUnload` → `TimeoutC2` → `RunWithdraw` 释放 `"forecast"` → `Load(<<"w","s1">>)` 接管 `"forecast"` → `<<"w","s2">>` 的 `LateWithdraw` 按名清空 `svcTable["forecast"]`，删掉 `<<"w","s1">>` 的活跃服务 |
| `budget-mut-error-corrupts-live` | `Mutation = "errorCorruptsLive"`（`LateUnhandledError` 省去吸收/归属守卫） | `NoSpuriousRowFailure` | `Load(<<"w","s1">>)` 与 `Load(<<"w","s2">>)` 并行运行 → `StartUnload(<<"w","s1">>)` → `TimeoutC2` → `LateUnhandledError` 将健康运行中的 `rowStatus[<<"w","s2">>]` 置为 `"failed"` |
| `budget-mut-taint-by-bare-name` | `Mutation = "taintByBareName"`（污染表只按裸 `rowName` 键控） | `HealthyInstanceNotBlocked` | `Load(<<"w","s1">>)` → `StartUnload` → `TimeoutC2`：`timedOutInEpoch = {<<"w","s1">>}`，但 `taintedSlots = {<<"w","s1">>, <<"w","s2">>}`，误阻塞另一作用域的健康 `"w"` 副本 |

## 结论

- 在穷举范围内，**路线 B**（跑完框架撤回 + 跳过后续用户清理 + 按 `(rowName, scope)` 作用域污染直至重启 + 共享表真实守卫）同时满足全部 9 条安全不变式，覆盖叶子栈模式与 Node 原生 Cordis 模式；同名不同作用域的健康副本与无关兄弟行均可自由运行、接管服务与重载，重启运行时进程后被污染槽干净恢复。
- **路线 A** 的反例直接证明：在 JS / Python 无法强杀残余清理的前提下，允许同进程静默原位重载必然导致同一作用域行的两个代次在共享进程内并发执行，违背[设计原则](../../docs/design-philosophy.md) §7 的“不静默降级、显式优于隐式”。
- 6 个变异配置分别锁死了 D8 撤回、D3 串行保护、晚到 `provide` 的活跃守卫、晚到 `withdraw` 的登记身份比对、晚到异常的行表隔离，以及多实例作用域污染隔离；没有任何一条不变式是空性质或重言式。

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
