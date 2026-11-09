# 跨运行时同步调用与死锁策略（TLA+）

[English](README.en.md)

用 TLC 穷举"多个语言运行时经 Rust 宿主互相同步调用"的交错，回答多语言设计 §九 留下的问题：两个候选策略哪个能消除卡死。对应 issue #208，质量规范 Q11.2、Q6.2.8、Q6.8.3，现状 §7.1（风险 P10、C6）。

**对账（2026-11-09，#614）。** 模型的等待图按当前 `crates/rutis-bridge/src/session/rpc/waits.rs` 重写：[#561](https://github.com/arcships/rutis/issues/561) 的慢路径保留调用链、[#597](https://github.com/arcships/rutis/issues/597) 的宿主自发同步调用记一跳、854bef5d 的"已标记链上的异步调用也记一跳"；补上 Go / Swift / Kotlin 的 `Profile`（`stacked = false`，常量 `Threaded`）、`bail` / `waterfall-sync` 的同步分发、调用方在本地放弃同步调用（不发 `cancel`）、Python 从别的线程发起的同步调用（宿主看不见的边）、会话结束，以及 SYN-9-3 的明确不保证。`#535` 之后的语义变化见"结果"和"反例"。

## 对应的设计和代码

| 模型部分 | 设计 | 实现 |
| --- | --- | --- |
| 转发、`path` 改写 | [多语言设计](../../docs/design-multilang-runtimes-2026-10-03.md) §五；[M1](../../docs/design-multilang-m1-2026-10-04.md) §七 | `crates/rutis-bridge/src/session/rpc.rs` `rebase`、`forward`；`rpc/relay.rs`；`runtime/rows.rs` `RowService::invoke` |
| Rust 收到调用后交给哪个线程 | M1 §7.2 | `rpc.rs` `receive`：按 `path` 从后往前找正在同步等待的调用号 |
| 等待表的六个操作（`profile`、`marked`、`answered`、`returned`、`ended`、`forward`）与调用的三种分类（同步、已标记链上的异步、入口） | 多语言设计 §九 "实现" | `crates/rutis-bridge/src/session/rpc/waits.rs`；`rpc.rs` `receive`/`send` |
| 每个调用一个线程的运行时（Go、Swift、Kotlin）：`sync-wait`、`reentrant-sync`，不声明 `sync-stack` | 多语言设计 §九（2026-10-14） | `waits.rs` `Profile.stacked`；`go/rutis/internal/peer/peer.go`、`swift/rutis/Sources/Rutis/Session/Peer.swift`、`kotlin/rutis/rutis/src/shared/kotlin/arcships/rutis/internal/peer/Peer.kt` |
| 没有声明 `sync-wait` 的运行时不计入 | 多语言设计 §九「新旧版本混用」 | `waits.rs` `forward`：没有 `sync` 的调用不登记；`profile` |
| 同步的事件分发 `bail`、`waterfall-sync` | [事件协议](../../docs/design-events-protocol-2026-10-18.md) §5.4 | `crates/rutis-bridge/src/events/hub.rs` `dispatch`（`Mode::Bail`、`Mode::Waterfall if sync`）、`call_sync`、`Waits`/`wait_here` |
| Node 运行时：不可重入 | 多语言设计 §九；[插件 API](../../docs/guide/plugin-api.md)"同步调用与可重入" | `node/rutis-runtime/src/session.mjs` `#requestSync`、`#run` |
| Python 运行时：可重入；从别的线程发同步调用 | 同上 | `python/rutis/rutis/peer.py` `_call_sync`、`_run`；`run_coroutine_threadsafe` |
| 本地放弃同步调用（不发 `cancel`） | 多语言设计 §九（#561 后） | Go 的 context、Swift 的 Task、Kotlin 的中断 |
| 策略 B（A 不采用） | 多语言设计 §九 | `waits.rs`（#228）；调用帧的 `sync` 字段见 `session.mjs`、`peer.py`、`bun/rutis-bun/src/session.ts` |

