# 远端租约：接管与重连（TLA+）

[English](README.en.md)

用 TLC 穷举远程运行时的租约、新连接接管、link 重连与停止的交错。对应 issue #210 与 #615 的并入项，质量规范 Q11.2、Q6.13.3，现状 §7.2（风险 E4、E5）。两个模型：

- `RemoteLease.tla`：监听的远端运行时，和拨号连它的两个控制方 link（issue 的原始范围）；
- `AccessPoint.tla`：远端拨入 `rutis-host/listen` 的一行：握手并行、每个 id 一个 session、同 id 的新连接顶替旧的，以及拨出方的重连规则。

**对账（2026-11-09）。** 本模型现在覆盖 [#615](https://github.com/arcships/rutis/issues/615) 的四项：接管顺序里的鉴权一步（`Auth = "step"`）、旧租约清理的截止（`Cleanup = "deadline"`）、应用托管停止后再开始（`MaxRounds`，性质 `RestartIsFresh`），以及静态监听 link 被吊销后的注册与 4003（`access-static-revoke`）。对应表补了 Swift / Kotlin / 应用托管。仍未并入 #615 的：故障之后的活性（`MaxFaults` 配置）、Cordis 节点连接（`bridge/link.mjs`）的退避与接管、Kotlin 接管时新连接心跳的起算点、Node / Bun 强杀时间改为跟随 `RUTIS_SHUTDOWN_TIMEOUT`，以及 `RutisApp.kt:142-144` 在 start / stop 交错下的线程问题（#630 的 Fray）。吊销钩子在 0.19 已删除（#273），静态监听 link 的配置按追踪表的规则（DIA-5-7、DIA-5-10、DIA-5-11、DIA-5-13）建，当前没有实现可对。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 控制方（`rutis-host/dial` 行）：拨号、退避重连、按失败类型等待、停止 | [远程插件](../../docs/design-remote-plugins-2026-10-03.md) §3.2；[拨出稿](../../docs/design-remote-runtime-dial-2026-10-14.md) §六；[远程会话](../../docs/design-remote-session-2026-10-28.md) §4.1 | `crates/rutis-host/src/connect/dial.rs`：拨号、握手、装上会话、等它卸载、退避 |
| 运行时的租约与接管顺序（鉴权 → 关闭旧会话 → 清理旧租约 → 回应新 hello）；清理的截止 | 远程插件 §4.4；[远程会话](../../docs/design-remote-session-2026-10-28.md) §4.3 | `python/rutis/rutis/__main__.py` `serve`；`go/rutis/internal/runtime/listen.go` `listen`；`node/rutis-runtime/src/serve.mjs` `serve`（Bun 的 `src/remote.ts` 同）；Swift `Remote/Listen.swift`、`Remote/App.swift`；Kotlin `internal/remote/Listen.kt` |
| 应用托管：停止后再开始，stop 期间的 start 在清理后生效；拨号次数与轮次从头数 | [Swift SDK](../../docs/design-swift-sdk-2026-10-12.md) §七；[Android SDK](../../docs/design-android-runtime-2026-10-12.md) §五 | `RutisApp.start` / `stop`：Kotlin `RutisApp.kt`、Swift `Remote/App.swift` |
| listen：握手并行，同 id 顶替（旧 session 卸载完 → 旧 Channel 以 4002 关闭 → 装新 session） | [远程会话](../../docs/design-remote-session-2026-10-28.md) §4.2 | `crates/rutis-host/src/connect/listen.rs`：`Listening::run`、`Replaceable` |
| listen 的静态 link 被吊销：保留注册、`Waiting { AuthRejected }`、之后同 id 的连接在 hello 后以 4003 关闭 | [拨出稿](../../docs/design-remote-runtime-dial-2026-10-14.md) §五、§六；[追踪表](../../docs/formal-trace/rules/protocol-remote.md) DIA-5-7 / 5-10 / 5-11 / 5-13 | 0.19 删除吊销钩子（#273），没有实现可对；断开现有连接靠停掉那一行 |
| 拨出方的重连规则 | 拨出稿 §六 | `python/rutis/rutis/dialing.py` `keep_dialing`；`go/rutis/internal/runtime/dial.go`；`node/rutis-runtime/src/dialing.mjs`；`bun/rutis-bun/src/dialing.ts`；Swift `Remote/Dial.swift`；Kotlin `internal/remote/Dialing.kt`（Cordis 节点连接不在表里，#615） |

## 模型

两个模型共同的约定：每条连接每个方向按顺序送达，延迟任意长（比 issue 写的"最多延迟 2 步"更一般）；一方关闭连接后不再读它，发给它的帧丢弃（只有需要读它们的变异保留）。环境的动作（停止、超时、断线、吊销）和运行时的停止与开始任何时刻都可能发生，配置决定打开哪些。

### `RemoteLease.tla`

- **参与者**：控制方 `c1`、`c2`（rutis 的 `rutis-host/dial` 行），一个监听的远端运行时。连接 `<<c, n>>` 是 `c` 第 n 次拨号。
- **控制方**（`connect/dial.rs`）：拨号成功后升级完成，运行时的监听器把连接排进队列；等运行时的 hello（握手），收到后装上会话、状态 `Ready`；之后 loader 经会话装载行（`load` 帧）。会话结束（被接管 4002，或运行时结束会话）按可重试处理，退避后重连。停止（dispose）中止任务：关闭会话、取消重试。环境：拨号失败、握手超时或断线（`Lose`）、停止。
- **运行时**（Python `serve`、Swift `RutisApp` 的监听循环、Kotlin `Listening` 等）：一次只服务一个控制方。取下一个连接时若有当前会话，按 §4.4：① 鉴权（`Auth = "step"` 时是单独一步：连接先排队，运行时接受后才进入接管队列；拒绝只关闭这条连接，不动当前会话）；② 关闭旧会话的通道（4002），不再读它的帧；③ 清理旧租约（行全部卸载）；④ 新会话开始，回应 hello。会话的对端关闭时清理它的租约。
- **清理的截止**（`Cleanup = "deadline"`）：③ 分成"清理开始"与"完成或超时强杀"两步。强杀按旧进程消失处理：旧会话的行随它消失，之后才回应新 hello；每次运行最多一次强杀。`Cleanup = "instant"` 保留旧模型的一步清理。
- **停止后再开始**（`MaxRounds = 2`，应用托管的 `RutisApp.listen`）：停止关闭当前会话和已接受但未服务的连接、清理租约、不再接受拨号；开始开出新一轮。停止清理期间请求的开始在清理完成后生效；旧一轮的会话和排队连接不会在新一轮被服务（`RestartIsFresh`）。一次运行最多停一次，且只在还有下一轮时。
- **重连次数**：每个控制方最多拨 4 次（3 次重连）；新配置按 2–4 次，见"结果"。

### `AccessPoint.tla`

- **参与者**：拨出的远端 `d1`、`d2`，自报同一个 id（`SameId`）或各自的 id；rutis 侧是一行 `rutis-host/listen`。
- **拨出方**（`dialing.py`）：拨号即连接送到 listen、开始握手。收到 rutis 的 hello 即"connected"；连接被关闭（4002、断线、握手失败）后等待再拨。停止后不再拨号。
- **listen**（`Listening::run`）：
  - 握手在 `JoinSet` 里并行：`HandshakeDone`（回 hello，远端的插件这时一个都还没注册）或 `HandshakeFails`（关闭这条连接，什么别的都不动）。
  - `Join`：循环取一个握手完成的连接。这个 id 没有 session 就装上；有就把旧连接的 closer 标为 replaced、从 `live` 拿掉、开始卸载旧 session，然后循环停在 `unload().await` 上，别的什么都不做。
  - `Replace`：旧 session 卸载完（`Unloaded`：远端插件全部卸载，Channel 最后关闭，因为标了 replaced 所以是 4002），装新 session。
  - `Register`：装上的 session 注册远端的插件。
  - `EndSelf`、`Unloaded`、`ReadEnding`：session 自己结束（远端关闭或断线），卸载；循环读到它的结束时，只有 `live` 里还是它（同一个 serial）才删掉。
- **吊销**（`Faults = {"revoke"}`）：环境吊销一个 id。已装上的 session 卸载，Channel 以 4003（模型里的 `"rejected"` 帧）关闭，拨出方按认证失败等待（`outcome = "authRejected"`）。监听行保留它的注册（`registered`），所以之后这个 id 的连接照样握手、收到 rutis 的 hello，然后在 `Join` 里以 4003 关闭，不装 session、不注册插件。吊销发生在会话已装上、拨出方已收到 hello（`ready`）或不在握手途中时；握手途中的吊销不在模型里。
- **重连次数**：每个拨出方最多拨 4 次（3 次重连）；打开断线（`lose`）的配置为 3 次，全部故障一起的为 2 次，静态吊销的为 2 次，见"结果"。

## 简化

- 时间不建模：退避、30 秒、握手 10 秒、"连接稳定 60 秒后重置退避"都只体现为"之后某个时刻"。超时是任意时刻都可能发生的 `Lose`，比真实的计时更一般。清理的截止也一样：进入清理后，"完成"和"超时强杀"都是可发生的动作，没有计时；每次运行最多一次强杀（`ghost.killed`）。强杀按旧进程消失处理，旧会话的行随之消失（变异 `killLeavesRows` 是反例）；真实实现里强杀与清理线程的竞态细节不建模。
- 应用托管的停止与开始：`MaxRounds = 2`，一次运行最多停一次（`ghost.stopped`），且只在还有下一轮时停；运行时的循环空闲时才停，接管进行中的停止不建模。停止的清理一步完成（真实实现的 `shutdownTimeout` 到时会强杀，见上一条）；"停止清理期间请求开始"建成 `pending`，清理后开下一轮。`RutisApp.kt:142-144` 的线程交错属于 #630，不在模型里。
- 有限的重连次数是模型的边界，不是实现的行为。活性只在没有任何故障的运行里检查；在那里每个行为都是有限的（每次拨号用掉一次额度），所以 `WF_vars(Next)` 足以让每个能走的步骤走到。两个控制方配置了同一个运行时（或两个拨出方用同一个 id）时会互相顶掉（拨出稿 §六 已写明）；模型里额度用完时最后一个连上的持有租约，实现里靠退避把节奏放慢到每方约 30 秒一次。`lease-app-restart` 不查活性：运行时停止期间的拨号失败也消耗额度，公平性不能阻止"额度用完后才开始"，这是模型的边界。
- `RemoteLease`：控制方的 hello 不建模（运行时开始会话时读它，没有别的作用）。
- `AccessPoint`：token 路由（`Router`）不建模：拨号即送到这一行；`registered` 是一个布尔，表示这一行的注册在不在。远端插件只建模"全部注册"与"全部卸载"两个状态，逆序卸载合成一步；远端经会话的调用不建模。`old.closer.close(Replaced)` 在 `Unloaded` 之后，Channel 已经关闭，模型里不单独成步。没有关闭码：`"close"` 是 4002 或普通关闭，`"rejected"` 是 4003。监听行自身的状态没有单独的变量：`registered` 表示注册还在，`Waiting { AuthRejected }` 记在拨出方的 `outcome = "authRejected"` 上。
- 每个动作是原子的。

## 不变式与活性

| 名字 | 模型 | 内容 |
| --- | --- | --- |
| `OneLease` | RemoteLease | 运行时最多一个活动租约：正在服务的会话和还没清理的行加起来只属于一个连接 |
| `OldCommandsStayOld` | RemoteLease | 一个会话的指令（`load`）只作用于这个会话的租约，旧会话迟到的指令不作用于新租约 |
| `StoppedStaysDown` | 两者 | 停止的 link / 拨出方不再拨号，也不留打开的连接 |
| `ReadyIsCurrent` | RemoteLease | 控制方只在当前、仍打开的连接上 `Ready`：放弃了的握手迟到的 hello 不会让它复活 |
| `CleanBeforeHello` | RemoteLease | 运行时进入"回应新 hello"之前，旧租约的行已经全部清掉（强杀也一样） |
| `RuntimeStopped` | RemoteLease | 停下的运行时不服务会话、不持有租约，停止前接受但未服务的连接也不留着 |
| `RestartIsFresh` | RemoteLease | 运行时服务或即将服务的连接属于当前一轮；控制方仍 `Ready` 的旧连接必须已被运行时关闭（它还没读到关闭） |
| `OnePluginSetPerId` | AccessPoint | 同一个 id 永远不会有两个 session 的远端插件同时注册着：新 session 只在旧的完全卸载之后才注册 |
| `StaysRegistered` | AccessPoint | 吊销一个 id 不撤掉监听行的注册：之后的连接还能到达它 |
| `RevokedStaysOut` | AccessPoint | 被吊销的 id 不再有装上的 session：之后同 id 的连接在 hello 之后以 4003 关闭 |
| `EventuallyHeld` | 两者，无故障 | 最终有一个控制方持有租约（`RemoteLease`：某个控制方 `Ready`，运行时正在服务它的连接；`AccessPoint`：每个 id 有装上的 session、远端插件已注册，且它的拨出方已连上） |
| `EndsHeld` | 两者，无故障 | `[]<>Held`：在有限的运行里即"结束时有人持有租约"，排除"互相顶掉后谁都没拿到" |
| `EventuallyRevokedWaits` | AccessPoint，吊销 | 一个 id 被吊销后，拨出方最终以认证失败等待（`Waiting { AuthRejected }`） |

## 结果

TLC 2.19（tla2tools v1.7.4），Java 21，32 核（`-workers auto`）。"通过"指穷举完成、所列性质全部成立；这些行的状态数是确定的，时间随机器负载浮动。

| 配置 | 内容 | 结果 | 状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `lease-healthy` | 2 个控制方，各 4 次拨号，无故障；含活性 | 通过 | 791 | 1 s |
| `lease-faults` | 同上，停止、拨号失败、超时 / 断线任意时刻发生 | 通过 | 1,050,999 | 7 s |
| `lease-takeover` | 1 个控制方，最多 3 次拨号（2 次接管）；鉴权是单独一步，先于关闭旧会话 | 通过 | 864 | 1 s |
| `lease-cleanup-deadline` | 1 个控制方，2 次拨号（1 次接管）；清理有截止，强杀旧会话 | 通过 | 158 | 1 s |
| `lease-app-restart` | 1 个控制方；运行时停一次、开下一轮；停止清理期间可以请求开始 | 通过 | 191 | 1 s |
| `access-healthy-same` | 2 个拨出方同一 id，各 4 次拨号，无故障；含活性 | 通过 | 16,020 | 3 s |
| `access-healthy-distinct` | 各自的 id；含活性 | 通过 | 64 | 2 s |
| `access-lose` | 同一 id，各 3 次；拨号失败、握手失败、断线、session 结束 | 通过 | 6,350,873 | 36 s |
| `access-all` | 同一 id，各 2 次；全部故障（断线、停止） | 通过 | 129,131 | 3 s |
| `access-static-revoke` | 1 个拨出方，2 次拨号；id 在装上的会话之后被吊销；含活性 | 通过 | 38 | 1 s |

全部 21 个配置（含下文的变异）约 1 分钟。

打开断线的配置限制拨号次数：只开断线时每方 3 次，断线和停止一起时 2 次（3 次时 1,044 万个状态）。

## 实现里的反例（已修复）

TLC 找到过两处握手期间吊销身份仍建立会话（link 提供了 Peer、接入点在接管期间接受了被吊销的 id），已修复（#210）。身份吊销（`Revocations` / `Revocable`，#273）在 0.19 删除，配置 `lease-before-fix`、`access-before-fix`、`access-revoke-same`、`access-revoke-distinct`、`access-mut-revoke-cuts-all` 与性质 `RevocationScoped` 随之删除。`RevokedStaysOut` 随 #615 按追踪表的规则（DIA-5-7、DIA-5-10、DIA-5-11、DIA-5-13）重新加入（`access-static-revoke`），对的是规则，当前实现没有吊销钩子。

0.19 删掉接入点（`access_point.rs`）和注册代次（`registration.rs`），`AccessPoint.tla` 改为按 `rutis-host/listen` 的代码重建；配置 `access-registration`、`access-mut-route-by-id`、`access-mut-no-gen-check` 与性质 `OneInstancePerId`、`OldCallsStayOld`、`NoStaleHandOver` 随之删除。

除此之外 TLC 没有找到与模型不符的反例：§4.4 的接管顺序（含鉴权一步）、listen 的顶替顺序、停止时取消重试、清理超时强杀不留下旧租约、应用托管停止后再开始的新一轮都不服务旧连接，都与模型一致。新增配置没有对实现运行；Swift / Kotlin / 应用托管的顺序是照代码读出来的（见上表）。

## 变异：确认模型不是空的

每个主要的不变式和活性性质配一个故意引入的错误，TLC 必须找到反例（`expected.txt` 里的 `violated:<名字>`；活性为 `violated:liveness`）；`CleanBeforeHello` 由 `lease-mut-kill-leaves-rows` 覆盖，`RuntimeStopped` 与 `EventuallyRevokedWaits` 没有单独的变异：

| 配置 | 变异 | 被违反的性质 |
| --- | --- | --- |
| `lease-mut-start-before-clean` | 运行时在清理旧租约之前回应新 hello（§4.4 的 ③ ④ 对调） | `OneLease` |
| `lease-mut-read-old` | 运行时继续读被接管的会话，把它的指令作用到当前租约（共用一张行表） | `OldCommandsStayOld` |
| `lease-mut-stop-keeps-retry` | 停止一个正在等待重连的 link 时没有取消重试 | `StoppedStaysDown` |
| `lease-mut-late-hello` | 已放弃的握手迟到的 hello 让 link 进入 `Ready` | `ReadyIsCurrent` |
| `lease-mut-wait-for-old` | 接管时运行时停止读旧会话，但不关闭它，等旧控制方自己断开（无故障运行） | 活性（`EndsHeld`） |
| `lease-mut-kill-leaves-rows` | 清理超时强杀旧会话时，旧租约的行没有随进程消失 | `OneLease` |
| `lease-mut-stale-round` | 运行时停止时保留已接受但未服务的连接，新一轮把它们服务了（被取代的运行服务最新一轮） | `RestartIsFresh` |
| `access-mut-accept-before-release` | 顶替时先装新 session，再卸载旧的（§4.2 的 3 挪到 1 之前） | `OnePluginSetPerId` |
| `access-mut-stop-keeps-retry` | 停止一个正在等待再拨的拨出方时没有取消再拨 | `StoppedStaysDown` |
| `access-mut-revoke-installs` | 被吊销 id 的连接握手完成后照样装上 | `RevokedStaysOut` |
| `access-mut-revoke-unregisters` | 吊销时把监听行的注册也撤了，之后的连接在升级时 403，到不了监听行 | `StaysRegistered` |

每个变异都在 2 秒内找到反例（几十到几千个状态）。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/remote-lease/run.sh                     # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/remote-lease/run.sh lease-faults        # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/remote-lease/run.sh       # 限定 TLC 的线程数
```

`run.sh` 按名字选模型（`lease-*` → `RemoteLease.tla`，`access-*` → `AccessPoint.tla`），把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；日志和反例写在 `TLC_OUT`（默认一个临时目录）。CI 的 `specs` 任务在改了 `specs/**` 的 PR 和每晚的完整运行里跑它（[CI](../../docs/ci.md) §4）。

## 维护

远程插件 §3.2、§4.4、拨出稿 §五、§六 或远程会话 §4.2、§4.3 修改时，或 `connect/dial.rs`、`listen.rs`（`Listening::run`）、各语言监听端的接管顺序与清理截止、`RutisApp.start/stop`、拨出方的重连规则改变时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。静态监听 link 的吊销没有再实现；追踪表的 DIA-5-7 / 5-10 / 5-11 / 5-13 修改时改 `access-static-revoke`。
