# Swift SDK：macOS 插件运行时与 Apple 应用接入（设计稿）

[English](design-swift-sdk-2026-10-12.en.md)

状态：设计稿，未实现（建议开 issue 跟踪）。日期：2026-10-12；同日修订：起草时把目标从总体稿 M3 的"macOS 系统能力"扩展为"对接 Apple 生态与应用"，多出形态②③（§二、§十七）。基准：`main` `6f91ce8`。
依据：[多语言总体稿](design-multilang-runtimes-2026-10-03.md)（本文是其 M3，对其 dlopen 方案的修订见附录 A）、[M1](design-multilang-m1-2026-10-04.md)、[M2](design-multilang-m2-2026-10-04.md)、[Go 插件运行时](design-go-runtime-2026-10-10.md)（形态①大量沿用其决策与机器，逐项对照见附录 C）、[Bun 运行时](design-bun-runtime-2026-10-09.md)、[连接节点](guide/nodes.md)（形态②长路的机器）、[插件 API](guide/plugin-api.md)。

范围外：Swift 侧 Cordis；SwiftPM 之外的编译系统；宿主运行时编译 Swift 代码；Linux 与 Windows（§十三）；App 上架流程本身（审核、证书管理只给合规注意点）。

## 一、决策

| # | 决策 | 附录 |
| --- | --- | --- |
| S1 | 形态①：一个二进制一个运行时进程；宿主同时运行多个 Swift 运行时 | A |
| S2 | 形态①：一个二进制含一个或多个插件，由其 `main.swift` 的 `Rutis.serve(...)` 决定 | A |
| S3 | 形态①行名 `swift:<插件名>`，`SwiftResolver` 按清单路由；限定写法 `<运行时名>:<插件名>` | — |
| S4 | 形态①运行时名由文件名得出（前缀 `swift-`），是唯一的用户可见名 | — |
| S5 | 形态①二进制自描述：`--rutis-manifest` 输出清单，解析不需要启动进程 | — |
| S6 | 形态①进程运行期间，解析以启动清单为准；换文件在重启后生效，不自动重启 | — |
| S7 | 按需启动，空闲停止；`eager` 全部启动且不停止 | — |
| S8 | 同二进制内调用不走 IPC；跨二进制、跨语言经 rutis 按名字转发 | — |
| S9 | 每个进来的调用一个 Task：可重入，声明 `reentrant-sync`；不声明 `sync-stack`。适用于全部形态 | B |
| S10 | 调用链放 `@TaskLocal`；同步等待挂起当前 Task，绝不阻塞线程；方法默认 `async`，`.sync` 标记 | B |
| S11 | 叶子插件；插件 API 版本 1；配置 `Codable` 解码，Schema 手写在配置类型上 | C |
| S12 | 提供服务：显式闭包表（Swift 无方法反射）；同进程直调 | C |
| S13 | 值按类型系统分界：值类型按数据复制，闭包按引用；`Rutis.ref` 显式按引用传对象 | D |
| S14 | 使用服务：实现 `RutisClient` 的闭包字段结构体，从代理构造（参数包推断） | C |
| S15 | 宿主不编译；`run` 只运行现成二进制，`dev` 只重建项目自身 | — |
| S16 | 不同 SDK 版本的二进制可共存，宿主与二进制之间只约定线协议、清单格式、插件 API 版本 | — |
| S17 | SDK 位于 `swift/rutis`，分发经仓库根 `Package.swift`，版本跟主发布线的 `v` tag；核心只依赖 Foundation | E |
| S18 | 平台：macOS 13+（形态①②均可）与 iOS 17+（仅形态②）；Linux 列为后续，Windows 不做 | — |
| S19 | SDK 是跨 Apple 平台的库：核心（会话 + 叶子 API）无进程与文件系统假设；iOS 静态链入应用，同一套 API（§二） | — |
| S20 | 形态②近路：应用作为远程运行时的监听者（`listen:`，同 Go G2 语义，一次一个控制方）；宿主侧只加 `remote[].language: "swift"` | — |
| S21 | 形态②长路：应用作为 peer 拨号者（dial 到监听的节点，export/import 服务，按失败类别退避重连）；宿主侧机器已有，拨号方是 SDK 的新角色 | — |
| S22 | iOS 后台存活：v1 以前台连接 + 重连为主；APNs 唤醒、后台模式列为后续 | — |
| S23 | 应用不是节点：不监听来运行别人的插件（不开 `host: true`）；App Intents 桥（Apple 生态消费 rutis）列为后续 | — |

## 二、部署形态：一个 SDK，三种接法

目标是对接 Apple 生态与应用。macOS 上插件直接跑在宿主旁边；iOS 上插件模型根本不成立，集成单位是**应用**、方向是**应用拨出**：

| | macOS | iOS |
| --- | --- | --- |
| rutis 宿主能拉起运行时进程吗 | 能 | 不能：沙盒、无 exec、App Store 禁止下载并执行代码 |
| 能动态装载插件吗 | 能 | 不能：一切静态链进应用 |
| 集成单位 | 插件（进程组） | 应用本身 |
| 连接方向 | 宿主拉起 / 拨入 | 应用主动拨出（或前台监听） |
| 授权（TCC / entitlements） | 记在插件二进制 | 记在应用的签名与 provisioning profile |
| 后台存活性 | 进程常驻 | 应用被挂起，需重连 / APNs |

由此得到三种部署形态，共用一个 SDK 核心（会话协议、叶子 API、值规则、并发模型——§四至§六的全部内容对三种形态一致）：

```text
                    Swift SDK 核心（会话 + 叶子 API，macOS / iOS）
       ┌──────────────────┬─────────────────────┬──────────────────┐
   ① serve + 清单      ②a 监听者（listen）     ②b 拨号者（dial）     ③ App Intents（后续）
   macOS 插件二进制      应用作为远程运行时        应用作为 peer         应用把 rutis 服务
   宿主经 GoRuntimes     宿主拨入控制            应用拨出到节点        暴露成快捷指令/Siri
   那套清单机器拉起       行、门控在宿主          export 什么应用说了算
```

| 形态 | 用途 | 骑在什么机器上 | 控制权 |
| --- | --- | --- | --- |
| ① macOS 插件二进制 | Mac 系统能力（EventKit、通讯录、辅助功能、屏幕录制、Core ML）、AppleScript / AX 自动化 Mac 应用 | `SwiftResolver` / `SwiftRuntimes`（共用 Go 那套清单机器） | rutis |
| ②a 应用·监听者 | 应用在前台作为远程运行时，宿主拨入并管理其内置插件的装载 | 远程运行时机器（Go G2）：`runtimes.remote`、租约、一次一个控制方 | rutis（行、门控在宿主配置） |
| ②b 应用·拨号者 | 应用拨出到一个监听的节点，把自身能力 export 成服务；后台重连 | nodes / link 机器（`PeerPlugin`）：dial、token、export/import、退避重连 | 应用（export 什么应用决定，宿主 import） |
| ③ App Intents 桥 | Apple 生态消费 rutis：应用把 rutis 服务定义为 App Intents，快捷指令 / Siri / Spotlight 驱动 | App Intents 框架 | 用户 |

