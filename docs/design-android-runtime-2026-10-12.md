# Android 运行时（Kotlin 远程节点）

[English](design-android-runtime-2026-10-12.en.md)

状态：设计稿，未实现。日期：2026-10-12。基线：`main` `ebf0711`（0.9.1）。
依据：[多语言总体设计](design-multilang-runtimes-2026-10-03.md)、[远程插件设计](design-remote-plugins-2026-10-03.md) §4.4、[Go 运行时](design-go-runtime-2026-10-10.md)（G2 远程先例）、[插件 API 约定](guide/plugin-api.md)。桌面 JVM 版是另一份稿（[Kotlin 运行时](design-kotlin-runtime-2026-10-12.md)），共享 KMP SDK。

定位：宿主和 agent 在桌面/服务器，通过设备上的辅助 APK 使用 Android 生态与应用——对应总体设计里 Swift/M3 的坑位（"系统能力经由有自己权限身份的辅助程序暴露"）。

范围外：Rust core 嵌 App（拓扑 B，增量见 §10）；云中继与设备出拨；USB/ADB；Play 合规。

## 1. 决定

| # | 决定 |
| --- | --- |
| D1 | 拓扑 A：设备上一个辅助 APK 作为**远程运行时**连入；行 `droid:<插件>`（默认名，可改），多设备 = 多命名远程运行时 |
| D2 | 设备侧纯 Kotlin，无 Rust 上设备：会话层（端点协议 3）+ ws 监听 + 叶子 SDK |
| D3 | 方向不变：设备只监听、宿主出拨（远程设计规则） |
| D4 | **TLS 放宽**（相对 Python/Bun"仅 loopback"的明示偏差）：`ws://` 允许，但逐连接仅接受私有网段来源（RFC 1918 / ULA / 链路本地）且 token 必需；`wss://` 自签证书 + 指纹固定 |
| D5 | 插件 = DEX：内置的打进 APK（app classloader 直接加载）；dev 的经认证端点上传 zip（`classes.dex` + `rutis.json`）→ `DexClassLoader`；协议零改动（entry 在设备解析） |
| D6 | 热载有界：ART 不卸载类，每插件预算 50 次；到限 `dispose` → 自重启 → 宿主租约接管（宿主视角是普通重启） |
| D7 | 权限即生命周期：未授权拒载（点名权限）；运行中撤销 → 撤服务（rutis 撤销语义停使用者）；启用引导经 `droid:system` 服务 |
| D8 | 断线只做翻译：断联 = 停（cleanup 有序），重连 = 再启动（`apply` 重跑、依赖序重载）；插件看不到"断线"概念；持久化归插件，运行时不提供存储契约 |
| D9 | 线程：每个进来的调用一个平台线程（Go 式；ART 无虚拟线程）；能力 `sync-wait` + `reentrant-sync`，不声明 `sync-stack`/`objects` |
| D10 | SDK = KMP 库（android + jvm），只依赖 kotlin-stdlib；无运行时反射（KSP 编译期生成，此前 `Plugin.schema` 显式） |
| D11 | 宿主侧改动一处：远程语言映射加 `"kotlin"|"kt"`；其余全用现有能力 |
| D12 | conformance 先在桌面 jvm target 打透，再上模拟器；桌面 JVM 运行时在 A4 后接入 |

## 2. 模型

```text
桌面 / 服务器                          Android 设备
rutis host (Rust)                      rutis-droid APK（前台服务）
 ├─ 本地行: node/py/bun/go/dylib        ├─ 会话层（端点协议 3）+ ws 监听
 ├─ RuntimePlugin::remote("droid")      ├─ 能力插件（DEX）: contacts / 通知 /
 └─ 行 droid:contacts / droid:apps ...  │   Intent / 传感器 / 应用桥接
     （peer 行 dial 设备提供会话）       └─ 权限桥（授权状态 ↔ 行生命周期）
```

- 设备进程死亡：本运行时行停止；重连即新一代运行时，行按依赖序重载（现有租约/接管语义，Go G2 已验证）。
- 同设备插件互调拿对象本身；跨设备/跨语言按名字走宿主。

## 3. 设备侧

```text
APK（前台服务, START_STICKY）
 ├─ 监听 :7443 /rutis，子协议 rutis.3；PUT /rutis/plugin/<name> 上传（同 token）
 ├─ token：首启生成，常数时间比较；拒绝码 404/401/403/400
 ├─ 心跳 30 s（RUTIS_HEARTBEAT）；16 MiB 帧超限 1009；新连接接管（旧 4002）
 └─ 通知栏实时显示当前局域网地址；网络变化重绑监听
```

- 电池：前台服务 + 电池优化白名单引导（标准 intent）；国产 ROM 的"自启动/后台不限制"只能附手动指引。
- 深睡 Doze 如实文档化：充电/亮屏/白名单最可靠；深睡表现为断线，亮屏后自动恢复。

### 3.1 断线

