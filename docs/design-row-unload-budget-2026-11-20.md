# Node / Bun / Python 单行卸载预算与诊断

[English](design-row-unload-budget-2026-11-20.en.md)

状态：草稿，PoC 已实现（分支 `design/1323-unload-budget`，未合并）。日期：2026-11-20。基准：`main` `5c2e143e`（0.23.0 之后）。
Issue：[#1323](https://github.com/arcships/rutis/issues/1323)。相关：[卸载顺序](design-unload-order-2026-11-01.md)（D7、D8、§三、§六）、[Cordis 兼容边界](guide/cordis-boundaries.md)（§7、§8）、[诊断设计](design-diagnostics-2026-10-26.md)（§4.5、§六）、[事件协议](design-events-protocol-2026-10-18.md)（§7.1）。

## 一、现状与要解决什么

当宿主对 Node、Bun 或 Python 运行时中的某一行发起单行卸载（`rows.unload` —— 由配置移除、重载、配置变更、依赖下线或加载失败回滚触发）时，运行时会一直等待该行的用户清理完成，**没有任何单行时间预算**：

- Node 叶子插件（`node/rutis-runtime/src/leaf.mjs` 的 `drain`）逐条 `await` 卸载栈；原生 Cordis 插件（`node/rutis-runtime/src/runner.mjs` 的 `unloadRow`）直接 `await row.fiber.dispose()`。
- Bun（`bun/rutis-bun/src/runner.ts` 的 `unload`）逐条 `await this.within(key, step)`。
- Python（`python/rutis/rutis/runner.py` 的 `unload`）逐条 `await _settle(step())`。

只要其中任意一条用户清理永不返回（例如等待永不落定的 Promise / 协程、未关闭的连接池、或者在[卸载顺序](design-unload-order-2026-11-01.md)改为逆序串行后两个清理互相等对方）：

1. 运行时的 `rows.unload` 永远不会回复，宿主侧该行永久卡在 `Unloading` 状态，无法完成卸载或重载；
2. 该行在进程内仍占着尚未跑到的框架撤回（例如 `provide` 登记的撤回），同一行名或服务名无法恢复；
3. `--shutdown-timeout` / `RUTIS_SHUTDOWN_TIMEOUT`（默认 10 秒）只在**整个宿主停止或会话断开**时才生效，并且 Node / Bun / Python 到点后是直接退出整个运行时进程（退出码 1），而不是只解除卡住的那一行；
4. 运行时本身在单行卸载卡住时没有任何诊断输出（[卸载顺序](design-unload-order-2026-11-01.md) §三 与 §六 记下了这一缺口，[Cordis 兼容边界](guide/cordis-boundaries.md) §8 也明确区分了调用超时与宿主关闭截止时间）。

六种语言当前的卸载预算现状：

| 运行时 | 单行 `rows.unload` 清理预算（本稿前） | 会话结束 / `dispose` 清理预算（本稿前） |
| --- | --- | --- |
| Node（叶子插件） | 无：一直等到栈跑完 | 无逐行预算；会话断开后 `RUTIS_SHUTDOWN_TIMEOUT` 到点整进程退出（退出码 1） |
| Node（原生 Cordis） | 无：一直等到 `row.fiber.dispose()` 落定 | 同上 |
| Bun | 无：一直等到栈跑完 | 同上 |
| Python | 无：一直等到栈跑完 | 同上 |
| Kotlin | 无：一直等到栈跑完 | `dispose(shutdownMillis)` 逐条按剩余时间跑，撤回不受限（D8） |
| Go | 无：一直等到栈跑完 | `dispose(ctx)` 受 `RUTIS_SHUTDOWN_TIMEOUT` 约束，到点放弃当前行及后续行 |
| Swift | 无：一直等到栈跑完 | `dispose(within:)` 受 `RUTIS_SHUTDOWN_TIMEOUT` 约束，到点放弃当前行及后续行 |

本稿先解决 Node、Bun、Python 三家的单行卸载预算与诊断（Go、Swift、Kotlin 的单行预算同步纳入 §六 后续步骤，共用同一环境变量与语义）。

## 二、决策

| # | 决策 | 为什么；放弃的做法 |
| --- | --- | --- |
| D1 | **显式开启（opt-in），默认不限时**：新增环境变量 `RUTIS_ROW_UNLOAD_TIMEOUT`（单位：**秒**，正有限浮点数，解析规则与 `RUTIS_SHUTDOWN_TIMEOUT` 完全一致）；未设置、空串、`<= 0`、`NaN`、`Infinity` 或无法解析时均表示**关闭（不限时）**。同时为测试和嵌入方提供显式运行时选项：Node `run({ rowUnloadTimeout })`（毫秒）与 `toCordis(..., unloadOptions)`、Bun `new Runtime(project, { rowUnloadTimeout })`（毫秒）、Python `Runtime(row_unload_timeout=...)`（秒） | 默认关闭保证不改变原生 Cordis 与现有 rutis 行为（合法的慢速刷盘 / 模型释放不会被悄悄截断）；显式开启让开发环境热重载、运维和自动化测试可以按需设限，并支持用 `RUTIS_ROW_UNLOAD_TIMEOUT=0` 覆盖继承来的环境变量。放弃“默认开启 10 秒单行预算”：会破坏依赖慢速清理的既有插件；放弃“直接复用 `RUTIS_SHUTDOWN_TIMEOUT`”：宿主停机预算（往往 30 秒以上）与单行热重载预算（往往几秒）是两件事，混用会让两者互相牵制 |
| D2 | **预算只约束用户清理，永不约束框架撤回**（延续[卸载顺序](design-unload-order-2026-11-01.md) D8）：单行预算是一次卸载中该行**全部用户清理步骤**（`ctx.effect` 与 `apply` 返回的清理）共享的总预算。框架自身的撤回与收尾——`provide` 登记的服务撤回、监听器撤回（`bus.withdraw`）、导出器处置（`exporter.dispose`）、句柄失效与槽位清理（`gone` / `slots.delete`）、行表移除与 `provideWaitingLater`——**永远不受此预算限制，必须跑完**。卸载第 4 步（等待正在运行的监听器）仍沿用[事件协议](design-events-protocol-2026-10-18.md) §7.1 的 `RUTIS_SHUTDOWN_TIMEOUT`；单行清理预算从卸载栈开始排干时起算 | 框架撤回是内存表更新与撤回通知，不会挂住；跳过它们会把陈旧服务永久留在进程表里，导致同行重载或别的行提供同名服务失败，同时别的行也读不到它。放弃“每条清理各给一份预算”：10 条卡住的清理会把等待时间放大 10 倍；放弃“把整个 `rows.unload` 含撤回一起限时”：违背 D8，会在超时时泄漏服务注册 |
| D3 | **超时后停止等待当前清理、跳过余下用户清理、但必须跑完余下全部框架撤回**：当某条用户清理耗尽剩余预算（或下一条用户清理开始前剩余预算已为 0）时：(1) 停止等待该清理（Python 经 `asyncio.wait` + `task.cancel()` 发出取消，不使用会在协程吞掉 `CancelledError` 时卡死的 `asyncio.wait_for`；JS / Python 都挂上完成回调以防延迟抛错触发未处理拒绝）；(2) **不再执行**栈上余下的任何用户清理；(3) **照常执行**栈上余下的每一条框架撤回，并走完 `finally` 收尾 | 在卡住的清理仍在后台执行的条件下，不可能同时宣称“解除卡死”又宣称“保持严格逆序串行”：继续执行更早登记的用户清理（例如关闭数据库连接池）会与尚未退出的后登记清理（例如正在往连接池刷数据）发生竞态并破坏 LIFO 契约。框架撤回则相反，必须执行以保持服务表与监听器表一致。放弃“超时后继续跑后面的用户清理”：破坏 LIFO 保证并制造并发竞态；放弃“超时后整栈放弃”：泄漏服务注册 |
| D4 | **正常回复 `rows.unload` 并向 stderr 打印一条逐字诊断行（不含 rutis 内部词）**：跑完余下撤回与 `finally` 后，`rows.unload` 正常返回 `null` / `None`（若超时前已有清理抛出异常，Bun / Python 照旧抛出该异常，Node 叶子照 Cordis 记入 `context.logger.error`；超时本身不抛协议错误）。运行时通过 `say(...)` 向不带行前缀的 stderr 输出一条逐字诊断（见 §三 与 §四），严格遵守[诊断设计](design-diagnostics-2026-10-26.md) §4.5，不出现 `rutis`、`Cordis`、`fiber`、`Promise.all`、`rows.unload` 等内部词 | 与第 4 步监听器超时（打印诊断行后继续卸载）以及 Kotlin `unloadRow` 保持一致；Node 叶子跑在 Cordis `_unload` 里本身不向外抛处置异常，统一正常返回使三家行为一致，也让 `rows.update` 与宿主重调谐能够立刻推进而不是被卡死。放弃“让 `rows.unload` 以超时错误失败”：会导致 Bun / Python 的 `rows.update` 在 `load` 前直接中止，且与 Node 的 Cordis 处置语义分裂；放弃“新增 `rutis/*` 协议事件”：不增加协议面与概念成本，留待将来需要宿主自动重启策略时再议 |
| D5 | **如实说明语言边界与重载建议**：JavaScript（Node / Bun）和 Python 均无法强杀任意已卡住的 Promise、拒绝响应 `CancelledError` 的协程或占住事件循环线程的同步阻塞代码。因此超时之后：(1) 卡住的清理可能仍在后台运行；(2) 由于框架表已彻底清理（服务、监听器、槽位、行表均已移除且旧 `ctx` 写入被拒绝），**再次重载同一行在框架表层面可以成功**；(3) 但若卡住的清理仍占着端口、文件锁或共享内存等外部资源，同进程重载可能与之冲突，因此诊断行明确提示 `the stuck cleanup may still be running; fix it or restart the runtime`，生产环境对不可信或易卡住的插件应重启运行时进程或将其隔离到独立运行时进程 | 诚实面对 JS / Python 运行时能力，不做虚假的“强杀清理”承诺；同步死循环占住唯一事件循环线程时异步定时器无法触发，继续由现有的 5 秒阻塞提示（[诊断设计](design-diagnostics-2026-10-26.md) §6.4）和关闭看门狗（`RUTIS_SHUTDOWN_TIMEOUT`）兜底。放弃“单行超时直接 `SIGKILL` 整个运行时进程”：共享运行时里一行的清理超时不该拖垮同进程里所有健康的行 |
| D6 | **Node 原生 Cordis 插件单独处理，不给 `_unload` 打补丁**：当开启单行预算时，`runner.mjs` 通过 `LEAF_FIBERS.has(row.inner)` 区分叶子插件与原生 Cordis 插件。叶子插件由 `leaf.mjs` 的 `drain()` 自己计时并保证跑完余下撤回（`runner.mjs` 不再并行挂一个同毫秒数的外层定时器，避免外层定时器抢在 `drain()` 之前触发导致撤回未跑）；原生 Cordis 插件由 `runner.mjs` 在 `unloadRow` 与 `dispose` 里将 `row.fiber.dispose()` 与预算竞态，超时后停止等待、吞掉后续延迟拒绝、打印原生插件专用的逐字诊断行，并照常走完 `finally`（`row.unloaded = true`、`bus.withdraw(key)`、`bus.forgetRow(key)`）与 `provideWaitingLater()`。Cordis 4.0.4 内部的 `Promise.all` 无法打断，但其 `dispose()` 在进入 `_unload()` 前已把 fiber 移出 `ctx.registry`，`ctx.provide` 的 disposer 也在第一个微任务就从 `ctx.reflect.store` 删掉了服务，因此服务、导出器、监听器与 `waitingHosts` 仍能干净收尾 | 遵守[Cordis 兼容边界](guide/cordis-boundaries.md) §7“运行时不给 `_unload` 打补丁”的约束，同时避免叶子插件在 Node 里出现内外两层定时器竞态或重复打印诊断。原生 Cordis 的 `rows.update` 调用的是 `fiber.update`（把卸载与重新 `apply` 合并在同一次 `fiber.await()` 里），竞态 `fiber.await()` 会误伤慢速 `apply`，故原生 Cordis 的 `rows.update` 保持原样，仅 `rows.unload` 与 `dispose` 受预算约束（Node 叶子、Bun、Python 的 `rows.update` 均走栈排干，受预算约束） |
| D7 | **Go / Swift / Kotlin 纳入跨语言规划，不塞进本 PoC**：本稿及 PoC 先落 Node / Bun / Python 三家；Go、Swift、Kotlin 当前单行 `rows.unload` 同样无预算，留在 §六 作为后续步骤，统一使用 `RUTIS_ROW_UNLOAD_TIMEOUT` 与同一套诊断语义 | 控制 PoC 范围，优先补齐完全没有任何清理预算的三家运行时，同时确保接口命名与六家长期收敛 |

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
| 第 4 步等待正在运行的监听器 | **否**（沿用 `RUTIS_SHUTDOWN_TIMEOUT`） | 沿用 §7.1 规则 |

### 3.3 超时状态机与各语言行为

1. **预算未开启（默认）**：完全按原有代码路径执行，不创建定时器、不包裹额外任务、不改变任何行为。
2. **预算开启时的叶子卸载栈（Node 叶子、Bun、Python）**：
   - 取走该行的卸载栈并逆序遍历，每一项标明 `withdrawal: true | false`。
   - 遇到 `withdrawal === true`：无条件执行，不受 `timedOut` 或剩余时间影响。
   - 遇到 `withdrawal === false`：
     - 若已 `timedOut`：直接跳过，不调用该清理。
     - 计算 `remaining = max(0, endAt - now)`。若 `remaining === 0`（前面虽完成但已耗尽总预算）：置 `timedOut = true`，打印诊断行一次，跳过该清理及后续所有用户清理。
     - 否则启动该清理并等待至多 `remaining`：
       - **Node 叶子 / Bun**：`Promise.race` 竞态清理 Promise 与 `setTimeout(remaining)`；清理先落定时立刻 `clearTimeout`；定时器先触发时置 `timedOut = true`，打印诊断行一次，并对原 Promise 保留空拒绝处理器以防延迟拒绝导致未处理异常。
       - **Python**：将步骤包进 `asyncio.create_task` 后用 `asyncio.wait({task}, timeout=remaining)`（**禁止**用 `asyncio.wait_for`，因其在协程捕获 `CancelledError` 时会无限阻塞）；超时时调用 `task.cancel()` 并挂上结果回收回调（吞掉延迟结束时的 `CancelledError` / 异常，避免 `Task exception was never retrieved`），置 `timedOut = true`，打印诊断行一次。此外，整段栈循环包在 `try / finally` 中：即使 `unload` 自身被外部取消，`finally` 仍把栈中尚未跑到的全部 `withdrawal` 跑完再从 `rows` 移除并撤回监听器。
3. **Node 原生 Cordis fiber**：
   - `runner.mjs` 在 `unloadRow` 与 `dispose` 里检查 `LEAF_FIBERS.has(row.inner)`：为真时交给 `leaf.mjs` 的 `drain()`（不另起外层定时器）；为假时在 `runner.mjs` 里将 `row.fiber.dispose()` 与 `setTimeout(budgetMs)` 竞态。
   - 超时后清除定时器、吞掉 `row.fiber.dispose()` 后续可能的拒绝、打印原生 Cordis 诊断行一次，照常执行 `finally`（`row.unloaded = true`、`bus.withdraw(key)`、`bus.forgetRow(key)`）以及 `provideWaitingLater()`。
4. **超时前已发生的异常**：若某条用户清理在预算用完之前正常抛出异常，Bun / Python 照旧记录并在卸载结束时抛出首个异常，Node 叶子照旧交给 `context.logger.error`；超时本身不追加异常。
5. **秒数格式化**：不带多余尾零，Node / Bun 用 `String(Number((budgetMs / 1000).toPrecision(12)))`，Python 用 `f"{budget:g}"`（例如 `5`、`0.25`、`0.05`）。
6. **逐字诊断行格式**（经 `say(...)` 写入 stderr，不加 `[<row>] ` 前缀）：
   - 叶子插件（Node 叶子、Bun、Python）：
     ```text
     runtime <runtime>: cleanup of row <row> did not finish within <s> s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)
     ```
     Node 独立调用 `toCordis`（无 runner / 无行名）时，将 `row <row>` 写作 `plugin <plugin>`（未命名时为 `plugin leaf`）。
   - Node 原生 Cordis 插件：
     ```text
     runtime <runtime>: cleanup of row <row> did not finish within <s> s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it or restart the runtime)
     ```
7. **同步死循环 / 阻塞调用**：JS 与 Python 的事件循环是单线程的，同步阻塞清理占住线程期间异步定时器无法触发；它退出后若总预算已耗尽，才会跳过其后的用户清理。长时间同步阻塞由 5 秒阻塞检测提示与 `RUTIS_SHUTDOWN_TIMEOUT` 看门狗处理。

### 3.4 跨语言对照表（实施完成目标）

| 运行时 | 单行 `rows.unload`（开启 `RUTIS_ROW_UNLOAD_TIMEOUT`） | 单行 `rows.unload`（默认未设置） | `dispose` | 状态 |
| --- | --- | --- | --- | --- |
| Node（叶子） | `drain()` 按总预算限时用户清理；超时跳过后续清理，撤回照跑 | 不限时 | 逐行应用同一单行预算（断开后仍有 `RUTIS_SHUTDOWN_TIMEOUT` 兜底） | 本稿 PoC 已实现 |
| Node（原生 Cordis） | 竞态 `row.fiber.dispose()`；超时打印原生诊断并完成 `finally` 收尾（`Promise.all` 无法打断） | 不限时 | 同上 | 本稿 PoC 已实现 |
| Bun | `unload` 按总预算限时用户清理；超时跳过后续清理，撤回照跑 | 不限时 | `dispose` 调 `unload`，自动生效 | 本稿 PoC 已实现 |
| Python | `unload` 用 `asyncio.wait` + `cancel()` 限时用户清理；超时跳过后续清理，撤回照跑 | 不限时 | `dispose` 调 `unload`，自动生效 | 本稿 PoC 已实现 |
| Kotlin | `unloadRow` 接入 `RUTIS_ROW_UNLOAD_TIMEOUT`（复用现有逐条计时与 D8 `NonCancellable` 撤回） | 不限时 | 已有 `dispose(shutdownMillis)` | §六 后续步骤 |
| Go | `unload` 接入 `RUTIS_ROW_UNLOAD_TIMEOUT` 并保护剩余撤回（D8） | 不限时 | 已有 `dispose(ctx)` | §六 后续步骤 |
| Swift | `unload` 接入 `RUTIS_ROW_UNLOAD_TIMEOUT` 并保护剩余撤回（D8） | 不限时 | 已有 `dispose(within:)` | §六 后续步骤 |

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

卸载时的输出：**没有任何额外输出**。两条清理按逆序逐条执行，`forecast` 在其登记位置被撤回，与未设置预算完全一致。Python（`define_plugin` + `async def` 清理）与原生 Cordis 插件（`export function apply(ctx)`）差异仅在插件 API 写法，正常结束时同样没有任何额外输出。

#### 出错 / 超时时

| 触发场景 | 逐字输出（stderr） | 宿主错误码 / 记在哪一行 | 怎样改 |
| --- | --- | --- | --- |
| Node 叶子行 `weather` 的一条清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回 | `runtime node: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | 宿主收到正常回复（无错误码）；记在运行时 stderr 的 `row weather` 上，行退出 `Unloading`，服务已撤回，后续用户清理未执行 | 检查该清理里永不落定的 `await`（或两个 `ctx.effect` 互相等），监听 `ctx.signal` 或给清理内部的 I/O 加超时；若旧清理仍占外部资源，重启运行时 |
| Node 独立调用 `toCordis`（无行名，插件名 `weather`）的清理超时（50 ms） | `runtime node: cleanup of plugin weather did not finish within 0.05 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | 无宿主；记在 stderr 的 `plugin weather` 上 | 同上 |
| Bun 行 `weather` 的清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回 | `runtime bun: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | 宿主收到正常回复（无错误码）；记在运行时 stderr 的 `row weather` 上 | 同上 |
| Python 行 `weather` 的清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回（含捕获 `CancelledError` 拒绝退出的协程） | `runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)` | 宿主收到正常回复（无错误码）；记在运行时 stderr 的 `row weather` 上；运行时已向该任务发过 `cancel()` | 不要在清理里吞掉 `asyncio.CancelledError` 后无限等待，给异步关闭加 `asyncio.wait_for`；若占外部资源，重启运行时 |
| Node 原生 Cordis 插件行 `weather` 的清理在 `RUTIS_ROW_UNLOAD_TIMEOUT=5` 内未返回 | `runtime node: cleanup of row weather did not finish within 5 s; stopped waiting and finished unloading the row (the stuck cleanup may still be running; fix it or restart the runtime)` | 宿主收到正常回复（无错误码）；记在运行时 stderr 的 `row weather` 上；服务、监听器、导出器均已撤回，但未结束的处置仍在后台 | 修复原生插件中挂住的 `ctx.effect` 处置函数；不要依赖同一插件里多条 `ctx.effect` 的并行互相等待；若占外部资源，重启运行时 |
| 第一条清理先抛异常（如 `Error: flush failed`），第二条清理随后超时 | 既输出上面对应的超时诊断行，Bun / Python 仍抛出第一条清理的异常（Node 叶子仍由 `context.logger.error` 记录） | 宿主收到第一条清理的错误（`site.doing = "cleanup"`，记在 `row weather`）；超时本身不覆盖原错误 | 同时修复第一条清理的报错与第二条清理的卡住 |
| 清理里有同步死循环（如 `while (true) {}` 或 `time.sleep(60)`） | 异步卸载定时器无法抢占单线程事件循环，不会立刻打出超时行；5 秒后触发既有的阻塞提示，宿主停机时触发 `RUTIS_SHUTDOWN_TIMEOUT` 看门狗强杀 | 记在阻塞提示 / 运行时退出上 | 把同步阻塞清理改成异步 `await`（Python 可用 `asyncio.to_thread`） |

**超时后重载同一行看到什么**：框架表（服务、监听器、槽位、行表）已清理干净，因此再次加载 `weather` 不会报 `forecast is already provided by weather`；旧上下文再调 `ctx.provide` / `ctx.emit` / `ctx.effect` 仍按既有规则被拒绝。但如果卡住的旧清理还占着端口或文件锁，新加载的插件启动时会碰到操作系统级的占用错误（如 `EADDRINUSE`），此时需要修复清理并重启该运行时进程。

### 4.2 用 `rutis-host` 组装插件的人

#### 正常时

- 不设置 `RUTIS_ROW_UNLOAD_TIMEOUT`：行为与 0.23.0 完全相同。
- 在开发或运行环境显式开启：

```bash
RUTIS_ROW_UNLOAD_TIMEOUT=5 rutis-host dev
```

清理在 5 秒内完成时，终端输出与未设置时完全一致。

#### 出错 / 超时时

修改插件触发热重载或从 `rutis.json` 移除行 `weather`，而旧版 `weather` 的清理卡住 5 秒后，终端 stderr 打印一行（**不带** `[weather]` 前缀，因为这是运行时自己的诊断）：

```text
runtime py: cleanup of row weather did not finish within 5 s; skipped remaining cleanups and withdrew its services (the stuck cleanup may still be running; fix it or restart the runtime)
```

随后 `weather` 立即退出 `Unloading`，`rutis-host status` 不再显示它卡在卸载中，热重载继续加载新代码。若后续出现端口/文件占用或内存增长，重启 `rutis-host`（或对应的远程运行时）即可彻底回收遗留的后台任务。

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

- `process.unload_row(&key)` 在预算到期后立即返回 `Ok(())`（无协议错误码），不会把协调循环或 `reconcile` 永久锁死在 `Unloading`；
- 宿主可通过运行时 stderr 日志收集到逐字诊断行（不含平台内部词，可直接展示或告警）；
- 宿主侧随后释放的租约、服务投影和重新加载的同名行不会与旧注册冲突；如果宿主需要强隔离（不允许任何孤立后台任务留在共享进程里），应按运行时粒度触发重启或为高风险插件分配独立运行时进程。

### 4.4 部署的人

#### 正常时

在容器、systemd 或远程运行时配置里，与 `RUTIS_SHUTDOWN_TIMEOUT` 并列配置：

```bash
RUTIS_SHUTDOWN_TIMEOUT=15
RUTIS_ROW_UNLOAD_TIMEOUT=5
```

建议单行预算小于停机预算（例如 5 秒 vs 15 秒），这样停机逐行卸载时单行卡死仍能给其余行留出清理时间。要临时关闭已继承的预算，设为 `RUTIS_ROW_UNLOAD_TIMEOUT=0` 或取消该变量。

#### 出错 / 超时时

日志监控可按固定片段 `cleanup of row ` + ` did not finish within ` 匹配告警。看到某行反复触发单行卸载超时时，说明该运行时进程内可能累积了未结束的后台 Promise / 协程或句柄，应安排滚动重启该运行时进程，或将该插件拆到独立运行时隔离。

### 4.5 要懂的概念（对照 #663 概念预算）

对照 [#663](https://github.com/arcships/rutis/issues/663) 的各层概念预算（插件作者 7 个 / 调试时约 9 个、`rutis-host` 用户 6 个、宿主持有者 10 个，每层调试/运维允许最多 +3 个）：

- **插件作者**：**0 个新概念**。作者已经知道“清理”和“卸载栈”（[卸载顺序](design-unload-order-2026-11-01.md)）；单行预算默认关闭，只有在清理卡住且开启预算时才看到一条自解释的诊断行。
- **`rutis-host` 用户 / 宿主持有者 / 部署的人**：**+1 个可选环境变量**（`RUTIS_ROW_UNLOAD_TIMEOUT`），与已有的 `RUTIS_SHUTDOWN_TIMEOUT` 同单位、同解析规则，归入调试/运维格内，不新增概念层，未超预算。

## 五、兼容与迁移

- **默认完全兼容**：未设置 `RUTIS_ROW_UNLOAD_TIMEOUT` 时，Node（叶子与原生 Cordis）、Bun、Python 的行为、输出与性能均与本稿之前 100% 一致；不涉及破坏性变更，无需在迁移指南里写不兼容条目。
- **协议不变**：不改消息帧格式、不升协议版本、不新增控制方法或 `rutis/*` 事件。
  - **新运行时遇到旧宿主**：正常工作。旧宿主若向环境透传 `RUTIS_ROW_UNLOAD_TIMEOUT`，新运行时本地执行预算并正常回复 `rows.unload`。
  - **旧运行时遇到新宿主**：正常工作。旧运行时忽略 `RUTIS_ROW_UNLOAD_TIMEOUT`，维持不限时等待。
- **显式开启后的语义变化**：开启 `RUTIS_ROW_UNLOAD_TIMEOUT` 后，卡住的用户清理不再让 `rows.unload` 无限挂起，而是到点放弃等待、跳过后续用户清理、跑完剩余撤回并打印一条 stderr 诊断行。

## 六、实施

| 阶段 / 拆分 | 范围与改动 | 完成条件 |
| --- | --- | --- |
| 1. 本稿与 PoC（[#1323](https://github.com/arcships/rutis/issues/1323)，当前分支） | 双语设计稿 `docs/design-row-unload-budget-2026-11-20.{md,en.md}`；最小可运行 PoC：`node/rutis-runtime/src/leaf.mjs`、`node/rutis-runtime/src/runner.mjs`、`bun/rutis-bun/src/runner.ts`、`python/rutis/rutis/runner.py` 及三家针对性单元测试 | 默认不限时行为不变；开启预算后卡住清理及时返回、逐字输出与 §四 完全一致、跳过后续用户清理、仍完整撤回服务与收尾；Node 原生 Cordis fiber 超时路径覆盖；三家测试全过且文档链接 / 约束检查通过 |
| 2. 宿主与文档整合（后续 issue） | `rutis-host` 透传与文档更新（`docs/guide/rutis-host.{md,en.md}`、`docs/guide/cordis-boundaries.{md,en.md}` §7–§8、`docs/design-events-protocol-2026-10-18.{md,en.md}` §7.1）；可选 `--row-unload-timeout` 参数；`crates/rutis-loader` 端到端测试（验证行脱离 `Unloading` 并可重载）；晋升稳定后登记 `docs/guarantees.md` | 端到端测试锁定三家运行时的逐字输出与重载恢复，指南双语同步 |
| 3. Go / Swift / Kotlin 对齐（后续 issue） | Go（`go/rutis/internal/runtime/runtime.go`）、Swift（`swift/rutis/Sources/Rutis/API/Runtime.swift`）、Kotlin（`kotlin/rutis/.../internal/Runtime.kt`）的 `rows.unload` 统一支持 `RUTIS_ROW_UNLOAD_TIMEOUT`、遵守 D8 撤回不变量并采用相同诊断措辞 | 六种语言在单行卸载预算上的环境变量、撤回不变量与诊断格式完全一致 |