## 模型

- **参与者**：Rust 宿主和 3 个运行时。运行时分两种：一个线程一个栈的（`Threaded` 之外，声明 `sync-stack`）和每个进来的调用一个线程或任务的（`Threaded`，Go / Swift / Kotlin）。每个运行时和 Rust 之间一个会话，每个方向按顺序送达。
- **一个栈的运行时**：只有栈顶在跑。栈顶发起同步调用后，线程一边等一边读进来的帧（pump）：回复标记对应的调用已返回；调用空闲时直接执行，等待中时按可重入与否嵌套执行或放到一边；正在跑的栈顶不读帧。
- **一个调用一个线程的运行时**：每个进来的调用直接在自己的线程上开始，谁也不等谁；同一运行时里没有栈顺序，也不会有调用被放到一边。它自己发出的同步调用照样等。
- **Rust**：按会话顺序读帧，转发调用、改写 `path`，把结果送回调用方；自己的线程不是受限资源（README"简化"）。它自己也发起同步调用（Rust 插件调用某个运行时），并执行 `bail` / `waterfall-sync` 的分发。
- **调用的分类**（`waits.rs` 的 `forward`）：调用方在同步等待、且它的调用链最后一跳是已标记的调用 → **一跳**（`kind` = `"hop"`）；在同步等待但链上没有已标记的调用 → 一跳，`src` 为空（#597）；异步调用、但链的最后一跳已标记 → 也记为一跳（`kind` = `"async"`，没人真的等它）；其余 → **入口**（`entry`）。没有声明 `sync-wait` 的运行时（不在 `Declared`）发来的同步调用记为入口（`kind` = `"hidden"`），宿主看不见这条等待。
- **发出与投递**：正在执行的活动可以发出一个事件（总数 `MaxPosts`）：一个没有调用方等待的 `events.dispatch`，活动接着往下执行，Rust 随后把监听器的调用投递给监听器所在的运行时（入口）。`EmitChain` 决定这次投递带不带发出方的调用链（#561：不带）。
- **同步分发**：`bail` 和 Cordis 插件的 `waterfall` 让发出方等整个分发；宿主调一个监听器并等它。模型里 `Dispatch` 只在一个顶层活动上发生（插件的 `ctx.emit` / `ctx.bail`），`DispatchHop` 决定 Rust 是否把这次分发本身记成一跳；`bail` 的监听器调用不带调用链，`waterfall` 的带（调用回发出方进程时要落在它正在等的那条链上）。
- **会话结束**：`Ends` 打开时，会话可以在没有占用别的会话时结束：它的调用从表里移除，进入它的调用以错误回复（`SYN-9-30`）。
- **调用号**：每个会话各自编号；模型另外给每个调用记一份真实的祖先链（`truth`），和线协议里的 `path` 分开。
- **策略**（常量 `Policy`）：`current` 不检测；`A` 拒绝两个不可重入运行时之间的同步调用；`B` 每次转发前建等待图，成环则回 `SyncWaitCycle`；`Bchain` 是 B 去掉"栈顺序"那类边。

### 等待图

节点是 Rust 已转发、还没返回的调用（`hops`），另加没人等待的投递（`posts`）和不记进表的分发（`dispatches`）。边 c → d 表示 c 要等 d 完成；成环即卡死。