②a 与 ②b 不是新旧关系，是两种治理模型：设备是宿主方的资产（公司配的 iPhone，宿主配置决定装什么）走 ②a；应用是用户装的（应用自主决定暴露什么）走 ②b。同一个 SDK，两边都支持。

## 三、形态①：macOS 插件运行时

### 3.1 运行时模型

```text
rutis 宿主
 ├─ LocalRuntime::node / ::python ...
 ├─ SwiftRuntimes（SwiftResolver + 生命周期）
 │    ├─ plugins/swift/weather ── swift-weather ── weather
 │    ├─ plugins/swift/kits   ── swift-kits    ── calendar、contacts
 │    └─ plugins/swift/ml     ── 未启动（无行使用）
 └─ LoaderPlugin + 每个运行中的运行时一个 RuntimeRowsPlugin
```

- 与 Go 运行时完全相同：行依赖 `RuntimeRows#<运行时名>` 与插件 `inject` 的全部名字（叶子运行时，rutis 门控全部名字）；插件 `provides` 的服务从行的 fiber 发布到 `host_key(name)`；进程退出时其全部行停止，使用其服务的行随之停止。
- `isolate`、实例标签、服务 id、导出句柄：与 Python / Go 运行时相同。
- TCC 记在插件所在的二进制上（§八）。

### 3.2 二进制与清单

- 插件作者分发：Swift 包（导出插件定义）+ `cmd/<名字>/main.swift`（`serve` 自己的插件），或一个包里若干插件编成一个二进制。
- 宿主接受任何响应 `--rutis-manifest` 的可执行文件（来源见附录 A.5）。

```swift
// example.com/kits/cmd/kits/main.swift
import Rutis
import WeatherKits

Rutis.serve(WeatherKits.calendar, WeatherKits.contacts)
```

| `serve` 规则 | |
| --- | --- |
| 参数 | `--rutis-manifest`：打印清单后退出；否则 `<channel> [--id <endpoint>] [--peer <endpoint>] <project>` |
| 插件同名 | 启动即退出，指明两个定义 |
| 登记方式 | 只在 `serve` 的参数里，不用静态初始化的注册表 |
| 子命令 | `Rutis.serveArgs(args, defs...)`；另有 `Rutis.serveConn(channel, defs...)`（内存会话，测试用） |

```json
{
  "manifest": 1,
  "sdk": "0.10.0",
  "pluginApi": 1,
  "runtime": "rutis-swift-runtime:1",
  "platform": "macos/arm64",
  "plugins": {
    "calendar": {
      "config": { "type": "object", "properties": { "calendars": { "type": "array", "items": { "type": "string" } } } },
      "inject": [],
      "provides": { "calendar": { "events": "async", "units": "sync" } },
      "version": null
    }
  }
}
```

| 项 | 规则 |
| --- | --- |
| 输出 | stdout，退出码 0；不连通道，不执行 `apply` |
| 插件条目 | 与 `rows.schema` 回复相同，同一段代码生成 |
| `runtime` | SDK 标记串 `rutis-swift-runtime:1`：目录扫描只执行含它的文件 |
| `platform` | `macos/<arch>`；宿主核对不符报"不是为 <os>/<arch> 构建的" |
| `sdk` | 常量 `Rutis.sdkVersion` |
| 插件 `version` | v1 为作者静态串，缺省 `null`；git 生成版本见 §十五（S3） |
| 执行超时 | 30 秒（同 Go：macOS 上新二进制第一次运行要等系统评估） |
| 缓存键 / 绕过 / 信任 | 与 Go 相同：路径、大小、mtime（Unix 加 ctime）；启动、`restart`、`check` 绕过；只执行配置给出的来源 |

### 3.3 `SwiftResolver`

位于 rutis-loader（feature `swift`），实现 `Resolver`。与 `GoResolver` 同构：

| 项 | 规则 |
| --- | --- |
| 来源 | `SwiftBinaries`：文件列表 + 目录列表；遇到未知插件名时重扫一次目录 |
| 目录中的候选文件 | 可执行、不以 `.` 开头、内容含 SDK 标记串；只执行候选文件 |
| 取清单失败 | `binaries` 中为配置错误；目录中跳过 + 诊断；`check` 列出并以非零退出 |
| 运行时名 | 去扩展名 → 小写 → 非 `[a-z0-9-]` 换 `-` → 加前缀 `swift-`（`Net.Kits_v1` → `swift-net-kits-v1`） |
| `swift:<插件>` | 查所有清单；无：`NotFound`；多个：歧义错误，提示 `<运行时名>:<插件>` |
| `<运行时名>:<插件>` | 只查该运行时的清单 |
| 重名 | 与其他运行时（`node`、`py`、`bun`、`go`、远程）或彼此重名：`binaries` 中为配置错误；目录中跳过 + 诊断 |
| 解析结果 | 叶子结果，与 `GoResolver` 共用"由声明构造 `Resolved`"的代码 |
| meta | `{ source: "swift", binary, runtime, version, sdk }` |
| 运行中换文件 | 以启动清单解析；新二进制才有的插件报"已更换，`restart("<运行时名>")` 后生效" |
| 第二段放行 | `RuntimeRowsPlugin` 已接受 trait（Go G1 改造），`SwiftResolver` 按运行时实现同一 trait |

与 `GoResolver` 的差异只有三处常量（标记串、前缀、feature 名），实现走共用代码（§九）。

### 3.4 `SwiftRuntimes`

与 `GoRuntimes` 同构，位于 rutis-loader，挂在 loader 之后。

```rust
let swift = Arc::new(SwiftResolver::new(SwiftBinaries::new().dir("plugins/swift").file("bin/weather")).with_catalog(&catalog));
chain = chain.with_shared(swift.clone());
root.plugin(SwiftRuntimes::new(swift, project).idle(Duration::from_secs(60)));
```

