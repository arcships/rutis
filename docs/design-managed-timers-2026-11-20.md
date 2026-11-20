# 托管定时器与裸资源边界

[English](design-managed-timers-2026-11-20.en.md)

状态：草稿（Rust / Node 叶子 / Bun / Python 最小 PoC 已实现，Go / Swift / Kotlin 待实施）。日期：2026-11-20。
Issue：[#1325](https://github.com/arcships/rutis/issues/1325)。按本稿实施的 issue 见 §六。

## 一、现在怎样 / 要解决什么

插件经常需要延后执行一次任务（防抖、重试、启动后延迟上报）或周期性执行任务（心跳、缓存刷新、轮询外部状态）。现在 `ctx` 上只有底层的 `ctx.effect`，没有托管定时器：

1. **裸定时器悄悄泄漏**：插件作者在 `apply` 里直接写 `setTimeout` / `setInterval`（JS/TS）、`asyncio.create_task`（Python）、`tokio::spawn`（Rust）或 `time.NewTicker`（Go）而忘记配对 `ctx.effect` 清理时，行在卸载或 `rutis-host dev` 热重载之后不会报任何错，旧定时器却继续在同一个运行时进程里触发——拿着已经失效的状态或旧配置跑，重载几次就叠出几份重复轮询。
2. **手写安全定时器样板过多**：即使作者记得挂进 `ctx.effect`，要同时做到“提前取消幂等”“卸载开始后不再触发新回调”“异步回调执行超时时不与下一拍重叠”“回调抛错不炸掉共用运行时进程且写明归属哪一行”“卸载期间再次登记被拒绝”，每次都要手写约 30 行容易漏掉边界条件的胶水代码。
3. **裸资源的职责边界没说清**：除了定时器，插件还会直接创建原始 socket、WebSocket、数据库连接池、后台线程等外部资源。哪些资源由 rutis 托管、哪些必须由插件自己用 `ctx.effect` / 取消信号托管，以及 `rutis-host check` 为什么不做跨语言静态 AST 扫描，都需要定下来写进规范，而不是靠猜测。

## 二、决策

| # | 决策 | 为什么；放弃的做法 |
| --- | --- | --- |
| D1 | 托管定时器是 `Ctx` / 各语言 SDK `ctx` 上的纯增量辅助方法，**严格构建在已有 `effect`（卸载栈）之上**，不新增内核原语、不改线协议、不升 `PLUGIN_API` | 遵守[设计哲学](design-philosophy.md) §七 原则 1、2（“能放在内核之外吗”）：定时器是进程内本地能力，复用既有卸载栈即可自动继承逆序卸载、提前取消与失活拒绝（`InactiveEffect` / `sdk/unload-order` D5）。放弃“在内核或运行时协议里加定时器调度原语”：把纯本地调度拉过进程边界只会增加协议复杂度而无收益 |
| D2 | 七种语言统一使用 `timeout(delay, callback)` 与 `interval(period, callback)` 两个名字（Go 按导出习惯首字母大写为 `Timeout` / `Interval`），时长参数类型与单位随各语言标准库惯例 | 跨语言心智一致，同时避免在不同语言里反直觉地换算单位：Rust 用 `Duration`、Node/Bun 用毫秒 `number`、Python 用秒 `float`、Go 用 `time.Duration`、Swift 用 `Duration`、Kotlin 用 `kotlin.time.Duration`。放弃叫 `setTimeout` / `setInterval`（与 JS 裸全局重名且在非 JS 语言里突兀）；放弃强制七种语言都传毫秒整数（与 Rust/Go/Swift/Kotlin 的强类型时长和 Python 的 `asyncio.sleep` 冲突） |
| D3 | 返回可提前取消的幂等句柄；卸载时通过卸载栈自动取消；自然结束的一次性 `timeout` 自行从卸载栈摘除 | 句柄支持提前取消（防抖重置、条件满足即停），重复调用或在回调内部调用都安全。卸载时不再启动新回调；进行中的回调会被取消（协作式语言）或等待收尾（JS）。已触发完毕的 `timeout` 自行摘除，长寿插件反复调用 `ctx.timeout` 不会在卸载栈里堆积死记录。放弃不返回句柄（无法提前取消）；放弃等卸载时才清理已结束的 `timeout`（造成内存增长） |
| D4 | `interval` 采用**不重叠的固定延迟（fixed-delay）**：上一拍异步回调完成（正常返回或抛错）之后，再等 `period` 才安排下一拍；第一拍在 `period` 之后触发（不在 `t = 0` 立即触发） | 插件周期任务大多包含网络 I/O 或跨进程调用，单拍耗时偶发超过 `period` 很常见。固定速率（fixed-rate，如裸 `setInterval`）会让多拍并发叠罗汉，耗尽连接池或破坏插件内部状态；固定延迟天然保证同一 `interval` 至多一拍在运行，且在慢响应或连续失败时自动背压。放弃固定速率或加 `mode` 选项（增加概念面且对绝大多数插件是陷阱；真正需要严格节拍对齐的少数场景可继续用 `ctx.effect` + 单调时钟手写） |
| D5 | **错误隔离**：定时回调抛出的异常、rejected Promise、返回的 `Err` 或 panic 全部就地捕获并归属到所属行/插件报告，绝不崩溃共用运行时进程；`interval` 在某一拍失败后，仍按 `period` 安排下一拍 | Node、Bun、Python 多行共用一个进程，Rust 多个插件共用一个宿主进程；后台定时回调如果走未捕获异常，在 Bun 与 Node `run` 下会退化成“来源不明”，或直接触发整行重启风暴。就地捕获并送往 Rust `ErrorSink` / SDK 带行名 stderr 行，与 `emit` 监听器一致；`interval` 遇偶发错误继续下一拍，让网络抖动或依赖短暂不可用能自动恢复（想遇错即停的作者在回调里调用取消句柄即可）。放弃“回调抛错就重启整行”或“抛错即静默终止 interval”（前者造成抖动重启风暴，后者让后台轮询静默死亡） |
| D6 | 卸载期间或已处置后登记定时器，遵循[卸载顺序](design-unload-order-2026-11-01.md) D5：拒绝登记且**绝不启动定时器** | 卸载一旦取走卸载栈，此时新挂的定时器永远不会被本次卸载清理。Rust 返回 `Err(ErrorKind::InactiveEffect)`（根关闭时 `Err(ErrorKind::Closed)`）；各 SDK 写逐字一行 `rutis: row <行> is not active: timer is not registered`（Node 叶子写 `plugin <插件>`），并返回一个空操作的取消句柄。放弃静默丢弃或仍启动无主定时器 |
| D7 | 登记时立即校验参数：`timeout` 要求有限且 `>= 0`，`interval` 要求有限且 `> 0`；JS/TS 额外限制不超过 `2147483647` ms（`2^31 - 1`） | `delay = 0` 是合法的“下一拍执行”；`period = 0` 会忙等饿死事件循环（Tokio 的 `interval(Duration::ZERO)` 会直接 panic）。Node/Bun 的底层 `setTimeout` 会把超过 `2^31 - 1` ms（约 24.8 天）的数值静默折成 `1` ms，必须显式拒绝而不是静默提前 24 天触发。参数校验先于活跃状态检查，同步抛出明确错误。放弃像浏览器 `setTimeout` 那样把负数/`NaN` 静默钳位成 `0` 或 `1` ms |
| D8 | Node 运行时里，`ctx.timeout` / `ctx.interval` **只加在 rutis 叶子插件的 `ctx`**（`node/rutis-runtime/src/leaf.mjs`），绝不修改原生 Cordis `Context` | Node 运行时同时承载叶子插件与原生 Cordis 插件，许多 Cordis 插件会安装 `@cordisjs/plugin-timer` 或在 `Context` 上挂载定时器服务。只在 `toCordis` 构造的叶子 `ctx` 上暴露这两个方法，保证 Cordis 插件拿到原封不动的 Cordis `ctx`，永不遮蔽或破坏 `@cordisjs/plugin-timer`。放弃往 `Context.prototype` 上打补丁 |
| D9 | **裸资源边界**：`ctx.effect` 之外创建的裸定时器、socket、数据库连接池、原生线程仍由插件作者负责；**不在 `rutis-host check` 里内置多语言静态 AST 扫描** | 1）高误报率：请求内部的短命超时（如 `AbortSignal.timeout`、带退避的局部 `new Promise(r => setTimeout(r, 50))`、单次请求内打开并关闭的连接）完全合法且不会跨卸载泄漏，静态扫描会制造大量噪声警告和豁免注释；2）六语言解析维护成本：`rutis-host` 是单个 Rust 二进制，在其中维护 JS/TS、Python、Go、Swift、Kotlin、Rust 六种语言的 AST 解析与逃逸分析得不偿失；3）全局 monkey-patch `setTimeout` 会误伤第三方库内部计时器。改为在插件指南写清 `ctx.effect` 与取消信号的标准写法，并列出各生态自带的可选 lint 规则（§三 §3.6） |

## 三、规则

### 3.1 七种语言的签名

| 语言 | `timeout` 签名 | `interval` 签名 | 时长单位 | 取消句柄 |
| --- | --- | --- | --- | --- |
| **Rust** (`rutis::Ctx`) | `pub fn timeout<F, Fut>(&self, delay: Duration, f: F) -> Result<Disposer, Error>`（`F: FnOnce() -> Fut + Send + 'static`, `Fut: Future<Output = Result<(), Error>> + Send + 'static`） | `pub fn interval<F, Fut>(&self, period: Duration, f: F) -> Result<Disposer, Error>`（`F: FnMut() -> Fut + Send + 'static`, `Fut: Future<Output = Result<(), Error>> + Send + 'static`） | `std::time::Duration` | `Disposer`（`.dispose().await` 取消并等待退出，幂等） |
| **Node 叶子** (`@arcships/rutis`) | `ctx.timeout(delayMs: number, callback: () => unknown): () => void` | `ctx.interval(periodMs: number, callback: () => unknown): () => void` | 毫秒 (`ms`) | `() => void`（同步取消，幂等） |
| **Bun** (`@arcships/rutis`) | `ctx.timeout(delayMs: number, callback: () => unknown): () => void` | `ctx.interval(periodMs: number, callback: () => unknown): () => void` | 毫秒 (`ms`) | `() => void`（同步取消，幂等） |
| **Python** (`rutis`) | `ctx.timeout(delay: float, callback: Callable[[], Any]) -> Callable[[], None]` | `ctx.interval(period: float, callback: Callable[[], Any]) -> Callable[[], None]` | 秒 (`s`) | `Callable[[], None]`（同步取消，幂等） |
| **Go** (`rutis`) | `func (c *Ctx) Timeout(delay time.Duration, fn func(context.Context) error) func()` | `func (c *Ctx) Interval(period time.Duration, fn func(context.Context) error) func()` | `time.Duration` | `func()`（同步取消，幂等） |
| **Swift** (`Rutis`) | `public func timeout(_ delay: Duration, _ body: @escaping @Sendable () async throws -> Void) throws -> () -> Void` | `public func interval(_ period: Duration, _ body: @escaping @Sendable () async throws -> Void) throws -> () -> Void` | `Duration` | `() -> Void`（同步取消，幂等） |
| **Kotlin** (`arcships.rutis`) | `fun timeout(delay: Duration, block: suspend () -> Unit): () -> Unit` | `fun interval(period: Duration, block: suspend () -> Unit): () -> Unit` | `kotlin.time.Duration` | `() -> Unit`（同步取消，幂等） |

### 3.2 调度与生命周期规则（七种语言一致）

1. **`timeout(delay, callback)`**：
   - 登记时校验参数，再检查当前行/fiber 是否活跃。不活跃则按 §3.4 拒绝，绝不启动定时器。
   - 活跃时把取消清理压入卸载栈（经 `effect`），等待 `delay` 后若未被取消且行仍活跃，则执行 `callback` 一次（支持同步函数与异步函数/协程/Future）。
   - 执行完毕后，该条清理记录自动从卸载栈摘除。
2. **`interval(period, callback)`**：
   - 同上完成参数校验、活跃检查与卸载栈登记。
   - 首次触发在登记后至少 `period` 时长之后（不在 `t = 0` 立即执行）。
   - **严格不重叠（固定延迟）**：每一拍等待 `callback`（含其返回的 Promise / 协程 / Future）完全结束（正常返回或错误被捕获报告）后，若未被取消且行仍活跃，才开始下一次 `period` 的等待。
   - 某一拍出错并被报告后，循环继续按 `period` 等待下一拍，除非已被取消或行开始卸载。
3. **提前取消**：
   - 调用返回的取消句柄后，待触发的等待立即取消，之后不会再启动任何回调。
   - 句柄幂等：重复调用多次或在回调已完成后调用都是空操作。
   - 允许在回调内部调用自身的取消句柄（例如 `interval` 满足条件后自停）：当前这一拍正常执行完毕，不再安排下一拍。
     - 差异说明（Rust）：Rust 的 `Disposer::dispose(self)` 消费自身并等待定时任务彻底退出，与 `ctx.on` 的 `Disposer` 相同，不能在回调任务自身里 `.await` 自己的 `Disposer`（会自等死锁）；需要回调内自停时可用普通 `AtomicBool` 标志或在回调外持有 `Disposer`。
4. **卸载行为与卸载栈顺序**：
   - 行开始卸载那一刻起（Rust 的 `pre_cancel`、Node 的 `signal` 中止与 `drain`、Bun 的移出行表、Python 的 `row.unloading = True`、Go/Swift/Kotlin 的行取消），不再启动任何新的定时回调。
   - 卸载栈按后进先出（LIFO）逆序执行到该定时器的清理条目时：
     - 仍在等待的定时器立即取消，零次触发。
     - 正在执行中的回调：Rust（`tokio::select!` 丢弃 future）、Python（`task.cancel()` 并 `await task`）、Go（取消 `context.Context` 并等待返回）、Swift（取消 `Task` 并等待）、Kotlin（取消协程 `Job` 并等待）会协作取消并等待任务彻底退出；Node 与 Bun（JS Promise 不能从外部强制取消）会停止后续调度并等待当前已启动的 Promise 结束（受宿主 `RUTIS_SHUTDOWN_TIMEOUT` 约束，与其他异步清理相同）。
   - 因为定时器通过 `effect` 进同一条卸载栈，在 `provide` 之后登记的定时器会先于服务撤回被停掉，在 `provide` 之前登记的则后于服务撤回被停掉。
5. **输出前缀与行归属**：
   - 定时回调在所属行的异步上下文中执行（Node 继承 `AsyncLocalStorage`，Bun 经 `within(row.key, ...)`，Python 经 `contextvars` 携带 `current_row`，Go/Swift/Kotlin/Rust 绑定所属行）。开启 `RUTIS_PREFIX_OUTPUT=1` 时，回调里的普通日志自动带 `[<行>] ` 前缀；运行时自身写出的 `rutis: timer of …` 报告行不重复加前缀。

### 3.3 参数校验规则

参数校验在活跃状态检查**之前**同步执行：

| 语言 | 无效回调 | `timeout` 无效 `delay` | `interval` 无效 `period` |
| --- | --- | --- | --- |
| Rust | 编译期类型检查 | `Duration` 恒为非负有限值，不会失败 | `period.is_zero()` 返回 `Err(ErrorKind::Validation { issues: ["interval period must be greater than zero"] })` |
| Node 叶子 / Bun | 抛 `TypeError: timeout needs a callback function` / `interval needs a callback function` | 非数字、非有限数或 `< 0` 抛 `RangeError: timeout delay must be a non-negative finite number of milliseconds, got <值>`；`> 2147483647` 抛 `RangeError: timeout delay must be at most 2147483647 ms, got <值>` | 非数字、非有限数或 `<= 0` 抛 `RangeError: interval period must be a positive finite number of milliseconds, got <值>`；`> 2147483647` 抛 `RangeError: interval period must be at most 2147483647 ms, got <值>` |
| Python | 抛 `TypeError: timeout needs a callable callback` / `interval needs a callable callback` | `bool`、非 `int`/`float`、非有限数或 `< 0` 抛 `ValueError: timeout delay must be a non-negative finite number of seconds, got <repr>` | `bool`、非 `int`/`float`、非有限数或 `<= 0` 抛 `ValueError: interval period must be a positive finite number of seconds, got <repr>` |
| Go | `fn == nil` panic `"rutis: Timeout needs a callback"` / `"rutis: Interval needs a callback"` | `delay < 0` panic `"rutis: timeout delay must be non-negative"` | `period <= 0` panic `"rutis: interval period must be greater than zero"` |
| Swift | 编译期类型检查 | `delay < .zero` 抛 `RutisError(name: "RangeError", message: "timeout delay must be non-negative")` | `period <= .zero` 抛 `RutisError(name: "RangeError", message: "interval period must be greater than zero")` |
| Kotlin | 编译期类型检查 | `delay.isNegative()` 抛 `IllegalArgumentException("timeout delay must be non-negative")` | `!period.isPositive()` 抛 `IllegalArgumentException("interval period must be greater than zero")` |

### 3.4 卸载期间或已处置后的登记规则

| 语言 | 行为与逐字输出 | 返回值 |
| --- | --- | --- |
| Rust | 不启动任务，返回 `Err(ErrorKind::InactiveEffect)`（根关闭中返回 `Err(ErrorKind::Closed)`，旧代上下文返回 `Err(ErrorKind::StaleGeneration)`） | `Err(...)` |
| Node 叶子 | 不启动定时器，向 stderr 写一行：`rutis: plugin <插件> is not active: timer is not registered`（未声明 `name` 时 `<插件>` 为 `leaf`） | 空操作 `() => {}` |
| Bun / Python / Go / Swift / Kotlin | 不启动定时器，向 stderr 写一行：`rutis: row <行> is not active: timer is not registered` | 空操作取消句柄 |

### 3.5 回调错误的隔离与报告格式

| 语言 | 报告通道 | 逐字格式 |
| --- | --- | --- |
| Rust | `ctx.error_sink()`（默认 sink 打印到 stderr），以 `ErrorKind::PluginFailed` 包装，不崩溃线程 | `<归属>: timer failed: <错误>`（行取行名，Rust 插件取插件名，根取 `the host`） |
| Node 叶子 | stderr（`say`，不重复加 `[行]` 前缀），不触发 `uncaughtException` / `unhandledRejection` | `rutis: timer of plugin <插件> failed: <错误名>: <消息>` |
| Bun | stderr（`say`），不触发 `uncaughtException` / `unhandledRejection` | `rutis: timer of row <行> failed: <错误名>: <消息>` |
| Python | stderr（`say`），不走事件循环未捕获异常处理器 | `rutis: timer of row <行> failed: <异常类名>: <消息>` |
| Go | stderr，panic 经 `peer.Protect` 恢复 | `rutis: timer of row <行> failed: <错误>` |
| Swift | 运行时日志 / stderr | `rutis: timer of row <行> failed: <错误>` |
| Kotlin | `Report.line`（stderr） | `rutis: timer of row <行> failed: <异常类名>: <消息>` |

注：JS/TS 里如果抛出的不是 `Error` 对象（例如 `throw 'boom'`），`<错误名>: <消息>` 退化为 `String(error)`（即 `boom`）。

### 3.6 裸资源边界与推荐模式

以下资源**不在** rutis 自动托管范围内，仍由插件作者负责在卸载时释放：

- 直接调用语言原生的 `setTimeout` / `setInterval`、`asyncio.create_task`、`threading.Thread`、`tokio::spawn`、`go func()`；
- 原始网络监听与连接（`net.createServer`、`WebSocket`、gRPC 通道、长连接 HTTP 客户端）；
- 数据库连接池、文件句柄、子进程、消息队列消费者。

推荐的两种标准写法（在[插件 API](guide/plugin-api.md) 指南中给出完整对照）：

1. **资源获取即登记 `ctx.effect`**：把关闭逻辑紧跟在创建语句之后登记，卸载时按逆序执行：
   ```ts
   const pool = createPool(config.dsn)
   ctx.effect(() => pool.end())
   ```
2. **把生命周期取消信号传进长阻塞 I/O**：
   - Node / Bun：`fetch(url, { signal: ctx.signal })`、监听 `ctx.signal.addEventListener('abort', ...)`；
   - Rust：`tokio::select! { _ = ctx.cancelled() => ..., ... }`（配合 `ctx.effect` 等待任务退出）；
   - Go：把 `ctx`（本身即 `context.Context`）传给数据库/HTTP 调用，后台协程用 `ctx.Go(...)`；
   - Python / Swift / Kotlin：挂在托管定时器或托管任务里，卸载取消当前 Task / 协程，并在 `ctx.effect` 里关闭连接池。

希望在自家插件仓库里禁止裸 `setTimeout` / `setInterval` 的团队，可以直接启用各语言生态自带的 lint 规则，而无需 rutis 在 `rutis-host check` 中维护六套解析器：

- JS/TS（ESLint）：`no-restricted-globals: ["error", "setTimeout", "setInterval"]`；
- Python（Ruff）：`flake8-tidy-imports.banned-api`（限制直接调用 `asyncio.create_task` 等）；
- Rust（Clippy）：`clippy.toml` 的 `disallowed-methods`（限制插件 crate 直接调用 `tokio::spawn`）。

## 四、开发者看到什么

### 4.1 插件作者

#### 正常时

以 TypeScript（Node 叶子 / Bun 写法完全相同）为例：

```ts
import { definePlugin } from '@arcships/rutis'

export default definePlugin({
  name: 'heartbeat',
  inject: ['store'],
  config: {
    type: 'object',
    properties: {
      periodMs: { type: 'number', default: 5000 },
      dsn: { type: 'string' },
    },
    required: ['dsn'],
  },
  apply(ctx, config) {
    const store = ctx.use('store')

    // 1. 一次性延迟任务：返回取消句柄；自然完成后自动从卸载栈摘除
    const cancelWarmup = ctx.timeout(500, async () => {
      await store.ping()
    })

    // 2. 周期任务：上一拍结束后再等 periodMs（绝不重叠），卸载时自动停止
    const stopLoop = ctx.interval(config.periodMs, async () => {
      await store.flush()
    })

    // 3. 裸资源（如数据库连接池）：显式用 ctx.effect + ctx.signal 托管
    const pool = openPool(config.dsn)
    ctx.effect(() => pool.close())

    ctx.provide('heartbeat', {
      pause() {
        cancelWarmup()
        stopLoop()
      },
    })
  },
})
```

正常运行时无额外输出；在 `rutis-host dev` 中修改文件触发热重载时，旧实例的 `timeout` 与 `interval` 在卸载瞬间停止，不会出现新旧两套心跳同时运行的现象；如果某一次 `store.flush()` 耗时 8 秒而 `periodMs` 是 5000，下一拍会在那次 `flush()` 返回后再等 5 秒开始，不会并发叠出多个 `flush()`。

其他语言只在 API 名字与时长单位上不同：

- **Rust**：`let stop = ctx.interval(Duration::from_secs(5), || async move { ... Ok(()) })?;`（提前取消用 `stop.dispose().await?`）；
- **Python**：`stop = ctx.interval(5.0, flush)`（单位为秒，`flush` 可为 `def` 或 `async def`）；
- **Go**：`stop := ctx.Interval(5*time.Second, func(ctx context.Context) error { ... })`；
- **Swift**：`let stop = try ctx.interval(.seconds(5)) { try await flush() }`；
- **Kotlin**：`val stop = ctx.interval(5.seconds) { flush() }`。
- **Node 里的原生 Cordis 插件**：拿到的仍是原生的 Cordis `ctx`，无 `ctx.timeout` / `ctx.interval`，现有 `@cordisjs/plugin-timer` 用法完全不变。

#### 出错时

| 触发情形 | 语言 | 逐字输出（与错误码） | 记在哪一行 | 怎样改 |
| --- | --- | --- | --- | --- |
| 定时回调抛错或 reject / 返回 `Err` / panic | Node 叶子 | `rutis: timer of plugin heartbeat failed: Error: connection refused` | `heartbeat`（插件名） | 修复回调逻辑或捕获预期瞬时错误；运行时进程继续，`interval` 下一拍照常运行 |
| 定时回调抛错或 reject | Bun | `rutis: timer of row heartbeat failed: Error: connection refused` | `heartbeat` | 同上 |
| 定时回调抛异常 | Python | `rutis: timer of row heartbeat failed: RuntimeError: connection refused` | `heartbeat` | 同上 |
| 定时回调返回 `Err` 或 panic | Rust | `heartbeat: timer failed: connection refused`（送入 `ErrorSink`，`ErrorKind::PluginFailed`） | `heartbeat`（行名或插件名；根为 `the host`） | 同上 |
| 定时回调返回错误或 panic | Go / Swift / Kotlin | `rutis: timer of row heartbeat failed: connection refused`（Kotlin 带异常类名：`IllegalStateException: connection refused`） | `heartbeat` | 同上 |
| `timeout` 传入负数 / `NaN` / `Infinity` / 非数字 | Node 叶子 / Bun | `RangeError: timeout delay must be a non-negative finite number of milliseconds, got -5`（`apply` 内抛出导致本行加载失败：`heartbeat: failed in apply: RangeError: …`） | `heartbeat` | 传入 `>= 0` 且有限的毫秒数 |
| `timeout` 传入超过 `2147483647` ms | Node 叶子 / Bun | `RangeError: timeout delay must be at most 2147483647 ms, got 3000000000` | `heartbeat` | 拆成更短周期或传入 `<= 2147483647` 的毫秒数 |
| `timeout` 传入负数 / `NaN` / `bool` | Python | `ValueError: timeout delay must be a non-negative finite number of seconds, got -1` | `heartbeat` | 传入 `>= 0` 且有限的秒数 |
| `interval` 传入 `0` 或负数 | Node 叶子 / Bun | `RangeError: interval period must be a positive finite number of milliseconds, got 0` | `heartbeat` | 传入 `> 0` 且有限的毫秒数 |
| `interval` 传入 `0` 或负数 | Python | `ValueError: interval period must be a positive finite number of seconds, got 0` | `heartbeat` | 传入 `> 0` 且有限的秒数 |
| `interval` 传入 `Duration::ZERO` | Rust | `config validation failed: ["interval period must be greater than zero"]`（`ErrorKind::Validation`） | 调用方（`apply` 返回时记在当前插件/行） | 传入非零的 `Duration` |
| 回调不是函数 / 不可调用 | Node 叶子 / Bun / Python | `TypeError: timeout needs a callback function` / `TypeError: interval needs a callback function`（Python：`TypeError: timeout needs a callable callback` / `TypeError: interval needs a callable callback`） | `heartbeat` | 传入函数 / callable |
| 在清理（卸载期间）或卸载后再调用 `ctx.timeout` / `ctx.interval` | Node 叶子 | `rutis: plugin heartbeat is not active: timer is not registered`（不启动定时器，返回空操作句柄） | `heartbeat` | 不要在卸载清理中再启动新定时器；把收尾工作直接写在当前清理函数内 |
| 同上 | Bun / Python / Go / Swift / Kotlin | `rutis: row heartbeat is not active: timer is not registered`（不启动定时器，返回空操作句柄） | `heartbeat` | 同上 |
| 同上 | Rust | `Err(ErrorKind::InactiveEffect)`（显示：`fiber disposed`；根关闭中为 `Err(ErrorKind::Closed)`，显示：`root has been shut down`） | 调用方 | 同上 |
| 在 `ctx.effect` 外直接写裸 `setInterval` 或打开 socket | 所有语言 | 卸载时**无报错**，旧回调或连接继续泄漏 | — | 定时任务改用 `ctx.timeout` / `ctx.interval`；socket/连接池用 `ctx.effect` 登记关闭并监听取消信号 |

#### 要懂的概念（对照 #663 概念预算）

对照 [#663](https://github.com/arcships/rutis/issues/663) 写下的插件作者概念预算（7 个核心概念：插件、服务、`inject`、配置、事件、测试工具、`dev` 与行，以及持有外部资源时的 `ctx.effect`）：**新增 0 个概念**。`ctx.timeout` 与 `ctx.interval` 只是既有 `ctx.effect` 生命周期模型上的便捷方法，反而消除了作者自己拼装非重叠循环、取消标志与错误隔离的负担。

### 4.2 用 `rutis-host` 组装插件的人

- **正常时**：`rutis.json` 与 CLI 命令无任何变化；`rutis-host dev` 热重载插件时，使用 `ctx.timeout` / `ctx.interval` 的行自动清空旧定时器，不再堆积重复回调。
- **出错时**：某行定时回调失败时，stderr 打印一行带行名的报告（见 §4.1 表），其他行和运行时进程不受影响；某行在 `apply` 里把配置项算成了负数传给 `ctx.interval`，该行按常规 `apply` 失败报告，不影响其他行。
- **要懂的概念**：**新增 0 个概念**。

### 4.3 宿主持有者（嵌入 `rutis` 的应用）

- **正常时**：可在任意 `Ctx`（插件或根）上调用 `ctx.timeout` / `ctx.interval`。自定义 `ErrorSink` 通过 `Arc<ErrorKind>` 收到定时回调失败（`ErrorKind::PluginFailed`，`to_string()` 为 `<归属>: timer failed: <错误>`）。若某回调在同步代码里卡住导致 `shutdown_with_timeout` 超时，`Stuck` 通过 `#[track_caller]` 继续写出定时器登记处的文件与行号（`still running the cleanup registered at src/lib.rs:42:9`）。
- **出错时**：`ctx.interval(Duration::ZERO, ...)` 返回 `Err(ErrorKind::Validation)`；在已卸载/卸载中的 fiber 上登记返回 `Err(ErrorKind::InactiveEffect)`；根关闭中返回 `Err(ErrorKind::Closed)`。
- **要懂的概念**：**新增 0 个概念**（复用 `Disposer`、`ErrorSink`、`InactiveEffect`、`Validation`）。

## 五、兼容与迁移

- **纯增量，零破坏性变更**：仅在 `Ctx` 与各语言叶子 `ctx` 上新增 `timeout` / `interval` 两个方法，不改任何已有方法签名、不改线协议、不升 `PLUGIN_API`（保持为 `3`）。
- **Node Cordis 兼容**：原生 Cordis 插件不经过 `leaf.mjs`，`@cordisjs/plugin-timer` 及其他 Cordis 服务完全不受影响。
- **新旧相遇**：
  - 新运行时跑旧插件：100% 兼容，行为无变化。
  - 旧运行时跑调用了 `ctx.timeout` / `ctx.interval` 的新插件：在 `apply` 时立即报常规缺少方法错误（JS `TypeError: ctx.timeout is not a function`，Python `AttributeError: 'Context' object has no attribute 'timeout'`）；使用该 API 的插件在其文档/依赖中要求升级到包含本能力的运行时与 SDK 版本。
- **`docs/guarantees.md`**：本稿当前为草稿与最小 PoC；待七种语言全部实施后，在 `docs/guarantees.md` 与 `docs/guarantees.en.md` 登记 `sdk/managed-timers` 并挂接七语言测试。

## 六、实施

| # | 范围 | 内容 | 完成条件 |
| --- | --- | --- | --- |
| T-1（本 PoC） | 设计稿 + Rust / Node 叶子 / Bun / Python | 双语设计稿；`crates/rutis/src/ctx.rs`、`node/rutis-runtime/src/leaf.mjs`、`node/rutis/src/index.d.ts`、`bun/rutis-bun/src/runner.ts`、`python/rutis/rutis/{plugin,runner}.py`（及 `public-api.txt`）实现 `timeout` / `interval` | 四种语言单元测试覆盖：一次性触发/提前取消/卸载前取消；周期触发/卸载后零触发/慢异步不重叠/回调内自停；非法参数校验；卸载期与卸载后登记逐字拒绝；回调异常/panic 隔离与逐字输出；Node Cordis 互不遮蔽 |
| T-2 | Go SDK | `go/rutis/internal/runtime` 实现 `Ctx.Timeout` / `Ctx.Interval`，更新 `go/rutis/public-api.txt` | Go 单元测试覆盖 §三 全部规则与 §四 逐字输出，通过 `goleak` 零 goroutine 泄漏检查 |
| T-3 | Swift SDK | `swift/rutis/Sources/Rutis/API/Ctx.swift` 实现 `timeout` / `interval`，更新 `swift/rutis/public-api.txt` | `SelfTest` 覆盖 §三 全部规则与 §四 逐字输出 |
| T-4 | Kotlin SDK | `kotlin/rutis/.../Ctx.kt` 实现 `timeout` / `interval`，更新 `api/*.api` | `jvmTest` 覆盖 §三 全部规则与 §四 逐字输出 |
| T-5 | 指南、保证登记与 Rust API 快照 | 更新 `docs/guide/plugin-api.md` / `.en.md`（新增 `ctx.timeout`/`ctx.interval` 节与裸资源 `ctx.effect` 指南）、更新 `crates/rutis/public-api.txt`、在 `docs/guarantees.md` / `.en.md` 登记 `sdk/managed-timers` | `check-md-links.mjs`、`check-guarantees.mjs`、`formal-trace.mjs --check`、`public-api.sh` 全部通过 |
