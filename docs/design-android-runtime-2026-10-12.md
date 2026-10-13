# Android 运行时（Kotlin 远程节点）

[English](design-android-runtime-2026-10-12.en.md)

状态：设计稿，未实现。日期：2026-10-12。基线：`main` `ebf0711`（0.9.1）。
依据：[多语言插件总体设计](design-multilang-runtimes-2026-10-03.md)、[远程插件设计](design-remote-plugins-2026-10-03.md)（§4.4 通道与租约）、[Go 插件运行时设计](design-go-runtime-2026-10-10.md)（G2 远程实现）、[Bun 运行时设计](design-bun-runtime-2026-10-09.md)（§4 远程）、[插件 API 约定](guide/plugin-api.md)。桌面 JVM 版 Kotlin 运行时（对接 Maven 生态、dev 伴跑）是另一份设计（[Kotlin 运行时](design-kotlin-runtime-2026-10-12.md)），两者共享 KMP SDK；本文档是主线。

定位：Kotlin/ART 运行时的作用是**对接 Android 生态与应用**——桌面/服务器上的宿主和 agent 通过装在设备上的辅助 APK 使用设备能力与应用桥接。这对应总体设计里 Swift/M3 的坑位："系统能力经由一个有自己权限身份的辅助程序暴露"。

范围外（D22）：拓扑 B（Rust core 嵌入 Android App，见 §13 评估）；云中继与设备出拨；USB/ADB；Play 上架合规；宿主在运行时编译。

## 1. 决定

| # | 决定 | 章节 |
| --- | --- | --- |
| D1 | 拓扑 A：宿主在桌面/服务器，设备上一个辅助 APK 作为**远程运行时**连入；行 `<name>:<插件>`，默认名 `droid` | 2 |
| D2 | 设备侧纯 Kotlin，无 Rust 上设备：会话层（端点协议 3）+ ws 监听 + 叶子 SDK | 2 |
| D3 | 连接方向不变：**设备只监听、宿主出拨**（远程设计 §4.4）；中继/出拨是后续加成 | 4 |
| D4 | TLS 放宽（相对 Python/Bun"仅 loopback"的明示偏差）：无 TLS 的 `ws://` 监听允许，但仅接受来自**私有网段**（RFC 1918 / ULA / 链路本地）的连接，且 token 必需；非私有来源拒绝；`wss://` 无此限制 | 4 |
| D5 | 认证：token（宿主侧 `RUTIS_TOKEN_<NAME>`，设备侧首启生成或用户设定），常数时间比较；拒绝码 404/401/403/400 同远程设计 | 4 |
| D6 | 能力插件 = DEX 模块：**内置的打进 APK**（app classloader 直接加载，无需 DexClassLoader）；**dev 推送的**经认证上传端点存应用私有目录，`DexClassLoader` 加载；协议零改动（`rows.load` 的 entry 在设备解析） | 5 |
| D7 | 插件包 = zip（`classes.dex` + `rutis.json` 描述符）；描述符与加载后声明不一致时以实际为准并警告 | 5 |
| D8 | 热载有界：ART 不卸载类，每插件重载预算（默认 50 次），到限运行时自重启（前台服务 `START_STICKY` 拉起，宿主链路重连即租约接管） | 5 |
| D9 | **权限即生命周期**：能力插件的 Android 权限声明在 APK 清单；未授权 → 加载失败（错误点名权限）；运行中被撤销 → 撤服务（rutis 撤销语义停掉使用者）；Accessibility / NotificationListener 的启用引导经 `droid:system` 服务 | 6 |
| D10 | 线程模型：每个进来的调用一个平台线程（Go 式；设备并发度低）；能力声明 `sync-wait` + `reentrant-sync`，不声明 `sync-stack`、不声明 `objects` | 7 |
| D11 | 插件 API 为 suspend（Kotlin 协程）；行作用域 `ctx.launch`（`Dispatchers.Default`），卸载时取消并 join | 7 |
| D12 | 无运行时反射：schema 与描述符编译期生成（KSP，A4）或显式 `Plugin.schema`；kotlin-reflect 不上设备 | 8 |
| D13 | SDK = KMP 库 `io.github.arcships:rutis`（android + jvm 双 target）；同一份插件源码桌面与设备共用 | 8 |
| D14 | 设备运行时打包为 AAR `io.github.arcships:rutis-droid`（可嵌入任意 App）+ 一个嵌入它的辅助 APK（`rutis-droid` app 模块） | 9 |
| D15 | 方法默认 `async`，`sync` 显式标记（同步方法会阻塞事件循环型对端） | 8 |
| D16 | 值规则同其他叶子运行时：JSON 兼容类型复制、函数与 `Ref` 按引用、`ctx.provide` 总是按引用、`undefined` 是专用值 | 8 |
| D17 | 插件 API 版本 `PLUGIN_API = 1`，照抄现有机制 | 8 |
| D18 | 会话细节同协议：单帧 16 MiB（超限 1009）、心跳默认 30 s（`RUTIS_HEARTBEAT` 可调）、新连接接管旧租约（旧连接 4002）、文本帧 | 4 |
| D19 | 宿主侧改动一处：`rutis-host/src/host.rs` 远程语言映射加 `"kotlin"|"kt"` → `modules`；其余零改动（peer 行 + remote 配置现有） | 10 |
| D20 | 测试策略：conformance 先在 **jvm target** 上过（快迭代），再上模拟器（设备专有路径：权限、DexClassLoader、上传端点）；`multilang.rs` 加远程 Kotlin 列 | 11 |
| D21 | 桌面 JVM 运行时保留为独立设计（[Kotlin 运行时](design-kotlin-runtime-2026-10-12.md)），排在 Android 阶段之后 | 12 |
| D22 | 范围外：拓扑 B、云中继/出拨、USB/ADB、Play 合规、宿主编译 | 13 |