| 边 | 含义 | Rust 能否看到 |
| --- | --- | --- |
| 链 | d 是在 c 之下发起的（d 的 `path` 含 c 的号） | 能：`path` 改写后带会话标签 |
| 放到一边 | c 进入一个不可重入、只有一个栈、正在等待的运行时 y，c 不在其链上，也还没在那里开始，c 要等 y 发出的所有调用 | 能：y 的未完成调用都在 Rust 手里；y 是否可重入、有没有一个栈由 `reentrant-sync`、`sync-stack` 得知 |
| 栈顺序 | c 和 d 先后进入同一个只有一个栈的运行时 y，d 已经在 y 上开始执行，c 要等 d | 能：同一会话的帧有序 |
| 分发 | 宿主在一次同步分发里调用监听器，分发等这个调用；调用回发出方时落在它等的那条链上 | 分发本身不在表里（`DispatchHop` = FALSE）；监听器调用是宿主自己的同步调用（一跳） |
| 入口 | 没人同步等待的调用（`emit` 的 `events.call`、已标记链上的异步调用、宿主看不见的同步调用）是链的边界 | 表里记了（`entry`），但不在"放到一边"里算作 y 发出的调用 |

模型里有两份图：`TrueSucc` 用运行时的真实栈、队列和每个节点的 `real`（真的有人在等），只用于判断；`RustSucc` 只用 Rust 自己的状态和它记下的 `entry`（策略 B 实际能做的）。一个节点的 `real` 为假时（没人等它：投递、已标记链上的异步调用、被本地放弃的调用），它不在真实的环上，但可能在 Rust 的图里。

## 简化

- 运行时发出的调用可以是同步的、异步的（`Async`）或"自己等、但帧上不写"的（`Hidden`）；Rust 的投递和同步分发照上面建。
- 同一运行时内部的调用不经过 Rust，不建模。
- 只建模服务调用和函数（回调）调用；`await`、引用计数、值的编码不建模。
- 同步分发只在顶层活动上发生；`next`（waterfall 的链）和终态不建：模型里一个分发只有一个监听器。监听器在发出方自己的运行时里的情形建了，`next` 会多出"监听器 → 宿主"那条边，模型没有它。
- 取消只建"调用方在本地放弃、不发 `cancel`"（`Abandoners`，只对 `Threaded` 的运行时开放，Go / Swift / Kotlin 的实际情形）：迟到的回复被丢弃。带 `cancel` 的取消、迟到的 `ret` 堵住读线程、Node 的按键扣住和 behind 任务不建。
- 会话结束只在该会话没有发出调用、也没有参与分发时发生（`Ends`）；结束时进入它的调用以错误回复。断开、重连、`rows.ended` 不建。
- Rust 的线程不是受限资源：每次转发在等待这条链的线程上或新线程上执行（#614 的 `Workers`、有界执行器不建）。
- 每个活动默认只发起一次调用；`policy-b-2node-seq` 允许先后两次。
- 规模：3 个运行时，调用链 ≤ 3 跳（`async-policy-b` 到 4 跳），同时存在的顶层活动 ≤ 2，投递 / 分发 ≤ 1。

## 不变式

| 名字 | 内容 |
| --- | --- |
| 不死锁 | TLC 的死锁检查：还有未完成的调用时总有参与者能继续。全部完成后的状态用 `Done` 原地停留。模型里的每个行为都是有限的，所以"没有死锁"等价于"每个调用最终都会完成" |
| `RoutingSound` | 一个调用被当作某条等待链上的调用执行（不可重入运行时嵌套执行它，或 Rust 把它交给某个等待中的线程）时，它确实属于那条链（按真实祖先，不要求中间的节点有人等） |
| `ChainRecognised` | 不可重入运行时不会把自己链上的调用放到一边。"链上"按真实祖先、并且链上每个节点都真的有人等（投递等不算） |
| `NoFalseCycle` | 策略 B 回 `SyncWaitCycle` 时，真实等待图确实成环 |
| `NoLegitimateRefused` | 策略 A 不拒绝合法调用（转发它不会让真实等待图成环） |
| `NeverCycle` | 只用于确认 B 的检测确实会触发（预期被违反） |
| `NeverNested` | SYN-9-3（预期被违反）：可重入运行时的服务在它自己的同步调用中途被调用。这不是缺陷，是明确不保证，用一条性质把它钉住 |

## 结果

