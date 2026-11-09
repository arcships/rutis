# 节点托管行：每个会话的装载表、实例键与迟到的代次通知（TLA+）

[English](README.en.md)

用 TLC 穷举一个宿主为一个对端节点运行行时的交错：旧会话断线、新会话顶替，每个会话自己的已装载表，行的实例键跨重启不变，`rows.ended`/`rows.failed` 的迟到通知，以及行服务的导入与撤销。对应 issue [#613](https://github.com/arcships/rutis/issues/613)“并入 #640”一节列出的 `specs/peer-rows/`；质量规范 Q11.2；设计见[远程插件](../../docs/design-remote-plugins-2026-10-03.md) §4.4、§5.1–§5.3，[远程会话](../../docs/design-remote-session-2026-10-28.md) §4.2、§4.3，[会话追踪契约](../../docs/formal-trace.md) §3.0。

这是行生命周期的第二块：`row-lifecycle` 建运行时一侧的行（一个运行时会话里的加载、卸载与重连），这一块建的是“宿主替对端装载行”：每个会话自己的已装载表、跨重启不变的实例键、服务导入/撤销，以及旧代迟到通知对新代的作用。

当前代码里 `peer:` 行与 `PeerRow` 已被 0.19 的远程连接定稿删除（远程会话 §八），等价路径是 `<连接行>:<插件>` 经 `run`；`run` 行的键每次装载都不同（`plugin#n`，`run.rs:168`），同一实例因此不会由两代共用键。本模型按 issue #613 的疑点写“键是 `ctx.instance()`、跨重启不变”的旧规则，正是为了回答：没有“每次新键”时，安全性靠什么。所以下面的结论是设计规则；当前 `run` 行另有一份证据（`session_rows.rs` 的两个用例），两者不能互相代替。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 每个控制会话有自己的已装载表；同一个键在一个会话里只装载一次 | 远程插件 §4.4（每个运行时实例只有一个活动会话） | `crates/rutis-bridge/src/session/plugin/rows.rs:61`（`Server.loaded`）、`:110-112`（已装载则拒绝） |
| 行按实例键装载，键跨重启不变；代次是模型给一次装载起的名字 | 远程插件 §5.3；issue #613 的疑点 | 旧 `PeerRow` 用 `ctx.instance()`；当前 `run` 行用 `plugin#n`（`run.rs:168`），`rows.rs` 按 key 记 |
| `rows.ended` 只由这一代还没被对端卸载的行发出；会话取消后不发 | 远程会话 §4.3 | `rows.rs:138-175`（`follow`：`loaded.remove(&key)`，`ended && !cancelled`） |
| `rows.failed` 每代一次，宿主保留这一行，由对端卸载后重装 | 远程插件 §4.2；#688 | `crates/rutis-bridge/src/runtime/process.rs:543-568`、`run.rs:171-175` |
| `rows.unload` 幂等：行已不在就只回答 | 行契约 `rows.v2` | `rows.rs:197-209`（“Already gone: unloading is idempotent”） |
| 顶替：关旧会话、不再读它的帧、清理旧租约的行和服务，再回答新 `hello` | 远程插件 §4.4；远程会话 §4.2 | 运行时监听端的顺序；`rows.rs` 每个会话一个 `Server`，新会话的表是新的 |
| 行随会话停止、下一个会话再装载 | 远程会话 §4.3、§7 | `run.rs` 的 `RowHandle`/`RunRow`；`crates/rutis-loader/tests/session_rows.rs::rows_run_on_the_session_of_their_connection_row`、`::a_far_session_unloading_first_leaves_the_row_waiting` |
| 服务的导入随行：行卸载、结束或会话没了就撤销 | 远程插件 §2.6 决定 1、§5.3 | `crates/rutis-bridge/src/session/plugin/services.rs:192-378`（每个会话的 `Imports`）；`process.rs:1306-1314`（`forget_exports`）、`:1356-1366`（`unload_row` 之后 `forget_exports`/`imports.forget_row`） |
| 旧会话的迟到指令不作用于新租约 | 远程插件 §4.4 | 会话各自一个处理器；README 的“简化”说明模型怎样把它写成代次匹配 |

## 模型

- **参与者**：一个宿主（替对端装载行：每个会话一张表、行的结束/失败通知、服务导入）、一个对端节点（自己的两行，跨会话重装）和它的两条控制会话：会话 1（旧）与会话 2（新，顶替用）。两行 `a`、`b`，每行一个实例键，键在每次装载时不变；每行最多 2 代（每个会话一代）。
- **会话**：`1..MaxSessions`（2 条）。对端按序拨号；一条会话两个方向各自 FIFO、延迟任意长。断开后对端不再读它（`PLoss`），宿主也不再读它的帧（`CanRead`）；已经写出、还没读到的 `rows.ended`/`rows.failed` 可能迟到（`late`）。
- **对端**（`PDial`、`PGreeted`、`PLoad`、`PLoaded`、`PUnload`、`PUnloaded`、`PEnded`、`PFailed`、`PLoss`、`PLateEnded`、`PLateFailed`）：
  - `PLoad(r)` 在会话上装载一行：键是实例，代次加一；`prow` 是 `idle | loading | loaded | unloading | gone`。
  - `PEnded`/`PFailed` 只对“这一行当前代次”的帧起作用：旧代迟到的通知不结束、不重启新代。
  - `PLoss(s)`：会话断开，这一会话上的行回到 `idle`（等下一个会话），这一会话导入的服务随之消失；队列里没读到的结束/失败记成迟到通知。
  - `PLateEnded`/`PLateFailed`：迟到通知到达。正确协议下它不属于当前代次，什么也不做；变异 `lateEnding` 让它按行键作用。
- **宿主**（`HTakeUp`、`HAnswer`、`HEndSession`、`HReadLoad`、`HReadUnload`、`HEndSelf`、`HFail`、`HSweep`、`Break`）：
  - `HTakeUp(s)`：读到新连接的 `hello`，关闭旧会话（不再读），开始清理它。
  - `HAnswer(s)`：所有更早会话的行都清理完（`Swept`）才回答新 `hello`；这是模型检查的核心规则。
  - `HReadLoad` 把行装进**这一会话自己的**表（键已装载就拒绝，模型里这条不可达，见“简化”）；`HReadUnload` 幂等，行在就处置一次并撤销服务，不在就只回答。
  - `HEndSelf` 是插件自行结束（宿主侧 Cordis 插件 dispose 自己）：从表里去掉、发 `rows.ended`；`HFail` 每代发一次 `rows.failed`，行保留。
  - `HEndSession` 是宿主看到断线（或顶替关掉旧会话）：会话结束，它的行由 `HSweep` 逐行清理；已写出未读的结束/失败通知记成迟到的。
  - `Break(s)` 是环境：连接断开。

## 简化

- 规模是有意的有限：1 个宿主、1 个对端、2 条会话、2 行，每行最多 2 代。真实实现的重连次数、退避和计时不建：环境只体现为“某个时刻断线”，比真实计时更一般。
- 两条会话是顺序的：新会话只在旧会话断线之后才拨。同一 id 的并发握手、握手期间旧会话仍活着、接入点的接管顺序不建（`remote-lease` 与 `access-*` 覆盖）；这里只保留“宿主回答新 `hello` 时旧会话的行可能还没清完”这个交错。
- 代次是模型给一次装载起的名字：帧里带它，真实帧只带键。实现里对端能区分两代的依据是“这条通知来自哪个注册（哪个会话的哪个 `Process`）”，模型把它写成代次；变异 `lateEnding` 就是“只按行键、不看它属于哪次装载”。
- 会话取消时不发 `rows.ended`（`rows.rs` 的 `!cancelled`）：行按断线处理，等下一个会话再装载。模型因此把迟到的通知一律忽略（`PLateEnded`/`PLateFailed` 的正确行为是什么也不做）；迟到在这里是环境可能做出的事，不是协议允许的指令。
- 服务公告/撤销的帧和它们的延迟不建：装载被接受时导入、行消失时撤销，各是一个原子步。版本规则（旧公告不恢复已撤销的服务，`services.rs:239-245`）和多跳不建。
- 行装载/卸载本身是原子动作。装载中被取消、卸载先到会留下孤儿插件（#625 移来的疑点）不建，`NoOrphan` 不在本模型里。
- 握手只建“`hello` → 回答”；10 秒截止、鉴权、能力、`link.offers` 与 epoch、`plugins.describe`/`schema` 刷新、`PeerRows` 的两阶段门控不建（#640 的另一半）。
- 事件、代理、`plugins.update`、租约升级、退避与计时不建；内存序、任务调度和帧编码不建；每个动作原子。
- 一个会话里同键的第二次装载（`rows.rs` 的拒绝）不可达：对端的行在 `idle` 时才会发装载，一个会话里每行只装载一次；模型把这条写成“已装载就忽略”，不建拒绝的回复。

## 不变式与活性

| 名字 | 内容 |
| --- | --- |
| `OneInstanceAtATime` | 一个行的实例同时只在一个会话里装载：旧会话的行清理完之后，新会话才装载同一实例 |
| `OwnGenerationOnly` | 对端只对当前代次的 `rows.ended`/`rows.failed` 起作用：旧代迟到的通知不结束、不重启新代 |
| `ServicesFollowRows` | 一行的服务只在提供它的那一代装载期间被导入：行卸载、自行结束或会话结束时撤销，导入不留在行之外 |
| `CleanupOnce` | 一行的同一代最多处置一次：`rows.unload`、行自行结束和会话清理不会处置两次 |
| `EndedRowsCleaned` | 活性：一个会话结束之后，它装载过的行最终被清理（服务随之撤销） |

## 结果

TLC 2.19（tla2tools v1.7.4），Java 21.0.12（Temurin），32 核，`-workers auto`。`通过` 指穷举完成、所列性质全部成立；被违反的配置找到第一个反例就停，状态数取决于搜索顺序，每次可能不同。整个目录约 10 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `peer-healthy` | 2 行，2 条会话，可断线；行可自行结束；全部安全不变式 | 通过 | 36,083 | 1 s |
| `peer-failure` | 2 行，2 条会话，可断线；行的代码可失败，对端卸载后重装；全部安全不变式 | 通过 | 161,503 | 2 s |
| `peer-clean` | 2 行，2 条会话，可断线；行可自行结束；只检查活性 | 通过 | 36,083 | 1–4 s |
| `peer-mut-late-ending` | 迟到的旧代 `rows.ended`/`rows.failed` 按行键作用于新代 | 违反 `OwnGenerationOnly` | 5,065–10,888（找到反例即停） | 0–1 s |
| `peer-mut-two-sessions` | 顶替时不等旧会话的行清理完就回答新 `hello`，新会话装载同一实例 | 违反 `OneInstanceAtATime` | 2,876–4,536（找到反例即停） | 0–1 s |
| `peer-mut-keep-rows` | 宿主结束会话但不清理它的行 | 违反活性 | 36,083（找到反例即停） | 1–3 s |
| `peer-mut-service-residue` | 行卸载或会话结束时不撤销它的服务 | 违反 `ServicesFollowRows` | 171–238（找到反例即停） | 0–1 s |

## 变异与反例

每个被检查的性质都配一个故意引入的错误，TLC 必须找到反例（`expected.txt` 的 `violated:<名字>`；活性为 `violated:liveness`）：

| 配置 | 变异 | 违反 | 反例 |
| --- | --- | --- | --- |
| `peer-mut-late-ending` | `PLateEnded`/`PLateFailed` 不看代次，按行键结束或重启这一行 | `OwnGenerationOnly` | 会话 1 上 `a` 是第 1 代；插件自行结束，`rows.ended(a,1)` 写进会话 1 的队列但没被读；连接断，对端拨会话 2，把 `a` 装载成第 2 代；迟到的结束到达，把新代结束（`pEndedFrom = 1`，`pkey = 2`） |
| `peer-mut-two-sessions` | `HAnswer` 不要求更早会话已 `Swept`（§4.4 的“清完再回答”被去掉） | `OneInstanceAtATime` | 会话 1 上 `a` 是第 1 代；断线；对端拨会话 2；宿主关掉旧会话但不等清理就回答；对端把 `a` 装载成第 2 代并在会话 2 被接受：`loaded[1][a] = 1` 与 `loaded[2][a] = 2` 同时存在 |
| `peer-mut-keep-rows` | `HSweep` 关闭（会话结束后不清理行） | 活性（`EndedRowsCleaned`） | 会话 1 装载了行后断线；宿主结束会话（`ended[1]`），但行永远留在表里，`Swept(1)` 永远不成立 |
| `peer-mut-service-residue` | 行卸载、自行结束或会话清理时不撤销服务 | `ServicesFollowRows` | 会话 1 上 `a` 装载，服务导入为 `<<1,1>>`；插件自行结束，宿主从表里去掉这一行但导入还在：行不在了、服务还在 |

`peer-mut-two-sessions` 的变异同时会让“回答前必须清理完”的规则不成立；分开列配置是为了让每个反例只对应一个结论（同 `row-lifecycle`、`event-close` 的做法）。`CleanupOnce` 没有单独的变异：模型里三条处置路径都先检查这一代还在表里，第二次处置不可达；任何一条忘了检查都会让它失败。

## 结论

- 按远程插件 §4.4 的规则——顶替时不再读旧会话、清理完旧会话的行才回答新 `hello`——在穷举范围内成立：同一个实例不会同时在两个会话里运行；旧会话清理完之后新会话才装载；行随会话停止、下一个会话再装载。
- 对端只按“这条通知属于哪一次装载”处置 `rows.ended`/`rows.failed`：旧代迟到的结束不会结束新代，迟到失败的不会重启新代。键跨重启不变时，这必须靠通知与装载的对应关系；只按行键就正好是 `lateEnding` 的反例。
- 服务的导入随行：行卸载、自行结束、会话结束都会撤销；`serviceResidue` 说明“导入不随行”会被抓到。
- 会话必须真的清理它的行：`keepRows` 让活性失败。
- 模型没有证明的：装载中被取消的托管行（#625 的孤儿疑点）、同一 id 的并发握手、`PeerRows` 的解析/门控/离线/`link.offers` 撤回、`plugins.describe`/`schema`、服务公告的版本与多跳、事件、退避与计时、各语言运行时的监听端差异、内存序和真实调度。这些是 #640 的其余部分。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/peer-rows/run.sh                     # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/peer-rows/run.sh peer-mut-late-ending # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/peer-rows/run.sh      # 限定 TLC 的线程数
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；日志和反例写在 `TLC_OUT`（默认一个临时目录）。CI 的 `specs` 任务矩阵还没加 `peer-rows`（见 [CI](../../docs/ci.md) §4，属于 #640 的收口）。

## 维护

远程插件 §4.4、§5.1–§5.3 或远程会话 §4.2、§4.3 修改时，或 `crates/rutis-bridge/src/session/plugin/rows.rs` 的每会话表、`services.rs` 的导入表、`runtime/process.rs` 的 `rows.ended`/`rows.failed`/`unload_row`、`session/plugin/run.rs` 的行键改变时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。