## 2. 模型

```text
桌面 / 服务器                                 Android 设备
rutis host (Rust)                             rutis-droid 辅助 APK（前台服务）
 ├─ 本地行: node / py / bun / go / dylib       ├─ 会话层（KMP android target，端点协议 3）
 ├─ RuntimePlugin::remote("droid")             ├─ ws / wss 监听 + token 认证 + 插件上传端点
 │   （等 RuntimeSession#droid，由 peer 行提供）├─ 能力插件（DEX）：
 └─ 行 droid:contacts / droid:notifications     │   contacts / 通知 / Intent / 传感器 /
     droid:apps / droid:automation ...          │   Accessibility 自动化 / 应用桥接
                                                └─ 权限桥（授权状态 ↔ 行生命周期）
```

- 宿主侧它就是一个**远程语言运行时**：`runtimes.remote` 一项 + 一个 `rutis-bridge/peer` 行 dial 设备；行 `<name>:<插件>` 的解析发生在设备（`RuntimeResolver::modules`，不缓存，二段刷新）。
- 多设备 = 多个命名远程运行时（`droid-pixel`、`droid-tablet`），互不干扰。
- 设备进程死亡/重启：会话断开，本运行时全部行停止；宿主链路重连后是新的一代运行时，行按依赖序重新加载（现有租约/接管语义，Go G2 已验证）。
- 设备上的插件互相调用拿对象本身；跨设备/跨语言按名字走宿主。

## 3. 设备侧：APK 结构

```text
app（辅助 APK，前台服务型）
 └─ 依赖 rutis-droid AAR
     ├─ DroidService：前台服务（connectedDevice 类型，常驻通知"rutis 节点运行中"）
     ├─ 监听器：HTTP + WebSocket 服务端（小型实现：NanoHTTPD+websockets 或 Ktor CIO；仅文本帧）
     │    ├─ GET /rutis        → WebSocket 升级（子协议 rutis.3）
     │    └─ PUT /rutis/plugin/<name>  → 插件包上传（同一 token 认证）
     ├─ 会话层：KMP android target（帧、引用计数、取消、调用链、租约接管）
     ├─ 插件注册表：内置模块（APK 类）+ 上传模块（私有目录 zip → DexClassLoader）
     └─ 权限桥：授权状态监听 → 行装载/撤服务
```