TLC 2.19（tla2tools v1.7.4），Temurin 21，16 核；`MaxRoots = 2`、`MaxDepth = 3`、`MaxCalls = 1`，另注明的除外。

| 配置 | 运行时（`n` 不可重入，`p` 可重入，`g` 一个调用一个线程） | 结果 | 状态数 | 时间 |
| --- | --- | --- | --- | --- |
| `current-1node-1python` | n, p | 通过 | 606 | <1 s |
| `current-2node` | n1, n2, p | **死锁**（反例 1） | 1,006 | <1 s |
| `current-1node` | n, p1, p2 | **死锁**（反例 2） | 8,908 | 1 s |
| `current-reentrant` | p1, p2, p3 | 通过 | 88,824 | <1 s |
| `current-norebase` | n1, n2, p，不改写 `path` | **违反 `RoutingSound`**（反例 3） | 268 | <1 s |
| `policy-a-1node` | n, p1, p2（A 本来设想的组合） | **死锁**（同反例 2） | 9,706 | <1 s |
| `policy-a-2node` | n1, n2, p | **死锁**（同反例 2 的形状） | 3,066 | <1 s |
| `policy-a-refusals` | n1, n2, p | **违反 `NoLegitimateRefused`**（反例 4） | 93 | <1 s |
| `policy-bchain-2node` | n1, n2, p | **死锁**（同反例 2） | 5,798 | 1 s |
| `policy-b-2node` | n1, n2, p | 通过 | 37,311 | 1 s |
| `policy-b-1node` | n, p1, p2 | 通过 | 55,327 | 1 s |
| `policy-b-nonreentrant` | n1, n2, n3 | 通过 | 27,000 | 1 s |
| `policy-b-2node-depth4` | n1, n2, p；链 ≤ 4 | 通过 | 149,659 | 1 s |
| `policy-b-2node-3roots` | n1, n2, p；3 个顶层 | 通过 | 1,190,945 | 8 s |
| `policy-b-1node-3roots` | n, p1, p2；3 个顶层 | 通过 | 2,468,607 | 16 s |
| `policy-b-2node-seq` | n1, n2, p；每个活动 2 次调用，链 ≤ 2 | 通过 | 2,549,258 | 19 s |
| `policy-b-acts` | n1, n2, p | 违反 `NeverCycle`（预期：B 会触发） | 408 | <1 s |
| `current-posts` | n, p, q（p、q 可重入）；2 个顶层调用、1 次发出 | **卡死**（反例 5） | 334,773 | 2 s |
| `policy-b-posts` | 同 `current-posts`，策略 B 带入口；`MaxDepth=2` 的 CI 规模 | 通过 | 1,152,323 | 本地 32 核 16 s，CI 4 核估计 1–2 min |
| `policy-b-posts-node` | n, p, q（p 不可重入，`q` 可重入），策略 B；`MaxDepth=2` 的 CI 规模 | 通过 | 641,339 | 本地 32 核 10 s，CI 4 核估计 1–2 min |
| `threaded-policy-b` | n, g, p（g 一个调用一个线程） | 通过 | 62,128 | <1 s |
| `unmarked-policy-b` | n, g, p；g 不声明 `sync-wait` | **死锁**（反例 9，明确不保证） | 762 | <1 s |
| `bail-policy-b` | r, t；`bail` 分发，Rust 不记分发本身 | **死锁**（反例 6） | 449 | <1 s |
| `waterfall-policy-b` | r, t；`waterfall` 分发 | 通过 | 3,277 | <1 s |
| `bail-recorded` | r, t；`bail`，Rust 把分发记成一跳 | 通过 | 3,494 | <1 s |
| `emit-unchained` | n, p, q；`emit` 的监听器调用不带链（`EmitChain` FALSE），1 个顶层 | 通过 | 69,591 | 2 s |
| `host-call` | n1, p；宿主自己的同步调用 | 通过 | 5,208 | <1 s |
| `before-host-call` | 同上，#597 之前（记为入口） | 通过 | 5,208 | <1 s |
| `abandon-policy-b` | n, g, p；g 本地放弃一次同步调用 | **违反 `NoFalseCycle`**（反例 7） | 71,002 | 1 s |
| `hidden-policy-b` | n, p, q；p 从别的线程发同步调用 | **违反 `ChainRecognised`**（反例 10，明确不保证） | 772 | <1 s |
| `async-policy-b` | n, p, q；已标记链上的异步调用 | **违反 `NoFalseCycle`**（反例 8） | 29,660 | 1 s |
| `nested-service` | p1, p2 | 违反 `NeverNested`（预期，SYN-9-3） | 118 | <1 s |
| `end-session` | n1, n2, p；允许会话结束 | 通过 | 84,695 | 1 s |

