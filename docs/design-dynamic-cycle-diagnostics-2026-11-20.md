# 长环与未知提供者的动态诊断

[English](design-dynamic-cycle-diagnostics-2026-11-20.en.md)

状态：草稿，含 PoC。日期：2026-11-20。基准：`main` `5c2e143e`（0.23.0）。
Issue：[#1326](https://github.com/arcships/rutis/issues/1326)。按本稿实施：[#1326](https://github.com/arcships/rutis/issues/1326)（§六 拆分）。
依据：[设计哲学](design-philosophy.md) §七、§八（依赖环诊断）；[设计稿约定](design-docs.md)；[诊断设计](design-diagnostics-2026-10-26.md) §4.5、§4.6；[出错信息](design-error-messages-2026-11-03.md) §3.3、§3.5、§4.4（#663 概念预算）。

范围外：

- 内核依赖解析与调度策略：`rutis` 内核不承诺环检测（[设计哲学](design-philosophy.md) §八），本设计完全留在 `rutis-loader` 的诊断快照里。
- 同步调用重入环（`SyncWaitCycle`）：那是调用期死锁，由桥和运行时报错（[诊断设计](design-diagnostics-2026-10-26.md) §4.3），不在启动依赖等待诊断的范围内。
- `rutis-host check` 的静态环检测算法：`check` 已经能报任意长度的声明环（`crates/rutis-loader/src/loader/check.rs` 的 `cycles()`）；本稿让运行时的 `Loader::waits` / `rutis-host status` / `run` / `dev` 与它对齐，并补上动态未知提供者链。
- 会话协议、插件 API 与 `rutis.json` 格式：都不新增字段，不升版本。

## 一、现在怎样 / 要解决什么

运行中的宿主里，等待原因来自 `Loader::waits` / `Loader::waits_of`（`crates/rutis-loader/src/loader/waits.rs`），由 `describe_waits`（`crates/rutis-loader/src/loader/status.rs`）格式化给 `rutis-host run`、`dev`、`status` 和嵌入宿主。

今天 `provider()` 只往后看 **1 跳**（`waits.rs:366`：`services.iter().find(|service| own.contains(service))`），带来两个盲区：

1. **三行及以上的动态环只报成普通等待**：当 `a -> bsvc -> b -> csvc -> c -> asvc -> a` 构成 3 行环（或在热更新、动态 `rutis/loader` 增行、实例内组合成环，跳过了启动前 `check`）时，`a` 看到的只是：
   ```text
   a: waiting for bsvc (row b waits too, for csvc)
   ```
   开发者看不出 `a`、`b`、`c` 已经首尾成环，也不知道该改哪几行的 `inject`；必须手动在 `rutis-host status` 里逐行拼接整条依赖链。
2. **链条卡在未声明提供者时没有线索**：当某行（或一串等待行 `c -> a -> b`）卡在一个没人声明的服务 `storage` 上，而同一作用域里另有尚未启动的行 `u`（例如没写 `provides`、只在 `apply` 里 `ctx.provide` 的插件，`announces: true`，或尚未解析完成的插件）时，今天分别只看到：
   ```text
   b: waiting for storage (nothing provides it yet)
   c: waiting for asvc (row a waits too, for bsvc)
   ```
   尤其当 `b`（`announces: true`）等 `a` 的 `asvc`、而 `a` 等 `b` 运行后才会提供的 `bsvc` 时，两行实际陷入了动态死锁，但 `a` 只说 `nothing provides it yet`，`b` 只说 `row a waits too, for bsvc`，谁也没点出"未启动的行 `b` 跑起来后才可能提供服务"。同时，如果不同作用域（不同 `isolate`）里恰好有同名声明，旧实现也没有比对 `isolate` 标签。

## 二、决策

| # | 决策 | 为什么；放弃的做法 |
| --- | --- | --- |
| D1 | 在 `Loader::waits_of` 内按**同作用域等待图**做广度优先遍历（BFS），作用域同时匹配实例副本 `scope` 与每个服务的有效 `isolate` 标签；只对同一作用域里**唯一存活提供者**（`Active`/`Loading`/`Pending` 仅此一行，或 `ProviderInactive` 绑定指向的行）连边，缺失服务按名字排序保证确定性。 | 与 `Loader::check` 的单提供者判定一致，避免有备用启动中提供者或跨 `isolate` 隔离时误报成环；`visited` 集合把遍历严格界定在 $O(\|V\| + \|E\|)$ 内，绝不死循环。放弃把环检测放进 `rutis` 内核：内核不掌握 `provides` 声明，且违背设计哲学 §七 第 2 条（能放内核外就不进内核）。 |
| D2 | 2 行环（$N = 2$）**原样保留** `WaitReason::Cycle { row, service }` 与既有逐字输出（`row b waits for asvc too: they inject each other`）；3 行及以上环（$N \ge 3$）新增 `#[non_exhaustive]` 兼容变体 `WaitReason::CycleChain { links: Vec<CycleLink> }`，复用已有的 `CycleLink`（补充 `CycleLink::new` 构造器），并沿用 `rutis-host check` 的环措辞（谁等谁、等哪个服务、怎么打破）。 | 保证 `loader/wait-words`、`host/row-state-lines` 与所有既有测试和脚本零破坏；`CycleLink` 已在 `rutis_loader::report` 公开，不造重复结构体。放弃把 2 行环也改成 `CycleChain`：会破坏既有保证和按变体匹配的调用方。 |
| D3 | 位于环外、只是依赖环内某行的**引入行**（lead-in，如 `d -> a -> b -> c -> a` 或 `c -> a <-> b`）保持 `WaitReason::ProviderWaiting { row: "a", services: ["bsvc"] }`，不把自己标进环里。 | 与 `rutis-host check` 一致（`check` 只把环成员放进 `cycles`，不含引入行）；避免 20 个下游行重复打印整条长环或误导开发者去删 `d` 的 `inject`；同时完整保留既有测试 `rows_that_inject_each_other_say_so` 的契约，让 2 行环与 3+ 行环的引入行行为完全一致。放弃在引入行上新增 `ProviderInCycle` 变体：会在下游扇出大时刷屏，且破坏 2 行引入行的既有契约。 |
| D4 | 新增 `WaitReason::UnknownProvider { chain: Vec<CycleLink>, missing: String, candidates: Vec<String> }`，在直接等待或沿等待链走到一个**无人声明、且无相近拼写**（`closest == None`）的缺失服务 `T`、且同作用域内存在尚未启动的未知提供者候选行（`Pending`/`Loading`/`Unresolved` 且 `announces == true` 或声明未知）时报告：展示有界等待链、点名可能运行后才提供服务的未启动行（把能到达链尾等待者的候选行排在最前，最多列 3 行加 `and N more`），并给出处理办法（声明 `provides` 或检查候选行为何未启动）。 | 明确区分"已证明的声明环"和"因未声明而无法证明的动态阻塞"：既帮开发者一眼看见隐蔽的动态死锁链，也绝不把未知提供者误判成环。有近名拼写（`did you mean`）时仍优先走拼错提示，不列出无关行（沿用 #937 D5）。放弃把已运行的 `Active` 行也列进候选：已执行完 `apply` 的正常行在多数语言里默认 `announces: true`，列它们会把所有普通缺服务都变成噪音。 |
| D5 | 输出全部设硬上限且格式化绝不失败：环链路和未知提供者等待链最多展开 8 跳（超出写成 `and N more steps`），候选行最多列 3 个（复用 `some_of` 规则写成 `and N more`），空向量等退化输入走兜底句、不 panic。 | 满足[诊断设计](design-diagnostics-2026-10-26.md) §4.6 的有界与不失败要求，防止大图把单行状态撑爆。放弃无上限全量输出：在几十行的配置里会把终端与 `status` 输出打穿。 |

## 三、规则

### 3.1 作用域与等待图

对 `Loader::waits_of` 的每次快照查询，按被查询行的实例作用域 `scope: Option<u64>` 构建行事实表 `facts`（按配置树顺序）：

1. **有效隔离标签**：每行沿 `parent` 向上收集 `raw_scope.isolate`，最内层覆盖外层（与 `check.rs` 的 `isolated_in` 一致）。两行 `u`、`v` 对服务 `S` 处于同一作用域，当且仅当两者同在 `scope`（含根作用域行）且 `isolate_of(u, S) == isolate_of(v, S)`。
2. **行事实字段**：除 `status`、`plugin`、`provides`、`unknown_off` 外，再记录：
   - `announces: bool`：插件声明的 `Declared::announces`；
   - `unknown_decl: bool`：行期望运行（`wanted == true`）但未能拿到有效声明（未解析、`declares == None` 或求值出错）。
3. **唯一存活提供者边**：对一个 `Pending` 且**未在等自己运行时**的行 `u`，以及它缺失的一项非 `OwnRuntime` 服务 `(key, status)`（名为 `S`，按服务名升序遍历）：
   - `status == DependencyStatus::ProviderInactive(_)`：经 `diagnostics.bindings` 和 `row_of` 找到提供者行 `v`；若 `v != u` 且 `v` 在 `facts` 中，即构成边 `u --S--> v`。
   - `status == DependencyStatus::Missing` 且 `unexported(&root, S) == None`：在 `facts` 中筛选满足 `v != u`、`v.provides.contains(S)` 且与 `u` 对 `S` 隔离标签相同的行。仅当其中处于存活状态（`Active`、`Loading` 或 `Pending`）的行**恰好有 1 个**时，才构成指向该行 `v` 的唯一存活边 `u --S--> v`。若有多于 1 个存活声明者，不连环/链边（避免有另一行正在启动时误报成环）。

### 3.2 任意长度环检测（`N >= 2`）与引入行

当行 `start` 等服务 `first_service`，且其同作用域提供者行 `first_provider`（`!= start`）处于 `Pending` 且未等自己运行时，先执行环检测 BFS：

1. 队列从 `first_provider` 出发，记录父边 `first_provider <- (start, first_service)`；`visited` 初始只含 `{first_provider}`（不含 `start`，以便回到 `start` 时闭合）。
2. 每次弹出 `Pending` 行 `curr`（若 `curr` 在等自己运行时则跳过其出边），按服务名升序遍历它的每条唯一存活边 `curr --svc--> next`：
   - 若 `next == start`：找到经过 `(start, first_service, first_provider)` 回到 `start` 的最短简单环，按父边还原 `links: Vec<CycleLink>`：
     - `links.len() == 2`：返回 `WaitReason::Cycle { row: first_provider, service: links[1].service }`；
     - `links.len() >= 3`：返回 `WaitReason::CycleChain { links }`。
   - 否则若 `next` 为 `Pending` 且不在 `visited` 中：加入 `visited`，记录父边 `next <- (curr, svc)`，入队。
3. 若队列耗尽仍未回到 `start`（包括进入了一个不包含 `start` 的下游环，例如 `d -> a -> b -> c -> a`），则 `start` 不在环上，环检测返回 `None`，进入 §3.3；若 §3.3 也无命中，则落回 `WaitReason::ProviderWaiting { row: first_provider, services }`。

### 3.3 未知 / 未声明提供者链诊断

对缺失服务 `T`（位于直接等待者 `W` 上，`DependencyStatus::Missing`、`unexported == None`、同作用域内无行声明 `T` 且排除 `W` 自身声明后的近名匹配 `closest_for(W, T) == None`，避免把行自己提供的服务当作 `did you mean` 建议给它自己），定义**未启动候选行集合** `candidates(W, T)`：

1. **纳入条件**：`facts` 中所有满足以下全部条件的行 `R`：
   - `R != W`；
   - `isolate_of(W, T) == isolate_of(R, T)`；
   - `R.announces || R.unknown_decl`；
   - `R.status` 为 `Running(Pending)`、`Running(Loading)` 或 `Unresolved(_)`。
   已运行的 `Active`、已失败的 `Failed`、被禁用的 `Disabled`、未激活的 `Inactive` 和已停止的 `Stopped` 均不纳入。
2. **排序**：分两档去重输出：
   - 第一档：在 `Pending` 等待图里能沿唯一存活边到达 `W` 的候选行（即可能与 `W` 构成动态死锁环的嫌疑行，例如 `b -> asvc -> a -> bsvc` 中的 `b`），按到 `W` 的最短跳数升序，同距离按树顺序；
   - 第二档：其余候选行，按树顺序。
3. **两条触发路径**：
   - **直接等待**：`start` 等待 `S` 时同作用域无声明提供者、`closest(S, &known) == None` 且 `candidates(start, S)` 非空 → 返回 `WaitReason::UnknownProvider { chain: vec![], missing: S, candidates }`。若 `candidates` 为空，则沿用既有规则：存在未解析禁用行（`unknown_off`）返回 `NoEnabledProvider`，否则返回 `NoProvider`。若 `closest(S, &known)` 为 `Some(_)`，一律走既有拼写提示（`NoEnabledProvider` / `NoProvider`），不走 `UnknownProvider`。
   - **链式等待**：在 §3.2 环检测未返回环之后，从 `first_provider` 做 BFS（不重访 `start` 和已访问节点）。若弹出 `Pending` 行 `curr`（未等自己运行时）有某缺失服务 `T` 满足同作用域无声明提供者、`unexported == None`、`closest(T, &known) == None` 且 `candidates(curr, T)` 非空，则按父边还原 `chain: Vec<CycleLink>`（`start -> first_provider -> ... -> curr`），返回 `WaitReason::UnknownProvider { chain, missing: T, candidates }`。否则继续沿唯一存活 `Pending` 边扩展。
   - **避免误报**：
     - 普通启动等待（提供者处于 `Loading`）：有声明提供者，走 `ProviderStarting` / `ProviderWaiting`，不会触发 `CycleChain` 或 `UnknownProvider`；
     - 普通缺服务（没有其他未启动的 `announces`/`unknown_decl` 行，或 `announces: true` 的行早已 `Active`）：`candidates` 为空，走 `NoProvider` / `ProviderWaiting`；
     - 拼写错误（`closest != None`）：走 `did you mean`，不走 `UnknownProvider`。

### 3.4 格式化与边界上限

`crates/rutis-loader/src/loader/status.rs` 的 `why()` 按下列精确模板格式化，只使用开发者写过的行名、服务名和 `inject` / `provides`，不出现任何内部词（[诊断设计](design-diagnostics-2026-10-26.md) §4.5）：

- `WaitReason::Cycle { row, service }`（不变）：
  `row {row} waits for {service} too: they inject each other`
- `WaitReason::CycleChain { links }`（常量 `MAX_LINKS = 8`）：
  - `links.is_empty()`：`they inject each other; break the cycle in their inject`
  - `1..=8` 跳：
    `rows {r0}, {r1}, ... wait for each other: {r0} waits for {s1} from {r1}, ..., {rn-1} waits for {s0} from {r0}; break the cycle in their inject`
  - `> 8` 跳（设 `rest = links.len() - 8`）：
    `rows {r0}, ..., {r7} and {rest} more wait for each other: {r0} waits for {s1} from {r1}, ..., {r7} waits for {s8} from {r8}, and {rest} more steps back to {r0}; break the cycle in their inject`
- `WaitReason::UnknownProvider { chain, missing, candidates }`（`MAX_LINKS = 8`，候选行上限 `MAX_CANDIDATES = 3`）：
  - 候选行短语 `tail`：
    - `candidates.is_empty()`：空串
    - `[one]`：`; unstarted row {one} may provide services once it runs; declare provides or check why {one} has not started`
    - 2～3 个：`[a, b]` / `[a, b, c]` → `; unstarted rows {list} may provide services once they run; declare provides or check why they have not started`
    - `> 3` 个（设 `rest = candidates.len() - 3`）：`; unstarted rows {c0}, {c1}, {c2} and {rest} more may provide services once they run; declare provides or check why they have not started`
  - `chain.is_empty()`（直接等待）：
    `nothing declares it yet{tail}`
  - `1..=8` 跳：
    `{r0} waits for {s1} from {r1}, ..., {last.provider} waits for {missing}; nothing declares {missing} yet{tail}`
  - `> 8` 跳（展示前 7 跳 + `and {rest} more steps` + 尾部 `{last.provider} waits for {missing}`，其中 `rest = chain.len() - 7`）：
    `{r0} waits for {s1} from {r1}, ..., {r6} waits for {s7} from {r7}, and {rest} more steps, {last.provider} waits for {missing}; nothing declares {missing} yet{tail}`

跨语言一致性：上述逻辑全部在 Rust 侧 `rutis-loader` 执行，Node、Bun、Python、Go、Swift、Kotlin 插件看到的 `rutis-host status` / `run` / `dev` 与 `rutis/loader` 的 `status()` JSON 输出完全一致，没有语言差异。

### 3.5 明确无法静态或仅凭快照证明的动态环边界

以下情形在理论或架构边界上**无法**从单次静态检查或 `Loader::waits` 快照中证明为环，rutis 明确不猜测成环：

1. **已运行行里的条件式或延迟 `ctx.provide`**：某行 `apply` 已结束（`Active`），只在收到请求、定时器或事件后才调用 `ctx.provide("x", ...)`。快照无法判定它将来会不会提供 `x`，只能报 `nothing provides it yet`。
2. **未声明 `provides` 的动态提供者（`announces: true`）**：在未启动行真正跑完 `apply` 之前，rutis 不知道它到底会提供哪些服务名。因此 `a --bsvc--> b(未声明) --asvc--> a` 只能报告为 `UnknownProvider`（提示 `b` 尚未启动、可能运行后才提供服务），绝不伪证成 `Cycle` / `CycleChain`。补上 `provides` 声明后，它即成为可证明的环。
3. **`apply` 内动态挂载的子插件（`ctx.plugin(...)`）**：父行启动前看不到子插件的 `inject` / `provide`；子插件挂载后其 fiber 进入 `diagnostics()`，若已产生非活跃绑定则经 `ProviderInactive` 纳入图遍历。
4. **外置运行时内部私有依赖**：同一 Node 运行时内未经服务目录共享的 Cordis 内部依赖不经过 rutis 纤维门控，`Loader::waits` 不可见。
5. **跨节点远端未声明提供者**：断开连接或尚未上报声明的对端服务无法从本机快照拼接出对端的 `Pending` 图（本机侧仅表现为等连接行或 `nothing provides it yet`）。
6. **调用期同步重入环**：两行都已启动、在同步方法调用中互调形成死锁，属于运行时 `SyncWaitCycle`，不属启动依赖等待图。

## 四、开发者看到什么

### 4.1 用 `rutis-host` 组装插件的人

正常启动或单向等待时，配置和输出完全不变：

```json
{
  "rows": [
    { "id": "c", "name": "./c.mjs" },
    { "id": "b", "name": "./b.mjs" },
    { "id": "a", "name": "./a.mjs" }
  ]
}
```

当 `c` 正在启动（`Loading`）、`b` 等 `csvc`、`a` 等 `bsvc` 时，`rutis-host status` 与以前一致，不会误报成环：

```text
rows:
  a: waiting for bsvc (row b waits too, for csvc)
  b: waiting for csvc (row c is starting)
  c: starting
```

**出错 / 卡住时的每种情况**（`rutis-host status`、`run`、`dev` 逐字一致；等待状态没有错误码）：

| # | 怎样引起 | 输出（逐字） | 码 | 记在哪 | 怎样改 |
| --- | --- | --- | --- | --- | --- |
| 1 | 2 行互相 `inject`（保持不变） | `a: waiting for bsvc (row b waits for asvc too: they inject each other)` | 无（状态） | 环上两行（`a`、`b`） | 打破 `a` 或 `b` 的 `inject` |
| 2 | 3 行成环（`a -> bsvc -> b -> csvc -> c -> asvc -> a`） | `a: waiting for bsvc (rows a, b, c wait for each other: a waits for bsvc from b, b waits for csvc from c, c waits for asvc from a; break the cycle in their inject)` | 无（状态） | 环上每一行（`a`、`b`、`c`，各自从自身起头） | 打破 `a`、`b`、`c` 其中一行的 `inject` |
| 3 | 超长环（例如 10 行 `r0`..`r9` 成环，超过 8 跳上限） | `r0: waiting for s1 (rows r0, r1, r2, r3, r4, r5, r6, r7 and 2 more wait for each other: r0 waits for s1 from r1, r1 waits for s2 from r2, r2 waits for s3 from r3, r3 waits for s4 from r4, r4 waits for s5 from r5, r5 waits for s6 from r6, r6 waits for s7 from r7, r7 waits for s8 from r8, and 2 more steps back to r0; break the cycle in their inject)` | 无（状态） | 环上每一行 | 打破环中任意一行的 `inject` |
| 4 | 环外引入行等环内提供者（`d -> asvc -> a`，而 `a -> b -> c -> a` 成环） | `d: waiting for asvc (row a waits too, for bsvc)` | 无（状态） | 引入行 `d`（环说明记在 `a`、`b`、`c` 各自状态行上） | 先看 `a` 的状态行，打破 `a`、`b`、`c` 的环；不要改 `d` |
| 5 | 直接等未声明服务 `bsvc`，另有未启动行 `b`（`announces: true`）等 `asvc` | `a: waiting for bsvc (nothing declares it yet; unstarted row b may provide services once it runs; declare provides or check why b has not started)` | 无（状态） | `a` | 在 `b` 上声明 `provides`；检查 `b` 为何没启动（例如是否反过来在等 `a`） |
| 6 | 多行等待链撞上未声明服务（`c -> asvc -> a -> bsvc -> b -> storage`，未启动行 `u` 有 `announces: true`） | `c: waiting for asvc (c waits for asvc from a, a waits for bsvc from b, b waits for storage; nothing declares storage yet; unstarted row u may provide services once it runs; declare provides or check why u has not started)` | 无（状态） | `c`（`a`、`b` 各自显示从自己起头的链） | 在 `u` 上声明 `provides`，或检查 `u` 为何未启动 / 补上提供 `storage` 的行 |
| 7 | 多个未启动未知提供者（例如 5 行 `b`..`f`） | `a: waiting for bsvc (nothing declares it yet; unstarted rows b, c, d and 2 more may provide services once they run; declare provides or check why they have not started)` | 无（状态） | `a` | 补齐相关行的 `provides` 声明，或检查这些未启动行的等待原因 |
| 8 | 拼错服务名（即便另有未启动的 `announces: true` 行） | `weather: waiting for lmm (nothing provides it yet; did you mean llm?)` | 无（状态） | `weather` | 把 `lmm` 改成 `llm` |
| 9 | 普通缺服务（所有 `announces: true` 行都已 `running`，或所有行都只用声明服务） | `store: waiting for blob (nothing provides it yet)` | 无（状态） | `store` | 增加提供 `blob` 的行 |
| 10 | 跨 `isolate` 同名服务（`a` 私有隔离 `bsvc`，`b` 在外提供 `bsvc` 且等 `asvc`） | `a: waiting for bsvc (nothing provides it yet)`；`b: waiting for asvc (row a waits too, for bsvc)` | 无（状态） | `a`、`b` | 共享同一 `isolate` 标签或在 `a` 的隔离域内提供 `bsvc` |

### 4.2 插件作者

正常写插件不变。推荐为会提供服务的插件声明 `provides`（以 TypeScript 为例；Python、Go、Swift、Bun、Kotlin 一样）：

```ts
import { definePlugin } from '@arcships/rutis'

export default definePlugin({
  inject: ['asvc'],
  provides: { bsvc: { get: 'sync' } },
  apply(ctx) {
    ctx.provide('bsvc', { get: () => 'ok' })
  },
})
```

**出错 / 卡住时**：

- 如果插件漏写 `provides`、只在 `apply` 里调用 `ctx.provide('bsvc', ...)`，且恰好与 `a`（提供 `asvc`、注入 `bsvc`）互相等待，插件作者在 `rutis-host dev` 里会看到两行明确线索：
  ```text
  a: waiting for bsvc (nothing declares it yet; unstarted row b may provide services once it runs; declare provides or check why b has not started)
  b: waiting for asvc (b waits for asvc from a, a waits for bsvc; nothing declares bsvc yet; unstarted row b may provide services once it runs; declare provides or check why b has not started)
  ```
- 一旦在 `b` 中补上 `provides: { bsvc: ... }`，热重载后立刻变成明确的环诊断：
  ```text
  a: waiting for bsvc (row b waits for asvc too: they inject each other)
  b: waiting for asvc (row a waits for bsvc too: they inject each other)
  ```
- 若涉及 3 个插件（`a`、`b`、`c`），`dev` 直接逐行打印 3 行环（同 §4.1 第 2 行），所有语言一致，无语言差异。
- `rutis/loader` 的 `status()` JSON 接口形状不变，每行的 `reason` 字符串即上述逐字文本。

### 4.3 宿主持有者（嵌入 rutis 的应用）

正常读取等待原因与快照的代码不变：

```rust
let status = loader.status();
eprintln!("{status}");

for entry in loader.entries() {
    for wait in loader.waits(&entry) {
        match &wait.reason {
            WaitReason::Cycle { row, service } => { /* 2 行环（不变） */ }
            WaitReason::CycleChain { links } => { /* 3+ 行环，links 为 Vec<CycleLink> */ }
            WaitReason::UnknownProvider { chain, missing, candidates } => { /* 未知/未声明提供者链 */ }
            _ => {}
        }
    }
}
```

- `WaitReason` 早已标为 `#[non_exhaustive]`，新增 `CycleChain` 与 `UnknownProvider` 不破坏既有 `match` 的源码兼容。
- `CycleLink` 补充 `CycleLink::new(row, service, provider)`，方便宿主和单测构造断言。
- `rutis_loader::status::describe_waits(&waits, &runtime)` 直接产出 §4.1 表格中的逐字行；没有新增内核错误码。

### 4.4 要懂的概念

**新增概念：0 个。**

- 插件作者（#663 预算 7）：行、服务、`inject`、`provides`、`dev` 都是已有概念。
- `rutis-host` 用户（#663 预算 6）：行、服务、`inject`、`provides`、`status`、`check` 都是已有概念；3+ 行环复用与 `check` 完全一致的措辞。
- 宿主持有者（#663 预算 10）：在既有的 `WaitReason` 枚举里增加两个变体并复用既有的 `CycleLink`，不引入新抽象层或新概念。

## 五、兼容与迁移

- **Rust API 兼容**：`WaitReason` 已有 `#[non_exhaustive]`，新增 `CycleChain { links }` 与 `UnknownProvider { chain, missing, candidates }` 为纯增量兼容变更；`CycleLink::new` 为纯增量方法；`WaitReason::Cycle { row, service }`、字段与语义 100% 不变。
- **既有输出兼容**：
  - 2 行环文字不变（`row b waits for asvc too: they inject each other`）；
  - 环外引入行文字不变（`row a waits too, for bsvc`）；
  - 拼错名字、普通缺服务（无其他未启动 `announces`/未知声明行时）、禁用未解析行、启动中提供者文字全部不变；
  - 发生变化的仅是原本信息不足的三类场景：3+ 行动态环（从 `row b waits too, for csvc` 升级为完整 `CycleChain` 句）、撞上未启动 `announces`/未知声明行的等待链（升级为 `UnknownProvider` 句），以及跨 `isolate` 隔离时不再误匹配不同隔离域的提供者。
- **协议 / 配置 / 插件兼容**：会话协议版本（4）、`rutis.json` 格式（1）、插件 API（3）全部不变；旧运行时配新宿主、新运行时配旧宿主均无需改动。
- **发布说明与文档**：落地正式 PR 时同步更新[rutis-host 指南](guide/rutis-host.md)（及[英文版](guide/rutis-host.en.md)）的「run 打印什么」等待原因表与对应版本迁移说明。

## 六、实施

| # | 内容（含位置） | 完成条件 |
| --- | --- | --- |
| D-1 等待图与新变体 | `crates/rutis-loader/src/loader/check.rs` 增加 `CycleLink::new`；`crates/rutis-loader/src/loader/waits.rs` 新增 `WaitReason::CycleChain`、`WaitReason::UnknownProvider`，扩展 `Fact`（`announces`、`unknown_decl`、`isolates`），实现同作用域 BFS 环检测（2 行保旧、3+ 行产 `CycleChain`、引入行保 `ProviderWaiting`）与未知提供者链诊断 | `cargo test -p rutis-loader --all-features` 全绿；覆盖 2 行环、3 行环、引入行、直接与链式未知提供者、正常启动等待、普通缺服务、跨 `isolate` 零误报 |
| D-2 格式化与逐字验收测试 | `crates/rutis-loader/src/loader/status.rs` 实现 `CycleChain` 与 `UnknownProvider` 的有界格式化（8 跳 / 3 候选上限、退化输入不 panic）；在 `crates/rutis-loader/src/loader/status.rs` 与 `crates/rutis-loader/tests/waits.rs` 补齐逐字断言，与本稿 §四 完全一致 | `cargo test -p rutis-loader -p rutis-host` 全绿，逐字输出与 §四 匹配，且不含[诊断设计](design-diagnostics-2026-10-26.md) §4.5 禁用词 |
| D-3 指南与保证登记（后续 PR） | 更新 `docs/guide/rutis-host.md` / `.en.md` 等待原因表；按需要把长环与未知提供者诊断登记进保证表与追踪表 | 链接检查、保证检查、追踪检查全绿 |