- 进程模型：一个前台服务进程；`START_STICKY`，被杀后系统拉起，服务重建监听与会话。
- 电池/Doze：前台服务 + 活动套接字；心跳默认 30 s（可调）。Doze 深睡时网络受限是已知现实，文档如实说明（充电/亮屏时最可靠）。
- 电池优化白名单：首启引导用户把 App 加入系统电池优化白名单（标准 intent）；国产 ROM（MIUI/ColorOS 等）的"自启动/后台不限制"无法用代码设置，附逐厂商手动设置指引。
- 网络变化重绑：监听系统网络变化（Wi-Fi 切换、地址变更），重建监听 socket；服务通知实时显示当前局域网地址（DHCP 换 IP 是最常见的"假死"，宿主配置里的地址会失效）。
- UI：极简——首启生成/展示 token 与局域网地址（供宿主配置），权限请求入口，重载预算余量。

## 4. 通道、认证与 TLS 放宽

| 项 | 规则 |
| --- | --- |
| 监听地址 | 默认全部接口的 `:7443`，路径 `/rutis`，子协议 `rutis.3`（端点格式，协议 3） |
| 方向 | 设备只监听、宿主出拨（D3，远程设计规则不变） |
| TLS | `wss://` 用设备生成的自签证书（首启生成，设置页显示指纹，宿主侧 `RUTIS_CERT` 固定） |
| **放宽**（D4） | `ws://`（无 TLS）监听允许，但**逐连接检查来源地址**：仅私有网段（10/8、172.16/12、192.168/16、fc00::/7 ULA、fe80::/10 链路本地）接受；非私有来源直接拒绝。Python/Bun 的"无 TLS 仅 loopback"规则在设备侧放宽到此为止，是本运行时的明示偏差，记录在案 |
| token | 必需（`ws` 与 `wss` 都要）；设备侧生成/设定，宿主侧 `RUTIS_TOKEN_DROID`（或通用 `RUTIS_TOKEN`）；常数时间比较 |
| 拒绝码 | 404 路径不对、401 缺凭据、403 token 错、400 子协议不对（同远程设计） |
| 心跳 | 默认 30 s 无应答断开；`RUTIS_HEARTBEAT` 可调 |
| 帧与上限 | 文本帧；单帧 16 MiB，超限 1009 关闭 |
| 接管 | 新连接先认证 → 关旧连接（不再读帧）→ 清理旧租约全部行与代理 → 应答新 `hello`；旧连接 4002 |
| 启动输出 | 监听就绪后 stderr/日志 `rutis: listening on …`（服务通知同步显示地址） |

### 4.1 断线与恢复

断联走现有机制，不需要新协议：

| 情况 | 行为 |
| --- | --- |
| 网络抖动（秒级） | TCP 自行恢复，无感知；真断线由心跳在超时内检出（半开连接只有心跳能发现） |
| 检出断线 | 手机侧清理本会话全部插件（cleanup 逆序、有时限）；电脑侧 `droid:*` 行停止，依赖方级联停止（rutis 原生撤销语义） |
| 重连 | 电脑侧链路自动重拨（按失败类别的指数退避，会话稳定后归零——`rutis-bridge` link 现有行为）；连上即新一代运行时，行按依赖序自动重载，配置在宿主侧不丢 |
| 进程被杀（LMK/厂商清后台） | `START_STICKY` 稍后拉起重建监听（间隔随厂商，可能数分钟）；宿主仍在退避重拨，两端总会接上 |
| 深睡 Doze | 前台服务 + 电池白名单下基本不受影响；真深睡时表现为断线，亮屏/维护窗口后自动恢复 |
| 插件视角 | 插件感知不到"断线"：运行时把断联翻译成 rutis 已有的停/启——cleanup 按序跑、`apply` 重跑。要不要持久化状态是插件自己的事（§5） |

## 5. 插件：DEX 模型与热载

### 5.1 两种来源

| 来源 | 装载 | 热载 |
| --- | --- | --- |
| 内置（打进 APK） | app classloader 直接加载（就是 APK 里的类，无需 DexClassLoader） | 换 APK（正常应用更新） |
| 上传（dev） | `PUT /rutis/plugin/<name>`（同一 token）：插件包 zip = `classes.dex` + `rutis.json`，存应用私有目录；`rows.load` 按名字经 `DexClassLoader` 装载 | 见 §5.2 |