"通过"指穷举完成：无死锁，所列不变式全部成立；这些行的状态数是确定的。死锁和不变式被违反的行，TLC 找到第一个反例就停，状态数取决于搜索的先后，每次运行都可能不同，只作参考。

表里前 20 行是 #535 时的结果和状态数。这次扩展后记录多了字段（`kind`、`entry`、`real`、`mk`），同样的配置状态数会略增；开发过程中重跑过这些配置，结果都和表里一致；`policy-b-posts`、`policy-b-posts-node` 为适配 CI 把 `MaxDepth` 从 3 降到 2，表里是缩小后的状态数。新增的配置都跑完了，每个 1 分钟以内。

`current-1node-1python` 支持多语言设计 §九 里"一个 Node 和一个 Python 之间不会卡死"的说法。`policy-b-2node-seq` 在链 ≤ 3 时 10 分钟内状态数超过 4,400 万仍在增长，所以缩到链 ≤ 2。

### 反例

1. **两个 Node 运行时交叉同步调用**（`current-2node`）。n1 同步调用 n2 的服务，同时 n2 同步调用 n1 的服务。两个调用都不在对方的链上，各自被放到一边，互相等待。这是 §九 已知的情况。
2. **可重入运行时上的栈顺序**（`current-1node`，A、Bchain 同理）。
   1. n 同步调用 p2 的服务 `s1`；
   2. `s1` 同步调用 p1；
   3. p1 自己（比如定时器）同步调用 p2 的服务 `s2`。p2 正在等，可重入，于是把 `s2` 嵌套在 `s1` 上面执行；
   4. `s2` 同步调用 n 的服务。n 正在等 `s1`，这个调用不在它的链上，放到一边；
   5. `s1` 等的调用已经返回，但 `s2` 在栈顶，`s1` 要等 `s2` 返回；`s2` 等 n；n 等 `s1`。卡死。

   这说明"叶子运行时都可重入"本身不能消除卡死：可重入把"放到一边"换成了"压在下面"，只要还有一个不可重入的运行时，就能成环。只有一个 Cordis 运行时也会发生。