| 项 | 规则 |
| --- | --- |
| 结构 | 每个运行中的运行时 = 一个子 `Ctx`：`LocalRuntime::swift(binary, project).named(名)` + `RuntimeRowsPlugin`；停止 = 释放子 `Ctx` |
| 启动 | 解析出一行且其运行时未运行时启动（另有周期检查补足）；多个冷启动并行 |
| 空闲停止 | 最后一行卸载后等 `idle`，无未禁用条目解析到它则停止；同一时刻启动 / 重启 / 停止只一个在进行 |
| 崩溃 | 行停止，记录原因，不自动重启；重启条件：`SwiftRuntimes::restart(名)`，或二进制文件已变化 |
| `eager()` / `runtimes()` | 与 Go 相同 |
| 单个固定二进制 | 可不用 resolver：`LocalRuntime::swift` + `RuntimeResolver::modules` |

## 四、Swift SDK（共享核心）

位于 `swift/rutis`（SwiftPM 目标 `Rutis`），Swift 5 语言模式，工具链 5.9 及以上（参数包需要 5.9），部署目标 macOS 13 / iOS 17。分层：

| 层 | 内容 | 平台 |
| --- | --- | --- |
| 核心 | 会话协议（compat / endpoint）、叶子 API、值、并发、`RutisTest` | macOS + iOS（只依赖 Foundation；不触碰进程与文件系统之外的系统设施） |
| 通道·进程 | `fd:3`、`unix:`、`tcp:`（POSIX） | macOS（形态①） |
| 通道·监听 | `listen:` 的 WebSocket 服务端（Network.framework `NWListener` + `NWProtocolWebSocket`） | macOS + iOS（形态①远程、形态②a） |
| 通道·拨号 | `dial:` 的 WebSocket 客户端（Foundation `URLSessionWebSocketTask`） | macOS + iOS（形态②b） |
| serve 打包 | `Rutis.serve` / `serveArgs` / 清单 CLI | macOS（形态①；iOS 上同一逻辑经 `RutisApp` 驱动，§七） |

### 4.1 插件

```swift
import Rutis

struct Config: RutisConfig {          // 配置类型：Schema 与解码都在类型上
    static let schema = Schema.object(
        properties: ["city": .string(description: "要查天气的城市")],
        required: ["city"])
    var city: String                   // Codable 合成
}

struct LLM: RutisClient {             // 使用服务：闭包字段结构体
    var ask: (RutisCtx, String) async throws -> String
    var listModels: ((RutisCtx) async throws -> [String])?   // 只声明用到的方法
    init(_ s: RutisService) {         // 从代理构造；参数包按字段类型推断
        ask = s.method("ask")
        listModels = s.method("listModels")
    }
}

final class Weather {
    var llm: LLM; var city: String
    func today(_ ctx: RutisCtx, _ question: String) async throws -> String { ... }
}

let weather = Rutis.define(Plugin(
    name: "weather",
    inject: ["llm"],
    provides: ["weather": Shape(.async("today"), .sync("unit"))],   // 声明形状（进清单）
    config: Config.self,
    apply: { ctx, config in                                          // 实现（装载时）
        let llm = ctx.use("llm", LLM.self)
        let weather = Weather(llm: llm, city: config.city)
        ctx.provide("weather", [
            .async("today") { try await weather.today($0, $1) },
            .sync("unit") { _ in "celsius" },
        ])
        ctx.effect { print("weather unloaded") }
    }))
```

| API | 规则 |
| --- | --- |
| `Rutis.define` | 校验声明（名称、闭包表与形状一致），返回可 serve 的定义 |
| `apply` 错误 | 装载失败；已登记的清理照常运行 |
| `ctx.use(name, C.self)` | `C: RutisClient`；`name` 须在 `inject` 中；同进程提供者直接给本地实现 |
| `ctx.provide(name, methods)` | 闭包表须与 `provides` 声明的形状一致；至卸载或返回的撤销函数为止 |
| `ctx.effect(cleanup)` | `cleanup` 可 `async`，卸载时逆序执行 |
| `ctx.spawn { ... }` | 子 Task 中运行，捕获错误写 stderr；卸载时取消并等待 |
| `RutisCtx` | 行作用域：取消（卸载时取消行 Task 树）、`project`、调用链（§五） |
| `Rutis.NoConfig` | 无配置 |

### 4.2 提供服务：闭包表

Swift 没有方法反射（Mirror 不能枚举类型的方法，也不能合成任意签名的方法），所以不像 Go 用 `MethodsOf` 反射、Python 用 `inspect`，而是**显式闭包表**：

- 声明（define 时）：`Shape(.async("today"), .sync("unit"))` → 清单与 `rows.schema`。
- 实现（apply 时）：`.async("today") { (ctx, city) in ... }`，闭包签名任意（参数按位置解码进闭包参数类型，返回值按 §4.5 编码）；SDK 校验表与形状一致。
- 同进程调用：直接调用闭包，不编码、不跨进程（S8）。
- 后续可选宏（§十五 S3）：`@RutisService` 从类型生成形状与闭包表，恢复 Go 的 `MethodsOf` 人体工学。

### 4.3 使用服务：`RutisClient`

```swift
protocol RutisClient { init(_ service: RutisService) }
```

- 结构体的闭包字段即要用的方法：字段名即方法名（Swift 已是 lowerCamelCase，无需 Go 的首字母缩写转换）；只写用到的方法，每个字段须对应服务声明的方法，否则构造时把错误抛给 `use`。
- `s.method("ask")` 是参数包泛型：`func method<each A, R>(_ name: String) -> (RutisCtx, repeat each A) async throws -> R`，从字段类型推断闭包签名（本机 Swift 6.3 实测可行，PoC 见附录 E.3）。
- 跨进程代理一律 `async`（S10）：没有阻塞式的 `callSync`。

### 4.4 配置

| 项 | 规则 |
| --- | --- |
| 解码 | `JSONDecoder`，`Config: Codable`；失败即装载失败 |
| Schema | `RutisConfig.schema` 手写（Swift 反射产不了 Schema，与 Python 的手写 `Config` 一致；宏见 S3） |
| 值映射 | JSON Schema 语义与 Go §6.3 相同 |

### 4.5 值

| Swift 值 | 跨进程 |
| --- | --- |
| `nil`、`Bool`、`Int`（±2^53−1 内）、`Double`、`String` | 复制；整数超限报错 |
| `Optional`、`Array`、`Dictionary<String, T>`、`Codable` 的 struct / enum | 递归复制，按 `Codable` 编码 |
| 闭包（函数引用） | 引用；参数与返回值按本表编码 |
| `Rutis.ref(v)` | 引用；`v: AnyObject & RutisExportable`（自带闭包表，见 §4.2），远端得到调用其方法的代理 |
| class 实例 | 不显式 `ref` 不可跨进程：报错并提示改用 struct 或 `Rutis.ref` |
| `Error` | 复制名字与消息；名字 = 错误类型名（`String(reflecting:)`），可经 `RutisErrorName` 覆盖 |
| `RutisFuture` | 异步结果；`await fut.value()` |
| `Data`（字节）、非字符串键字典、循环引用、非 `Codable` | 不可跨进程（同 Go 的 `[]byte` 等） |

