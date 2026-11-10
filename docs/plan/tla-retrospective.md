# 从引入 TLA 那天开始：0.22 的落地记录

## 1. 引入那天

2026-11-09，任务开始时要求先看远端 `main`、读形式化验证设计稿、准备实现。随后范围定为 0.22：把 0.23 合并进来，TLA 模型作为其中一块落地。当天的工作从读文档、对 issue、看现有 `specs/` 开始。

## 2. 为什么引入

背景有四点：

- rutis 0.22 的核心改动 #1025 是事件关闭接纳，涉及子树关闭、marker 发布、监听器接纳、cleanup 的并发交错。
- 质量规范 Q9.1.2 / Q11.2 要求多方并发协议在设计阶段做模型检查，设计变更时同步模型。
- 形式化验证设计稿把协议分成事件、跨运行时调用、远程租约、行生命周期、调用生命周期、节点托管行。
- issue #613 列出了具体模块、性质和一批历史 bug。当时仓库只有 `cross-runtime-sync` 和 `remote-lease` 两个旧模型。

历史 review 和偶发失败里有很多并发问题。测试只能覆盖构造出来的时序，无法穷举交错。引入 TLA 的目标是：

- 在设计层穷举参与者、消息、断线、重试、崩溃的交错；
- 把设计承诺写成性质，找反例；
- 把历史 bug 变成修复前配置，防止回归；
- 给实现和评审一份可执行的规格。

## 3. 落地过程

分三批：

- 第一批：`event-close`、`row-lifecycle`、`call-lifecycle` 初版，先跑通。
- 第二批：shuttle、trace、属性测试并行；TLA 扩到 `events` 四模块、`peer-rows`，并补 `row` / `call` / `lease` / `sync`。
- 第三批：六个目录扩到设计稿范围，约 215 个配置。新增配置全部实跑，CI 的 `specs` 七项全部通过。

## 4. 遇到的问题

**状态空间。** `table-healthy` 跑到 10 分钟、9 GB；`policy-b-posts` 有 1400 万状态，把 4 核 CI 的 30 分钟任务跑超时。最后缩 `MaxDepth` 3 到 2、加 `SYMMETRY`、2 运行时 × 1 行，状态降到 115 万，CI 变成 7 分 25 秒。

**模型自身错误。** `peer-rows` 的 `PFailed` 把一个变量同时写进赋值和 `UNCHANGED`，动作恒为假，失败路径从未被走过，配置仍然显示通过。`row-lifecycle` 的一次重写也把 `cleanCount` 放进了不该放的 `UNCHANGED`。后来补了可达性检查和变异配置。

**配置描述旧代码。** `bail-policy-b` 一直显示死锁。实际打印 `forward` 输入后发现，当前合规运行时已经把分发调用号放进监听器调用的 `path`，等价 `bail-recorded`。`bail-policy-b` 描述的是修复前行为，我们差点照着它改 `waits.rs`。

**按模型改代码改坏了实现。** `async-policy-b` 说已标记链上的异步调用记一跳会误拒。按模型改成“只把 `sync` 当跳”之后，对象中继的 `a_synchronous_cycle_through_objects_is_refused` 挂住。帧上只有 `sync`，分不清“发出即不管”和“之后才 `await`”。代码已回退，开了 #1098。

**真 bug。** `events::link::accept` 旧代次回插、Kotlin `RutisApp` 起停竞态、Node/Bun 接管 5 秒强杀早于清理截止。三个都已修。

**疑似 bug。** peer row 的 `unload` 先于 `load` 可能留下孤儿、Kotlin 心跳从 accept 起算、宿主单 worker 的等待图盲区。已开 #1101–#1103。

**设计缺口。** async 的“是否等待结果”信号、取消表清理与取消后回复、peer row 的键身份。已开 #1098–#1100。

## 5. TLA 的价值

这次实践的结果：

- 找到并修了 3 个真 bug；
- 确认 `bail` 当前实现正确，避免一次误改；
- 把 3 个设计缺口和 3 个疑似实现问题变成可跟踪 issue；
- 建立 9 个模型、约 215 个配置、`expected.txt` / `run.sh` / README / CI / 追踪表的可复用资产；
- 设计评审有了可执行规格，历史 bug 有了修复前配置。

TLA 的定位是设计工具。它证明的是“在这个有界模型里没有找到反例”。代码正确性需要 trace validation、shuttle、一致性测试和普通测试。

TLA 适合并发、多方、会交错、会失败重试的协议。确定性逻辑、计时、调度、内存序、编码、性能不适合用 TLA。

## 6. 环境与配置成本

**软件。** Java 21 加 `tla2tools.jar` v1.7.4，固定 sha256。没有其他依赖。

**本地运行。** `TLA2TOOLS=/tmp/tla2tools.jar specs/<dir>/run.sh`。32 核跑全量几分钟。

**CI。** `specs` 矩阵 7 个 job，每个 job 装 Java 21、下载 jar、跑对应目录的 `run.sh`。路径过滤只在 `specs/**` 变更或全量 / nightly 时触发，普通 PR 不跑。

**CI 时间。** `cross-runtime-sync` 7 分 25 秒，`remote-lease` 3 分 7 秒，`event-close` 1 分 31 秒，`events` 1 分 20 秒，`row-lifecycle` 1 分 13 秒，`call-lifecycle` 1 分 9 秒，`peer-rows` 45 秒。单 job 超时 30 分钟。

**人力。** 6 个 agent 并行写模型，主线程负责审阅、整合、跑全量和改 CI。主要成本在模型写作、状态空间调试和规格评审。LLM 能加速初稿，规格和边界仍需人审。

**维护。** 设计变更时同步模型；每个目录 README 记录性质、简化、结果和未覆盖；新增配置进 `expected.txt` 和 `run.sh`。

## 7. 后续

- 确认或关闭 #1098–#1103；
- 补 trace validation、Kani/Miri、#639 的用例；
- refinement 暂不做。
