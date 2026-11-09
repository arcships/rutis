# 会话中的行：断线、同 id 重连与行的清理（TLA+）

[English](README.en.md)

用 TLC 穷举一个运行时会话里的行从加载到清理的生命周期，以及断线、同 id 重连时旧会话与新会话的交错。对应 issue [#613](https://github.com/arcships/rutis/issues/613) 的行生命周期部分；质量规范 Q11.2；设计见[远程会话](../../docs/design-remote-session-2026-10-28.md) §4.2、§4.3、[远程插件](../../docs/design-remote-plugins-2026-10-03.md) §4.4、[会话追踪契约](../../docs/formal-trace.md) §3.0。这是行生命周期模型的第一块：只回答“一个会话的行、断线、重连和运行时顶替”，不建代次/拒绝/服务公告/宿主服务租约（#625 的范围），也不建事件登记与 `rows.ended` 的帧序（#613 的事件表）、调用生命周期和节点托管行。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 宿主一次只有一条会话，丢了才重拨；行通过会话加载，会话没了就停下等下一个 | 远程会话 §4.2；远程插件 §4.4 | `crates/rutis-host/src/connect/dial.rs` `Dialing::run`（`installed.unloaded().await` 之后才退避重拨）；`crates/rutis-loader/src/runtime/sessions.rs` `SessionRowsPlugin`（行等会话，会话结束就停下） |
| 宿主按代次键装载一行：每次装载一个新键，旧代的迟到回复不作用于新代 | 质量规范 §11.2；#625 的代次规则（这里只用到键） | `crates/rutis-loader/src/runtime.rs:719-727`（`ctx.instance()` + `GENERATION`）；`crates/rutis-bridge/src/runtime/process.rs` 的 `on_row_ended` / `on_row_failed`（按键登记，`unload_row` 之后移除） |
| `rows.ended`：行自行结束，宿主结束这一行并把它卸掉（不再装载） | 远程会话 §4.3；`loader/self-dispose` | `crates/rutis-bridge/src/runtime/process.rs:531-541`；`node/rutis-runtime/src/runner.mjs:379-385`（`endRow`：先 `unloadRow`，再发 `rows.ended`） |
| `rows.failed`：行的代码失败，宿主重启它（卸载旧代、装载新代），运行时保留这一行 | 远程会话 §4.3；#688 | `crates/rutis-bridge/src/runtime/process.rs:543-568`；`node/rutis-runtime/src/runner.mjs:1120-1135`（`rows.failed` 之后行仍加载） |
| `rows.unload` 幂等：行已不在就只回答 | 行契约 `rows.v2` | `crates/rutis-bridge/src/session/plugin/rows.rs:197-207`；`node/rutis-runtime/src/runner.mjs:743-746`（`unloadRow` 找不到就返回） |
| 运行时顶替：关闭旧会话、不再读它的帧、清理旧会话的全部行、然后回答新 hello | 远程插件 §4.4；远程会话 §4.2 | 运行时监听端（Python `serve`、Go `listen.go`、Node `serve.mjs` 同一顺序）；模型按 Node 的行处理建（`runner.mjs`） |
| 会话结束（断线或顶替）时清理它的全部行，每行一次 | 远程插件 §4.4；`loader/remote-runtime-rows`、`loader/row-unload-leaves-nothing` | 运行时的 `serve`/`listen`；`crates/rutis-bridge/src/session/plugin/rows.rs` 的 `Server`（每个会话一张已装载表） |

## 模型

- **参与者**：一个宿主（loader 的 1–2 行 + 拨号行）和一个运行时（监听方 + 行表）。宿主一次只有一条连接，连接断了就重拨同一个 id；运行时一次只服务一个会话，新连接到来时顶替旧的。
- **连接**：`1..MaxConns`，最多 2 次重连。宿主按顺序拨号；每条连接两个方向各自 FIFO、延迟任意长；关闭了连接的一方不再读它，发给它的帧丢弃。
- **宿主**（`HDial`、`HGreeted`、`HLoad`、`HLoaded`、`HUnloaded`、`HEnded`、`HFailed`、`HLose`）：
  - `HLoad(r)` 用 `<<会话, 行, 代次>>` 作键发 `rows.load`；一个会话里一行最多装载 `MaxLoads` 次（失败重启用掉一次）。
  - `HEnded` 只结束键等于这一行当前代次的行，并像真实卸载栈一样再发一次 `rows.unload`（运行时已经处置过，只回答）；`HFailed` 把当前代次置为卸载中并等回复，回复之后才允许重新装载。
  - `HLose` 断开当前连接：这一会话上的行回到 `idle`（行停下，等下一个会话），宿主可以重拨；行已经 `gone`（自行结束）的保持 `gone`。
- **运行时**（`TakeUp`、`Answer`、`EndSession`、`RLoad`、`RUnload`、`EndSelf`、`SendEnding`、`RFail`、`Sweep`、`MarkSwept`）：
  - `TakeUp(s)`：读到新连接的 `hello`，关闭旧会话（`rside` 置 `closed`，不再读），开始清理旧会话的行。
  - `Answer(s)`：只有所有更早会话的行都清理完（`Swept`）才回答新 `hello`，所以两个会话的行不会同时存在。
  - `EndSession(s)`：运行时发现宿主关闭了会话；它的行由 `Sweep` 逐行清理。
  - `RLoad` 只在“当前会话、连接开着”（或变异 `readOld` 打开时，已结束会话）才读；`RUnload` 幂等，行在就清理一次、不在就只回答；`EndSelf` 是插件自行结束，行消失而 `rows.ended` 还要再发；`SendEnding` 只在会话还开着时发。
  - `RFail(k)` 每代最多发一次 `rows.failed`；行保持加载，等宿主卸载。
- **顶替的顺序**就在 `TakeUp` 和 `Answer` 之间：关旧、停止读、清行（`Swept`）、回答新 `hello`。`Answer` 的前置条件是这个模型检查的核心规则。

## 简化

- 规模是有意的有限：1 个宿主、1 个运行时、1–2 行、最多 3 条连接（2 次重连）、每个会话每行最多 2 次装载。真实实现的重连次数、退避和 30 秒/60 秒计时不建：环境只体现为“某个时刻断线”，比真实计时更一般。
- 握手只建“一端 `hello`、另一端回答 `hello`”：版本、能力、鉴权、10 秒截止不建（`remote-lease` 覆盖顶替与握手的其它部分）。
- 宿主→运行时的帧只有 `rows.load` 和 `rows.unload`；`rows.update`、`rows.schema`/`mount`、服务、租约、事件都不建。运行时→宿主只有 `hello`、`loaded`、`unloaded`、`ended`、`failed`。
- 宿主一行是一个 fiber：`idle | loading | loaded | unloading | gone`，键是 `<<会话, 行, 代次>>`（实现里是 `instance.generation`，每次装载一个新值）；loader 的两阶段解析、`SessionRows` 依赖、重启策略不建。“行自行结束 = 禁用、不再装载”按 `loader/self-dispose`。
- 运行时对一行的处置是一个原子动作（`Sweep`/`RUnload`/`EndSelf` 都是“行消失，清理计数加一”）；各语言卸载栈、`shutdown` 截止、`events.off` 的细节不建（#625 的范围）。
- 旧会话队列里还没读的帧留在模型里（设计里它们随连接关闭被丢弃）；设计侧由 `CanRead` 保证不再读，只有 `readOld` 变异会去读它们。这样变异有东西可作用，而正确协议下它们永远不会被读。
- 宿主在丢掉会话后不再读它（`dial.rs` 等旧会话卸载完才重拨），所以“旧会话的迟到 `rows.ended`”不会落到新会话的宿主上；跨会话的隔离在运行时一侧（不读旧会话、清理完再回答）。宿主一侧的同一规则体现在同一会话里的代次竞态：旧代的迟到 `rows.ended`/`rows.failed` 不结束新代（`OwnFramesOnly`），这正是 a88fb65b 修的缺陷。
- 内存序、任务调度、真实计时、帧编码不建模；每个动作原子。

## 不变式与活性

| 名字 | 内容 |
| --- | --- |
| `OneRowPerName` | 运行时里一个名字的行最多一个代次处于加载状态：旧会话的行清理完之后新会话才装载；失败重启等旧代卸载的回答，不并存 |
| `NewSessionOnlyAfterSweep` | 一个会话被回答（`hello`）时，所有更早会话的行都已经清理完：旧会话清理完才接受新 `hello` |
| `NoRowsAfterSweep` | 一个会话被清理过一次之后，它的帧不会再装载任何行：运行时不再读旧会话的指令 |
| `OwnFramesOnly` | 宿主只对“这一行当前代次”的 `rows.ended`/`rows.failed` 起作用：旧代的迟到帧不结束、不重启新代 |
| `CleanupOnce` | 一个行代次最多清理一次：会话清理、`rows.unload`、行自行结束不会把同一行处置两次 |
| `EndedLeadsToSwept` | 活性：一个会话结束之后，它的行最终被清理（断线后行不会留在运行时里） |

## 结果

TLC 2.19（tla2tools v1.7.4），Java 21.0.12（Temurin），32 核，`-workers auto`。`通过` 指穷举完成、所列性质全部成立；被违反的配置找到第一个反例就停，状态数取决于搜索顺序，每次可能不同。整个目录约 9 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `row-healthy` | 2 行，1 条连接；行可失败、可自行结束；全部安全不变式 | 通过 | 73,768 | 1–2 s |
| `row-reconnect` | 2 行，3 条连接（2 次重连），可断线；全部安全不变式 | 通过 | 191,830 | 2 s |
| `row-cleanup` | 2 行，1 条连接，可断线；只检查活性 | 通过 | 110 | 1 s |
| `row-mut-accept-before-clean` | 顶替时不等旧会话的行清理完就回答新 `hello` | 违反 `NewSessionOnlyAfterSweep` | 469（找到反例即停） | 1 s |
| `row-mut-accept-before-clean-loads` | 同上，接着新会话装载同名行 | 违反 `OneRowPerName` | 705（找到反例即停） | 1 s |
| `row-mut-read-old` | 顶替后运行时继续读旧会话 | 违反 `NoRowsAfterSweep` | 449（找到反例即停） | 0 s |
| `row-mut-keyed-by-row` | 宿主按行名而不是键匹配 `rows.ended`/`rows.failed` | 违反 `OwnFramesOnly` | 144（找到反例即停） | 1 s |
| `row-mut-reclean` | 对已经不在的行再跑一次卸载清理 | 违反 `CleanupOnce` | 53（找到反例即停） | 1 s |
| `row-mut-keep-rows` | 运行时关闭断线的会话但保留它的行 | 违反活性 | 29（找到反例即停） | 0 s |

## 变异与反例

每个被检查的性质都配一个故意引入的错误，TLC 必须找到反例（`expected.txt` 的 `violated:<名字>`；活性为 `violated:liveness`）：

| 配置 | 变异 | 违反 | 反例 |
| --- | --- | --- | --- |
| `row-mut-accept-before-clean` | `Answer` 不要求更早的会话已 `Swept`（§4.4 的“清理完再回答”被去掉） | `NewSessionOnlyAfterSweep` | 会话 1 装载了行 `a`，断线，拨会话 2；`TakeUp(2)` 关闭会话 1，`Answer(2)` 立即回答 `hello`，此时 `Swept(1)` 还是假 |
| `row-mut-accept-before-clean-loads` | 同上 | `OneRowPerName` | 接着会话 2 装载行 `a`（新键），会话 1 的 `a`（旧键）还在运行：同名两代并存 |
| `row-mut-read-old` | `CanRead` 允许读已结束会话的帧 | `NoRowsAfterSweep` | 宿主在会话 1 上发了 `rows.load` 但没被读，断线后拨会话 2；会话 1 被清理并 `MarkSwept` 之后，运行时读到那条迟到的 `rows.load`，给已清理的会话又装载了行 |
| `row-mut-keyed-by-row` | `HEnded`/`HFailed` 按行名匹配，不看键的代次 | `OwnFramesOnly` | 行 `a` 失败并卸载，插件在卸载前自行结束，`rows.ended` 在卸载回答之后才发出；宿主已经装载了新代，读到旧代的 `ended` 后把新代结束（`hEndedFrom` 是旧键，`hkey` 是新键）——a88fb65b 修的就是它 |
| `row-mut-reclean` | 对已不在的行也执行一次清理 | `CleanupOnce` | 行失败；插件自行结束（清理一次），随后的 `rows.unload` 发现行已不在，变异又清理一次：`cleanCount = 2` |
| `row-mut-keep-rows` | `Sweep` 关闭（断线后不清理行） | 活性（`EndedLeadsToSwept`） | 会话 1 装载了行后断线，运行时不清理：`Swept(1)` 永远不成立 |

`row-mut-accept-before-clean` 与 `row-mut-read-old` 也分别会让 `OneRowPerName` 或 `NewSessionOnlyAfterSweep` 失败；分开列配置是为了让每个反例只对应一个结论（同 `event-close` 的做法）。

## 结论

- 按远程插件 §4.4 的两条规则——顶替时不再读旧会话、清理完旧会话的行才回答新 `hello`——在穷举范围内成立：运行时不出现同一个名字的两代行；新会话的行只在旧会话清理完之后装载；旧会话剩下的帧不会在清理之后再装载行；断线后它的行被清理。宿主按键（代次）处置行的结束与失败，所以旧代的迟到帧不改变新代的状态；一行只被清理一次。
- 五个变异分别锁定五个前提：回答新 `hello` 必须在清理之后（`NewSessionOnlyAfterSweep`、`OneRowPerName`）；运行时必须停止读旧会话（`NoRowsAfterSweep`）；宿主的结束/失败必须按键而不是按名（`OwnFramesOnly`）；卸载清理必须幂等（`CleanupOnce`）；断线后必须真的清理（活性）。每条性质都不是空的。
- 模型没有证明的：内存序与真实调度；宿主 loader 的解析、重启、代次与拒绝、服务公告、宿主服务租约（#625 的 `specs/row-lifecycle/` 后续模型）；事件的登记、`rows.ended` 与 `events.*` 的帧序（#613 的事件表）；调用生命周期；运行时各语言卸载栈的差异与 `shutdown` 截止；计时与重连退避；帧编码。本目录只是 #613 行生命周期的第一块，不能当作完整行协议的证据。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh                    # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh row-reconnect      # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/row-lifecycle/run.sh      # 限定 TLC 的线程数
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；日志和反例写在 `TLC_OUT`（默认一个临时目录）。CI 的 `specs` 任务矩阵包含 `row-lifecycle`（见 [CI](../../docs/ci.md) §4）。

## 维护

远程会话 §4.2、§4.3、远程插件 §4.4 修改时，或 `crates/rutis-loader/src/runtime.rs` 的行键、`crates/rutis-bridge/src/runtime/process.rs` 的 `rows.ended`/`rows.failed`/`unload_row`、运行时监听端的顶替顺序、`node/rutis-runtime/src/runner.mjs` 的 `endRow`/`unloadRow` 改变时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。