- 分界来自类型系统本身（值类型 = 数据，引用类型须显式 `ref`），不需要 Go 的 `rutis.Ref` 那样区分"结构体是数据还是对象"（附录 D）。
- 接收：函数类型 → 调用远端函数的闭包（参数包构造）；其余按 `Codable` 解码进声明类型。不接收对象引用（S3，同 Go G3）。

## 五、并发、调用链、取消

适用于全部形态。

| 项 | 规则 |
| --- | --- |
| 会话结构 | 一个读线程（阻塞读通道，帧经 `AsyncStream` 进 async 世界；阻塞读绝不落在协作线程池上）；写帧一个 actor 串行；每个进来的 `call` / `invoke` / `get` / `await` / 控制操作一个新 Task；行状态各自加锁 |
| 可重入 | 任何时候执行任何进来的调用；声明 `reentrant-sync`。不声明 `sync-stack`：调用并发跑在不同 Task 上，无单栈顺序（与 Go 同类，见总体稿 §九） |
| 插件要求 | 服务闭包并发安全；调用其他服务时不持锁（写进指南，同 Go） |
| 调用链 | `@TaskLocal var path: [String]`：进来的调用在其 Task 里设 `path + [id]`；发出的调用从当前 TaskLocal 取链；`apply` / 清理带 `rows.load` / `rows.unload` 的链 |
| 同步等待 | `sync: true` 帧发出后，当前 Task 挂起在 continuation 上等回复帧——**不阻塞任何线程**。等待期间其他 Task 照常执行进来的调用 |
| `sync` 方法 | 闭包表 `.sync` 的闭包内不得发出跨进程调用（SDK 代理全部 async，编译期就调不了）；这是与 Go 的唯一行为差异，写进指南 |
| 进来的取消 | `cancel` 帧 → 该调用的 `Task.cancel()`；`Task.checkCancellation()` 抛 `CancellationError` |
| 发出的取消 | `async`：发 `cancel`，立即抛 `CancellationError`，丢弃迟到结果；`sync`：立即抛，丢弃迟到结果，对端照常执行 |
| 行卸载 | 取消该行 Task 树；撤销服务后等待进行中的调用（时限由 rutis 侧决定） |
| `SyncWaitCycle` | 自身不产生；收到时为 `Rutis.RemoteError`，`isSyncWaitCycle(err)` |

总体稿 §九 待定项由本文回答：**Swift 确认可重入**。至此 Python、Bun、Go、Swift 一致，"叶子运行时都可重入、只有 Cordis 不可重入"定为规则（随实现 PR 更新总体稿 §九 的进展段）。

## 六、运行时进程：通道与契约

### 6.1 通道

| channel | 形态 | 实现 |
| --- | --- | --- |
| `fd:3` | ① | Unix 本地继承：`dup`/`FileHandle` + POSIX read/write |
| `unix:<path>` | ① | 拨回：POSIX socket |
| `tcp:127.0.0.1:<port>` | ①（预留） | 先发 `RUTIS_CHANNEL_TOKEN` + 换行，随即从环境删除 |
| `listen:ws://…` / `listen:wss://…` | ①远程（S3）、②a | §七.1 |
| `dial:`（SDK 侧 API，不是 channel 字符串） | ②b | §七.2 |
| `ws://…` 直连 | — | ② 报错：应用只监听或只拨到节点 |