运行时只做翻译：心跳检出（半开连接唯一手段）→ 手机侧清理本会话插件（cleanup 逆序、有时限）→ 电脑侧行级联停止 → 宿主链路退避重拨（现有行为）→ 重连后自动重载，配置在宿主侧不丢。进程被杀由 `START_STICKY` 拉起，两端总会接上。要不要持久化状态是插件的事：它在 App 进程里有 Android 存储 API。

## 4. 插件

- entry 解析同 [JVM 稿](design-kotlin-runtime-2026-10-12.md) §8.2（含点 = 类全名；不含点 = 按 `rutis.json` 匹配）；描述符与实际声明不一致以实际为准。
- 上传新版本占一次预算；同内容跳过（哈希比对）。
- dev 循环：宿主不编译；项目自己的 `gradle build` → 上传 → 行重载（Go D15 同款）。

## 5. 权限即生命周期

| 事件 | 行为 |
| --- | --- |
| `rows.load` 时未授权 | 拒载，错误点名权限 |
| 运行中被撤销 | 撤该插件全部服务 → 行停止 → 使用者级联停止 |
| Accessibility / NotificationListener 未启用 | 对应行不启动；`droid:system` 提供状态查询与跳设置 deep link |

授权发生在设备 UI，提示指向辅助 APK（Swift/M3 验收项的 Android 对应）；撤销无广播，装载前检查 + 30 s 周期对账。

## 6. 线程与会话

每个进来的调用一个平台线程，出站同步调用阻塞在自己的 `CompletableFuture` 上；suspend 方法用 `runBlocking` 于本调用线程桥接；`ctx.launch` 跑在行协程作用域，卸载取消并 join；取消 = 协程取消 + 线程中断；插件线程未捕获异常 → 进程退出、行停止。方法形状、值规则、`PLUGIN_API=1` 与 [JVM 稿](design-kotlin-runtime-2026-10-12.md) §7 相同。

## 7. SDK 与打包

- `io.github.arcships:rutis`（KMP：android + jvm），插件作者只依赖它；只依赖 kotlin-stdlib。
- 设备运行时 = AAR `io.github.arcships:rutis-droid`（可嵌入任意 App）+ 一个嵌入它的辅助 APK；Android 10（API 29）起，arm64-v8a 与 x86_64。
- 仓库布局 `kotlin/`：`sdk`、`droid`、`app`、`tools`（KSP）；发布 Maven Central + APK 走 GitHub Release（sideload）；版本进 `scripts/train.mjs`。
- WS 服务端：NanoHTTPD+websockets 或 Ktor CIO（A2 定），仅文本帧。

## 8. 宿主侧

改动一处：`rutis-host/src/host.rs` 远程语言映射加 `"kotlin"|"kt"` → `RuntimeResolver::modules`。

```json
{
  "runtimes": { "remote": [{ "name": "droid", "language": "kotlin" }] },
  "rows": [
    { "id": "p", "name": "rutis-bridge/peer",
      "config": { "peer": "droid", "dial": "ws://192.168.1.42:7443/rutis" } },
    { "id": "contacts", "name": "droid:contacts" }
  ]
}
```

token 环境变量 `RUTIS_TOKEN_DROID`；`wss` 时 `RUTIS_CERT` 固定设备证书；`mount` 回复带 `implementation`/`engine`（kotlin + ART 版本），`check` 打印。

## 9. 测试与阶段

- 会话单元（jvm target，快）→ conformance：`runtime_conformance.rs` 先跑 jvm target，模拟器仪表化再跑 ART；`websocket_conformance.rs` 加 Kotlin 远程端；`multilang.rs` 加列。
- 设备专有测试：权限↔行生命周期、重载预算到限自重启、进程死亡后重连重载、上传端点。
- CI：JDK job（jvm target）+ 模拟器 job；Gradle 构建先行。

| 阶段 | 工作 | 验收 |
| --- | --- | --- |
| A1 | KMP SDK 骨架 + 会话层（jvm target） | 桌面过 `session::testing::session` |
| A2 | android target + 监听/token/TLS 规则 | 宿主 ws 连上模拟器，`websocket_conformance` 过；非私有来源被拒 |
| A3 | 插件框架 + 权限桥 + 辅助 APK + 首批能力（contacts、通知、Intent） | 真机：权限提示指向 APK；撤销 → 使用者停止；预算到限自恢复 |
| A4 | dev 循环（KSP、上传、`new --lang kotlin`、`check`） | 改插件 → 上传 → 重载全流程可用 |
| A5 | 中继/出拨、拓扑 B 评估、Accessibility 等能力 | 另立设计 |

## 10. 开放问题

- Doze 可靠性随厂商差异大；如实文档化，必要时事件缓冲。
- 上传端点滥用面：token 已认证；公网暴露场景要求 wss + 更强配对。
- 撤销对账 30 s 周期 vs 关键操作时核对。
- 拓扑 B 增量：A 的 Kotlin 层全复用（会话层走现有 `transport::memory`），增量 = core 的 NDK 交叉编译 + JNI 引导 + `.so` 打进 AAR + Play DCL 披露——有真实 on-device 宿主需求再立项。
