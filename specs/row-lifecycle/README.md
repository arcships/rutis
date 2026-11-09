# 行生命周期：代次、两阶段、服务、租约、崩溃与多运行时（TLA+）

[English](README.en.md)

用 TLC 穷举一个运行时会话里的行从解析、装载到清理的生命周期，以及断线、同 id 重连、旧代卸载与新代装载、服务公告与撞名拒绝、宿主服务租约、运行时崩溃/重启和两个运行时的交错。对应 issue [#613](https://github.com/arcships/rutis/issues/613) 的行生命周期部分（并入了 [#625](https://github.com/arcships/rutis/issues/625)）；质量规范 Q11.2；设计见[远程会话](../../docs/design-remote-session-2026-10-28.md) §4.2、§4.3、[远程插件](../../docs/design-remote-plugins-2026-10-03.md) §4.4、[多语言运行时](../../docs/design-multilang-runtimes-2026-10-03.md) §四、§五、§六、[会话追踪契约](../../docs/formal-trace.md) §3.0。

这是同一个模型的两块，靠常量开关区分：原有 9 个配置（`row-healthy`、`row-reconnect`、`row-cleanup` 与 6 个变异）只打开第一块，语义与上一版相同（三个穷举完成的配置状态数逐字相同：73,768 / 191,830 / 110；反例配置的状态数本来就随搜索顺序变）；新配置逐个打开两阶段、服务、依赖、租约、崩溃和第二个运行时。不建事件登记与 `rows.ended` 的帧序（#613 的事件表）、调用生命周期和节点托管行。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 宿主每个运行时一条会话，丢了才重拨；行通过会话加载，会话没了就停下等下一个 | 远程会话 §4.2；远程插件 §4.4 | `crates/rutis-host/src/connect/dial.rs` `Dialing::run`（`installed.unloaded().await` 之后才退避重拨）；`crates/rutis-loader/src/runtime/sessions.rs` `SessionRowsPlugin`（行等会话，会话结束就停下） |
| 宿主按代次键装载一行：每次装载一个新键，旧代的迟到回复不作用于新代 | 质量规范 §11.2；#625 `OwnGenerationOnly` | `crates/rutis-loader/src/runtime.rs:719-727`（`ctx.instance()` + `GENERATION`）；`crates/rutis-bridge/src/runtime/process.rs` 的 `on_row_ended` / `on_row_failed`（按键登记，`unload_row` 之后移除） |
| `rows.ended`：行自行结束，宿主结束这一行并把它卸掉（不再装载） | 远程会话 §4.3；`loader/self-dispose` | `crates/rutis-bridge/src/runtime/process.rs:531-541`；`node/rutis-runtime/src/runner.mjs:379-385`（`endRow`：先 `unloadRow`，再发 `rows.ended`） |
| `rows.failed`：行的代码失败，宿主重启它（卸载旧代、装载新代），运行时保留这一行 | 远程会话 §4.3；#688 | `crates/rutis-bridge/src/runtime/process.rs:543-568`；`node/rutis-runtime/src/runner.mjs:1120-1135`（`rows.failed` 之后行仍加载） |
| `rows.unload` 幂等：行已不在就只回答 | 行契约 `rows.v2` | `crates/rutis-bridge/src/session/plugin/rows.rs:197-207`；`node/rutis-runtime/src/runner.mjs:743-746`（`unloadRow` 找不到就返回） |
| 运行时顶替：关闭旧会话、不再读它的帧、清理旧会话的全部行、然后回答新 hello | 远程插件 §4.4；远程会话 §4.2 | 运行时监听端（Python `serve`、Go `listen.go`、Node `serve.mjs` 同一顺序）；模型按 Node 的行处理建（`runner.mjs`） |
| 会话结束（断线或顶替）时清理它的全部行，每行一次 | 远程插件 §4.4；`loader/remote-runtime-rows`、`loader/row-unload-leaves-nothing` | 运行时的 `serve`/`listen`；`crates/rutis-bridge/src/session/plugin/rows.rs` 的 `Server`（每个会话一张已装载表） |
| 两阶段放行：进程起来先提供 `<语言>Runtime`；宿主用它刷新这些行的解析结果；再提供 `<语言>RuntimeRows`，行才按完整依赖等待和启动 | 多语言运行时 §四、§六；M1 §6.2；#625 `NoProvisional` | `crates/rutis-loader/src/runtime/sessions.rs` 开头的两阶段（`SessionRowsPlugin` 先等待、重新解析、再提供 `SessionRows`）；各语言运行时插件提供两个服务；刷新用 loader 的 `entries()` + `reload(id)` |
| 服务公告：运行时在装载期间报出这一行提供的服务名；宿主应用公告，第二个还在运行的提供者被拒绝；行等它注入的名字出现，名字消失就停下 | 多语言运行时 §五、§六；M1 §5.2、§5.3、§14；#625 `ServicesBeforeRunning`、`OneProviderPerName` | `crates/rutis-loader/src/runtime.rs` 的 `Refusals`、`projection`、`announced`、`JsRow::start`；`crates/rutis-bridge/src/runtime/process.rs` 的服务表 |
| 宿主服务租约：行装载时租用它注入的宿主服务，清理时在 `rows.unload` 之后释放；下一次装载等释放 | 多语言运行时 §五；M1 §5.4；#625 `LeaseWithinRow` | `crates/rutis-loader/src/runtime.rs:623-665`（`lease_host_for`、清理里的 `release`）、`:1027-1031`（`release`）；`HostLease` |
| 进程崩溃：两个服务都撤销、不自动重启，是否重启由应用决定；重启后行重新解析 | 多语言运行时 §四 | 各语言运行时插件；`crates/rutis-loader/src/runtime.rs` 的 `wait_for_the_runtime`（装载中运行时结束的行等待它的结束） |
| 两个运行时不互相等待：一个运行时的启动/会话/行不依赖另一个 | 多语言运行时 §四（运行时不依赖任何行的服务）；`loader/runtimes-do-not-wait-for-each-other` | 运行时插件只依赖启动本身；门控落在消费行上（`crates/rutis-loader/src/runtime.rs` 的 `gated` / `row_inject`） |

## 模型

- **参与者**：一个宿主（loader 的行、拨号、服务表、宿主服务租约）和 1–2 个运行时进程（监听方、行表、两个放行阶段）。宿主对每个运行时各有一条连接，连接断了就重拨；运行时一次只服务一个会话，新连接到来时顶替旧的。行的归属由配置的 `RowSet2` 决定：不在其中的行属于运行时 `r1`。
- **连接**：每个运行时 `1..MaxConns`，每条连接两个方向各自 FIFO、延迟任意长；关闭了连接的一方不再读它，发给它的帧丢弃。
- **两阶段**：`StartRuntime(rt)` 让进程从 `down` 到 `up`（提供 `<语言>Runtime`）；`Refresh(rt)` 让宿主重新解析这个运行时的行并把阶段置为 `rows`（提供 `<语言>RuntimeRows`）；`HLoad(r)` 要求本运行时的阶段是 `rows` 且这一行的解析是 `final`。崩溃把阶段打回 `down`、把该运行时的行打回 `provisional`；再次启动（受 `Restarts` 限制）后必须重新刷新。
- **宿主**（`HDial`、`HGreeted`、`HLoad`、`HLoaded`、`HUnloaded`、`HLoadFailed`、`HAnnounce`、`HEnded`、`HFailed`、`HLoadRefused`、`HLose`）：
  - `HLoad(r)` 用 `<<运行时, 会话, 行, 代次>>` 作键发 `rows.load`；一个会话里一行最多装载 `MaxLoads` 次（失败重启用掉一次）。装载时同时记下这一代租用的宿主服务。
  - `HLoaded` 处理装载回复；若这一代的服务公告被拒绝，行不进入 Running，而是进入卸载。`HUnloaded` 处理卸载回复：清掉这一代的服务公告、释放租约、行回 `idle`（`leaseLeak` 变异跳过释放）。`HLoadFailed` 处理 `loadfailed`：释放租约、行回 `idle`。
  - `HAnnounce` 应用一条服务公告：名字已被另一个还活着的键占用就拒绝（后来的行失败，先来的不受影响）；公告在这一行进入 Running 之前处理（`announceAfterLoad` 变异让运行时先回 `loaded`，用来检查 `ServicesBeforeRunning`）。
  - `HLoadRefused` 让被拒绝的那一代引起的下一次重启失败一次，然后清掉拒绝记录（`refusalSticky` 变异让它一直留着）。
  - `HEnded` 只结束键等于这一行当前代次的行，并像真实卸载栈一样再发一次 `rows.unload`；`HFailed` 把当前代次置为卸载中并等回复。`HLose` 断开当前连接，这一会话上的行回 `idle`；这一会话的服务公告、租约和别的运行时上因此失去依赖的行一并处理。
  - 依赖消失：提供者被卸载时，宿主在同一个动作里清掉名字，并把仍在 `loading`/`loaded`、因此少了依赖的行置为卸载（`Starved`）；行之后回到 `idle` 等名字回来（`keepOnDepGone` 变异不做这一步）。
- **运行时**（`TakeUp`、`Answer`、`EndSession`、`RLoad`、`RFailLoad`、`RUnload`、`EndSelf`、`SendEnding`、`RFail`、`Sweep`、`MarkSwept`）：
  - `TakeUp(s)`：读到新连接的 `hello`，关闭旧会话（不再读），开始清理旧会话的行。`Answer(s)`：只有本运行时更早会话的行都清理完才回答新 `hello`，所以两个会话的行不会同时存在；它不要求任何别的运行时（`mutualWait` 变异要求其他运行时也 fully ready 且全部 swept）。
  - `RLoad` 在读 `rows.load` 时把这一行提供的每个名字作为 `announce` 帧放进同一会话的回队列，然后才放 `loaded` 回复（`announceAfterLoad` 变异反过来），所以宿主按帧序先处理公告再标记 Running。`RFailLoad` 可选地回 `loadfailed`。
  - `RUnload` 幂等，行在就清理一次、不在就只回答；`EndSelf` 是插件自行结束，行消失而 `rows.ended` 还要再发；`SendEnding` 只在会话还开着时发；`RFail(k)` 每代最多发一次 `rows.failed`。
- **崩溃**：`HCrash(rt)` 清掉这个运行时所有已装载的行（每行清理计数加一次）、租约和服务公告，关闭它的全部会话，阶段回 `down`，该运行时的行回 `idle`、解析回 `provisional`；`StartRuntime` 的次数受 `Restarts` 限制，宿主重新拨号受 `MaxConns` 限制。一个运行时的崩溃只动它自己的状态（`RuntimeIsolation` 体现在动作里；配置检查另一个运行时的行仍能 Running）。
- **顶替的顺序**：`TakeUp` 关旧、停止读，清行（`Swept`），`Answer` 回答新 `hello`。`Answer` 的前置条件仍是这个模型检查的核心规则。

## 简化

- 规模是有意的有限：旧配置 1 个运行时、1–2 行、最多 3 条连接；新配置最多 2 个运行时、每运行时 2 行、每个会话每行最多 2 代、1 次崩溃、最多 2 次启动、2 次拨号。真实的重复崩溃、退避和 30 秒/60 秒计时不建：环境只体现为“某个时刻断线/崩溃”，比真实计时更一般。
- 握手只建“一端 `hello`、另一端回答 `hello`”：版本、能力、鉴权、10 秒截止不建（`remote-lease` 覆盖顶替与握手的其它部分）。
- 宿主→运行时的帧只有 `rows.load` 和 `rows.unload`；`rows.update`、`rows.schema`/`mount`、事件、调用链都不建。运行时→宿主只有 `hello`、`announce`、`loaded`、`loadfailed`、`unloaded`、`ended`、`failed`。
- 服务只有一个名字、宿主服务只有一个名字：配置用 `ServiceName` / `ProviderRows` / `ConsumerRows` / `HostServiceName` / `LeaseRows` 描述谁提供、谁注入。多名字、方法形状、服务版本（`NewerWins`）、服务更新和撤下公告、`WithdrawBeforeUnload` 不建；`hosts.provide` / `hosts.withdraw` 的帧和计数也不建，租约只表现为每一代持有哪些名字。
- 撞名拒绝按“被拒绝的那一代”记：它让紧接着的一次重启失败，然后清掉。实现里的拒绝记录挂在 fiber 实例上，作用域更细（只让引起它的那次重启失败；在等依赖而不是 apply 的重启不会失败）；模型近似为“一代一次”，`refusalSticky` 变异只锁定“不能把旧拒绝带给后面的代”。
- 行的装载失败（`loadfailed`）只表示运行时装载这一行失败；行的代码在运行中失败是 `rows.failed`，两者分开。`rows.update` 的易变字段就地提交（SYN-4-8）不建。
- 依赖出现只是 `HLoad` 的前置条件；依赖消失按“提供者卸载/掉线/崩溃时，宿主在同一个动作里让消费行卸载”建。刷新任务被重启打断后重做（REM-4.3-3）、刷新失败只失败这一行（M1-6.2-4）不建。
- 崩溃是一个原子动作（进程死、会话结束、行清理一起发生）；各语言卸载栈的差异、`shutdown` 截止、Node/Bun 的强杀时间不建（#625 的其余部分）。
- 宿主一行是一个 fiber：`idle | loading | loaded | unloading | gone`，键是 `<<运行时, 会话, 行, 代次>>`；loader 的完整解析器、配置分层、重启策略的计时不建。“行自行结束 = 禁用、不再装载”按 `loader/self-dispose`。
- 旧会话队列里还没读的帧留在模型里（设计里它们随连接关闭被丢弃）；设计侧由 `CanRead` 保证不再读，只有 `readOld` 变异会去读它们。
- 宿主在丢掉会话后不再读它，所以“旧会话的迟到 `rows.ended`”不会落到新会话的宿主上；宿主一侧的同一规则体现在同一会话里的代次竞态（`OwnFramesOnly`），这正是 a88fb65b 修的缺陷。
- 内存序、任务调度、真实计时、帧编码不建模；每个动作原子。

## 不变式与活性

| 名字 | 内容 |
| --- | --- |
| `OneRowPerName` | 运行时里一个名字的行最多一个代次处于加载状态：旧会话的行清理完之后新会话才装载；失败重启等旧代卸载的回答，不并存 |
| `NewSessionOnlyAfterSweep` | 一个会话被回答（`hello`）时，本运行时所有更早会话的行都已经清理完 |
| `NoRowsAfterSweep` | 一个会话被清理过一次之后，它的帧不会再装载任何行：运行时不再读旧会话的指令 |
| `OwnFramesOnly` | 宿主只对“这一行当前代次”的 `rows.ended`/`rows.failed` 起作用：旧代的迟到帧不结束、不重启新代 |
| `CleanupOnce` | 一个行代次最多清理一次：会话清理、`rows.unload`、行自行结束和崩溃不会把同一行处置两次 |
| `NoProvisional` | 行只在运行时完成两阶段、且自己的解析是 final 之后才装载/运行：不在临时解析上装载 |
| `ServicesBeforeRunning` | 宿主把一行记为 Running 时，这一行装载期间发出的服务公告都已处理：公告在装载的同步调用链里到达（帧序上先于 `loaded` 回复） |
| `OneProviderPerName` | 两个行不会同时以同一个名字 Running：后来的公告被拒绝，它的装载失败 |
| `RefusalOnce` | 一个被拒绝的代次引起的重启最多失败一次：拒绝不会带给后面的代 |
| `LeaseWithinRow` | 租约只属于这一行的当前代次；卸载回复之后下一次装载之前必须已释放；失败的装载不留租约 |
| `LoadedNeedsDeps` | Running 的行注入的每个名字都还在：名字消失时宿主同步地把它置为卸载，不会留着一个缺依赖的行在跑 |
| `EndedLeadsToSwept` | 活性：一个会话结束（断线、顶替或崩溃）之后，它的行最终被清理 |
| `RestartRecovers` | 活性：运行时崩溃且还能再启动、宿主还能再拨号时，它的行最终重新 Running |
| `RuntimesDoNotWait` | 活性：一个运行时崩溃（不再启动）时，另一个运行时的行仍然最终 Running |

## 结果

TLC 2.19（tla2tools v1.7.4），Java 21.0.12（Temurin），32 核，`-workers auto`。`通过` 指穷举完成、所列性质全部成立；被违反的配置找到第一个反例就停，状态数取决于搜索顺序，每次可能不同。整个目录约 1 分钟。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `row-healthy` | 2 行，1 条连接；行可失败、可自行结束；原有 5 个安全不变式 | 通过 | 73,768 | 1–2 s |
| `row-reconnect` | 2 行，3 条连接（2 次重连），可断线 | 通过 | 191,830 | 2–5 s |
| `row-cleanup` | 2 行，1 条连接，可断线；只检查活性 | 通过 | 110 | 1 s |
| `row-generations` | 2 行，1 条连接，两阶段，行可失败（每行两代）；`NoProvisional` 等 | 通过 | 419 | 1–2 s |
| `row-services` | 2 行都提供 `s`，两阶段，行可失败；公告、撞名拒绝、拒绝作用域 | 通过 | 2,643 | 2 s |
| `row-lease` | 2 行都租用宿主服务 `h`；行可失败、装载可失败；租约 | 通过 | 1,334 | 2 s |
| `row-deps` | `a` 提供 `s`、`b` 注入 `s`，`a` 可失败重启；依赖消失 | 通过 | 422 | 2 s |
| `row-restart` | 1 个运行时崩溃一次、再启动；宿主拨两次；旧行清理、新行装载 | 通过 | 1,232 | 1 s |
| `row-runtimes` | 2 个运行时各 2 行；`r1` 崩溃且不再启动，`r2` 照常；不互相等待 | 通过 | 1,988 | 1 s |
| `row-mut-accept-before-clean` | 顶替时不等旧会话的行清理完就回答新 `hello` | 违反 `NewSessionOnlyAfterSweep` | 121（找到反例即停） | 2 s |
| `row-mut-accept-before-clean-loads` | 同上，接着新会话装载同名行 | 违反 `OneRowPerName` | 662（找到反例即停） | 1 s |
| `row-mut-read-old` | 顶替后运行时继续读旧会话 | 违反 `NoRowsAfterSweep` | 235（找到反例即停） | 1 s |
| `row-mut-keyed-by-row` | 宿主按行名而不是键匹配 `rows.ended`/`rows.failed` | 违反 `OwnFramesOnly` | 118（找到反例即停） | 1 s |
| `row-mut-reclean` | 对已经不在的行再跑一次卸载清理 | 违反 `CleanupOnce` | 53（找到反例即停） | 1 s |
| `row-mut-keep-rows` | 运行时关闭断线的会话但保留它的行 | 违反活性 | 29（找到反例即停） | 0 s |
| `row-mut-load-provisional` | 行在临时解析、运行时还没放行行的时候就装载 | 违反 `NoProvisional` | 74（找到反例即停） | 1 s |
| `row-mut-announce-after-load` | 运行时先回 `loaded`，之后再报服务公告 | 违反 `ServicesBeforeRunning` | 23（找到反例即停） | 1 s |
| `row-mut-allow-clash` | 宿主接受第二个提供同名服务的公告 | 违反 `OneProviderPerName` | 128（找到反例即停） | 1 s |
| `row-mut-lease-leak` | 卸载/装载失败后不释放宿主服务租约 | 违反 `LeaseWithinRow` | 77（找到反例即停） | 1 s |
| `row-mut-keep-on-dep-gone` | 依赖消失后不停止消费行 | 违反 `LoadedNeedsDeps` | 201（找到反例即停） | 1 s |
| `row-mut-refusal-sticky` | 撞名拒绝一直留着，失败后面的重启 | 违反 `RefusalOnce` | 280（找到反例即停） | 2 s |
| `row-mut-mutual-wait` | 一个运行时的回答等另一个运行时完全就绪且全部清理 | 违反活性（`RuntimesDoNotWait`） | 84（找到反例即停） | 1 s |

## 变异与反例

每个被检查的性质都配一个故意引入的错误，TLC 必须找到反例（`expected.txt` 的 `violated:<名字>`；活性为 `violated:liveness`）：

| 配置 | 变异 | 违反 | 反例 |
| --- | --- | --- | --- |
| `row-mut-accept-before-clean` | `Answer` 不要求更早的会话已 `Swept` | `NewSessionOnlyAfterSweep` | 会话 1 装载了行，断线，拨会话 2；`TakeUp(2)` 关闭会话 1，`Answer(2)` 立即回答 `hello`，此时 `Swept(1)` 还是假 |
| `row-mut-accept-before-clean-loads` | 同上 | `OneRowPerName` | 接着会话 2 装载行 `a`，会话 1 的 `a` 还在运行：同名两代并存 |
| `row-mut-read-old` | `CanRead` 允许读已结束会话的帧 | `NoRowsAfterSweep` | 会话 1 上没被读的 `rows.load` 在会话被清理并 `MarkSwept` 之后才开始装载 |
| `row-mut-keyed-by-row` | `HEnded`/`HFailed` 按行名匹配，不看键的代次 | `OwnFramesOnly` | 行 `a` 卸载后插件才发 `rows.ended`；宿主已经装载了新代，读到旧代的 `ended` 后把新代结束 |
| `row-mut-reclean` | 对已不在的行也执行一次清理 | `CleanupOnce` | 行失败；插件自行结束（清理一次），随后的 `rows.unload` 发现行已不在，变异又清理一次：`cleanCount = 2` |
| `row-mut-keep-rows` | `Sweep` 关闭（断线后不清理行） | 活性（`EndedLeadsToSwept`） | 会话 1 装载了行后断线，运行时不清理：`Swept(1)` 永远不成立 |
| `row-mut-load-provisional` | `HLoad` 不要求 `stage = rows` 也不要求解析 final | `NoProvisional` | 初始阶段还是 `down`、行还是 `provisional`，变异让行直接 `loading`；`NoProvisional` 立即为假 |
| `row-mut-announce-after-load` | `RLoad` 先放 `loaded` 回复、再放 `announce` 帧 | `ServicesBeforeRunning` | `sentAnn` 已含 `s`，宿主的 `processedAnn` 还是空，`HLoaded` 已把行标成 `loaded` |
| `row-mut-allow-clash` | 名字已被另一个活着的键占用时仍接受公告 | `OneProviderPerName` | `a`、`b` 都提供 `s`；两次公告都被应用，两次装载都回 `loaded`，宿主上两行同时 Running |
| `row-mut-lease-leak` | `HUnloaded`/`HLoadFailed` 让行回 `idle` 但不释放租约 | `LeaseWithinRow` | 行失败并卸载（或装载失败）；行已 `idle`，`leaseHeld` 仍非空 |
| `row-mut-keep-on-dep-gone` | 服务消失时不停止消费行 | `LoadedNeedsDeps` | `a` 失败并卸载，`s` 从表里清掉；`b` 仍 `loaded`，它的依赖已不在 |
| `row-mut-refusal-sticky` | 拒绝记录不按代清掉 | `RefusalOnce` | `b` 撞名被拒；第一次重试失败（计数 1），记录留着，第二次重试又失败（计数 2） |
| `row-mut-mutual-wait` | `Answer` 额外要求其他运行时 `stage = rows` 且全部会话 swept | 活性（`RuntimesDoNotWait`） | `r1` 崩溃且不再启动，`r2` 的回答永远等不到 `r1` 就绪/清理，`r2` 的行永远不 Running |

`row-mut-accept-before-clean` 与 `row-mut-read-old` 也分别会让 `OneRowPerName` 或 `NewSessionOnlyAfterSweep` 失败；分开列配置是为了让每个反例只对应一个结论（同 `event-close` 的做法）。

## 结论

- 原有结论不变：按远程插件 §4.4 的两条规则（顶替时不再读旧会话、清理完旧会话的行才回答新 `hello`），在穷举范围内不出现同一个名字的两代行；旧会话剩下的帧不会在清理之后再装载行；断线后它的行被清理；宿主按键处置结束与失败，一行只被清理一次。
- 新块在穷举范围内成立：行只在两阶段完成且解析 final 之后装载；装载期间的服务公告在 `loaded` 回复之前处理，宿主不会把还欠公告的行标成 Running；两个行不会同时以同一个名字 Running，第二个被拒；拒绝只让引起它的那一次重启失败；宿主服务租约只属于当前代、在卸载/失败回复之后释放，下一代替换不了它；运行时的旧行在崩溃时只清理一次并能在重启后重新装载；服务消失时消费行同步停下；一个运行时崩溃且不再启动时，另一个运行时的行照常 Running。
- 新块没有证明的（#625 仍未覆盖）：服务版本与 `NewerWins`、行自己服务的 `WithdrawBeforeUnload`、服务更新/撤下公告、方法形状、`hosts.provide`/`withdraw` 的计数与帧序、拒绝的实例作用域精确语义（d94aae9）、装载中结束的行（54a7e824）、投影中途关闭（#372）、刷新任务取消与重做（REM-4.3-3）、各语言崩溃清理差异与 `shutdown` 截止、`rows.update` 的就地提交、`rows.ended` 与 `events.*` 的帧序（#613 的事件表）、调用生命周期、节点托管行、真实计时。本目录不能当作完整行协议的证据。
- 模型里“服务公告在同步调用链内到达”表现为同一会话的帧序（`announce` 先于 `loaded`），不是真正的跨进程调用链；真正的同步调用顺序在 `cross-runtime-sync` 和后续模型里。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh                    # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh row-runtimes       # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh      # 限定 TLC 的线程数
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；日志和反例写在 `TLC_OUT`（默认一个临时目录）。CI 的 `specs` 任务矩阵包含 `row-lifecycle`（见 [CI](../../docs/ci.md) §4）。

## 维护

远程会话 §4.2、§4.3、远程插件 §4.4、多语言运行时 §四、§五、§六 修改时，或 `crates/rutis-loader/src/runtime.rs` 的行键/`Refusals`/租约/投影、`crates/rutis-bridge/src/runtime/process.rs` 的 `rows.ended`/`rows.failed`/`unload_row`/服务表、运行时监听端的顶替顺序与两阶段、`node/rutis-runtime/src/runner.mjs` 的 `endRow`/`unloadRow` 改变时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。新增特性时先加常量（默认关闭），保持原有配置的语义和状态数不变。
