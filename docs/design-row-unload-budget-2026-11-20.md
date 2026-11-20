# Node / Bun / Python 单行卸载预算与诊断

[English](design-row-unload-budget-2026-11-20.en.md)

状态：草稿，PoC 与 TLA+ 模型已实现（分支 `design/1323-unload-budget`，未合并）。日期：2026-11-20（同日修订：按设计原则 §7 收敛到路线 B——超时后显式污染该行并拒绝同进程静默原位重载直到运行时重启，补齐语义状态分析与 `specs/row-unload-budget/` TLA+ 模型及 TLC 验证结果）。基准：`main` `5c2e143e`（0.23.0 之后）。
Issue：[#1323](https://github.com/arcships/rutis/issues/1323)。相关：[设计原则](design-philosophy.md)（§7）、[卸载顺序](design-unload-order-2026-11-01.md)（D7、D8、§三、§六）、[Cordis 兼容边界](guide/cordis-boundaries.md)（§7、§8）、[诊断设计](design-diagnostics-2026-10-26.md)（§4.5、§六）、[事件协议](design-events-protocol-2026-10-18.md)（§7.1）、[TLA+ 模型](../specs/row-unload-budget/README.md)。

## 一、现状、语义状态差异与要解决什么

### 1.1 现状问题

当宿主对 Node、Bun 或 Python 运行时中的某一行发起单行卸载（`rows.unload` —— 由配置移除、重载、配置变更、依赖下线或加载失败回滚触发）时，运行时会一直等待该行的用户清理完成，**没有任何单行时间预算**：

- Node 叶子插件（`node/rutis-runtime/src/leaf.mjs` 的 `drain`）逐条 `await` 卸载栈；原生 Cordis 插件（`node/rutis-runtime/src/runner.mjs` 的 `unloadRow`）直接 `await row.fiber.dispose()`。
- Bun（`bun/rutis-bun/src/runner.ts` 的 `unload`）逐条 `await this.within(key, step)`。
- Python（`python/rutis/rutis/runner.py` 的 `unload`）逐条 `await _settle(step())`。

只要其中任意一条用户清理永不返回（例如等待永不落定的 Promise / 协程、未关闭的连接池、或者在[卸载顺序](design-unload-order-2026-11-01.md)改为逆序串行后两个清理互相等对方）：

1. 运行时的 `rows.unload` 永远不会回复，宿主侧该行永久卡在 `Unloading` 状态，无法完成卸载或重载；
2. 排在卡住清理之后的框架撤回（例如 `provide` 登记的撤回）永远跑不到，陈旧服务注册滞留在进程表里；
3. `--shutdown-timeout` / `RUTIS_SHUTDOWN_TIMEOUT`（默认 10 秒）只在**整个宿主停止或会话断开**时生效，并且 Node / Bun / Python 到点后是直接退出整个运行时进程（退出码 1），而不是只解除卡住的那一行；
4. 单行卸载卡住时运行时没有任何诊断输出（[卸载顺序](design-unload-order-2026-11-01.md) §三 与 §六 记下了这一缺口，[Cordis 兼容边界](guide/cordis-boundaries.md) §8 也明确区分了调用超时与宿主关闭截止时间）。

### 1.2 超时引入的真实语义状态：`stale_cleanup_running`

在共享运行时进程（多行共用一个 Node / Bun / Python 进程）里，**“清理超时返回”与“正常卸载完成”不是同一个语义状态**：

- **正常卸载完成**：旧代次的所有用户清理已真正退出，进程内没有任何该行旧代次代码在执行。此时再次装载该行的新代次，天然保证**同一共享进程内同一行不会有两个代次并发运行**。
- **超时返回（`stale_cleanup_running`）**：JavaScript 无法强杀未落定的 Promise，Python 也无法强杀吞掉 `asyncio.CancelledError` 或阻塞在外部调用中的协程；
  - 在**叶子卸载栈**（Node 叶子、Bun、Python）里，运行时停止等待卡住的那条清理并跳过余下用户清理，但**卡住的那一条清理仍在后台存活**；
  - 在 **Node 原生 Cordis fiber** 里，Cordis 4.0.4 的 `_unload()` 用 `Promise.all` 并发启动了全部处置且无法打断，因此**可能有多条处置仍在后台存活**。

如果超时后假装这一行已经“干净卸载”并允许同进程静默原位重载（**路线 A**），就会把一个不可消除的并发状态静默降级为隐藏竞态——旧代次残余清理与新代次 `apply` / 服务方法在同一个进程里并发执行，争抢端口、文件锁、模块级单例、全局状态与连接池。这直接违背[设计原则](design-philosophy.md) §7 的两条核心原则：**“跨边界做不到的事写成边界规则，不静默降级”**与**“显式优于隐式”**。

因此，本稿对比两条路线并采用**路线 B**：

| 维度 | 路线 A（同进程静默允许原位重载——放弃） | 路线 B（同进程显式污染，拒绝静默原位重载直到运行时重启——采纳） |
| --- | --- | --- |
| 框架撤回（D8） | 跑完服务/监听器/槽位撤回 | 跑完服务/监听器/槽位撤回 |
| 健康兄弟行 | 继续运行 | 继续运行，完全不受影响 |
| 同一行在同进程再次 `rows.load` | 允许启动新代次，与残余旧清理并发（TLC `budget-route-a` 反例证伪 `NoConcurrentSameRowInSharedProcess`） | **显式报错拒绝**，直接告知旧清理超时且可能仍在运行、要求重启运行时或隔离该行（TLC `budget-route-b` / `budget-route-b-cordis` 全性质通过） |
| 恢复方式 | 寄望新旧两代不撞外部资源 | 重启该运行时进程（或将该行隔离到独立运行时）后干净装载 |
| 与第 2 阶段宿主策略配合 | 宿主无从区分“干净卸载”与“带残余退出” | 运行时立即强制拒绝静默降级；第 2 阶段宿主再补显式超时观察与可选策略（`taint` / 自动 `restart-runtime`） |

六种语言本稿前的卸载预算现状：

| 运行时 | 单行 `rows.unload` 清理预算（本稿前） | 会话结束 / `dispose` 清理预算（本稿前） |
| --- | --- | --- |
| Node（叶子插件） | 无：一直等到栈跑完 | 无逐行预算；会话断开后 `RUTIS_SHUTDOWN_TIMEOUT` 到点整进程退出（退出码 1） |
| Node（原生 Cordis） | 无：一直等到 `row.fiber.dispose()` 落定 | 同上 |
| Bun | 无：一直等到栈跑完 | 同上 |
| Python | 无：一直等到栈跑完 | 同上 |
| Kotlin | 无：一直等到栈跑完 | `dispose(shutdownMillis)` 逐条按剩余时间跑，撤回不受限（D8） |
| Go | 无：一直等到栈跑完 | `dispose(ctx)` 受 `RUTIS_SHUTDOWN_TIMEOUT` 约束，到点放弃当前行及后续行 |
| Swift | 无：一直等到栈跑完 | `dispose(within:)` 受 `RUTIS_SHUTDOWN_TIMEOUT` 约束，到点放弃当前行及后续行 |

本稿先落 Node、Bun、Python 三家（含路线 B 污染逻辑与 TLA+ 形式化模型）；Go、Swift、Kotlin 的单行预算同步纳入 §六 后续步骤，共用同一环境变量与语义。

## 二、决策

| # | 决策 | 为什么；放弃的做法 |
| --- | --- | --- |
| D1 | **显式开启（opt-in），默认不限时**：新增环境变量 `RUTIS_ROW_UNLOAD_TIMEOUT`（单位：**秒**，正有限浮点数，解析规则与 `RUTIS_SHUTDOWN_TIMEOUT` 完全一致）；未设置、空串、`<= 0`、`NaN`、`Infinity` 或无法解析时均表示**关闭（不限时）**。同时为测试和嵌入方提供显式运行时选项：Node `run({ rowUnloadTimeout })`（毫秒）与 `toCordis(..., unloadOptions)`、Bun `new Runtime(project, { rowUnloadTimeout })`（毫秒）、Python `Runtime(row_unload_timeout=...)`（秒） | 默认关闭保证不改变原生 Cordis 与现有 rutis 行为（合法的慢速刷盘 / 模型释放不会被悄悄截断）；显式开启让开发环境热重载、运维和自动化测试可以按需设限，并支持用 `RUTIS_ROW_UNLOAD_TIMEOUT=0` 覆盖继承来的环境变量。放弃“默认开启 10 秒单行预算”：会破坏依赖慢速清理的既有插件；放弃“直接复用 `RUTIS_SHUTDOWN_TIMEOUT`”：宿主停机预算与单行卸载预算是两件事 |
| D2 | **预算只约束用户清理，永不约束框架撤回**（延续[卸载顺序](design-unload-order-2026-11-01.md) D8）：单行预算是一次卸载中该行**全部用户清理步骤**（`ctx.effect` 与 `apply` 返回的清理）共享的总预算。框架自身的撤回与收尾——`provide` 登记的服务撤回、监听器撤回（`bus.withdraw`）、导出器处置（`exporter.dispose`）、句柄失效与槽位清理（`gone` / `slots.delete`）、行表移除与 `provideWaitingLater`——**永远不受此预算限制，必须跑完**。卸载第 4 步（等待正在运行的监听器）仍沿用[事件协议](design-events-protocol-2026-10-18.md) §7.1 的 `RUTIS_SHUTDOWN_TIMEOUT`；单行清理预算从卸载栈开始排干时起算 | 框架撤回是内存表更新与撤回通知，不会挂住；跳过它们会把陈旧服务永久留在进程表里（TLC `budget-mut-skip-withdraw` 直接抓出 `NoOrphanWithdrawal` 违反），导致健康的兄弟行或等待中的宿主服务也无法接管该服务名。放弃“每条清理各给一份预算”：会把卡住时间放大 N 倍；放弃“连撤回一起限时”：违背 D8 |
| D3 | **超时后停止等待当前清理、跳过余下用户清理、但必须跑完余下全部框架撤回**：当某条用户清理耗尽剩余预算（或下一条用户清理开始前剩余预算已为 0）时：(1) 停止等待该清理（Python 经 `asyncio.wait` + `task.cancel()` 发出取消，不使用会在协程吞掉 `CancelledError` 时卡死的 `asyncio.wait_for`；JS / Python 都挂上结果/拒绝回收回调以防延迟抛错触发未处理拒绝）；(2) **不再执行**栈上余下的任何用户清理；(3) **照常执行**栈上余下的每一条框架撤回，并走完 `finally` 收尾 | 在卡住的清理仍在后台执行的条件下，不可能同时宣称“解除卡死”又宣称“保持严格逆序串行”（TLC `budget-mut-continue-serial` 证伪 `SerialCleanupsWithinGen`）：继续执行更早登记的用户清理会与尚未退出的后登记清理竞态并破坏 LIFO 契约。框架撤回则相反，必须执行以保持表一致。放弃“超时后继续跑后续用户清理”与“整栈放弃” |
| D4 | **采用路线 B：超时行在本运行时进程内显式标记为污染（tainted），拒绝同进程静默原位重载直至运行时重启；`rows.unload` 正常完成并向 stderr 输出逐字诊断行**：超时发生后：(1) 运行时将该行的行名与行键记入本进程的 `tainted` 表，并在跑完余下撤回与 `finally` 后正常返回 `rows.unload`（不拖垮兄弟行；若超时前已有清理抛出异常则照旧保留）；(2) 运行时向不带行前缀的 stderr 输出一条逐字诊断行（不含 `rutis`、`Cordis`、`fiber`、`Promise.all` 等内部词）；(3) 之后在**同一运行时进程**内对该行（无论沿用旧 key 还是宿主分配的新 key）调用 `rows.load` / `rows.update` 时，运行时**显式报错拒绝**（逐字错误见 §三 与 §四），直到重启运行时进程或将该行隔离到独立运行时；(4) 同一进程里的其他健康行完全不受影响；(5) 第 2 阶段宿主整合可进一步暴露宿主侧可观察超时策略（如 `rutis-host status` 展示污染原因或按策略自动重启运行时） | 对齐[设计原则](design-philosophy.md) §7“显式优于隐式、不静默降级”，并用 TLA+ 证明 `NoConcurrentSameRowInSharedProcess` 在路线 B 下成立、在路线 A 下被反例击穿。相比“让 `rows.unload` 自身超时失败”，先让 `rows.unload` 干净撤回并结束 `Unloading`、再在同进程 `rows.load` 时显式拒绝，既避免宿主行永久卡死或拖垮无故障兄弟行，又绝不静默启动并发第二代。放弃路线 A（静默允许同进程原位重载）：允许新旧两代并发，隐蔽破坏状态 |
| D5 | **隔离残余后台清理的晚到动作，如实说明同步阻塞边界**：JS / Python 无法强杀卡住的 Promise、拒绝 `CancelledError` 的协程或同步阻塞代码，但运行时保证残余旧清理的任何晚到动作都不能破坏其他健康行或后续状态（TLC `NoCrossGenCorruption`）：(1) 晚到 `ctx.provide` 被拒（Node `leaf.mjs` 补齐 `if (draining)` 守卫，Bun / Python 已有活跃行守卫）；(2) 晚到 `ctx.effect` / `ctx.on` / `ctx.emit` 被拒；(3) 持有旧 `withdraw()` 句柄晚到调用时按登记身份比对，绝不误撤兄弟行的新服务；(4) 残余清理的晚到异常 / 拒绝被回收回调吸收，不拖垮进程也不错误归责给别的行。同步死循环占住唯一事件循环线程时异步定时器无法抢占，仍由 5 秒阻塞提示（[诊断设计](design-diagnostics-2026-10-26.md) §6.4）和 `RUTIS_SHUTDOWN_TIMEOUT` 看门狗兜底 | 诚实面对语言边界，同时用硬守卫和 TLA+ 模型把跨行/跨代污染堵死。放弃“单行超时直接 `SIGKILL` 整个共享运行时进程”：不该无差别杀死同进程里的健康行 |
| D6 | **Node 原生 Cordis 插件单独处理，不给 `_unload` 打补丁**：开启单行预算时，`runner.mjs` 通过 `LEAF_FIBERS.has(row.inner)` 区分叶子插件与原生 Cordis 插件。叶子插件由 `leaf.mjs` 的 `drain()` 自己计时、通过 `onTimeout` 通知 `runner.mjs` 标记污染、并保证跑完余下撤回（`runner.mjs` 不另挂同毫秒外层定时器，避免外层抢先触发）；原生 Cordis 插件由 `runner.mjs` 在 `unloadRow` 与 `dispose` 里将 `row.fiber.dispose()` 与预算竞态，超时后停止等待、标记污染、吞掉延迟拒绝、打印原生插件逐字诊断行，并照常走完 `finally`（`row.unloaded = true`、`bus.withdraw(key)`、`bus.forgetRow(key)`）与 `provideWaitingLater()`。Cordis 4.0.4 的内部 `Promise.all` 无法打断，但其 `dispose()` 在进入 `_unload()` 前已把 fiber 移出 `ctx.registry`，`ctx.provide` 的 disposer 也在首微任务从 `ctx.reflect.store` 删掉了服务，因此服务、导出器、监听器与 `waitingHosts` 仍能干净收尾 | 遵守[Cordis 兼容边界](guide/cordis-boundaries.md) §7“运行时不给 `_unload` 打补丁”的约束，且原生 Cordis 同样受路线 B 污染保护（TLC `budget-route-b-cordis` 通过全部 5 条不变式）。原生 Cordis 的 `rows.update` 走 `fiber.update`（合并卸载与重新 `apply`），为免误伤慢速 `apply` 暂保持原样，仅 `rows.unload` 与 `dispose` 受预算约束 |
| D7 | **Go / Swift / Kotlin 纳入跨语言规划，不塞进本 PoC**：本稿及 PoC 先落 Node / Bun / Python 三家与 TLA+ 形式化模型；Go、Swift、Kotlin 当前单行 `rows.unload` 同样无预算，留在 §六 后续步骤，统一使用 `RUTIS_ROW_UNLOAD_TIMEOUT`、路线 B 污染语义与相同诊断措辞 | 控制 PoC 范围，优先补齐完全没有任何清理预算的三家运行时，并保证六家长期收敛 |

## 三、规则

### 3.1 预算解析与优先级

1. **环境变量**：`RUTIS_ROW_UNLOAD_TIMEOUT`，单位为**秒**（正有限浮点数，与 `RUTIS_SHUTDOWN_TIMEOUT` 完全一致）：

| 环境变量值 | Node / Bun（毫秒） | Python（秒） | 行为 |
| --- | --- | --- | --- |
| 未设置 / `""` | `null` | `None` | 关闭（不限时） |
| `"5"` | `5000` | `5.0` | 开启，单行清理总预算 5 秒 |
| `"0.25"` | `250` | `0.25` | 开启，单行清理总预算 0.25 秒 |
| `"0"` / `"-1"` / `"NaN"` / `"Infinity"` / `"abc"` | `null` | `None` | 关闭（不限时；`0` 可显式关闭继承来的环境变量） |

2. **显式选项优先级**：
   - Node：`run({ ..., rowUnloadTimeout })` 传入毫秒数；未传（`undefined`）时读取 `env.RUTIS_ROW_UNLOAD_TIMEOUT`；显式传入 `null`、`<= 0` 或非有限数时强制关闭。独立调用 `toCordis(spec, rowInject, providerOf, unloadOptions)` 时，`unloadOptions.rowUnloadTimeout` 遵循同样规则（未传 `unloadOptions` 时读取 `process.env.RUTIS_ROW_UNLOAD_TIMEOUT`）。
   - Bun：`new Runtime(project, { rowUnloadTimeout })` 传入毫秒数；未提供 `rowUnloadTimeout` 键时读取 `process.env.RUTIS_ROW_UNLOAD_TIMEOUT`；显式传入 `null`、`<= 0` 或非有限数时强制关闭。
   - Python：`Runtime(row_unload_timeout=...)` 传入秒数；未传时读取 `os.environ.get("RUTIS_ROW_UNLOAD_TIMEOUT")`；显式传入 `None`、`<= 0` 或非有限数（以及 `bool`）时强制关闭。

### 3.2 计时范围与步骤分类（D8 不变量）

1. **计时起点与时钟**：使用单调时钟（Node / Bun：`performance.now()`；Python：`asyncio.get_running_loop().time()`）。在卸载第 4 步（等待正在运行的监听器，仍受 `RUTIS_SHUTDOWN_TIMEOUT` 封顶）结束之后、进入卸载栈排干（或原生 Cordis `row.fiber.dispose()`）那一刻起算，单行内所有用户清理共享同一份总预算。
2. **步骤分类**：

| 步骤 | 是否受单行预算约束 | 超时后是否仍必须执行 |
| --- | --- | --- |
| `ctx.effect(...)` 登记的清理、`apply` 返回的清理 | **是** | 否（卡住的那条停止等待，余下的用户清理跳过） |
| `ctx.provide(...)` 留在卸载栈上的服务撤回 | **否** | **是**（按栈上逆序位置逐条跑完） |
| 监听器撤回（`bus.withdraw` / `withdraw_row`）、`row.unloaded = true` | **否** | **是** |
| 导出器处置（`exporter.dispose`）、句柄标记失效（`gone`）、槽位删除（`slots.delete`） | **否**（栈前已完成） | **是** |
| `rows` 表删除、`bus.forgetRow`、Node `provideWaitingLater()` | **否** | **是** |
| 污染登记（`tainted` 记录行名与行键） | **否** | **是**（仅在发生超时时写入） |
| 第 4 步等待正在运行的监听器 | **否**（沿用 `RUTIS_SHUTDOWN_TIMEOUT`） | 沿用 §7.1 规则 |

### 3.3 超时状态机、污染规则与逐字输出

1. **预算未开启（默认）**：完全按原有代码路径执行，不创建定时器、不包裹额外任务、不写 `tainted`。
2. **预算开启时的叶子卸载栈（Node 叶子、Bun、Python）**：
   - 取走该行的卸载栈并逆序遍历，每一项标明 `withdrawal: true | false`。
   - 遇到 `withdrawal === true`：无条件执行，不受 `timedOut` 或剩余时间影响。
   - 遇到 `withdrawal === false`：
     - 若已 `timedOut`：直接跳过，不调用该清理。
     - 计算 `remaining = max(0, endAt - now)`。若 `remaining === 0`：置 `timedOut = true`，标记该行污染，打印诊断行一次，跳过该清理及后续所有用户清理。
     - 否则启动该清理并等待至多 `remaining`：
       - **Node 叶子 / Bun**：`Promise.race` 竞态清理 Promise 与 `setTimeout(remaining)`；清理先落定时立刻 `clearTimeout`；定时器先触发时置 `timedOut = true`，标记污染，打印诊断行一次，并对原 Promise 保留空拒绝处理器。
       - **Python**：将步骤包进 `asyncio.create_task` 后用 `asyncio.wait({task}, timeout=remaining)`（**禁止**用 `asyncio.wait_for`）；超时时调用 `task.cancel()` 并挂上 `_consume_task_result` 回调，置 `timedOut = true`，标记污染，打印诊断行一次。整段栈循环包在 `try / finally` 中，即使 `unload` 自身被外部取消，`finally` 仍把栈中尚未跑到的全部 `withdrawal` 跑完。
3. **Node 原生 Cordis fiber**：
   - `runner.mjs` 在 `unloadRow` 与 `dispose` 里检查 `LEAF_FIBERS.has(row.inner)`：为真时由 `leaf.mjs` 的 `drain()` 计时并通过 `onTimeout` 标记污染；为假时在 `runner.mjs` 里将 `row.fiber.dispose()` 与 `setTimeout(budgetMs)` 竞态，超时后清除定时器、吞掉后续拒绝、标记污染、打印原生 Cordis 诊断行一次，并照常走完 `finally` 与 `provideWaitingLater()`。
4. **路线 B 污染键与拒绝装载规则**：
   - 发生单行卸载超时时，运行时把该行的**行名**（`place.id` / `id`）与**行键**（`key`）同时记入本进程 `tainted` 表（值保存展示用行名与预算秒数）。
   - 此后本进程每次收到 `rows.load`（以及经由 `unload` + `load` 实现的 Bun / Python `rows.update`），在最开头检查 `tainted` 表：只要新请求的行名或行键命中，立刻抛出错误并拒绝装载，不修改 `rows` / `slots` / 服务表，也不导入或执行插件。
   - 未命中 `tainted` 的兄弟行照常加载、运行、接管已撤回的服务名、更新和卸载。
   - 重启运行时进程后，新进程的 `tainted` 表为空，之前被污染的行可干净装载。
5. **残余清理晚到动作隔离**：
   - Node `leaf.mjs` 的 `ctx.provide` 检查 `if (draining) throw new Error(...)`；Bun / Python 的 `provide`、`on`、`effect` 检查行活跃状态；三家 `emit` 等事件分发检查 `unloaded` / 行不在 `rows`；三家 `withdraw()` 句柄按登记对象身份比对。
6. **秒数格式化**：不带多余尾零，Node / Bun 用 `String(Number((budgetMs / 1000).toPrecision(12)))`，Python 用 `f"{budget:g}"`（例如 `5`、`0.25`、`0.05`）。
7. **逐字输出格式**（严格遵守[诊断设计](design-diagnostics-2026-10-26.md) §4.5，不出现内部词）：
   - **超时诊断行（经 `say(...)` 写入不带行前缀的 stderr）**：
     - 运行时内的叶子行（Node 叶子、Bun、Python）：
       ```text
       runtime <runtime>: cleanup of row <row> did not finish within <s> s; skipped remaining cleanups and withdrew its services; reloading row <row> in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)
       ```
     - Node 原生 Cordis 行：
       ```text
       runtime <runtime>: cleanup of row <row> did not finish within <s> s; stopped waiting and finished unloading the row; reloading row <row> in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)
       ```
     - Node 独立调用 `toCordis`（无 runner / 无行表）：
       ```text
       runtime <runtime>: cleanup of plugin <plugin> did not finish within <s> s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)
       ```
   - **同进程重载被污染行时的显式报错（Node / Bun / Python 一致）**：
     ```text
     row <row> cannot be loaded in runtime <runtime>: its previous cleanup did not finish within <s> s and may still be running; restart the runtime (or isolate this row in its own runtime) before loading it again
     ```

### 3.4 跨语言与形式化验证对照

| 运行时 | 单行 `rows.unload`（开启 `RUTIS_ROW_UNLOAD_TIMEOUT`） | 超时后同进程重载（路线 B） | `dispose` | 状态 |
| --- | --- | --- | --- | --- |
| Node（叶子） | `drain()` 按总预算限时用户清理；超时跳过后续清理，撤回照跑 | 显式报错拒绝直至运行时重启；兄弟行不受影响 | 逐行应用同一单行预算（断开后仍有 `RUTIS_SHUTDOWN_TIMEOUT` 兜底） | 本稿 PoC 已实现 + TLA+ 已验 |
| Node（原生 Cordis） | 竞态 `row.fiber.dispose()`；打印原生诊断并完成 `finally` 收尾 | 同上 | 同上 | 本稿 PoC 已实现 + TLA+ 已验 |
| Bun | `unload` 按总预算限时用户清理；超时跳过后续清理，撤回照跑 | 同上 | `dispose` 调 `unload`，自动生效 | 本稿 PoC 已实现 + TLA+ 已验 |
| Python | `unload` 用 `asyncio.wait` + `cancel()` 限时用户清理；跳过后续清理，撤回照跑 | 同上 | `dispose` 调 `unload`，自动生效 | 本稿 PoC 已实现 + TLA+ 已验 |
| Kotlin | `unloadRow` 接入 `RUTIS_ROW_UNLOAD_TIMEOUT`（复用逐条计时与 D8 撤回） | 同上 | 已有 `dispose(shutdownMillis)` | §六 后续步骤 |
| Go | `unload` 接入 `RUTIS_ROW_UNLOAD_TIMEOUT` 并保护剩余撤回（D8） | 同上 | 已有 `dispose(ctx)` | §六 后续步骤 |
| Swift | `unload` 接入 `RUTIS_ROW_UNLOAD_TIMEOUT` 并保护剩余撤回（D8） | 同上 | 已有 `dispose(within:)` | §六 后续步骤 |

**TLA+ 模型验证结果**（[`specs/row-unload-budget/`](../specs/row-unload-budget/README.md)，TLC 2.19）：
- `budget-route-b`（路线 B 叶子模式）：**通过**（1,185 个不同状态，1 s），全部 5 条不变式（`TypeOK`、`NoOrphanWithdrawal`、`SerialCleanupsWithinGen`、`NoCrossGenCorruption`、`NoConcurrentSameRowInSharedProcess`）成立；
- `budget-route-b-cordis`（路线 B Node 原生 Cordis 模式）：**通过**（702 个不同状态，<1 s），全部 5 条不变式成立；
- `budget-route-a`（路线 A 允许同进程重载）：**违反 `NoConcurrentSameRowInSharedProcess`**（198 个不同状态找到反例，1 s）；
- 3 个变异配置（`budget-mut-skip-withdraw`、`budget-mut-continue-serial`、`budget-mut-corrupt-new-gen`）分别捕获 `NoOrphanWithdrawal`、`SerialCleanupsWithinGen`、`NoCrossGenCorruption`，全部与 `expected.txt` 一致。

## 四、开发者看到什么

### 4.1 插件作者

#### 正常时

以 TypeScript（Node 叶子 / Bun）为例：插件提供服务并登记两条清理，开启 `RUTIS_ROW_UNLOAD_TIMEOUT=5`，清理在预算内正常结束：

```ts
import { definePlugin } from '@arcships/rutis'

export default definePlugin({
  name: 'weather',
  apply(ctx) {
    ctx.effect(async () => {
      await closeCache()
    })
    ctx.provide('forecast', { today: () => 'sunny' })
    ctx.effect(async () => {
      await flushMetrics(ctx.use('forecast'))
    })
  },
})
```

卸载时的输出：**没有任何额外输出**。两条清理按逆序逐条执行，`forecast` 在其登记位置被撤回，行不被污染，可以照常重载。Python（`define_plugin` + `async def` 清理）与原生 Cordis 插件（`export function apply(ctx)`）差异仅在插件 API 写法，正常结束时同样没有任何额外输出。

#### 出错 / 超时时

| 触发场景 | 逐字输出（stderr 或错误消息） | 宿主错误码 / 记在哪一行 | 怎样改 |
| --- | --- | --- | --- |
| Node 叶子行 `weather` 的一条清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回 | `runtime node: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` 正常回复（无错误码），行退出 `Unloading`、服务已撤回；诊断记在运行时 stderr 的 `row weather` 上 | 检查该清理里永不落定的 `await`（或两个 `ctx.effect` 互相等），监听 `ctx.signal` 或给清理内部 I/O 加超时；修复后重启运行时再装载 |
| Node 独立调用 `toCordis`（无行名，插件名 `weather`）的清理超时（50 ms） | `runtime node: cleanup of plugin weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | 无宿主；记在 stderr 的 `plugin weather` 上 | 同上 |
| Bun 行 `weather` 的清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回 | `runtime bun: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` 正常回复（无错误码）；记在运行时 stderr 的 `row weather` 上 | 同上 |
| Python 行 `weather` 的清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回（含吞掉 `CancelledError` 的协程） | `runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` 正常回复（无错误码）；记在运行时 stderr 的 `row weather` 上；运行时已向该任务发过 `cancel()` | 不要在清理里捕获 `asyncio.CancelledError` 后无限等待，用 `asyncio.wait_for` 限时关闭；修复后重启运行时再装载 |
| Node 原生 Cordis 插件行 `weather` 的清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回 | `runtime node: cleanup of row weather did not finish within 5 s; stopped waiting and finished unloading the row; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)` | `rows.unload` 正常回复（无错误码）；记在运行时 stderr 的 `row weather` 上；服务、监听器、导出器均已撤回 | 修复原生插件中挂住的 `ctx.effect` 处置函数；修复后重启运行时再装载 |
| 在同一运行时进程里尝试再次装载已超时的行 `weather`（无论沿用旧 key 还是新 key，Node / Bun / Python 一致） | `row weather cannot be loaded in runtime py: its previous cleanup did not finish within 5 s and may still be running; restart the runtime (or isolate this row in its own runtime) before loading it again`（Node 写 `runtime node`，Bun 写 `runtime bun`） | `rows.load` 失败（普通 `Error` / `RuntimeError`，记在 `row weather` 的 `apply` / 加载失败上）；同进程健康兄弟行不受影响 | 修复插件的清理代码后，重启该运行时进程（或把该行拆到独立运行时）再加载 |
| 第一条清理先抛异常（`flush failed`），第二条清理随后超时 | 同时输出上面对应的超时诊断行，Bun / Python 仍抛出第一条清理的异常（Node 叶子仍由 `context.logger.error` 记录），并且该行同样被标记为污染 | 宿主收到第一条清理的错误（`site.doing = "cleanup"`，记在 `row weather`）；超时不掩盖原错误 | 同时修复第一条清理的报错与第二条清理的卡住，然后重启运行时 |
| 清理里有同步死循环（`while (true) {}` 或 `time.sleep(60)`） | 异步卸载定时器无法抢占单线程事件循环，不会立刻打出超时行；5 秒后触发既有的阻塞提示，宿主停机时触发 `RUTIS_SHUTDOWN_TIMEOUT` 看门狗强杀 | 记在阻塞提示 / 运行时退出上 | 把同步阻塞清理改成异步 `await`（Python 可用 `asyncio.to_thread`） |

### 4.2 用 `rutis-host` 组装插件的人

#### 正常时

- 不设置 `RUTIS_ROW_UNLOAD_TIMEOUT`：行为与 0.23.0 完全相同。
- 在开发或运行环境显式开启：

```bash
RUTIS_ROW_UNLOAD_TIMEOUT=5 rutis-host dev
```

清理在 5 秒内完成时，终端输出与未设置时完全一致。

#### 出错 / 超时时

修改插件触发热重载或从 `rutis.json` 移除行 `weather`，而旧版 `weather` 的清理卡住 5 秒后：

1. 终端 stderr 打印一行不带 `[weather]` 前缀的诊断：
   ```text
   runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services; reloading row weather in this runtime is blocked until the runtime restarts (the stuck cleanup may still be running; fix it or restart the runtime)
   ```
2. `weather` 立刻退出 `Unloading`，同进程的其他行继续服务；
3. 如果紧接着在同一个运行时进程里尝试重新加载 `weather`（例如 `dev` 立刻尝试重载），加载会显式失败并打印：
   ```text
   row weather cannot be loaded in runtime py: its previous cleanup did not finish within 5 s and may still be running; restart the runtime (or isolate this row in its own runtime) before loading it again
   ```
   不会把新版 `weather` 悄悄叠到仍在运行的旧清理上。修复清理并重启 `rutis-host`（或对应的运行时进程）即可恢复。

### 4.3 宿主持有者

#### 正常时

Rust 宿主在启动 Node / Bun / Python 运行时进程时，把 `RUTIS_ROW_UNLOAD_TIMEOUT` 传入子进程环境变量；测试或直接嵌入运行时的 JS / TS / Python 代码也可通过构造/工厂选项传入：

```ts
// Bun 嵌入 / 测试
const runtime = new Runtime(projectDir, { rowUnloadTimeout: 5_000 })
```

```js
// Node 工厂 / 测试
await run({ argv, env: { ...env, RUTIS_ROW_UNLOAD_TIMEOUT: '5' }, connect, exit })
// 或显式毫秒选项：
await run({ argv, env, rowUnloadTimeout: 5_000, connect, exit })
```

```python
# Python 嵌入 / 测试
runtime = Runtime(row_unload_timeout=5.0)
```

#### 出错 / 超时时

- `process.unload_row(&key)` 在预算到期后立即返回 `Ok(())`，协调循环与无故障兄弟行不被锁死；
- 若宿主在未重启该运行时的前提下再次为该行调用 `rows.load`，运行时显式返回上述可读错误（无内部词），宿主可清晰将其记录为该行失败并提示重启运行时，而不是静默制造同进程双代并发；
- 第 2 阶段宿主整合将把单行卸载超时状态做成宿主显式可观察（可选策略：默认保持污染报错，或按配置自动重启该运行时进程）。

### 4.4 部署的人

#### 正常时

在容器、systemd 或远程运行时配置里，与 `RUTIS_SHUTDOWN_TIMEOUT` 并列配置：

```bash
RUTIS_SHUTDOWN_TIMEOUT=15
RUTIS_ROW_UNLOAD_TIMEOUT=5
```

建议单行预算小于停机预算（例如 5 秒 vs 15 秒），这样停机逐行卸载时单行卡死仍能给其余行留出清理时间。要临时关闭已继承的预算，设为 `RUTIS_ROW_UNLOAD_TIMEOUT=0` 或取消该变量。

#### 出错 / 超时时

日志监控可按固定片段 `cleanup of row ` + ` did not finish within ` 以及 `cannot be loaded in runtime ` 匹配告警。看到某行进入污染状态后，其余健康行仍可继续对外提供服务；待维护窗口或按运维策略重启该运行时进程（或将该插件拆到独立运行时）即可彻底回收遗留后台任务并重新装载该行。

### 4.5 要懂的概念（对照 #663 概念预算）

对照 [#663](https://github.com/arcships/rutis/issues/663) 的各层概念预算（插件作者 7 个 / 调试时约 9 个、`rutis-host` 用户 6 个、宿主持有者 10 个，每层调试/运维允许最多 +3 个）：

- **插件作者**：**0 个新概念**。作者已经知道“清理”和“卸载栈”（[卸载顺序](design-unload-order-2026-11-01.md)）；单行预算默认关闭，只有在清理卡住且开启预算时才看到两条自解释的输出（超时诊断行 + 拒绝同进程重载错误），不需要学任何新术语或内部状态名。
- **`rutis-host` 用户 / 宿主持有者 / 部署的人**：**+1 个可选环境变量**（`RUTIS_ROW_UNLOAD_TIMEOUT`），与已有的 `RUTIS_SHUTDOWN_TIMEOUT` 同单位、同解析规则，归入调试/运维格内，不新增概念层，未超预算。

## 五、兼容与迁移

- **默认完全兼容**：未设置 `RUTIS_ROW_UNLOAD_TIMEOUT` 时，Node（叶子与原生 Cordis）、Bun、Python 的行为、输出与性能均与本稿之前 100% 一致；不涉及破坏性变更，无需在迁移指南里写不兼容条目。
- **协议不变（本阶段）**：不改消息帧格式、不升协议版本、不新增控制方法或 `rutis/*` 事件。
  - **新运行时遇到旧宿主**：正常工作。旧宿主若向环境透传 `RUTIS_ROW_UNLOAD_TIMEOUT`，新运行时本地执行预算、撤回服务并在同进程拒绝静默原位重载。
  - **旧运行时遇到新宿主**：正常工作。旧运行时忽略 `RUTIS_ROW_UNLOAD_TIMEOUT`，维持不限时等待。
- **显式开启后的语义变化**：开启 `RUTIS_ROW_UNLOAD_TIMEOUT` 后，卡住的用户清理不再让 `rows.unload` 无限挂起，而是到点放弃等待、跳过后续用户清理、跑完剩余撤回、打印一条 stderr 诊断行，并在本运行时进程内阻止该行静默原位重载直至运行时重启。

## 六、实施

| 阶段 / 拆分 | 范围与改动 | 完成条件 |
| --- | --- | --- |
| 1. 本稿、路线 B PoC 与 TLA+ 模型（[#1323](https://github.com/arcships/rutis/issues/1323)，当前分支） | 双语设计稿 `docs/design-row-unload-budget-2026-11-20.{md,en.md}`；TLA+ 模型 `specs/row-unload-budget/`（注册到 `specs/README.md` 与 `.github/workflows/ci.yml`）；Node（`leaf.mjs` / `runner.mjs`）、Bun（`runner.ts`）、Python（`runner.py`）路线 B PoC 与单测 | TLC 全部 6 个配置与 `expected.txt` 一致（路线 B 叶子/Cordis 全通过，路线 A 与 3 个变异捕获预期反例）；三家单测证明默认不变、超时逐字输出、跳过后续清理、撤回服务、拒绝同进程重载、健康兄弟行不受影响、残余动作隔离、重启运行时后恢复；全部链接/约束/`check-model-pr` 检查通过 |
| 2. 宿主可观察超时策略与指南整合（后续 issue） | 宿主侧显式观察单行卸载超时结果并提供可配置策略（默认 `taint` 保持拒绝重载并在 `rutis-host status` 展示；可选 `restart-runtime` 自动重启受污染运行时）；更新 `docs/guide/rutis-host.{md,en.md}`、`docs/guide/cordis-boundaries.{md,en.md}` §7–§8、`docs/design-events-protocol-2026-10-18.{md,en.md}` §7.1；可选 `--row-unload-timeout` 参数；`crates/rutis-loader` 端到端测试；晋升稳定后登记 `docs/guarantees.md` | 端到端测试锁定三家运行时的逐字诊断、污染拒绝与运行时重启恢复，指南双语同步 |
| 3. Go / Swift / Kotlin 对齐（后续 issue） | Go（`go/rutis/internal/runtime/runtime.go`）、Swift（`swift/rutis/Sources/Rutis/API/Runtime.swift`）、Kotlin（`kotlin/rutis/.../internal/Runtime.kt`）的 `rows.unload` 统一支持 `RUTIS_ROW_UNLOAD_TIMEOUT`、遵守 D8 撤回不变量与路线 B 污染语义，并采用相同诊断与报错措辞 | 六种语言在单行卸载预算上的环境变量、D8 撤回不变量、路线 B 污染语义与逐字输出完全一致 |