3. **不改写 `path`**（`current-norebase`）。n1 和 n2 都在等各自的 `node:1`；n1 的调用转到 n2 时 `path` 是 `["node:1"]`，n2 把它当成自己的 `node:1` 链上的调用，嵌套执行了。现在的 `rebase` 防止的正是这个。
4. **A 拒绝不会卡死的调用**（`policy-a-refusals`）。n1 同步调用 n2，n2 空闲，没有任何别的调用，A 仍然拒绝。
5. **投递压在等待上面**（`current-posts`）。n 同步调用 p 的 `x`，`x` 同步调用 q；这时 q 自己发出一个事件，监听器在 p，Rust 把监听器的调用投递给 p；p 可重入，把它压在 `x` 上面执行；监听器同步调用 n，n 在等 `x`，把它放到一边。`x` 被压在下面，n 等 `x`、`x` 等监听器、监听器等 n。没有调用方等这次投递，旧的图里没有它，环查不出来；把它记为入口后（`policy-b-posts`），监听器的调用被拒绝。监听器调用不带调用链时（`emit-unchained`，#561）这个规模下环仍然查得到：那次的边来自"放到一边"和栈顺序，不靠发出方的链。
6. **同步分发上的一块盲区**（`bail-policy-b`，2026-11-09）。r（不可重入）发一次 `bail`：r 等这次分发，宿主调 t 的监听器并等它，监听器同步调用 r。`waits.rs` 不把这次分发记成一跳，`bail` 的监听器调用也不带调用链（`Mode::Bail` 分支没有压入 `Waiting`），所以 r 不认识这个回调、把它放到一边；r 等分发、分发等监听器、监听器等 r。策略 B 看不到这条环，卡死。
   - `waterfall-policy-b` 是同一形状的 `waterfall-sync`：它的调用回发出方时带发出方那次等待的链，r 把回调嵌套执行，通过。
   - `bail-recorded` 是候选修法：Rust 把这次分发的调用号放进监听器调用的 `path`（相当于把分发本身记成一跳），r 认得回调，通过。结论：`bail` 分发要在等待图里可见，否则协议里"每个同步调用都参加等待环检测"对 `bail` 不成立。
7. **本地放弃之后留着的那条边**（`abandon-policy-b`）。Go 的 context、Swift 的 Task、Kotlin 的中断放弃一个同步调用时不发 `cancel`：调用方的线程接着跑，宿主表里那条调用还在，直到回复到达。图里它仍然是"这个运行时发出的调用"，于是 B 可能因为一条没人再等的边拒绝一次本来能完成的调用（违反 `NoFalseCycle`）。修法是放弃时发 `cancel`（#561 已定的规则），或者接受误报。
8. **已标记链上的异步调用**（`async-policy-b`，854bef5d）。调用方不等这次异步调用，但它在已标记的链上，`waits.rs` 仍把它记成一跳。n 在等一次调用，p 的异步调用进入 n 时被放到一边，Rust 的图里却是"n 发出的调用"加上这条新调用，于是一次不会卡死的调用被拒绝（违反 `NoFalseCycle`）。要修就得在调用帧上区分"同步等待"和"发出即不管"。
9. **没有声明 `sync-wait` 的运行时**（`unmarked-policy-b`，SYN-9-26）。g 的同步调用不带 `sync`，宿主不登记，整个会话在检测之外；n → g → p → n 的环查不出来，卡死。这是**明确不保证**（旧版 SDK 与以前一样可能卡死，但不误报）。
10. **别的线程发起的同步调用**（`hidden-policy-b`）。Python 经 `run_coroutine_threadsafe` 从另一个线程发同步调用，帧上没有 `sync`，宿主看不见这条等待；调用进入正在等的不可重入运行时时被放到一边，`ChainRecognised` 被违反（卡死的第一步）。这是**明确不保证**。
11. **可重入运行时中途被调**（`nested-service`，SYN-9-3）。p1 同步调用 p2 的服务；p2 在同一调用里又同步调用 p1；p1 的另一个活动同步调用 p2，p2 可重入，把这次调用嵌套在执行中的调用上面。`NeverNested` 被违反，这是**明确不保证**——"调用 rutis 服务时不要持有锁"无法强制，模型用一条预期被违反的性质把它钉住。

## 推荐：策略 B（含栈顺序）

维护者已采用（写入[多语言设计](../../docs/design-multilang-runtimes-2026-10-03.md) §九，实现见 #228）。

理由（#535 时）：A 不够；B 在模型范围内消除了卡死而且不误报；B 只用 Rust 已有的信息；栈顺序那类边不能省；新语言的运行时可以自己选可重入或不可重入，只要如实声明。

B 实现时需要满足的条件（模型的前提）：