- 按行分帧；单条消息长度上限同 [#173](https://github.com/arcships/rutis/issues/173)；`project` 参数经 `Rutis.project` 提供（②中为应用的约定目录）。

### 6.2 契约（形态①、②a）

compat 协议版本 2；特性 `["rows.v2", "hosts", "leaf", "scopes"]`；能力 `signals`、`reentrant-sync`；会话内调用号 `node:` 前缀（与其他叶子运行时相同）。

| 控制操作 | 行为 |
| --- | --- |
| `mount` | `{ services: {}, features, implementation: { name: "github.com/arcships/rutis", version: Rutis.sdkVersion }, engine: { name: "swift", version: <构建时嵌入的工具链版本> } }` |
| `rows.schema(entry)` / `rows.load` / `rows.update` / `rows.unload` | 与 Go §8.2 相同；`rows.load` 忽略 `inject`（叶子运行时门控由 rutis 按 `inject` 声明做） |
| `hosts.provide` / `hosts.withdraw`、`release` / `get`、`dispose` | 与 Go 相同 |

### 6.3 退出与清理

| 情况 | 规则 |
| --- | --- |
| `apply`、方法、清理抛出的错误 | 捕获为错误回复 |
| `fatalError` / `try!` / 强解包崩溃、`ctx.spawn` 里未捕获的错误 | Swift 不可捕获 → 形态①进程退出、行全部停止（与 Go 对齐）；形态②是**应用崩溃**：连接断开，宿主侧行停止 / 服务撤销，同一恢复路径 |
| 会话已结束 | 卸载全部行，清理合计至多 5 秒；超时放弃余下清理，stderr 记录涉及的行；形态① `exit(0)`，形态② 结束 serve 循环（应用继续活着） |
| `rows.unload`、`dispose` 时限 | 不设时限（同 Go / Python），时限由 rutis 侧决定 |

## 七、形态②：应用接入

### 7.1 近路：应用作为监听者（远程运行时语义）

应用把编译进自身的插件定义经由 `listen:` 交给宿主管理——语义与 Go G2 的远程运行时完全一致（一次一个控制方；新连接接管，旧租约清理完再问候新会话；`RUTIS_TOKEN` / `RUTIS_CERT` / `RUTIS_KEY` 凭据，token 恒定时间比较；消息 16 MiB 上限；30 秒心跳，`RUTIS_HEARTBEAT` 可调）。宿主把它当一台远程机器上的 Swift 运行时：

```swift
// 应用里：前台启动一个远程运行时
try await RutisApp.serve(
    plugins: [health, location],          // 应用内置的插件定义
    listen: "wss://0.0.0.0:7443/rutis",
    id: "my-iphone", peer: "mac-main")    // 凭据经环境（RUTIS_TOKEN 等）
```

```json
// 宿主（Mac）：与远程 Go 运行时同一套配置
{
  "runtimes": { "remote": [{ "name": "iphone", "language": "swift" }] },
  "rows": [
    { "id": "iphone", "name": "rutis-bridge/peer", "config": { "peer": "iphone", "dial": "wss://192.168.1.20:7443/rutis", "runtime": "iphone" } },
    { "id": "health", "name": "iphone:health", "config": { "days": 7 } }
  ]
}
```

| 项 | 规则 |
| --- | --- |
| 行名 | `iphone:<插件名>`：插件在应用里按名解析（`rows.schema` 声明），无本地清单 |
| 插件装载 | 宿主的行决定装载应用里的哪些插件；行卸载即插件停——`apply` / 清理照常执行 |
| 授权 | 发生在 `apply`（或方法调用）时，TCC / HealthKit 授权提示指向**应用**（§八） |
| TLS | 必须：远程运行时无 TLS 只允许回环，而应用的宿主在另一台机器；自签证书 + 宿主 `RUTIS_CA` |
| 局域网 | iOS 14+ 的本地网络权限（`NSLocalNetworkUsageDescription`）按连接方向触发，写进应用清单 |
| 前台限制 | 应用进后台连接即断（挂起），前台恢复后重等宿主拨入；后台存活见 S22 |
| 实现 | Network.framework `NWListener` + `NWProtocolWebSocket`（子协议、TLS、ping 均支持），语义对齐 Go G2 §九，组合细节 S1/A1 期间 PoC（§十六） |

### 7.2 长路：应用作为拨号者（peer 语义）

应用拨出到一个**监听的 rutis 节点**（节点机器已有：`listen` 配置、token 核对、`PeerPlugin`），把自己的能力 export 成服务，也可以 import 节点上的服务。宿主侧**零改动**；拨号方目前只有 Rust 宿主实现，这是 SDK 的新角色：

```swift
let link = RutisLink(
    dial: "wss://office.example.com/rutis",
    token: ProcessInfo.processInfo.environment["RUTIS_TOKEN"]!)

try await link.connect()                          // 问候、鉴权；断线按失败类别退避重连

link.export("health", HealthService())            // 应用决定暴露什么；服务在连接上发布
let llm = try await link.imported("llm", LLM.self) // 可选：用节点上的服务（同一套 RutisClient）
```

| 项 | 规则 |
| --- | --- |
| 协议 | endpoint 协议（子协议 `rutis.3`）+ link 语义：与 `PeerPlugin` 同一套；`export` / `import`（events 为后续项） |
| 重连 | 按失败类别退避：可重试失败从 0.5 秒起翻倍至最长 30 秒（会话稳定后重置）；凭据被拒每 30 秒重试；协议不兼容停止并报告 |
| 服务可见性 | 连接断开 → 应用 export 的服务从节点撤销，依赖它们的行停下；重连后重新发布，调用链跨节点保持 |
| 调用安全 | 跨节点同步调用、回调里再调回去，由节点机器与等待环检测（#228）覆盖，SDK 侧只要正确带链（§五） |
| 一次几个连接 | 一个 `RutisLink` 一条连接；应用可同时拨多个节点 |
| 证书 | 自签 CA 经应用侧信任配置（`RUTIS_CA` 语义带到 SDK API） |

### 7.3 iOS 特有事项

| 项 | 规则 |
| --- | --- |
| 打包 | SDK 静态链入应用（SPM 库依赖）；无动态装载，插件定义编译在应用里，App Store 合规无"下载执行代码"问题 |
| 授权 | entitlements（HealthKit 等）与 usage descriptions 在应用的 Info.plist / provisioning profile；提示指向应用（§八） |
| 后台 | v1：前台连接 + 重连（S22）；后续：APNs 唤醒（节点侧配合）或后台模式，列为待定 |
| 生命周期 | `RutisApp.serve` / `RutisLink` 是可停止的（`stop()`）：应用切后台时可主动收，前台再起，比等系统掐断干净 |
| App Store 审核 | 提供的能力须与应用用途匹配（如健康数据工具读 HealthKit）；SDK 本身是普通库依赖，无审核负担 |

### 7.4 形态③：App Intents 桥（后续项）

应用把 rutis 服务（本地的或经 `RutisLink` import 的）定义为 App Intents，快捷指令 / Siri / Spotlight 即可驱动 rutis 侧能力——Apple 生态消费 rutis 的另一半。`AppIntent.perform()` 调 rutis 服务是普通 async 调用，无新协议；范围（哪些服务暴露、参数映射、Spotlight 索引）有需求时再设计（S3）。

## 八、TCC 与签名

授权记在"谁调用系统 API"上：形态①是插件二进制，形态②是应用。

### 8.1 形态①（macOS 插件二进制）

| 项 | 规则 |
| --- | --- |
| 授权归属 | 运行时二进制自身；提示里的名字来自其签名与内嵌 Info.plist |
| 用途说明 | 裸可执行文件把 Info.plist 链进 `__TEXT,__info_plist`（linker `-sectcreate`，SwiftPM `linkerSettings`）；SDK 文档给出片段，`new --lang swift` 模板带好 |
| 本地开发 | ad-hoc 签名即可（Apple Silicon 上 `swift build` 产物默认 ad-hoc）；提示照常、归属正确 |
| 分发 | Developer ID 签名 + 公证 + hardened runtime；entitlements 随签名 |
| 库验证 | 不 dlopen 任何东西，无 library validation 问题（附录 A.4） |
| 隔离属性 | 复用 Go 的 `xattr` 检查与提示 |
| 多插件同二进制 | 共享一个 TCC 身份；需要不同粒度分属不同二进制 |

### 8.2 形态②（应用）

| 项 | 规则 |
| --- | --- |
| 授权归属 | 应用；entitlements 在 provisioning profile，usage descriptions 在应用 Info.plist |
| 提示时机 | 插件 `apply` 或方法调用触及受保护能力时；拒绝授权 → 错误照常跨连接传回宿主调用方 |
| 无额外签名负担 | 应用本来就签名；SDK 是静态库 |

验收（M3 原文 + 本文扩展）：EventKit 插件在签名二进制里运行，授权提示指向该二进制（形态①）；iOS 应用里 HealthKit 插件的授权提示指向应用，拒绝后错误干净传回（形态②）。

## 九、Rust 侧改动

Go G1/G2 已把"清单式二进制运行时"的机器建好；形态①以**共用**为主，形态②几乎零改动：

| 改动 | 位置 |
| --- | --- |
| 把 `go.rs` 中与语言无关的部分抽成共用模块：清单执行与缓存、目录扫描与标记串嗅探、运行时名推导、按需生命周期与 restart、文件替换语义 | `rutis-loader/src/runtime/binary.rs`（新）；`go.rs` 改为薄封装，行为不变，现有测试保护 |
| `SwiftResolver`、`SwiftBinaries`、`SwiftRuntimes` | `rutis-loader/src/runtime/swift.rs`（新，薄封装） |
| `swift_runtime_name`；`Launcher::swift`（与 `Launcher::go` 同一条 spawn 路径）；`LocalRuntime::swift` | `rutis-bridge/src/runtime/{local,process}.rs` |
| feature：rutis-bridge `swift = []`；rutis-loader `swift = ["runtimes", "rutis-bridge/swift"]`；rutis-host 依赖开 `swift` | 三个 `Cargo.toml` |
| 远程 npm 命名拒绝 `swift:` 与 `<swift 运行时名>:` 前缀 | 已实现（拒绝任何 `<名字>:` 前缀），仅加测试 |
| **形态②a**：`remote[].language` 加 `"swift"` | `rutis-host` / loader 配置 |
| **形态②b**：无（节点 listen、`PeerPlugin`、token、重连、服务发布已实现；等待环检测 #228 覆盖跨节点同步） | — |
| `scripts/train.mjs` 版本正则加 `swift/rutis/Sources/Rutis/Version.swift` | `scripts/train.mjs` |

若 `go.rs` 抽取耦合过深，降级方案：`swift.rs` 先照抄、后续再统一——设计取向是抽取，出入随实现记录在本文。

## 十、rutis-host

```json
{
  "runtimes": { "swift": { "dir": "plugins/swift", "binaries": ["bin/weather"], "start": "on-demand", "idle": 60 } },
  "rows": [{ "id": "weather", "name": "swift:weather", "config": { "city": "Oslo" } }]
}
```

字段与 `runtimes.go` 相同（`dir` / `binaries` 至少一个；`start`：`on-demand`（默认）/ `eager`；`idle` 秒；另有 `project`）。形态②a 的配置见 §7.1（`runtimes.remote`）。

| 命令 | 行为 | 阶段 |
| --- | --- | --- |
| `check` | 每个二进制：运行时名、平台、SDK / 插件 API 版本、插件；歧义；每行的目标二进制；失败的候选文件；隔离属性 | S2 |
| `dev` | 无 `rutis.dev.json` 时据 `Package.swift` 判定 Swift 项目；重建与重启见 §十一 | S2 |
| `new <name> --lang swift` | 插件 target、`cmd/<name>/main.swift`、含 usage description 的 `Info.plist` 与 `linkerSettings`、测试、`rutis.dev.json`、发布工作流 | S2 |
| `swift add <URL>` | 下载预编译二进制到 `dir`，校验 SHA-256 | S3 |
| `run` | 不需要 Swift 工具链 | — |

## 十一、换代码（仅形态①）

与 Go §十二 相同：`dev` 监视 `.swift`、`Package.swift`、`Package.resolved` → `swift build -c release` 到缓存目录新文件 `<name>-<序号>` → 成功替换该运行时的文件并重启它；失败打印错误，旧进程继续。部署：改名替换 → `SwiftRuntimes::restart("<运行时名>")`。形态②换代码 = 发新版本应用（App Store / TestFlight）。

## 十二、仓库与发布

| 项 | 规则 |
| --- | --- |
| 位置 | `swift/rutis`：目标 `Rutis`（核心 + API）、`RutisTest`；会话层 `Sources/Rutis/Internal/Peer`；conformance 可执行目标 `RutisConformance` |
| 清单 | **仓库根** `Package.swift`，包名 `rutis-swift`，目标用 `path:` 指向 `swift/rutis/Sources/...`；平台 `macOS(.v13)`、`.iOS(.v17)`。SwiftPM git 依赖只认仓库根（实测见附录 E.2） |
| 版本 | `Rutis.sdkVersion` 跟主发布线：现有 `v0.9.x` tag 正是 SwiftPM 认的 `vx.y.z` 形式，无需新 tag 命名空间；`from:` 从首个含根 `Package.swift` 的 release 起 |
| 依赖方写法 | 插件作者与应用同一条：`.package(url: "https://github.com/arcships/rutis", from: "0.10.0")` + `.product(name: "Rutis", package: "rutis")`；应用在 Xcode 里直接添加包 |
| 依赖 | 核心仅 Foundation；网络通道用 Network.framework / URLSession（Apple 平台）；不引入 uniffi / swift-bridge（附录 B） |
| 兼容 | 同一协议版本内，任意 SDK 版本的二进制 / 应用可在任意宿主版本运行，前提是插件 API 不高于宿主支持 |
| 语言模式 | Swift 5 模式起步，API 按 Sendable 友好设计；工具链策略同 Go："当前两个稳定版"（今日为 6.3、6.4） |
| 文档 | `docs/guide/swift-plugin.md`（中英）、`docs/guide/swift-app.md`（形态②，中英）；`docs/guide/plugin-api.md` 加 Swift 列；README / CONTRIBUTING 加工具链说明 |

## 十三、平台

| 平台 | 形态① | 形态② | 说明 |
| --- | --- | --- | --- |
| macOS 13+ | ✅ | ✅（含 Mac 应用） | Swift 运行时随系统；CLI 二进制自足 |
| iOS 17+ | — | ✅ | 集成单位是应用；无进程 spawn、无动态装载（§二） |
| watchOS / visionOS / iPadOS | — | 后续 | 同 iOS 模型，有需求时接入 |
| Linux | 后续 | 后续 | SE-0342 静态 runtime、swift-foundation 跨平台；Network.framework 通道需换实现（swift-nio 或 POSIX）；有真实需求再接 |
| Windows | 不做 | 不做 | `tcp:` 回环与平台核对机制已在，仅此 |

## 十四、测试

| 测试 | 位置 | 内容 |
| --- | --- | --- |
| 会话层单元 | `swift/rutis/Tests` | 编解码、引用计数、同步中回调、取消、TaskLocal 取链、并发、挂起等待不阻塞 |
| iOS 核心单测 | 同上，CI 加 `xcodebuild test -destination 'platform=iOS Simulator'` | 核心层在 iOS 上编译并测试（值、叶子 API、内存会话） |
| 会话一致性 | `rutis-bridge/tests/runtime_conformance.rs` + `RutisConformance` 目标 | `session::testing::session` |
| 契约 × 通道 | `session_matrix.rs` 加 `Swift` | `fd:3`、拨回 |
| Swift 专项 | `rutis-bridge/tests/swift_runtime.rs` | 取消到 Task；错误形状；`fatalError` 退出撤回服务；引用与 release；夹具 `conformance-session/weather/greeter`，`crash()` 退出码 17 |
| 清单 / 多二进制 / 按需 | `rutis-loader/tests/swift_rows.rs` | 与 Go 的 `go_rows.rs` 同构全套 |
| 运行时一致性 | `multilang.rs`（feature `swift`） | Swift 版叶子插件；多语言冷启动互调；提供者移除只停使用者；`inject` 门控 |
| 交叉同步调用 | `multilang.rs` | Node→Swift→回调 Node（带链）、Swift↔Go、Swift↔Python：不卡死；`SyncWaitCycle` 不误报 |
| 实例 | `instance_runtimes.rs` | 标签隔离、实例自有服务 |
| 远程（②a） | `remote_rows.rs`、`leases.rs` 加 Swift 列 | 监听者被拨入控制；租约接管；断开后行停、回来重载 |
| 拨号者（②b） | `swift/rutis/Tests` + `rutis-loader/tests/swift_link.rs` | `RutisLink`：问候、token 被拒退避、断线重连后服务重发布、export/import、调用链跨节点 |
| TCC / HealthKit（e2e） | e2e 套件新增 | 形态① EventKit；形态② iOS 应用 HealthKit：授权提示归属、拒绝后错误传回。CI 无法点授权：交互部分人工验收清单，其余自动化 |
| 夹具 | `crates/rutis-loader/tests/fixtures/swift` | 测试开始时构建成至少两个二进制 |
| CI | `runtimes-swift`（macOS runner，Xcode 自带；两个稳定工具链；含 iOS Simulator 单测）；`test`、`network-macos` 加 Swift 夹具构建 | `swift test` + `xcodebuild test`；`cargo test -p rutis-bridge --features swift,…`；`-p rutis-loader --features swift,…`；`-p rutis-host`；`cargo check --no-default-features --features swift` |

- E2E 里程碑：`new --lang swift` 循环（S2）、跨语言与崩溃恢复（S3）、无工具链运行下载的二进制（S9）各加 Swift。

## 十五、分阶段

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| S1 | SDK 核心（会话、叶子 API、值、并发）+ 形态①的 `serve`（`fd`/`unix`）、清单、`RutisTest`；`binary.rs` 抽取 + `go.rs` 薄封装；`Launcher::swift`、feature；`SwiftResolver`；`SwiftRuntimes`（仅 `eager`） | §十四 除按需、远程、TCC 外全部通过；多二进制并行；iOS Simulator 单测过；Go 原有测试全绿 |
| S2 | 形态①按需启动与空闲停止；rutis-host（`runtimes.swift`、`dev`、`check`、`new --lang swift`）；指南；TCC / EventKit | 按需启动测试过；`dev` 改代码只重启本运行时；授权提示指向二进制、拒绝错误传回 |
| A1 | 形态②a：SDK 的 `listen:`（`NWListener`）；`remote[].language: swift`；远程租约与一致性 | 前台应用（iPhone 或模拟器）作为 swift 远程运行时被宿主拨入控制：行装载 / 卸载 / 断开恢复测试过 |
| A2 | 形态②b：`RutisLink`（dial / export / import / 重连）；节点侧零改动 | 应用拨入节点，export 的服务被宿主行消费；断网重连后服务恢复；token 错误按退避重试 |
| S3（按需） | 下载预编译二进制；接收对象引用；`@RutisService` / Schema 宏；版本 build plugin；App Intents 桥；events 转发；watchOS / visionOS；Linux；APNs 唤醒 | 有需求时定 |

A1 只依赖 S1 的会话层，可与 S2 并行；A2 依赖 S1 + A1 的网络通道层。

## 十六、待定

| 项 | 当前取向 |
| --- | --- |
| 清单获取方式 | 执行二进制（同 Go）；备选构建时写文件 |
| `idle` 默认值 | 60 秒（同 Go） |
| sync 方法发不出调用 | 编译期已挡；开发模式运行期警告随落地看需要 |
| iOS 最低版本 | 17；可降（与 App Intents 生态对齐） |
| `NWListener` + `NWProtocolWebSocket` 的子协议 / 自签 TLS / ping 组合 | 语义对齐 Go G2；细节 S1/A1 期间本机 PoC 后定稿 |
| 后台存活策略 | v1 前台 + 重连；APNs 唤醒（需节点侧配合）或后台模式，S3 定 |
| 形态②b 的 events 转发 | v1 只 export / import；events 随需求 |
| App Intents 桥范围 | S3 |
| 通用二进制（universal） | v1 按本机 arch 出包；发布是否 universal 随 `new` 工作流定 |
| 二进制体积 | release 数 MB 级；不预先优化 |

## 十七、与总体稿的出入

| 总体稿原文 | 本文 |
| --- | --- |
| M3 目标"macOS 的系统能力" | 扩展：对接 Apple 生态与应用（三形态，§二）。形态②③超出总体稿范围，建议随实现 PR 在总体稿 §十一 补记里程碑（如 Go A.3 修订 M4 的先例） |
| "一个签名的辅助程序，用 `dlopen` 加载插件 bundle" | 修订：形态①一个插件组一个签名二进制（理由与代价见附录 A） |
| 行名 `swift:Calendar.bundle` | 形态① `swift:<插件名>`；形态②a `<远程名>:<插件名>`（远程运行时命名） |
| §二.7 把 Swift 归为"按名字加载代码的" | 归类改为与 Go 相同的"插件就是编译好的程序"；iOS 上集成单位是应用 |
| M3 验收 | 保留并扩展（§八） |
| §九 待定"Swift（M3）确认可重入规则" | 已回答：可重入（§五），规则定为"叶子运行时都可重入" |
| 只支持 Apple 平台（§七） | 收窄并分层：macOS + iOS（形态②）；Linux 后续；Windows 不做 |

## 附录

### A. 为什么不用 dlopen 辅助程序（对总体稿 M3 的修订）

总体稿把 Swift 归为"运行时加载代码"一类（与 Node / Python 同），用 `dlopen` 加载插件 bundle。本文改为 Go 的"编译好的程序"模型，理由按重要性排序：

**A.1 Go（M4）先落地，机器已建成。** `GoResolver`、`GoRuntimes`、两段放行、按需启动、restart、`check`、`dev`、`new` 都在（#227），形态①以共用代码接入（§九）；dlopen 路线则要新造 bundle 发现与装载、辅助程序与 bundle 之间的第二层 API、以及 Go 已解决过的清单 / 缓存 / 平台核对问题的另一套答案。iOS 上 dlopen 更无从谈起（§二）。

**A.2 Swift 的跨工具链二进制兼容不可靠。** bundle 与辅助程序分开编译时，Swift 侧跨模块调用需要 module stability + library evolution，实践中对"任意作者、任意 Swift 版本编出的 bundle"不可保证；绕开的办法是把边界压成纯 C ABI，那就要再造一层与 Go/Python 都不同的 API。SwiftPM 也没有 loadable-bundle 产品类型。

**A.3 TCC 粒度。** 辅助程序一个 TCC 身份：给 A 插件授权日历，同进程的 B 也能读日历。可执行文件模型里每个插件组一个窄授权，隐私故事更好，且仍满足 M3 验收（提示指向辅助程序 = 指向该二进制）。形态②里授权本来就归属应用，无此问题。

**A.4 签名与库验证。** 开 hardened runtime 的签名辅助程序 dlopen 非同队签名的 bundle，需要 `disable-library-validation`（扩大攻击面）或所有插件作者同一团队签名。可执行文件 / 静态链入应用都无此问题。

**A.5 损失与替代。** 失去"往运行中的进程里丢一个新 bundle 就装载"。替代：(1) 部署方在 `cmd/main.swift` 里组合多个插件包为一个二进制（依赖由部署方合并，同 Go 附录 H.3）；(2) 多个插件各自成二进制、按需启动、空闲即停（进程数与 Go 同量级）。分发来源同 Go 的 H：作者发布、`swift add <URL>`（S3）、部署方自组、`dev` 构建；形态②里 = 发新版应用。

### B. Swift 事实表（调研来源）

| 事实 | 来源 |
| --- | --- |
| Swift 6.3 起 `@_cdecl` 转正为 `@c`（SE-0495）；本设计不需要 C ABI（会话协议自实现，同 Go） | [SE-0495](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0495-cdecl.md) |
| macOS 10.14.4+ Swift 运行时随系统；CLI 二进制无需捆绑 runtime | [swift.org ABI stability](https://swift.org/blog/abi-stability-and-apple/) |
| Swift 6.4（2026-09）工具链最低部署目标 macOS 13；发布节奏每年 3 / 9 月 | [swift-evolution 版本表](https://github.com/swiftlang/swift-evolution/blob/main/README.md)、[platform-support](https://swift.org/platform-support/) |
| Linux：SE-0342 起运行时默认静态链入；Static Linux SDK（musl）可全静态但禁 `dlopen` | [SE-0342](https://github.com/apple/swift-evolution/blob/main/proposals/0342-static-link-runtime-libraries-by-default-on-supported-platforms.md)、[Static Linux SDK](https://www.swift.org/documentation/articles/static-linux-getting-started.html) |
| swift-foundation 提供 FoundationEssentials，跨平台 | [swift-foundation](https://github.com/swiftlang/swift-foundation) |
| uniffi Swift 6 支持标注 partial（async 生成代码不满足 Sendable）；swift-bridge 0.x 且曾断更约 16 个月——均不采用 | [UniFFI Swift](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html)、crates.io |
| setup-swift@v3 支持 Ubuntu / macOS；macOS runner 自带 Xcode | [setup-swift](https://github.com/swift-actions/setup-swift) |
| 阻塞读不应落在协作线程池（池宽 = 核数）；专用线程 + continuation 是标准做法 | Swift concurrency 文档与社区共识 |
| 裸可执行文件把 Info.plist 链进 `__TEXT,__info_plist`（`-sectcreate`）是长期既有技术 | CMake / xamarin.macOS / swift-forums 多处实践 |
| Network.framework 的 `NWListener` / `NWProtocolWebSocket` 支持带子协议与 TLS 的 WebSocket 服务端；`URLSessionWebSocketTask` 是客户端 | Apple 文档（组合细节随 §十六 PoC 定稿） |
| iOS：无进程 spawn、沙盒禁止下载执行代码、后台应用挂起、本地网络权限（iOS 14+） | Apple 平台文档 |

### C. 与 Go 运行时逐项对照（形态①）

| | Go | Swift |
| --- | --- | --- |
| 进程模型 | 一组插件一个二进制 | 同 |
| 自描述 | `--rutis-manifest` | 同 |
| 行名 / 运行时名 | `go:<插件>` / `go-<名>` | `swift:<插件>` / `swift-<名>` |
| 每调用执行单元 | goroutine（阻塞等待） | Task（挂起等待，不占线程） |
| 可重入 / sync-stack | 可重入 / 不声明 | 同 |
| 调用链 | `context.Context` | `@TaskLocal` |
| 方法枚举 | 反射 `MethodsOf` | 显式闭包表（宏为后续项） |
| Schema | 反射生成，可覆盖 | 手写（宏为后续项） |
| 值默认 | 数据，`rutis.Ref` 显式引用 | 值类型即数据；class 须 `Rutis.ref`（分界来自类型系统） |
| sync 方法内发调用 | 可以（阻塞 goroutine） | 不可以（代理全 async） |
| panic / fatalError | 捕获为错误；goroutine 未捕获 → 进程退出 | 错误即 `throws`；`fatalError` → 进程退出（不可捕获） |
| 平台 | Linux / macOS / Windows | macOS（①）/ macOS + iOS（②） |
| 分发 | git tag `go/rutis/vX.Y.Z` + 模块代理 | 根 `Package.swift` + 主发布线 `v` tag（插件与应用同一条） |
| 依赖 | 仅标准库 | 核心仅 Foundation；网络通道 Network.framework / URLSession |

### D. 值分界为什么不需要 `Ref` 那样的规则

Go 需要显式 `Ref` 是因为 Go 结构体既可以是数据也可以是对象，语言不分。Swift 的值类型（struct / enum）与引用类型（class）在类型系统里就是两个世界：struct 天然是数据，class 天然是对象身份。所以默认规则从语言本身读出：struct / enum 按数据复制；class 要跨进程必须显式 `Rutis.ref` 并自带方法表（Swift 无法反射枚举其方法，方法表必须作者给出，与 §4.2 的闭包表同一机制）。

### E. 本机 PoC 实测（2026-10-12，Swift 6.3.2 / Xcode）

**E.1**：根 `Package.swift` 声明子目录路径的目标（`path: "swift/rutis/Sources/Rutis"`）构建、运行正常。

**E.2 SwiftPM 版本 tag 规则**：对 git 依赖实测——`1.0.0`（纯 semver）与 `v1.1.0`（v 前缀）被接受为版本；`rutis-swift-1.0.0`（包名前缀）、`swift/rutis/1.0.0`（斜杠）、`rutis-mono-1.2.0`（URL 名前缀）均被忽略。结论：仓库现有 `v0.9.x` 主发布 tag 直接可用作 SwiftPM 版本；Swift SDK 与主仓库同版本发布，无需 Go 那种 `go/rutis/v…` 形式（SwiftPM 不认斜杠 tag）。

**E.3 参数包上下文推断**：`func method<each A, R>(_:) -> (Ctx, repeat each A) async throws -> R` 在 `LLM(ask: s.method("ask"), …)` 处按字段闭包类型完成推断并编译通过（Swift 5.9 特性）。§4.3 的 `RutisClient` 形状据此设计；若极端签名推断不稳，回退方案是 0–3 参数的重载集。
