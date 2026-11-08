# rutis 0.22 设计与交付路线图

[English](roadmap-0.22-2026-11-08.en.md)

Status：设计基线，尚未实现。日期：2026-11-08。
跟踪：[#694](https://github.com/arcships/rutis/issues/694)（版本规划）、[#1025](https://github.com/arcships/rutis/issues/1025)（事件关闭接纳 CPU）和 [#183](https://github.com/arcships/rutis/issues/183)（质量控制）。形式化验证设计见[形式化验证重新设计](design-formal-verification-2026-11-08.md)；#1040 已明确延期。

## 1. 目的与现状

0.22 从 `v0.21.0` 标签开始，目前没有 release branch 或实现代码。本文件在开始编码前冻结设计范围。计划中的变更保持窄范围：

1. 从事件 listener liveness 检查中移除祖先遍历，同时保持现有关闭/接纳边界（[事件关闭设计](design-event-close-admission-2026-11-08.md)）。

动态服务公告继续保持现有的 Cordis 兼容行为。宿主侧拒绝策略暂缓：在没有明确需求时，不应由这一层负责不可信插件的防御。

当前 workspace package 声明仍是 `Cargo.toml` 中的 `version = "0.6.0"`；release 工作必须先确认这是有意的 workspace 约定还是遗留声明。在决定 release 流程前，不要把它作为任一 feature 的一部分修改。

## 2. 版本承诺

0.22 承诺：

- 在不改变公开事件、shutdown、取消或协议行为的情况下，降低 `close_cpu` 工作负载中的事件 listener 接纳 CPU；
- 中英文文档、迁移指南和 release notes 都写明精确可见输出。

0.22 不新增：每进程一个服务 namespace、全局 allowlist、未声明公告的 `rutis.json` 配置、动态名称 `isolate`，或跨语言终态的“策略拒绝” row 状态。现有 Cordis 兼容动态公告行为保持不变。

## 3. 工作流与 gate

### W0 — 形式化基线和关键语义（#183、#613、#622、#624、#630、#631、#634、#635、#639）

形式化验证不推迟到 0.23：0.22 先为 #1025 的关闭接纳边界建立最小可执行模型和确定性竞态测试。实现前确认：marker commit 后不能再接纳新的 callback；已接纳 callback 即使在 commit 后开始仍按既有规则 drain；parent、child、sibling 并发关闭不会漏清理或执行两次；`Entry::live()` 不再遍历祖先后仍与 owner marker 一致。跨运行时/远程租约的既有模型先按当前规格对账，不能把旧架构模型当成新实现的证明。

完整四层计划（协议基础、并发与生命周期、跨进程部署、输入与长期可靠性）及 TLA+、shuttle、trace validation、一致性、属性/模糊、虚拟时钟和 E2E 的职责边界，见[形式化验证重新设计](design-formal-verification-2026-11-08.md)。W0 的完成不表示所有形式化覆盖已完成；它表示 0.22 的高风险关闭语义已进入实现前验证，并为 0.23 的完整扩展保留追踪闭环。

Gate：模型和 race test 都能在故意注入 marker 发布顺序、admission 锁或最终 `live()` 检查错误时失败；正常模型检查、现有 shuttle 检查和 #1025 定向测试通过，并把结果与保证编号/追踪表关联。

### W1 — 事件关闭接纳（#1025）

设计和实现遵循关联设计文档：

- 增加永久 `FiberInner::lineage_closed` marker；
- 在 `Shared.admission` 下先标记完整固定子树，再发布既有 `closing` flags，之后执行 cleanup/shutdown；
- 让 `Entry::live()` 读取 owner marker，不再遍历祖先；`Entry::start()` 保留 `CallFlight::begin(owner)` 和最终 `live()` 检查；
- 保持 registration 检查、instance-event permit、callback 顺序、取消、shutdown 输出和跨进程行为不变。

Gate：确定性的 ancestor-close 竞态及现有 sync/async dispatch mode 证明 marker commit 后不再接纳新的 callback；已接纳 callback 即使在 commit 后开始仍 drain。issue 的 `creation <= 30s` 是待记录的性能目标，不是当前 `close_cpu` 的绝对耗时断言；自动 gate 继续比较 `CREATE_RATIO`、`CLOSE_RATIO` 和 `IDLE_SHARE`，且不回归 queued-emits memory。

CI 验收 job 是 `.github/workflows/stress.yml:381-417` 的 Ubuntu 24.04 release job；它运行 `close_cpu` 和 `queued_emits_memory`，两项结果都必须保留在 summary 中。

### W3 — Release、迁移与文档（#694）

在宣布实现完成前：

1. 决定并记录 package/version 更新流程，包括 `Cargo.toml` workspace 声明和 `v0.21.0` 标签；
2. 增加 `migration-0.22.md` 与 `migration-0.22.en.md`；说明动态公告行为不变，事件优化不需要 API 迁移；
3. 增加 `docs/releases/0.22.0.md` 和 `.en.md`，只在 CI 运行后写入实测 benchmark 数值；
4. 同步更新事件、host、plugin 指南以及 `release.md` 和 `release.en.md` 的中英文版本；
5. 保持本路线图与事件关闭设计稿同步；#1040 已延期，不得为 0.22 增加 JSON 字段或 wire-visible 行为；
6. 如果将来重新讨论信任边界，应先基于明确需求写新的设计文档，确定责任层次后再实现。

Gate：中英文审查确认决定、精确输出/error/code/data、按角色的 “What developers see”、迁移指引和验收标准一致。

## 4. 工作顺序

1. **基线：**核对 tag、package-version 约定、当前 status 序列化和 benchmark 环境；改代码前记录测量值；
2. **验证基线：**按形式化验证设计对账现有模型和 shuttle；先写 #1025 的关闭/接纳模型、反例和确定性 race test；
3. **合约：**提交事件关闭设计和本路线图；在 #1025 下拆出 core、race-test、benchmark 和文档任务；#1040 不进入 0.22 实施计划；
4. **核心变更：**独立实现 marker 发布，并保持可单独回滚；
5. **验收：**运行事件 race tests、`cargo test -p rutis-loader --release --test close_cpu -- --nocapture` 以及 CI stress pair；确认模型、实现测试和 benchmark 的结果分别记录；
6. **发布准备：**依据实测结果更新 migration/release 文档，解决 package-version 策略，再进行正常 release 审查。

本设计阶段不包含 release commit、tag、package-version 变更或公开 API 实现。

## 5. 0.22 中开发者看到什么

### 插件作者

现有 Cordis 兼容动态公告行为保持不变。事件关闭优化没有新的插件 API、输出或错误。

### 宿主作者与部署者

0.22 没有新的信任边界设置。事件关闭优化没有新的输出、status 字段、协议版本或迁移设置；现有状态行和既有失败文字保持不变。

### Rust 宿主作者

public Rust API、事件协议和 shutdown result 不变。关闭后的调用仍返回 `ErrorKind::Closed`，文字为 `root has been shut down`，没有新增 `code` 或 `data`。

### 运行时作者与贡献者

本路线图不新增运行时或验证工具的命令、退出码、成功输出或失败输出；既有模型、shuttle 和 CI 输出保持原样。验证诊断不是插件用户看到的运行时输出。

## 6. 明确不在 0.22 内的后续

- 未声明动态公告的宿主拒绝策略或信任边界防御；#1040 已延期，因为在没有明确需求时，这一层不是合适的责任方；
- 服务 namespace、全局 allowlist、`rutis.json` row-level policy 或动态名称 isolation 模型；
- 所有 SDK 和 loader restart handling 的 typed wire rejection 及终态 policy-refused row；
- 初版 `SeqCst` marker 实现之外的新 memory-order 方案；
- 在未先核对仓库 release 约定前进行任何 package-version 修改。

这些都是独立设计，不由 0.22 承诺推导出来。
