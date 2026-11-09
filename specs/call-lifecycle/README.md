# 会话中的调用：发起、执行、答复、取消、引用与会话关闭（TLA+）

[English](README.en.md)

用 TLC 穷举一个会话里调用从发起到结算的全部交错：调用号、请求到达、方法执行、答复、取消、迟到答复、引用的授予与释放，以及会话关闭。对应 issue [#613](https://github.com/arcships/rutis/issues/613) 的调用生命周期部分；质量规范 Q11.2；设计见[会话协议](../../docs/protocol-session.md) §5、§6、§7、§10，[会话追踪契约](../../docs/formal-trace.md) §3.4、§3.5，[形式化验证设计](../../docs/design-formal-verification-2026-11-08.md) §3.4。这是 #613 的第二块：第一块是 [`row-lifecycle`](../row-lifecycle/README.md)（一个会话的行）。本块回答“一次调用从发起到结算”，并按 [#626](https://github.com/arcships/rutis/issues/626) 补了三个问题：读线程写回在通道满时会不会卡死、被调方在取消后回不回复、本地放弃的同步调用会不会留下清理不掉的边。它不建跨运行时转发、调用链 `path`、等待图的成环判断、事件帧序和节点托管行；同步等待只在“本地放弃”这一处记一条 ghost 边。

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
| 读线程的写回交给写线程：`throw`（解码失败、过深）和丢弃迟到答复的 `release` 都由读线程产生，读线程不等对端；在通道的 `send` 上按背压等待的是写线程 | 会话协议 §10（`SESS-75`、`SESS-76`）；[远程会话设计](../../docs/design-remote-session-2026-10-28.md) §三、§七（#1005） | `Connection::write` 与 `Outbound::push`（`rpc.rs:1825-1840,1387-1404`）、`write_frames`（`rpc.rs:1411-1444`） |
| 被调方在取消后是否回复：两种实现都允许。Rust 对排队中的调用不回复，已在运行的同步调用照常回复；模型把两种都跑通 | 会话协议 §7（`SESS-57`、`SESS-61`“明确不保证”） | `Frame::Cancel`（`rpc.rs:2693`）、`respond_to`（`rpc.rs:3083`） |
| 本地放弃的同步调用不发 `cancel`：调用方保留等待表项到回复，读线程读回复时清等待图的边；把表项和边一起丢掉而不发 `cancel`，回复就成了“未知调用”并结束会话 | 会话协议 §5.4（`SESS-41`）；追踪契约 §3.4 | `request_sync`（`rpc.rs:2004-2035`，没有 Drop 守卫）、`CancelOnDrop`（`rpc.rs:1115`，只有异步调用有）、`reply_target` 的 `waits::returned`（`rpc.rs:2494-2501`） |
| 调用方的 `cancelled` 表只由迟到回复清理：排队中被取消的调用没有任何实现会回复，这条表项留到会话结束（#626 的 `BoundedTables` 缺口） | 会话协议 §7（`SESS-57`、`SESS-61`） | `calls.cancelled`、`orphans`（`rpc.rs:1072-1080`）、`Connection::cancel`（`rpc.rs:2038-2053`） |

## 模型

- **参与者**：一个会话的两个端点。A 发起调用，B 接收、执行并答复。两个方向各自 FIFO、延迟任意长：A→B 上是 `invoke`、`cancel`、`release`，B→A 上是 `return`、`throw`。因此答复可以在取消写出时还在路上，取消也可以越过它要取消的答复。
- **A（调用方）**：`AStart` 取下一个号并写 `invoke`；`ADeliverLive` 把答复（`return` 或 `throw`）结算给等待它的调用，`return` 带的引用收下，`throw` 不带引用；`ACancel` 放弃一次调用（写出 `cancel`，调用以调用方的错误结算）；`AAbandon` 放弃一次同步调用但**不**写 `cancel`；`ADeliverLate` 把已取消或已放弃调用的迟到答复解码后丢弃（`return` 带的引用释放掉，会话继续）；`ADeliverStray` 处理“调用方已经放不下”的答复——按 `SESS-41` 是 `response for unknown call`，会话结束；`ARelease` 放弃它持有的引用并写 `release`；`AClose`/`ALearn` 结束这一端。
- **B（被调方）**：`BReadInvoke` 收下调用（还排队）；`BRejectInvoke` 在读到时拒绝（参数解码失败、值过深），由读线程写回 `throw`；`BRun` 开始执行；`BReply` 答复并把一次引用授予写出去；`BDrop` 是按“取消后不回复”的规则丢掉一次运行中被取消的调用（不写帧）；`BReadCancel` 处理取消——还在排队的丢掉、不答复，已在运行的按规则答复或丢掉，已答复的不动；`BReadRelease` 给出被释放的授予；`BClose`/`BLearn` 结束这一端。
- **关闭**：任一端可以关闭。关闭的一端把等待中的调用以错误结算、清空自己持有的引用表、丢掉两个方向上还在队列里的帧；另一端通过通道结束（`ALearn`/`BLearn`）做同样的事。`closeCause` 是记录“哪一端、因为什么先关闭”的 ghost。
- **通道与写线程**（`Bound`、`Cap`）：`Bound = FALSE` 时没有通道上限，帧写出即入队，和第一版模型一样；`Bound = TRUE` 时每个方向是一条容量 `Cap` 的通道，帧先进写线程的队列（`aout`、`bout`，无上限），写线程（`FlushA`、`FlushB`）在通道有空位时把队首写进去。读线程处理入站帧，必要时写回（见下）。`ablock`/`bblock` 是“读线程正卡在这一帧上写不出去”；`AUnblock`/`BUnblock` 在通道有空位时把它写完。
- **读线程写回**（`ReaderWrites`）：`"direct"` 是 #1005 之前的形状——读线程在通道上直接写，满就卡在 `send` 里，读帧也停下；`"queued"` 是现在的形状——读线程把帧交给写线程的队列就继续读。两个读线程写回点：A 丢弃迟到答复时的 `release`（`ADeliverLate`），B 读时拒绝一次调用写回的 `throw`（`BRejectInvoke`）。
- **取消后的回复**（`Replies`）：`"always"` 时运行中被取消的调用照常答复（迟到答复被丢弃、引用归还）；`"never"` 时方法结束后由 `BDrop` 丢掉、不写帧。排队中被取消的调用在两种规则下都不回复（`SESS-57`）。
- **本地放弃的同步调用**（`Sync`、`Abandon`）：`c ∈ Sync` 的调用在 A 记一条等待图的边（`aedge`，ghost），B 收下时记一条 mark（`bmark`，ghost）。`AAbandon` 只对同步调用可用（异步调用的 future 被丢弃时 `CancelOnDrop` 会发 `cancel`，同步等待没有这个守卫）。`"keep"` 保留等待表项和边直到回复；`"drop"` 把两者一起丢掉、不发 `cancel`，于是 B 的答复落在 `ADeliverStray` 上。
- **调用方的取消表**：`aTable` 记 A 的表里为一次调用留的项（`waiting`/`cancelled`/`abandoned`/`none`）。取消后是 `cancelled`，只有迟到答复或关闭把它去掉；`CancelledEntriesDrain` 在静止状态检查它是否为空。
- **终止**：`Done`（`AllSettled`）只在两端所有调用都已结算、所有通道和写线程队列为空、没有卡住的读线程时可用。这样 TLC 的死锁检查能区分“做完了”和“两个读线程互相等”，不需要给每个终态再加一个哑动作。

## 简化

- 规模是有意的有限：多数配置是 1 个调用方、1 个被调方、2 次调用、最多 1 次取消、1 个引用（每次答复授予一次，所以最多两单位授予）。`call-backpressure-before`/`-after` 用 3 次调用、1 次取消、每方向容量 1：要把两个读线程同时卡住，取消后答复的那次调用给 A 一个迟到答复，被拒绝的那次给 B 一个 `throw`，第三次的答复用来在被读走后重新填满另一方向——2 次调用时这条路径不可达。没有重连、没有第二个会话、没有多跳转发。
- 握手不建：模型从两端都已问候、会话可用开始。版本、端点、能力、`hello` 的顺序和 10 秒截止不建（`remote-lease` 覆盖顶替与握手的其它部分）；本文只从第一次调用开始。
- 帧只有 `invoke`、`cancel`、`release`（A→B）和 `return`、`throw`（B→A），`throw` 只建“读线程拒绝读到的调用”这一种来源；方法自己抛出的错误与 `return` 同一条结算路径，不单独建。`call`、`get`、`await`、`events.*`、`rows.*` 不建。
- 调用号只建“递增、不复用”：前缀、规范十进制形式、2^53 上限、`path` 里不得出现自己的号都不建。接收方对“号不增长”的拒绝也没有可达路径——正确的发送方不会重复号，重复号正是 `reuseId` 变异，所以这个检查在模型里是死代码。
- `release` 指向本端没有的引用、释放计数大于授予次数、`call`/`get`/`await` 指向没有的引用这类协议错误不建：正确的一侧不会发出它们，模型里每一条 `release` 都在授予范围内（会话错误另有追踪契约 §3.3、§3.5 的用例）。
- 调用链 `path`、`rebase`、会话标签、同步等待的成环判断（`SyncWaitCycle`）不建：那是跨运行时转发的范围（`cross-runtime-sync` 模型）。`Sync` 只用来记一条 `aedge` ghost，看本地放弃后有没有残留；它不是完整的等待图。
- 值、错误图、`bad-value`、`bad-frame`、16 MiB 上限和出站积压上限（`outbound_limit`）不建。`Bound`/`Cap` 只建通道容量与写线程的背压，不建“超过宿主设的上限就结束会话”。值编码与上限已在追踪契约 §3.5–§3.7 和属性测试里覆盖。
- 被调方在丢掉排队中的调用时释放参数里的引用（`Import` 的 `Drop` 写 `release`，也发生在读线程上）不建：模型里调用不带引用参数。它和 `BRejectInvoke` 的 `throw` 是同一种读线程写回，形状相同。
- 方法执行时间不建：调用从 `queued` 到 `running` 再到答复，答复可以发生在任意晚的时刻，包括取消之后；没有计时、没有调度顺序承诺。
- 关闭之后两个方向的队列都清空，任何一端都不再读：这是一处简化。真实实现里已经写到通道上的帧可能在通道结束前仍被对端读到；这个交错不在本模型里，README 的结论也不声称覆盖它。
- 关闭的原因不是协议帧：两端各自结束，另一端从通道结束得知。`closeCause` 只为区分“取消导致的关闭”“未知调用导致的关闭”这些错误设计。
- 内存序、真实调度、帧编码不建；每个动作原子。`Done` 的停机条件是按“没有在途工作”写的，不是实现里的某个状态。

## 不变式与活性

| 名字 | 内容 |
| --- | --- |
| `CancelKeepsSession` | 取消不结束会话：会话的关闭只来自调用方、被调方或通道结束（`closeCause ≠ "cancel"`） |
| `SettlesAtMostOnce` | 一次调用最多结算一次：答复、取消、本地放弃、会话结束四者不会让同一个调用结算两次 |
| `LateReplyDropped` | 迟到答复被丢弃：已取消或已放弃的调用保持当时的结果，答复不再结算它；每次调用最多丢弃一个答复 |
| `OwnReply` | 结算一个调用的答复就是这次调用自己的答复 |
| `NoIdReuse` | 一个调用号只属于一次调用：没有新调用再取它 |
| `NoOverRelease` | 调用方只归还它被授予的量：一份授予被释放、丢弃或清空一次，不会两次 |
| `GrantsConserved` | 静止时 B 授予的每一单位都还在 A 手里或已经归还：被丢弃的迟到答复没有漏掉它带的引用 |
| `ClosedClearsTables` | 关闭的一端不再持有引用：A 的导入表空、B 的导出表空 |
| `FailUnfinishedOnClose` | 关闭后两端都不留未完成的调用：A 侧没有还在等待的调用，B 侧没有排队或运行中的调用 |
| `WaitingSettles` | 活性（只在没有关闭的配置里检查）：A 等待的调用最终被答复、被取消或被放弃 |
| `AbandonKeepsSession` | 本地放弃的调用不结束会话：它的答复作为迟到答复被吸收，不是 `response for unknown call`；不保留表项就必须发 `cancel` |
| `CancelledEntriesDrain` | 静止时 A 的取消表里没有只等迟到答复来清理的表项（#626 的 `BoundedTables`） |
| `AbandonedCallsClean` | 静止时本地放弃的调用不留下等待图的边：答复读过之后边也清掉 |

## 结果

TLC 2.19（tla2tools v1.7.4），Java 21.0.12（Temurin），32 核，`-workers auto`。`通过` 指穷举完成、所列性质全部成立；被违反的配置找到第一个反例就停，死锁配置找到第一个死锁就停，状态数取决于搜索顺序，每次可能不同。整个目录约 20–30 秒（随机器负载）。

| 配置 | 内容 | 结果 | 不同状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `call-healthy` | 2 次调用，最多 1 次取消，两端都可以关闭；全部安全不变式 | 通过 | 1,155 | 1 s |
| `call-progress` | 不允许关闭，只检查活性（`WaitingSettles`） | 通过 | 409 | 1 s |
| `call-backpressure-before` | 3 次调用、1 次取消、每方向容量 1；读线程直接写回（#1005 之前），死锁检查开着 | **死锁**（`ablock` 与 `bblock` 各卡着一帧，两个通道都是满的） | 约 19,000–22,000（找到死锁即停） | 1–2 s |
| `call-backpressure-after` | 同一组调用、取消与容量；读线程把写回交给写线程的队列 | 通过（无死锁，`WaitingSettles` 成立） | 59,212 | 4–9 s |
| `call-cancel-reply` | 取消后回复的实现：运行中被取消的调用照常答复，迟到答复丢弃、引用归还；不允许关闭 | 通过 | 409 | 1 s |
| `call-cancel-no-reply` | 取消后不回复的实现：运行中被取消的调用由 `BDrop` 丢掉，不写帧 | 通过 | 437 | 1 s |
| `call-gap-cancel-table` | 现状的调用方取消表（只由迟到答复清理）+ 排队中被取消的调用（`SESS-57`：任何实现都不回复） | 违反 `CancelledEntriesDrain` | 378（找到反例即停） | 1–2 s |
| `call-sync-abandon` | 同步调用在本地放弃、不发 `cancel`，保留表项到答复 | 通过 | 615 | 1 s |
| `call-mut-cancel-closes` | 取消时关闭会话 | 违反 `CancelKeepsSession` | 11（找到反例即停） | 1 s |
| `call-mut-late-reply-twice` | 迟到答复被当成正常答复再结算一次 | 违反 `SettlesAtMostOnce` | 428（找到反例即停） | 1 s |
| `call-mut-late-reply-keeps-grant` | 丢弃迟到答复但不释放它带的引用 | 违反 `GrantsConserved` | 381（找到反例即停） | 0–1 s |
| `call-mut-close-leaks` | 关闭时保留导出表 | 违反 `ClosedClearsTables` | 358（找到反例即停） | 1 s |
| `call-mut-reuse-id` | 调用号可以被新调用复用 | 违反 `NoIdReuse` | 11（找到反例即停） | 0–1 s |
| `call-mut-reuse-id-settles` | 同一个变异的后半段：复用的号让答复结算了另一个调用 | 违反 `OwnReply` | 546（找到反例即停） | 0–1 s |
| `call-mut-abandon-drops` | 本地放弃时把等待表项和边一起丢掉，不发 `cancel` | 违反 `AbandonKeepsSession` | 467（找到反例即停） | 1–2 s |
| `call-mut-abandon-leaks` | 迟到答复被吸收，但等待图的边没有清掉 | 违反 `AbandonedCallsClean` | 615（找到反例即停） | 0–1 s |

## 变异与反例

每个被检查的性质都配一个故意引入的错误，TLC 必须找到反例（`expected.txt` 的 `violated:<名字>`）：

| 配置 | 变异 | 违反 | 反例 |
| --- | --- | --- | --- |
| `call-mut-cancel-closes` | `ACancel` 直接把 A 的会话关掉（§7 的“取消只作用于一次调用”被换成“取消等于结束”） | `CancelKeepsSession` | A 发起调用并取消它：`acall = "settled"`、`aclosed = TRUE`、`closeCause = "cancel"`——一次取消结束了整个会话 |
| `call-mut-late-reply-twice` | `ADeliverLate` 把已取消调用的答复当成仍然在等的结果结算 | `SettlesAtMostOnce` | A 取消调用 1（B 已在运行它），B 答复；`settles["c1"] = 2`：取消给了一个结果，答复又给了一个 |
| `call-mut-close-leaks` | `CloseB` 不清 `bgrants`（§6.1 的“会话关闭清空导出表”被去掉） | `ClosedClearsTables` | A 取消调用 1，B 照常答复并为它授予一次引用；A 关闭，B 得知后关闭，`bgrants = 1` 留在一个已关闭的会话里 |
| `call-mut-reuse-id` | `AStart` 不推进 `anext`：新调用取一个旧调用已经取过的号 | `NoIdReuse` | 调用 1 取号 1，随后调用 2 也取号 1：`aid = [c1 \|-> 1, c2 \|-> 1]` |
| `call-mut-reuse-id-settles` | 同一个变异，继续看它的后果 | `OwnReply` | 两个调用都叫 1；调用 2 被取消，B 答复调用 2（号 1）；A 用这个答复结算了调用 1：`settledBy["c1"] = "c2"` |
| `call-mut-late-reply-keeps-grant` | `ADeliverLate` 丢弃迟到答复但不释放它带的引用（追踪契约 §3.4 的“先解码、再释放”被去掉） | `GrantsConserved` | A 取消调用 1，B 已答复并为它授予一次引用；A 丢弃迟到答复后 `aheld = 0`、`aReleased = 0`、`granted = 1`，静止时 0 ≠ 1：这份授予没有人归还 |
| `call-mut-abandon-drops` | `AAbandon` 在 `"drop"` 下把等待表项和 `aedge` 一起清掉、不发 `cancel`（把“保留到答复，或者发 `cancel`”换成“本地忘掉”） | `AbandonKeepsSession` | 同步调用 1 被本地放弃，B 照常答复 ret 1；A 的表里已经没有它，`ADeliverStray` 把它算作未知调用，`closeCause = "unknownCall"`、`aclosed = TRUE`——一次本地放弃结束了会话 |
| `call-mut-abandon-leaks` | `ADeliverLate` 吸收已放弃调用的迟到答复，但不清 `aedge`（等价于读线程没有调用 `waits::returned`） | `AbandonedCallsClean` | 静下来时所有调用都已结算、队列为空，`aedge["c1"] = TRUE` 仍在：等待图里留下一条没人会再清理的边 |

两个复用号的配置是同一条变异的两个结论：先是不变式本身被破坏（一个号属于两次调用），然后是它带来的错误结算（答复落到别的调用上）。两个放弃的配置也一样：先是“本地忘掉会结束会话”，然后是“忘了清边会留下无法清理的边”。

## 修复前后的配置与缺口配置

两个背压配置的其余写路径相同：调用方和被调方的帧都先进入写线程的队列，由写线程写进通道；差别只在读线程的写回——`"direct"` 让它在通道上直接写、满了就停下读，`"queued"` 让它交给写线程。所以死锁只能归因于读线程写回这一处，而不是调用方或被调方的写。

| 配置 | 建模的行为 | 结果 |
| --- | --- | --- |
| `call-backpressure-before` | #1005 之前：读线程的写回直接写通道，通道满就卡住读线程 | TLC 找到死锁：`ablock = <<"release", 1>>`、`bblock = <<"throw", "c1", 3>>`，`toB = <<"cancel", "c2">>`、`toA = <<"throw", "c3", 2>>`，两边都是满的；没有关闭、调用和取消都用完，除了这两个写回没有别的动作。要腾出一个方向只能让对面的读线程读，而它正卡在自己的写回上 |
| `call-backpressure-after` | 现在：读线程把写回交给写线程的队列，等在 `send` 上的是写线程 | 无死锁，`WaitingSettles` 成立。模型锁住的是“读线程不直接写回”；实现里 #1005 已经是这个形状，**不需要改代码** |
| `call-gap-cancel-table` | 现在的调用方取消表：`calls.cancelled` 的项只由迟到答复清掉 | 违反 `CancelledEntriesDrain`：A 取消一次还在排队、B 未开始的调用（B 丢掉、不回复），所有帧处理完、没有关闭时 `aTable["c1"] = "cancelled"` 仍在。这条反例在 `Replies = "always"` 下也成立，因为 `SESS-57` 规定排队中被取消的调用在任何实现里都不回复；换掉回复规则不能让表项消失 |

## 结论

- **读线程写回不会卡死，前提是读线程不直接写。** #1005 之前（`call-backpressure-before`）用 3 次调用、1 次取消、每方向容量 1 就能走到死锁：两个读线程各卡在一帧写回上，两个通道都是满的，能腾出通道的读动作都在对方卡住的读线程手里。现在的形状（`call-backpressure-after`，读线程把帧交给写线程的队列）没有死锁，所有调用结算；实现在 #1005 已改成这个形状，本模型不改它，只把结论和回归配置留下来。模型不建宿主的上限（`SESS-75`），写线程队列在模型里无界；上限到期结束会话是另一条规则，不在本目录的结论里。
- **取消后的两种回复实现都允许，都不重复结算、不漏引用。** `call-cancel-reply`（运行时中被取消的调用照常答复）里，迟到答复被丢弃，答复带的引用随即释放；`call-cancel-no-reply`（方法结束后丢掉、不写帧）里，不产生答复也不产生授予。两种都通过 `SettlesAtMostOnce`、`LateReplyDropped`、`OwnReply`、`NoOverRelease`、`GrantsConserved`（静止时授予的每一单位都还在 A 手里或已归还）和活性；`call-mut-late-reply-keeps-grant` 说明最后这条不是空的。`SESS-61` 继续是“明确不保证”，本目录不改协议正文。
- **本地放弃的同步调用不留无法清理的边，前提是调用方保留调用记录直到答复。** 现在的 `request_sync` 没有 Drop 守卫（`CancelOnDrop` 只挂在异步调用上），同步调用被本地放弃时不发 `cancel`；等待表项留在 `calls.waiting`，读线程读到答复时 `reply_target` 摘掉它并 `waits::returned` 清边，B 的 mark 在答复时清掉。`call-sync-abandon` 通过，`call-mut-abandon-leaks` 说明边不清就会留下。把表项和边一起丢掉而不发 `cancel` 会让答复成为 `response for unknown call` 并结束会话（`call-mut-abandon-drops`），所以规则是：**要么保留到答复，要么发 `cancel`**；模型不要求同步放弃必须发 `cancel`。
- **#626 剩下的缺口：调用方的取消表。** `SESS-57` 规定排队中被取消的调用不回复，任何实现都不回复；调用方却要留着 `calls.cancelled` 的项来吸收迟到答复。于是每取消一次未开始的调用，这条表项留到会话结束——`call-gap-cancel-table` 在 `Replies = "always"` 下也违反 `CancelledEntriesDrain`。这不是回复规则能修的：要么被调方对每次取消都回一个确认（改 `SESS-57`），要么在协议里加一个取消确认帧，要么调用方用一个有界的记法（例如“已取消且未答复”的水位）并相应放宽 `SESS-41` 对未知答复的处理。本任务只改 `specs/call-lifecycle/`，没有改协议正文和实现，这一条留给 #626 的“结论写回”一节。
- 按协议 §5–§7 的其余规则——号只增不复用、一次调用只结算一次、取消只丢调用不丢会话、迟到答复丢弃、关闭时失败未完成调用并清空引用表——在穷举范围内仍然成立：没有调用被结算两次；取消不会关闭会话；已取消调用的迟到答复不会改变它的结果；一端的关闭让另一端也结束，两端都不留下未完的调用和引用；关闭前的每次授予只被归还一次。`call-healthy` 的状态数（1,155）与扩展前一致，说明新开关关掉时模型的行为没有变。
- 模型没有证明的：握手与鉴权；跨运行时转发、中继、调用链与同步等待成环（`cross-runtime-sync`）；事件与行的帧序；值编码、错误图、上限与出站积压上限；多跳取消传播（`SESS-59` 的中继部分）；关闭后仍在通道上的帧；被调方丢掉排队调用时释放参数引用的读线程写回（形状同 `throw`）；`PinnedTarget`（执行中的调用持有对象，模型里没有对象值）；真实调度、内存序与计时。本目录只是 #613 调用生命周期的一块，不能当作完整调用协议的证据。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh                    # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh call-healthy       # 单个配置
TLC_WORKERS=4 TLA2TOOLS=/tmp/tla2tools.jar specs/call-lifecycle/run.sh      # 限定 TLC 的线程数
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；结果是 `ok`、`deadlock` 或 `violated:<名字>`。日志和反例写在 `TLC_OUT`（默认一个临时目录）。CI 的 `specs` 任务矩阵包含 `call-lifecycle`（见 [CI](../../docs/ci.md) §4）。

## 维护

会话协议 §5、§6、§7、§10 修改时，或 `crates/rutis-bridge/src/session/rpc.rs` 的 `Calls::admit`、`reply_target`、`Frame::Cancel`/`Frame::Release`/`Frame::Return` 的处理、`Connection::cancel`、`Connection::close`、`Connection::write`/`Outbound::push`/`write_frames`（读线程写回与写线程）、`request_sync` 与 `CancelOnDrop`（本地放弃）改变时，或运行时一侧（`node/rutis-runtime/src/session.mjs` 等）的取消、回复与引用释放行为改变时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。