1. 宿主范围内一张等待表，记录所有已转发、未返回的同步调用（来源会话和调用号、目标会话和调用号、解开标签后的链）；
2. 返回在读帧的顺序里登记：同一会话里，先到的返回必须先于后到的调用从表里移除；
3. 每次转发同步调用前检查，成环则不转发，回 `SyncWaitCycle`，消息里写出环上的调用；
4. 每个运行时是否可重入、是否只有一个栈要可靠：握手里的 `reentrant-sync`、`sync-stack`（Node 运行时不声明 `reentrant-sync`；Go / Swift / Kotlin 不声明 `sync-stack`）；
5. 只把**同步**等待算进图（见"待定"）。

## 待定与未覆盖

- **区分同步和异步调用（已定，模型已建）**：调用帧的 `sync` 字段标明调用方在同步等待；只有带这个字段的调用、以及链尾已标记的调用计入等待图（#228、854bef5d）。模型显示"链尾已标记的异步调用也记一跳"会误报（反例 8），要修实现，或者只把 `sync` 的调用计入。
- **同步分发的可见性（新，反例 6）**：`bail` 的分发和它的监听器调用要能在等待图里连起来（记成一跳并让监听器调用带着发出方的等待号），否则卡死查不出。`waterfall-sync` 现在靠"调用回发出方进程时带最内层等待链"通过。
- **`#614` 仍未建的部分**：宿主的 `Workers`（有界工作线程，以及 `policy-b-workers1`）、`serial` / `parallel` / `waterfall` 的异步分发（宿主作为参与者上的执行）、带 `cancel` 的取消和迟到的 `ret`、Node 的按键扣住与 behind 任务、Cordis 节点的连接会话（一条连接上等待时不读别的会话）、跨主机的环、运行时一侧的有界线程池（Kotlin 的 IO 池、Swift 的协作池）。还有 `before-lost-chain`、`before-cancel` 两个修复前配置和 `cycle_among` 的差分属性测试、等待图的 trace validation。
- **Rust 自己发起的同步调用**建成了宿主的调用（`host-call`、`before-host-call`），但宿主的线程数不受限。
- **B 的开销**：每次同步转发都要在全局表上做一次可达性检查；表的大小等于正在进行的同步调用数。模型不测时间。
- 被拒绝的调用方收到 `SyncWaitCycle` 后如果马上重试，可能反复成环；由调用方决定是否退避，模型不覆盖。

## 怎么运行

需要 Java 11 以上和 `tla2tools.jar`（[v1.7.4](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)，sha256 `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`）。jar 不要提交进仓库。

```sh
mise install java@temurin-21
curl -sSLo /tmp/tla2tools.jar https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
TLA2TOOLS=/tmp/tla2tools.jar mise exec java@temurin-21 -- specs/cross-runtime-sync/run.sh            # 全部配置
TLA2TOOLS=/tmp/tla2tools.jar mise exec java@temurin-21 -- specs/cross-runtime-sync/run.sh bail-policy-b   # 单个配置
```

`run.sh` 把每个配置的结果和 `expected.txt` 比较，全部符合时退出码为 0；日志和反例写在 `TLC_OUT`（默认一个临时目录）。不用脚本，在本目录下直接跑一个配置：

```sh
java -cp /tmp/tla2tools.jar tlc2.TLC -workers auto -config bail-policy-b.cfg CrossRuntimeSync.tla
```

新加的配置每个 1 分钟内跑完；`policy-b-posts`、`policy-b-posts-node` 缩到 `MaxDepth=2` 后本地十几秒，CI 4 核估计 1–2 分钟；`policy-b-1node-3roots`、`policy-b-2node-seq` 十几秒。

## 维护

多语言设计 §五、§九 或 M1 §七 修改时，或 `rpc.rs`、`waits.rs`、`events/hub.rs`、`session.mjs`、`peer.py` 改变同步等待期间执行哪些调用、哪些调用记进等待图的规则时，同步修改这个模型和 `expected.txt`（规范 Q11.2.3）。