- entry 解析同 JVM 稿 §8.2：`droid:contacts.ContactsPlugin`（含点）→ 类全名；`droid:contacts`（不含点）→ 按 `rutis.json` 的 `name` 匹配；多个匹配报歧义。
- `rows.schema` 由设备上的声明应答（内置与上传同一路径）；描述符与实际声明不一致以实际为准并警告。
- **状态归插件**：断线、重载、自重启都会丢内存状态——这是模型的本意（插件可重建）。要不要持久化、怎么持久化（文件、DataStore、SQLite）是插件作者的事：插件就在 App 进程里，Android 存储 API 直接可用，运行时不提供也不规定。

### 5.2 有界热载（D8）

- ART 不卸载类：重载 = 新 `DexClassLoader`，旧 loader 的类常驻。预算默认每插件 50 次（设置页与 `check` 可见余量）。
- 到限：运行时有序 `dispose` 全部行 → 自杀进程 → 前台服务 `START_STICKY` 拉起 → 宿主链路重连（租约接管）→ 行按依赖序重载。宿主视角是一次普通的重启，不是错误。
- 上传新版本即占一次预算；同内容跳过（zip 内容哈希比对）。

### 5.3 dev 循环（A4）

```text
桌面: 插件项目 (Gradle, KMP/android lib)
  → gradle build → 插件包 zip (dex + rutis.json，KSP 生成描述符)
  → rutis-host dev: PUT 上传到设备 → 触发该行的 rows.load 重载
```

宿主不编译 Android 代码；`gradle build` 是项目自己的构建（Go D15 同款）。`rutis-host new --lang kotlin` 生成该模板。

## 6. 权限即生命周期（D9）

Android 权限绑定 APK 身份， rutis 的撤销语义正好对上：

| 事件 | 设备运行时行为 | 宿主视角 |
| --- | --- | --- |
| `rows.load` 时权限未授予 | 拒载，错误点名权限与请求方式 | 行加载失败，原因可读 |
| 授权（设置页/引导） | 该插件可加载（宿主 `reload` 或下次解析） | 服务出现 |
| 运行中权限被撤销（系统设置） | 撤该插件全部 provides（`service(id, None)` 通知）→ 行停止 | 提供者撤销 → 使用者按序停止 |
| Accessibility / NotificationListener 未启用 | 对应能力不可用；`droid:system` 服务提供状态查询 + 跳设置的 deep link | 依赖它的行不启动，`check` 说明原因 |

- 权限状态监听：装载前检查 + 运行中周期核对（撤销没有广播，周期对账是务实做法；周期默认 30 s，与心跳同阶）。
- 权限请求本身发生在设备 UI（用户在手机上操作）；宿主只能查询与引导，不能代授——权限提示**指向辅助 APK**，这正是 Swift/M3"签名辅助程序"验收项的 Android 对应。

## 7. 线程模型：同步与可重入

ART 没有虚拟线程（Loom 是 OpenJDK 的）；设备并发度低（一个人的手机），采用 Go 式模型：

```text
APK 进程
 ├─ 读线程：阻塞读帧，解析后派发
 ├─ 每个进来的 invoke/call/get/await/控制操作：一个新平台线程（默认栈，够用；并发几十内无压力）
 ├─ 一个写锁：帧串行写出
 └─ 行协程作用域：ctx.launch 的任务（Dispatchers.Default），卸载取消并 join
```

| 项 | 规则 |
| --- | --- |
| 出站同步调用 | 当前线程阻塞在 `CompletableFuture`，读线程完成它 |
| 可重入性 | 进来的调用任何时候都能跑（新线程）；声明 `reentrant-sync`。代价同 Python/Go：**服务可能在自己同步调用途中被再次调用，调用 rutis 服务时不要持有锁** |
| 能力组合 | `sync-wait` + `reentrant-sync`；不声明 `sync-stack`（并发多栈，只参与调用链边）；不声明 `objects` |
| `SyncWaitCycle` | 本地不产生；收到按远程错误呈现 |
| 取消 | `cancel` 帧 → 取消调用协程 + 中断线程；不可中断的计算跑完丢弃结果，如实呈现 |
| suspend 桥接 | suspend 方法在其专用线程上 `runBlocking`（阻塞的是自己的调用线程，合法）；`apply`/cleanup 同理 |
| 异常 | 服务方法异常 → 错误图返回；`apply` 异常 → 加载失败、cleanup 照跑；插件自起线程未捕获异常 → 进程退出、全部行停止、不自动重启（宿主链路会重连成新一代） |

