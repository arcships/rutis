# 会话中的调用：发起、执行、答复、取消、引用与会话关闭（TLA+）

[English](README.en.md)

用 TLC 穷举一个会话里调用从发起到结算的全部交错：调用号、请求到达、方法执行、答复、取消、迟到答复、引用的授予与释放，以及会话关闭。对应 issue [#613](https://github.com/arcships/rutis/issues/613) 的调用生命周期部分；质量规范 Q11.2；设计见[会话协议](../../docs/protocol-session.md) §5、§6、§7，[会话追踪契约](../../docs/formal-trace.md) §3.4、§3.5，[形式化验证设计](../../docs/design-formal-verification-2026-11-08.md) §3.4。这是 #613 的第二块：第一块是 [`row-lifecycle`](../row-lifecycle/README.md)（一个会话的行），本块只回答“一次调用从发起到结算”，不建跨运行时转发、调用链 `path`、同步等待图、事件帧序和节点托管行。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 调用号每端每会话从 1 严格递增，一个号只属于一次调用；已有号再次出现不是新调用 | 会话协议 §5.1（`SESS-32`、`SESS-33`、`SESS-34`） | `crates/rutis-bridge/src/session/rpc.rs` 的 `Calls::admit` 与 `Calls::received` |
| 一次调用恰好结算一次：回复到达时先从等待表摘除 | 会话协议 §5.4（`SESS-41`、`SESS-42`） | `rpc.rs:2494`（`reply_target`） |
| 取消只作用于一次调用：还在排队的被丢掉、不回复；已经在运行的照常完成并回复；取消不关闭会话 | 会话协议 §7（`SESS-56`、`SESS-57`、`SESS-60`） | `rpc.rs:2693`（`Frame::Cancel`）、`rpc.rs:2038`（`Connection::cancel`） |
| 对已取消调用的迟到答复被丢弃并计数，不当协议错误 | 会话协议 §7（`SESS-57` 第三条）、追踪契约 §3.4 | `reply_target` 的 `cancelled`/`orphans`（`rpc.rs:2494`）；`go/rutis/internal/peer/peer_test.go:135-170` |
| 对不存在的调用发 `cancel` 被忽略（取消越过它要取消的回复） | 会话协议 §7（`SESS-58`） | `rpc.rs:2693`；测试 `a_cancel_for_an_unknown_call_is_ignored` |
| 取消不撤销已经提交的引用授予；本地代理释放时接收方照常发 `release` | 会话协议 §7（`SESS-59`）、§6.1（`SESS-47`） | `Import` 的 `Drop`（`rpc.rs:992`）写出 `Frame::Release` |
| 答复里带的值先解码再判断：被丢弃的迟到答复也会释放它带来的引用 | 追踪契约 §3.4；`SESS-44` 的回滚规则 | `rpc.rs:2675`（`Frame::Return` 的 “Decode first”） |
| 会话关闭：等待中的调用以会话错误结算，导出表和导入表清空，未写出的帧随会话消失 | 会话协议 §5.4、§6.1（`SESS-42`、`SESS-51`）、追踪契约 §4“引用守恒” | `rpc.rs:1634`（`Connection::close`） |
| 一端关闭后，另一端从通道结束得知并同样结束 | 追踪契约 §3.0（会话结束先 dispose 再谈别的） | 读线程在通道结束后关闭本端（`rpc.rs` 的读循环与 `closed()`） |

## 模型

- **参与者**：一个会话的两个端点。A 发起调用，B 接收、执行并答复。两个方向各自 FIFO、延迟任意长：A→B 上是 `invoke`、`cancel`、`release`，B→A 上是 `return`。因此答复可以在取消写出时还在路上，取消也可以越过它要取消的答复。
- **A（调用方）**：`AStart` 取下一个号并写 `invoke`；`ADeliverLive` 把答复结算给等待它的调用并收下回复里带的引用；`ACancel` 放弃一次调用（写出 `cancel`，调用以调用方的错误结算）；`ADeliverLate` 把已取消调用的迟到答复解码后丢弃（释放它带的引用，会话继续）；`ARelease` 放弃它持有的引用并写 `release`；`AClose`/`ALearn` 结束这一端。
- **B（被调方）**：`BReadInvoke` 收下调用（还排队）；`BRun` 开始执行；`BReply` 答复并把一次引用授予写出去；`BReadCancel` 处理取消——还在排队的丢掉、不答复，已在运行的保持原样（稍后照常答复，这就是迟到答复的来源），已答复的不动；`BReadRelease` 给出被释放的授予；`BClose`/`BLearn` 结束这一端。
- **关闭**：任一端可以关闭。关闭的一端把等待中的调用以错误结算、清空自己持有的引用表、丢掉两个方向上还在队列里的帧；另一端通过通道结束（`ALearn`/`BLearn`）做同样的事。`closeCause` 是记录“哪一端、因为什么先关闭”的 ghost。

## 简化

- 规模是有意的有限：1 个调用方、1 个被调方、2 次调用、最多 1 次取消、1 个引用（每次答复授予一次，所以最多两单位授予）。没有重连、没有第二个会话、没有多跳转发。
- 握手不建：模型从两端都已问候、会话可用开始。版本、端点、能力、`hello` 的顺序和 10 秒截止不建（`remote-lease` 覆盖顶替与握手的其它部分）；本文只从第一次调用开始。
- 帧只有 `invoke`、`cancel`、`release`（A→B）和 `return`（B→A）。`throw` 与 `return` 是同一条结算路径，未单独建；`call`、`get`、`await`、`events.*`、`rows.*` 不建。
- 调用号只建“递增、不复用”：前缀、规范十进制形式、2^53 上限、`path` 里不得出现自己的号都不建。接收方对“号不增长”的拒绝也没有可达路径——正确的发送方不会重复号，重复号正是 `reuseId` 变异，所以这个检查在模型里是死代码。
- `release` 指向本端没有的引用、释放计数大于授予次数、`call`/`get`/`await` 指向没有的引用这类协议错误不建：正确的一侧不会发出它们，模型里每一条 `release` 都在授予范围内（会话错误另有追踪契约 §3.3、§3.5 的用例）。
- 调用链 `path`、`rebase`、会话标签、同步等待与 `SyncWaitCycle` 不建：那是跨运行时转发的范围（`cross-runtime-sync` 模型与其后续块）。
- 值、错误图、`bad-value`、`bad-frame`、16 MiB 上限、出站积压不建。它们都只让一个调用失败，与 `return` 的结算路径同形，已在追踪契约 §3.5–§3.7 和属性测试里覆盖。
- 方法执行时间不建：调用从 `queued` 到 `running` 再到答复，答复可以发生在任意晚的时刻，包括取消之后；没有计时、没有调度顺序承诺。
- 关闭之后两个方向的队列都清空，任何一端都不再读：这是一处简化。真实实现里已经写到通道上的帧可能在通道结束前仍被对端读到；这个交错不在本模型里，README 的结论也不声称覆盖它。
- 关闭的原因不是协议帧：两端各自结束，另一端从通道结束得知。`closeCause` 只为区分“取消导致的关闭”这个错误设计。
- 内存序、真实调度、帧编码不建；每个动作原子。

## 不变式与活性

| 名字 | 内容 |
| --- | --- |
| `CancelKeepsSession` | 取消不结束会话：会话的关闭只来自调用方、被调方或通道结束（`closeCause ≠ "cancel"`） |
| `SettlesAtMostOnce` | 一次调用最多结算一次：答复、取消、会话结束三者不会让同一个调用结算两次 |
| `LateReplyDropped` | 迟到答复被丢弃：已取消的调用保持取消时的结果，答复不再结算它；每次调用最多丢弃一个答复 |
| `OwnReply` | 结算一个调用的答复就是这次调用自己的答复 |
| `NoIdReuse` | 一个调用号只属于一次调用：没有新调用再取它 |
| `NoOverRelease` | 调用方只归还它被授予的量：一份授予被释放、丢弃或清空一次，不会两次 |
| `ClosedClearsTables` | 关闭的一端不再持有引用：A 的导入表空、B 的导出表空 |
| `FailUnfinishedOnClose` | 关闭后两端都不留未完成的调用：A 侧没有还在等待的调用，B 侧没有排队或运行中的调用 |
| `WaitingSettles` | 活性（只在没有关闭的配置里检查）：A 等待的调用最终被答复或被取消，被调方会答复它收下的调用 |

## 结果

TLC 2.19（tla2tools v1.7.4），Java 21.0.12（Temurin），32 核，`-workers auto`。`通过` 指穷举完成、所列性质全部成立；被违反的配置找到第一个反例就停，状态数取决于搜索顺序，每次可能不同。整个目录约 5 秒。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `call-healthy` | 2 次调用，最多 1 次取消，两端都可以关闭；全部安全不变式 | 通过 | 1,155 | 1 s |
| `call-progress` | 不允许关闭，只检查活性（`WaitingSettles`） | 通过 | 409 | 1 s |
| `call-mut-cancel-closes` | 取消时关闭会话 | 违反 `CancelKeepsSession` | 11（找到反例即停） | 1 s |
| `call-mut-late-reply-twice` | 迟到答复被当成正常答复再结算一次 | 违反 `SettlesAtMostOnce` | 530（找到反例即停） | 1 s |
| `call-mut-close-leaks` | 关闭时保留导出表 | 违反 `ClosedClearsTables` | 352（找到反例即停） | 0–1 s |
| `call-mut-reuse-id` | 调用号可以被新调用复用 | 违反 `NoIdReuse` | 11（找到反例即停） | 0–1 s |
| `call-mut-reuse-id-settles` | 同一个变异的后半段：复用的号让答复结算了另一个调用 | 违反 `OwnReply` | 570（找到反例即停） | 0–1 s |

## 变异与反例

每个被检查的性质都配一个故意引入的错误，TLC 必须找到反例（`expected.txt` 的 `violated:<名字>`）：

| 配置 | 变异 | 违反 | 反例 |
| --- | --- | --- | --- |
| `call-mut-cancel-closes` | `ACancel` 直接把 A 的会话关掉（§7 的“取消只作用于一次调用”被换成“取消等于结束”） | `CancelKeepsSession` | A 发起调用并取消它：`acall = "settled"`、`aclosed = TRUE`、`closeCause = "cancel"`——一次取消结束了整个会话 |
| `call-mut-late-reply-twice` | `ADeliverLate` 把已取消调用的答复当成仍然在等的结果结算 | `SettlesAtMostOnce` | A 取消调用 1（B 已在运行它），B 答复；`settles["c1"] = 2`：取消给了一个结果，答复又给了一个 |
| `call-mut-close-leaks` | `CloseB` 不清 `bgrants`（§6.1 的“会话关闭清空导出表”被去掉） | `ClosedClearsTables` | A 取消调用 1，B 照常答复并为它授予一次引用；A 关闭，B 得知后关闭，`bgrants = 1` 留在一个已关闭的会话里 |
| `call-mut-reuse-id` | `AStart` 不推进 `anext`：新调用取一个旧调用已经取过的号 | `NoIdReuse` | 调用 1 取号 1，随后调用 2 也取号 1：`aid = [c1 \|-> 1, c2 \|-> 1]` |
| `call-mut-reuse-id-settles` | 同一个变异，继续看它的后果 | `OwnReply` | 两个调用都叫 1；调用 2 被取消，B 答复调用 2（号 1）；A 用这个答复结算了调用 1：`settledBy["c1"] = "c2"` |

这两个复用号的配置是同一条变异的两个结论：先是不变式本身被破坏（一个号属于两次调用），然后是它带来的错误结算（答复落到别的调用上）。分开列是为了让每个反例只对应一个结论（同 `event-close`、`row-lifecycle` 的做法）。

## 结论

- 按协议 §5–§7 的规则——号只增不复用、一次调用只结算一次、取消只丢调用不丢会话、迟到答复丢弃、关闭时失败未完成调用并清空引用表——在穷举范围内成立：没有调用被结算两次；取消不会关闭会话；已取消调用的迟到答复不会改变它的结果，会话照常处理下一个调用；一端的关闭让另一端也结束，两端都不留下未完的调用和引用；会话关闭前的每次授予只被归还一次。
- 五个变异配置分别锁定五个前提：取消不能结束会话（`CancelKeepsSession`）；迟到答复必须丢弃（`SettlesAtMostOnce`）；关闭必须清空导出表（`ClosedClearsTables`）；号不能被新调用复用（`NoIdReuse`），复用的直接后果是答复结算错的调用（`OwnReply`）。每条性质都不是空的：`call-progress` 在没有关闭时要求被调方真的答复；把 `BReply` 从 `Next` 里去掉时它立刻失败。
- 模型没有证明的：握手与鉴权；跨运行时转发、中继、调用链与同步等待图；事件与行的帧序（`row-lifecycle` 与后续块）；值编码、错误图、上限与出站积压；多跳取消传播（`SESS-59` 的中继部分）；关闭后仍在通道上的帧；真实调度、内存序与计时。本目录只是 #613 调用生命周期的第一块，不能当作完整调用协议的证据。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh                    # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh call-healthy       # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh      # 限定 TLC 的线程数
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；日志和反例写在 `TLC_OUT`（默认一个临时目录）。CI 的 `specs` 任务矩阵包含 `call-lifecycle`（见 [CI](../../docs/ci.md) §4）。

## 维护

会话协议 §5、§6、§7 修改时，或 `crates/rutis-bridge/src/session/rpc.rs` 的 `Calls::admit`、`reply_target`、`Frame::Cancel`/`Frame::Release` 的处理、`Connection::cancel`、`Connection::close` 改变时，或运行时一侧（`node/rutis-runtime/src/session.mjs` 等）的取消与引用释放行为改变时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。
