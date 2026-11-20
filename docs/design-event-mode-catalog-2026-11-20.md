# 可选事件 mode 目录与比对

[English](design-event-mode-catalog-2026-11-20.en.md)

状态：草稿（核心总线原型已在 `crates/rutis` 实现；`rutis-bridge`、`rutis.json` 与 `rutis-host check` 尚未实现）。日期：2026-11-20。
Issue：[#1324](https://github.com/arcships/rutis/issues/1324)。按本稿实施：§六 拆分的步骤 1–4。

## 一、现在怎样与要解决什么

在 rutis 的统一事件模型里（[统一事件](design-events-2026-10-18.md)、[Cordis 兼容边界](guide/cordis-boundaries.md) §1），**一张监听器表**承接**五种分发方式**（`emit`、`parallel`、`serial`、`waterfall`、`bail`）：

- 监听器登记一次（`ctx.on("message", fn)`、`ctx.on::<Message>(...)`），不声明方式；
- 发出方在调用处自行选择方式（`ctx.emit`、`ctx.serial`、`ctx.bail` 等）；
- Rust 的 `Event` trait 只声明 `NAME` 和 `Value`，其余六种语言的事件名是普通字符串，都不约定必须用哪种 `Mode`。

这带来一类极难察觉的**静默失效**：

- **黑名单拦截场景**：宿主或消息管线约定 `"message"` 允许拦截插件返回 `true` 短路消息（需要 `bail` 或 `serial`）。黑名单插件登记 `ctx.on("message", msg => isBlocked(msg.user))` 返回 `true`；但某个发出方插件（例如刚迁移的 Cordis 插件，或另一种语言写的插件）误用了 `ctx.emit("message", msg)`。
- **今天会发生什么**：`emit` 照常启动黑名单监听器，监听器算出并返回 `true`，总线按 `emit` 的规则直接丢弃返回值（[Cordis 兼容边界](guide/cordis-boundaries.md) §1）。**没有警告、没有报错**，被拉黑的消息照常放行。
- 类似地：
  - 本该用 `waterfall` 的事件被误用 `emit` / `parallel` / `serial` 发出时，只有真正调用了 `next()` 的监听器才会在运行时报 `WaterfallNext`（在 `emit` 下还只打到错误汇集、不回到发出方）；
  - 本该用异步 `serial` 的事件被误用同步 `bail` 发出时，只要场上恰好只有同步监听器就一路通过，直到某天装上一个异步监听器才突然以 `AsyncListener` 失败（[Cordis 兼容边界](guide/cordis-boundaries.md) §2）。

### 为什么不能只靠 `rutis-host check` 静态扫描源码

一个自然的念头是让 `rutis-host check` 静态扫描插件源码里的 `emit`/`serial`/`bail` 调用点并与事件表比对。对 rutis 来说，**纯静态扫描从根本上做不全**：

1. **六种语言加编译产物**：`rutis-host` 同时跑 Node/TypeScript、Bun、Python、Go、Swift、Kotlin、Rust（`dylib` / 内置插件）以及远程节点。Go、Swift、Kotlin、Rust 插件都是编译好的二进制，`rutis-host check` 只拿到描述清单（`describe`），根本看不到源码。在 `rutis-host` 里塞进六种语言的 AST 分析器也违背平台职责边界（[平台清理](design-platform-cleanup-2026-10-28.md) §1）。
2. **未改源码的 Cordis 插件与封装函数**：npm 上的 Cordis 插件通过 `ctx.emit`、`ctx.events.dispatch(...)` 或自定义辅助函数分发，没有 rutis 的事件声明。
3. **动态事件名**：Rust 里 `NAME = ""` 并覆写 `Event::name(&self)` 的类型、`CtxInternal::named`，以及 JS/Python/Go 里的运行时字符串拼接（`ctx.emit(`room/${id}`, ...)`），静态阶段无法可靠还原；强行扫描要么漏报，要么制造大量误报。
4. **远程节点与运行后接入**：经 WebSocket 节点连接（`rutis-host/dial`、`rutis-host/listen`）接进来的发出方在 `check` 时可能根本不在线。

反过来，自[平台清理](design-platform-cleanup-2026-10-28.md) §7.5 去掉运行时本地快速路径之后，**所有六种运行时、未改动的 Cordis 插件以及 Rust 插件发出的每一个非 `internal/*` 事件，全部经过宿主唯一的 Rust 内核 `EventBus`**。

因此本稿采用**两层互补**的设计，以**内核总线运行时校验为主**、**`rutis.json` + `rutis-host check` 静态校验为辅**：

1. **核心机制（运行时总线）**：`EventBus`（通过 `CtxHost` 暴露给宿主）持有一张可选的**事件 mode 目录**（`事件名 -> 期望 Mode + 策略`）。发出目录里登记的事件且实际 `Mode` 不符时，默认告警（`warn`，经 `ErrorSink` 去重报告，原有分发照常执行，绝不破坏既有 Cordis 插件），可选严格拒绝（`error`，在选监听器之前以明确错误拒绝）。一处内核机制覆盖全部六种运行时、Cordis 插件、编译产物与运行时落到目录名的动态事件，且不改 Cordis API、不升 `PLUGIN_API`。
2. **互补机制（`rutis.json` 与 `rutis-host check`）**：`rutis.json`（`format: 1`）增加可选顶层字段 `"events"`。`rutis-host check` 校验目录本身（未知 mode 给拼写建议、非法策略、保留前缀等），并与自愿声明了静态事件期望的插件做比对，对未声明的插件和动态事件名明确跳过、零误报；`rutis-host run` / `dev` 启动时把同一份 `"events"` 目录装进根上下文。

## 二、决策

**D1 目录按精确事件名索引，完全可选。** 目录把精确事件名（如 `"message"`、`"session/created"`）映射到期望的 `Mode`（`emit | parallel | serial | waterfall | bail`）及可选的单事件策略（`warn | error`）。不在目录里的事件（包括未列出的动态事件名）完全不检查，零误报；目录为空时热路径只做一次原子布尔读取，不加锁。**放弃**：在 `rutis::Event` trait 上强制加 `const MODE: Mode`——破坏所有既有 Rust `Event` 实现、覆盖不了字符串语言和 Cordis 插件，也否定了合法的多方式事件；**放弃**：前缀或通配符规则——`"message/*"` 会把走 `emit` 的 `"message/deleted"` 误判进 `"message"` 的 `bail` 规则，且引入多规则优先级歧义。

**D2 两处互补：内核 `EventBus` 运行时执行（主）+ `rutis-host check` 启动前检查（辅）。** 理由见 §一：只有内核 `EventBus` 能完整覆盖六种语言、编译二进制、未改动的 Cordis 插件和动态事件名；`check` 则在启动前兜住 `rutis.json` 拼写错误以及显式声明了事件期望的插件冲突。**放弃**：只在 `rutis-host check` 做源码静态扫描——对编译产物、Cordis 封装和动态名均失效。

**D3 不匹配策略默认 `warn`，可按目录或按事件切到 `error`。**
- `EventModePolicy::Warn`（默认）：分发**照常执行**（返回值语义不变，既有 Cordis 插件不会因开启目录而中断），同时把 `ErrorKind::EventModeMismatch` 报给宿主的 `ErrorSink`。**`warn` 也是唯一在全部语言、全部五种方式下语义完全一致的默认策略**：在 `warn` 下，Rust、六种运行时与 Cordis 插件的调用点签名和返回行为零差异，且宿主 `ErrorSink` 统一收到完全相同的诊断行；而切到 `error` 后，`emit` 存在天然的语言接口差异（Rust `ctx.emit()` 返回 `Result<(), Error>` 可在调用点同步报错，其余语言的 `ctx.emit()` 是无返回值的发出即不管接口，无法在调用点同步抛出，见 §3.3 与 §4.1）。
- `EventModePolicy::Error`：在选取和调用任何监听器**之前**拒绝本次分发，直接返回 `ErrorKind::EventModeMismatch`（0 个监听器执行）。
- 目录有默认策略（默认 `Warn`），每个事件条目也可单独指定策略（`expect_with` / `on_mismatch`），便于宿主逐个事件从告警收紧到严格。**放弃**：默认 `error`——不仅会立刻打断尚未改完的既有 Cordis 插件，还会把 `emit` 在 Rust 与其他语言之间的调用点报错差异直接暴露给所有未显式选择严格模式的宿主。

**D4 `warn` 按 `(发出方, 事件名, 实际方式, 期望方式)` 去重（上限 4096 项）；`error` 不去重。** 高频事件（如 `"message"`）在 `warn` 下如果不去重会刷爆 stderr；只按事件名去重又会遮盖第二个同样写错方式的插件。按四元组去重使每个出错的“发出方 + 错误方式”在目录不变期间恰好报告一次，重新调用 `set_event_mode_catalog` 时清空；记录表用 FIFO 有界集合限制在 4096 项以内，避免动态插件名导致内存增长。`error` 模式下每次违规分发都必须被拒绝，绝不去重。**放弃**：按时间窗口限流——测试不可预测，且仍会重复刷日志。

**D5 在发出方宿主的 `EventBus` 派发入口检查一次，不依赖当前监听器数量。**
- 检查放在 `EventBus::snapshot_with`（`ensure_bus` 与 `rutis/*` 保留名检查之后，选表与过滤之前）。
- 即使该事件当前 **0 个监听器**，不匹配也会被报告或拒绝：黑名单插件尚未启动或暂时禁用时，调用方写错方式依然能立刻发现。
- 跨节点单条调用（`EventBus::invoke_entry` / `invoke_entry_sync`）不再重复检查目录，避免每个监听器重复告警；每个节点在自己发起分发时按自己的目录检查一次。
- 框架内部派发（`opts.framework == true`，即 `rutis/*` 与 `rutis::*`）跳过目录检查。**放弃**：只在监听器返回了非空值时告警——会漏掉 `waterfall`/`bail`/`serial` 在无监听器命中时的错误调用。

**D6 拒绝空事件名及 `rutis/*`、`internal/*` 保留前缀。** 在 `set_event_mode_catalog` 和 `rutis.json` 读取时，空字符串、`rutis/` 开头（归宿主保留，方式固定为 `emit`）和 `internal/` 开头（留在运行时进程内，不过总线）的条目一律报错拒绝，防止配置出永远不会触发的死规则。校验失败时**保留此前已生效的目录不变**。**放弃**：静默忽略——违背设计原则 §7 第 10 条（显式优于隐式）。

**D7 Rust API 收在 `rutis::host`，插件作者零新增概念；跨语言错误复用 `BindingError`。**
- `EventModeCatalog`、`EventModeEntry`、`EventModePolicy` 放在 `rutis::host`，`set_event_mode_catalog` / `event_mode_catalog` 放在 `CtxHost`（委托给 `EventBus`）；crate 根 `rutis::*` 不新增任何符号。
- `ErrorKind`（已是 `#[non_exhaustive]`）新增 `EventModeMismatch { name, actual, expected, emitter }`。
- 跨进程时（`rutis-bridge` 步骤 2），它按[诊断设计](design-diagnostics-2026-10-26.md) §4.3 与 `bad-config` 同款方式编码为 `BindingError`，错误码 `event-mode-mismatch`，`data` 为 `{"event", "actual", "expected"}`，无需给任何语言 SDK 新增异常类，也不升 `PLUGIN_API`。**放弃**：在六种 SDK 里各自新增 `EventModeMismatchError` 类——扩大全部 SDK 维护面且增加插件作者概念负担。

**D8 `rutis.json`（`format: 1`）加可选 `"events"` 对象，统一对象写法；插件声明事件方式保持可选。**
- 顶层 `"events"` 缺省为空，每个值统一写成 `{"mode": "...", "on_mismatch"?: "warn" | "error"}`（`#[serde(deny_unknown_fields)]`）。**放弃**：同时支持 `"message": "bail"` 字符串简写——多一种等价写法会增加校验与文档复杂度，与 `runtimes` / `rows` 的统一对象风格不一致。
- `rutis-loader` 的 `Declared` 可在步骤 3 增加可选 `events: BTreeMap<String, Mode>`（默认空）。未声明的插件（含所有既有插件与 Cordis 插件）和动态事件名在 `check` 中静默跳过，零误报；声明了的插件若与 `rutis.json` 目录或另一行声明冲突，`check` 报告冲突（`error` 策略或 `--strict` 时以退出码 1 失败）。

**D9 同步 waterfall 归为 `Mode::Waterfall`。** Rust 的 `ctx.waterfall(e).sync(...)` 与 Cordis 同步 `ctx.waterfall(...)` 在总线上都使用 `Mode::Waterfall`，均满足目录里的 `waterfall` 期望。

## 三、规则

### 3.1 数据模型与目录校验（`crates/rutis`）

- `EventModePolicy`：`Warn`（默认）、`Error`；serde 为 `kebab-case`（`"warn"` / `"error"`）。
- `EventModeEntry { mode: Mode, policy: Option<EventModePolicy> }`。
- `EventModeCatalog`：保存 `default_policy: EventModePolicy`（默认 `Warn`）与 `entries: HashMap<Arc<str>, EventModeEntry>`。
  - `EventModeCatalog::new()`：空目录，默认策略 `Warn`。
  - `.with_policy(policy)`：设置目录默认策略。
  - `.expect(name, mode)`：登记精确事件名，沿用目录默认策略。
  - `.expect_with(name, mode, policy)`：登记精确事件名并覆盖单事件策略。
  - `.get(name) -> Option<(Mode, EventModePolicy)>`：精确查找并返回生效的 `(期望方式, 生效策略)`。
- `CtxHost::set_event_mode_catalog(&self, catalog: EventModeCatalog) -> Result<(), Error>`：
  1. 按字典序校验全部键：
     - `""` → `"events: event name cannot be empty"`
     - 以 `"rutis/"` 开头 → `"events.<name>: rutis/* events are owned by the host and cannot be configured here"`
     - 以 `"internal/"` 开头 → `"events.<name>: internal/* events stay inside their runtime and cannot be configured here"`
     任一项不合法即返回 `Err(ErrorKind::Validation { issues })`，**不改动当前目录与去重状态**。
  2. 校验通过后替换目录、更新 `has_catalog` 原子标志（`!catalog.is_empty()`），并清空 `Warn` 去重记录表。传入空的 `EventModeCatalog::new()` 即关闭检查。

### 3.2 运行时总线检查顺序（`EventBus`）

对每一次非框架分发（`emit_erased`、`parallel_erased`、`serial_erased`、`waterfall_erased`、`bail_erased`、`waterfall_sync_erased`，涵盖 `Ctx::*` 类型化调用与 `EventBus::dispatch` / `dispatch_sync` 原始调用）：

1. 既有前置检查先跑：`bail` / 同步 `waterfall` 先执行 `ctx.registration_preflight()`；随后 `ensure_bus(ctx)`；再检查 `!opts.framework && name.starts_with("rutis/")` 返回 `ReservedEvent`。
2. 若 `opts.framework` 为真或 `has_catalog` 为假，跳过目录检查。
3. 在读锁内精确查找 `catalog.get(name)` 后立刻释放读锁。未命中或 `expected == actual` 时继续正常派发。
4. 命中且 `expected != actual` 时，确定发出方显示名 `emitter`：
   - 优先使用 `Dispatch.emitter`（`rutis-bridge` 填入发出行的行名，或无行时填运行时名）；
   - 否则若 `ctx.row()` 为 `Some(row)`，取 `row`；
   - 否则若为根 fiber，取 `"host"`；
   - 否则取插件名 `ctx.plugin_name()`（不出现 `<root>`、内部 id 或内部插件名，遵守[诊断设计](design-diagnostics-2026-10-26.md) §4.5）。
5. 构造 `ErrorKind::EventModeMismatch { name, actual, expected, emitter }`，显示文字固定为：
   ```text
   {emitter}: event {name} dispatched with {actual}, expected {expected}: use {expected} instead
   ```
6. 按生效策略执行：
   - `EventModePolicy::Error`：立即返回 `Err(report.into())`，不选表、不调过滤器、不执行任何监听器。
   - `EventModePolicy::Warn`：在去重锁内记录 `(emitter, name, actual, expected)`（上限 4096 项，超出时淘汰最早一项）；释放锁后，若为首次出现则在 `catch_unwind` 内调用 `ctx.error_sink()(Arc::new(report))`；随后照常继续选表与派发。

### 3.3 跨语言行为与 `Error` 下 `emit` 的语义差异

| 情形 | Rust | JS / Node（含 Cordis）/ Bun / Python / Go / Swift / Kotlin |
| --- | --- | --- |
| 命中 `warn`（全部五种方式） | 原分发照常执行；宿主 `ErrorSink` 收到一次 `EventModeMismatch` | **完全一致**：原分发照常执行（`emit`/`parallel` 仍忽略返回值）；宿主 `ErrorSink` 收到一次 `EventModeMismatch`，发出方写行名 |
| 命中 `error`（`serial` / `bail` / `parallel` / `waterfall`） | 调用点返回 `Err(ErrorKind::EventModeMismatch)`；0 个监听器执行 | **完全一致**：调用点直接抛出 / 返回 `BindingError(code: "event-mode-mismatch", data: {event, actual, expected})`；0 个监听器执行 |
| 命中 `error`（`emit`） | `ctx.emit()` 的签名是 `Result<(), Error>`，调用方在调用点**同步**收到 `Err(ErrorKind::EventModeMismatch)`；0 个监听器执行 | **调用点语义不同**：`ctx.emit()` 是发出即不管（fire-and-forget）接口（JS / Node / Bun / Cordis 返回 `undefined`，Python 返回 `None`，Go / Swift / Kotlin 无返回值），宿主拒绝**无法在 `ctx.emit()` 调用点同步抛出**。`rutis-bridge` 在 `SessionEvents::dispatch` 进 `run_emit` 队列**之前**调用 `EventBus::check_mode` 并拒绝该请求，确保 **0 个监听器执行**，拒绝结果经运行时 / 宿主的拒发 `emit` 错误报告路径输出，而 `ctx.emit()` 本身仍立即返回 `undefined` / `None` |
| 未列出事件 / 动态事件名未命中目录 | 无检查，零警告 | **完全一致**：无检查，零警告 |
| `internal/*`（Cordis） | 不适用 | 留在 Node 进程内，不过总线，不参与目录 |

正是上述 `Error` 策略下 `emit` 的跨语言调用点差异，决定了 **`warn` 是跨语言行为完全一致的默认策略**：在 `warn` 下，全部语言、全部五种方式都保留原有调用点行为并向宿主的 `ErrorSink` 报告同一条去重诊断；只有在宿主显式为某个事件（或整份目录）选择 `error` 时，才启用“拒绝派发、0 个监听器执行”，且文档如实告知开发者 `emit` 在非 Rust 语言里不会在调用点同步抛错，而 `serial` / `bail` / `parallel` / `waterfall` 都会在调用点直接抛出 `BindingError(code: "event-mode-mismatch")`。

### 3.4 `rutis.json` 与 `rutis-host check`（步骤 3）

- `rutis.json`（`format: 1`）顶层可选字段 `"events"`：
  ```json
  {
    "format": 1,
    "events": {
      "message": { "mode": "bail" },
      "http/request": { "mode": "waterfall", "on_mismatch": "error" }
    }
  }
  ```
- 解析校验（任意失败均让 `rutis-host check` / `run` / `dev` 以退出码 1 拒绝启动）：
  - `mode` 必填，必须是 `emit | parallel | serial | waterfall | bail` 之一；写错时用 `rutis::config::closest` 给拼写建议。
  - `on_mismatch` 可选，默认 `"warn"`，必须是 `warn | error` 之一。
  - 不接受未知字段（`#[serde(deny_unknown_fields)]`）。
  - 键不能为空、不能以 `rutis/` 或 `internal/` 开头。
- `rutis-host check` 比对规则：
  - 若行所属插件通过可选的 `Declared.events` 声明了静态事件期望，而目录里同名事件的 `mode` 不同：
    - 目录条目为 `on_mismatch: "error"` 时：该行记为不能运行（退出码 1）；
    - 目录条目为 `on_mismatch: "warn"` 时：默认打印 `warning`（退出码 0），传入 `--strict` 时升级为 `error`（退出码 1）。
  - 若两行均启用且通过 `Declared.events` 对同一静态事件名声明了不同 `mode`（即使 `rutis.json` 未配置 `"events"`）：默认打印 `warning`，`--strict` 时升级为 `error`（退出码 1）。
  - 未提供 `Declared.events` 的插件（全部既有插件、全部 Cordis 插件）以及动态事件名：静默跳过，不产生任何警告或错误。
- `rutis-host run` / `dev` 在 `Host::start_with` 挂行之前安装 `EventModeCatalog`，并在 `Host::reload` 时同步更新。

### 3.5 明确不保证

- 不保证在 `check` 阶段扫描插件源码或发现从未执行过的调用点。
- 不检查 Cordis 进程内的 `internal/*` 事件或宿主内部发出的 `rutis/*` 事件。
- 不跨 WebSocket 节点自动同步事件目录：节点各自对自己发起的分发执行本节点的目录。
- `warn` 策略不改变任何分发方式的语义（`emit` / `parallel` 依然忽略监听器返回值）。

## 四、开发者看到什么

以下所有输出均逐字锁定，只出现开发者写过的名字（行名、插件名、事件名、方式名与改法），不出现任何 rutis 内部词（[诊断设计](design-diagnostics-2026-10-26.md) §4.5）。

### 4.1 插件作者

**正常时（TypeScript 为例；Python、Go、Swift、Kotlin、Rust 与 Cordis 写法不变）：**

插件代码完全不需要改动，也不需要导入新符号。黑名单插件照常监听 `"message"`，分发方用约定的方式发出：

```ts
// blacklist 插件
ctx.on('message', (msg) => msg.user === 'mallory')

// chat 插件
const blocked = ctx.bail('message', { user: 'mallory', text: 'spam' })
if (blocked) return
```

输出：无额外输出，黑名单返回 `true`，消息被拦截。

**出错时：**

默认策略是 `warn`（所有语言、全部五种方式行为一致：原分发照常执行，宿主标准错误只报告一次诊断）。若宿主对某个事件开启了 `error`，`serial` / `bail` / `parallel` / `waterfall` 在全部语言里都会在调用点直接抛出 / 返回错误；唯独 `emit` 在 Rust 与其他语言之间有接口差异——Rust 的 `ctx.emit()` 返回 `Result<(), Error>` 会在调用点同步返回 `Err`，而 JS / Node / Bun / Python / Go / Swift / Kotlin / Cordis 的 `ctx.emit()` 是 fire-and-forget（立即返回 `undefined` / `None`），不会在调用点同步抛出，而是 0 个监听器执行并经运行时 / 宿主的拒发 `emit` 错误报告路径输出：

| 触发方式 | 逐字输出（与错误码） | 记在哪一行 | 怎么改 |
| --- | --- | --- | --- |
| 目录为 `warn`（默认，跨语言一致），`chat` 误用 `ctx.emit('message', msg)` 而目录期望 `bail` | 宿主标准错误打印一次：`chat: event message dispatched with emit, expected bail: use bail instead`（无错误码；`emit` 照常执行，全部语言一致） | `chat`（发出方行） | 把 `ctx.emit('message', msg)` 改成 `ctx.bail('message', msg)` |
| 目录为 `error`，`chat` 误用 `serial` / `bail` / `parallel` / `waterfall`（例如 `await ctx.parallel('message', msg)` 而目录期望 `bail`） | 调用点直接抛 / 返回 `BindingError`，`code` 为 `"event-mode-mismatch"`，`data` 为 `{"event":"message","actual":"parallel","expected":"bail"}`，消息：`chat: event message dispatched with parallel, expected bail: use bail instead`（0 个监听器执行） | `chat`（发出方行） | 把 `ctx.parallel('message', msg)` 改成 `ctx.bail('message', msg)` |
| 目录为 `error`，Rust 插件 `chat` 误用 `ctx.emit(Message { ... })` 而目录期望 `bail` | `ctx.emit()` 在调用点**同步**返回 `Err(ErrorKind::EventModeMismatch)`，消息：`chat: event message dispatched with emit, expected bail: use bail instead`（0 个监听器执行） | `chat`（发出方插件） | 把 `ctx.emit(...)` 改成 `ctx.bail(...)` |
| 目录为 `error`，JS / Node / Bun / Python / Go / Swift / Kotlin / Cordis 插件 `chat` 误用 `ctx.emit('message', msg)` 而目录期望 `bail` | `ctx.emit()` 仍立即返回 `undefined` / `None`（不在调用点同步抛出）；**0 个监听器执行**；运行时 / 宿主的拒发 `emit` 错误报告路径输出：`chat: event message dispatched with emit, expected bail: use bail instead`（`BindingError`，`code: "event-mode-mismatch"`） | `chat`（发出方行） | 把 `ctx.emit('message', msg)` 改成 `ctx.bail('message', msg)` |

**要懂的概念：** **+0**。对照 [#663](https://github.com/arcships/rutis/issues/663) 插件作者层 7 个概念预算（插件、服务、`inject`、配置、事件、`test` 工具、`dev` 与行），仍在 **7 / 7** 预算内；遇到报错时文字里只有开发者已经写过的行名、事件名与方式名。

### 4.2 用 `rutis-host` 组装插件的人

**正常时：**

在 `rutis.json` 里按需添加 `"events"`：

```json
{
  "format": 1,
  "rows": [
    { "id": "blacklist", "name": "./blacklist.mjs" },
    { "id": "chat", "name": "./chat.mjs" }
  ],
  "events": {
    "message": { "mode": "bail" },
    "http/request": { "mode": "waterfall", "on_mismatch": "error" }
  }
}
```

运行 `rutis-host check`：

```text
blacklist (file:///.../blacklist.mjs): ok
chat (file:///.../chat.mjs): ok
$ echo $?
0
```

（未声明事件期望的 Cordis / 叶子插件，以及未列入目录的动态事件名，都不打印任何多余警告。）

**出错时：**

| 触发方式 | 逐字输出（与退出码） | 记在哪一行 | 怎么改 |
| --- | --- | --- | --- |
| `rutis.json` 中 `mode` 拼错（`"baill"`） | `rutis.json: events.message.mode: unknown mode "baill" (expected emit, parallel, serial, waterfall, or bail; did you mean "bail"?)`（退出码 `1`） | 配置文件 `events.message.mode` | 把 `"baill"` 改成 `"bail"` |
| `rutis.json` 中 `on_mismatch` 非法（`"ignore"`） | `rutis.json: events.message.on_mismatch: unknown policy "ignore" (expected warn or error)`（退出码 `1`） | 配置文件 `events.message.on_mismatch` | 改成 `"warn"` 或 `"error"` |
| `rutis.json` 中事件名为空（`""`） | `rutis.json: events: event name cannot be empty`（退出码 `1`） | 配置文件 `events` | 填写非空事件名或删掉该条目 |
| `rutis.json` 中配置了 `rutis/*` 或 `internal/*` | `rutis.json: events.rutis/row: rutis/* events are owned by the host and cannot be configured here` / `rutis.json: events.internal/dispatch: internal/* events stay inside their runtime and cannot be configured here`（退出码 `1`） | 配置文件 `events.<name>` | 删除该保留事件条目 |
| `rutis.json` 中事件规则写了未知字段（`"strict": true`） | `rutis.json: events.message: unknown field "strict", expected "mode" or "on_mismatch"`（退出码 `1`） | 配置文件 `events.message` | 改成 `"on_mismatch": "error"` |
| `rutis-host check` 发现某行自愿声明的事件方式与 `warn` 目录冲突 | `rutis-host: warning: chat (chat-plugin): event message declares emit, catalog expects bail`（退出码 `0`；加 `--strict` 时为 `rutis-host: error: ...`，退出码 `1`） | `chat` | 修正插件的分发方式或调整 `rutis.json` 中 `events.message.mode` |
| `rutis-host check` 发现某行自愿声明的事件方式与 `error` 目录冲突 | `chat (chat-plugin): cannot run: event message declares emit, catalog expects bail`（退出码 `1`） | `chat` | 修正插件的分发方式或调整 `rutis.json` 中 `events.message.mode` |
| `rutis-host run` / `dev` 运行时发生方式不匹配 | 标准错误输出：`chat: event message dispatched with emit, expected bail: use bail instead`（`warn` 下每组 `(发出方, 事件, 实际, 期望)` 打印一次；`error` 下拒绝分发） | `chat` | 修改 `chat` 中对该事件的分发调用 |

**要懂的概念：** **+1（可选）**——仅当组装者需要使用 `"events"` 检查时，了解 `events.<name>.mode` 与 `on_mismatch`；不写 `"events"` 时为 +0，不超出组装者层预算。

### 4.3 宿主持有者（嵌入 `rutis` 的 Rust 应用）

**正常时：**

```rust
use rutis::events::Mode;
use rutis::host::{CtxHost, ErrorKind, EventModeCatalog, EventModePolicy};

let catalog = EventModeCatalog::new()
    .expect("message", Mode::Bail)
    .expect_with("http/request", Mode::Waterfall, EventModePolicy::Error);
root.set_event_mode_catalog(catalog)?;
```

宿主在自定义 `ErrorSink` 或错误处理里可直接按结构匹配 `ErrorKind::EventModeMismatch { name, actual, expected, emitter }`。匹配方式的派发零开销告警、无输出。

**出错时：**

| 触发方式 | 逐字输出（与错误种类） | 记在哪里 | 怎么改 |
| --- | --- | --- | --- |
| 向 `set_event_mode_catalog` 传入空事件名、`rutis/*` 或 `internal/*` | `Err(ErrorKind::Validation { issues })`，`issues` 逐字为：`events: event name cannot be empty`、`events.rutis/row: rutis/* events are owned by the host and cannot be configured here`、`events.internal/dispatch: internal/* events stay inside their runtime and cannot be configured here`（原目录保持不变） | 宿主调用点 | 去掉非法键后重试 |
| 根上下文 `root` 用错方式派发 `"message"` | 默认 `ErrorSink` / `Err` 显示：`host: event message dispatched with emit, expected bail: use bail instead`（`ErrorKind::EventModeMismatch`） | `host` | 改为使用 `root.bail(...)` |
| 子插件 `chat` 用错方式派发 `"message"` | 默认 `ErrorSink` / `Err` 显示：`chat: event message dispatched with emit, expected bail: use bail instead`（`ErrorKind::EventModeMismatch`） | `chat` | 改为使用 `ctx.bail(...)` |

**要懂的概念：** **+1（可选，仅限 `rutis::host`）**——`EventModeCatalog`（含 `EventModePolicy`）与 `ErrorKind::EventModeMismatch`。插件侧的 `rutis::*` 根模块零新增概念。

## 五、兼容与迁移

- **`PLUGIN_API` 不变**：不升级版本号，所有已编译的 Go / Swift / Kotlin / dylib 插件无需重新构建。
- **Cordis API 与 Cordis 插件完全不变**：未改源码的 Cordis 插件经既有桥接路由到宿主 `EventBus`，开目录后默认 `warn` 永不破坏既有行为，还能立刻发现静默丢返回值的问题。
- **会话协议不变**：不新增帧类型，不升级协议版本。跨语言拒绝时复用已有的 `BindingError` 加可选错误码 `event-mode-mismatch` 与 `data`；旧端忽略未知码与 `data`，仍能读到完整人话消息。
- **`rutis.json`（`format: 1`）向后兼容增量字段**：不写 `"events"` 的现有配置文件行为 100% 不变。需要注意：旧版 `rutis-host` 二进制在 `HostConfig` 上有 `#[serde(deny_unknown_fields)]`，读取含 `"events"` 的配置时会报未知字段，需升级 `rutis-host` 后再使用 `"events"`。
- **Rust 公开 API 100% 增量**：`ErrorKind` 已是 `#[non_exhaustive]`，`CtxHost` 是 sealed trait，新增类型均位于 `rutis::host`，不破坏任何现有调用方。

## 六、实施

| 步骤 | 范围 | 完成条件 |
| --- | --- | --- |
| 1（本 PR：设计稿 + 原型 + 形式化模型） | 中英文设计稿；`crates/rutis` 实现 `EventModePolicy`、`EventModeEntry`、`EventModeCatalog`、`ErrorKind::EventModeMismatch`、`EventBus` 运行时检查与有界去重、`CtxHost::set_event_mode_catalog` / `event_mode_catalog`、`Dispatch.emitter` 字段；`specs/event-mode-catalog/` TLA+ 模型与变异配置 | `crates/rutis/tests/event_mode_catalog.rs` 逐字验证黑名单 `warn`/`error`、五种方式匹配、未列出与动态事件零误报、零监听器检出、单事件策略覆盖、非法目录校验；`specs/event-mode-catalog/run.sh` 全部配置与 `expected.txt` 一致；`cargo test -p rutis`、`cargo fmt --check` 与文档/模型检查全绿 |
| 2（`rutis-bridge` 接通） | `SessionEvents::dispatch` 填入 `Dispatch.emitter`（行名 / 运行时名）并在 `Mode::Emit` 进队列前调用 `check_mode`；`events/errors.rs` 把 `EventModeMismatch` 编码为带 `code: "event-mode-mismatch"` 与 `data` 的 `BindingError`；更新[诊断设计](design-diagnostics-2026-10-26.md) §4.3 错误码表 | 真实 Node（含 Cordis 插件）与 Python 运行时集成测试逐字验证 §4.1 全部输出（含 `warn` 保持兼容与 `error` 拒绝），跨语言黑名单场景通过 |
| 3（`rutis-host` 与 `rutis-loader`） | `HostConfig` 支持 `rutis.json` 顶层可选 `"events"` 与拼写建议；`Host::start_with` / `reload` 安装目录；`rutis-loader` 的 `Declared` 增加可选 `events` 字段并在 `Loader::check` 与 `rutis-host check` / `--strict` 中比对输出 | 验收测试逐字比对 §4.2 表格中每一条配置报错、冲突告警、`--strict` 退出码与运行期输出，确认 Cordis 与动态事件零误报 |
| 4（指南与保证登记） | 更新 [rutis-host 指南](guide/rutis-host.md)、[Rust 宿主指南](guide/rust-host.md)、[Cordis 兼容边界](guide/cordis-boundaries.md)（双语），在[保证登记表](guarantees.md) 登记事件目录保证与对应测试 | `node tools/check-md-links.mjs`、`check-guarantees.mjs`、`formal-trace.mjs --check` 全部通过 |