## 8. KMP SDK

### 8.1 形态

- `io.github.arcships:rutis`（KMP：android + jvm）。插件作者只依赖它；编译到哪个 target 由项目决定（设备插件 → android；桌面伴跑 → jvm）。
- SDK 只依赖 kotlin-stdlib（suspend 是语言特性，`kotlin.coroutines` 在 stdlib 里；协程 builder 由运行时提供）。
- 无运行时反射（D12）：`rutis.json` 描述符与 config schema 由 KSP 处理器在编译期生成（A4）；在那之前 `Plugin.schema` 显式给定。

### 8.2 插件写法（示意，与 JVM 稿同一套源码）

```kotlin
data class Config(val group: String? = null)

interface Llm { suspend fun ask(question: String): String }

class Contacts(private val resolver: ContactsResolver) {
    suspend fun list(query: String): List<Contact> = resolver.query(query)
    fun count(): Int = resolver.count()
}

val plugin = rutis.definePlugin(
    name = "contacts",
    inject = listOf("llm"),
    provides = rutis.provides("contacts" to rutis.methodsOf<Contacts>(sync = "count")),
    config = rutis.schemaOf<Config>(),
) { ctx, config ->
    val llm = ctx.use<Llm>("llm")
    ctx.provide("contacts", Contacts(ContactsResolver(llm, config.group)))
}
```

- 需要 `android.*` 的实现藏在插件自己的 `androidMain` 源集里；`ctx.use`/`provide`/`effect`/`launch` 与声明部分是公共源集。
- 方法命名/签名/错误、值规则（D15/D16/D17）与 JVM 稿 §7.2–7.4 相同，不重复。

## 9. 仓库与打包

| 项 | 规则 |
| --- | --- |
| 位置 | `kotlin/`（Gradle 多项目）：`sdk`（KMP 库）、`droid`（AAR 运行时）、`app`（辅助 APK）、`tools`（KSP 处理器，A4） |
| 发布 | AAR + KMP 库到 Maven Central（`io.github.arcships` 命名空间）；辅助 APK 由 GitHub Release 附带（sideload；Play 上架是独立的合规工作，范围外） |
| 版本 | 进 `scripts/train.mjs`（新增 maven 坐标与 APK 版本） |
| 设备要求 | Android 10（API 29）起；arm64-v8a 为主，x86_64 供模拟器 |
| WS 服务端 | NanoHTTPD+websockets 或 Ktor CIO（二选一，A2 验证），仅文本帧 |

## 10. 宿主侧

改动一处（D19）：`rutis-host/src/host.rs` 的远程语言映射加 `"kotlin"|"kt"` → `RuntimeResolver::modules`。配置其余全是现有能力：

```json
{
  "runtimes": {
    "remote": [{ "name": "droid", "language": "kotlin" }]
  },
  "rows": [
    { "id": "droid-peer", "name": "rutis-bridge/peer",
      "config": { "peer": "droid", "dial": "ws://192.168.1.42:7443/rutis" } },
    { "id": "contacts", "name": "droid:contacts" },
    { "id": "notify", "name": "droid:notifications" }
  ]
}
```

- token：环境变量 `RUTIS_TOKEN_DROID`（现有规则，`config.rs` 的 `token()`）。
- `wss` 时宿主侧 `RUTIS_CERT` 固定设备自签证书（指纹在设备设置页展示）。
- `mount` 回复：`implementation: { name: "io.github.arcships:rutis-droid", version }`，`engine: { name: "kotlin", version, vm: <ART 版本> }`；`check` 打印。
- 多设备：`remote` 多项 + 各自 peer 行；行名前缀区分。

## 11. 测试

| 测试 | 位置 | 内容 |
| --- | --- | --- |
| 会话单元 | `kotlin/sdk`（jvm target，快） | 编码、引用计数、同步调用中回调、取消、调用链、并发、租约接管 |
| 会话 conformance（桌面） | `runtime_conformance.rs` + KMP **jvm** target 的 `conformance` 入口（内存/ws 通道） | `session::testing::session` 全套——先在桌面把协议打透 |
| 会话 conformance（设备） | 模拟器仪表化测试（android target，ws 回环） | 同一套断言在 ART 上过一遍 |
| 远程契约 | `websocket_conformance.rs` 加 Kotlin 远程端 | 端点协议 3、token、心跳、接管 4002 |
| 契约 × 通道 | `session_matrix.rs` | 上传端点、DexClassLoader 装载、内置/上传两条装载路径 |
| 行为 | `kotlin/` 仪表化 + `multilang.rs`（feature `kotlin`） | 权限↔行生命周期（拒载/撤销撤服务）、重载预算到限自重启、进程死亡后重连重载、多语言互调 |
| soak | 仪表化 | 重载预算内多次 load/unload 内存稳定；长连接心跳与 Doze 表现记录 |
| CI | `ci.yml` | JDK job（jvm target 测试 + conformance）+ 模拟器 job（android target）；Gradle 构建先行 |

## 12. 阶段

| 阶段 | 工作 | 验收 |
| --- | --- | --- |
| A1 | KMP SDK 骨架 + 会话层（jvm target） | 桌面上过 `session::testing::session` conformance |
| A2 | android target 会话层 + 监听器 + token + TLS 放宽规则；模拟器回环 | Rust 宿主经 ws 连模拟器内的运行时，`websocket_conformance` 通过；非私有来源被 `ws` 拒绝 |
| A3 | 插件框架（内置/上传/有界热载）+ 权限桥 + 辅助 APK + 首批能力（contacts、notifications、Intent） | 真机验收：权限提示指向 APK；撤销权限 → 使用者停止；重载到限自重启后自动恢复 |
| A4 | dev 循环（KSP 描述符/schema、上传、`new --lang kotlin` 模板、`check`） | 桌面改插件 → 上传 → 行重载全流程秒级到分钟级 |
| A5（后续） | 中继/设备出拨模式（需协议侧设计）、拓扑 B 增量评估、更多能力插件（Accessibility 自动化、应用桥接） | 另立设计 |

桌面 JVM 运行时（D21，[Kotlin 运行时](design-kotlin-runtime-2026-10-12.md)）在 A4 后择机接入（`Launcher::kotlin` 那条线），作为 Maven 生态运行时与桌面调试伴跑。

## 13. 开放问题与拓扑 B 评估

- **心跳与 Doze**：深睡时网络的可用性随厂商差异大；如实文档化（充电/前台/电池白名单最可靠），必要时提供事件缓冲。断线行为见 §4.1——恢复是自动的，可靠性取决于设备侧进程能否存活。
- **撤销对账周期**（§6）：30 s 周期对账 vs 更精确的钩子（无广播，只能周期或关键操作时核对）。
- **上传端点的滥用面**：同一 token 已认证；是否加内容哈希白名单/签名（dev 足够，公网暴露场景要求 wss + 更强配对）。
- **拓扑 B（core 嵌 App）的增量**：A 的 Kotlin 层全部复用（会话层走 rutis-bridge 现成的 `transport::memory`，契约不变）；增量 = core 的 NDK 交叉编译（cargo-ndk、tokio 可行）、JNI 引导与生命周期适配、AAR 打包 `.so`、Play DCL 披露。工程量主要在 Rust 侧；有真实 on-device 宿主需求再立项。
- **Accessibility 自动化的边界**：对第三方 App 的自动化受权限与合规双重约束，作为独立能力插件设计（A5），不进首版。

## 附录：与桌面 JVM 稿的关系

一份 KMP SDK、两个运行时：`rutis-droid`（本文档，Android/ART，远程节点）与桌面 JVM 运行时（[Kotlin 运行时](design-kotlin-runtime-2026-10-12.md)，`Launcher::kotlin` 本地行，对接 Maven 生态 + dev 伴跑）。共享：插件声明与 SDK API、方法形状/值规则/`PLUGIN_API`、会话层核心（jvm target 复用为桌面实现）。不同：装载（DEX/jar）、线程（平台线程/虚拟线程）、通道（网络监听/本地回连）、生命周期（App 权限/宿主进程）。
