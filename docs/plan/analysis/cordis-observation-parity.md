# Cordis 检查与拦截对齐：交付分析

需求：按 [design-cordis-observation.md](../design-cordis-observation.md) 验收三个已叠放的实现 PR，产出独立验收报告，供用户决定是否合入 main。

## 现状与输入版本

| 层 | PR 分支 | 提交 | 被审范围 |
|---|---|---|---|
| #53 投递前观察 | `feat/cordis-dispatch-observation` | `5e107bf` | `7d7402d..5e107bf` |
| #54 effect 清理树 | `feat/cordis-effect-tree` | `c9d4caf` | `5e107bf..c9d4caf` |
| #55 服务读写拦截 | `feat/cordis-service-intercepts` | `4c1e160`、`0decf15` | `c9d4caf..0decf15` |

- 三个分支均基于 `7d7402d`（设计文档基准），main 当前为 `d0b7498`，差异仅为 MR #39 的设计文档与 README 改动。
- 设计文档位于主仓库 main，不在实现分支上；验收者从主仓库绝对路径读取。
- 本轮只验收不合入；验收记录回写各任务文件。

## 任务划分

| 任务 | 内容 | 执行位置 | 状态 |
|---|---|---|---|
| [cordis-dispatch-01](../tasks/cordis-dispatch-01.md) | 独立验收 PR #53 | `/tmp/rutis-rev53` | done（pass） |
| [cordis-effect-02](../tasks/cordis-effect-02.md) | 独立验收 PR #54 | `/tmp/rutis-rev54` | done（pass） |
| [cordis-intercept-03](../tasks/cordis-intercept-03.md) | 独立验收 PR #55 | `/tmp/rutis-rev55` | done（pass） |
| [cordis-tests-04](../tasks/cordis-tests-04.md) | 补三个覆盖缺口测试（#53 重入、#55 不同键重入与摘除中写入） | `/tmp/rutis-dev55` | done（pass，`795c165`） |
| [cordis-fix-05](../tasks/cordis-fix-05.md) | 修复 PR #55 P1：失败写入锁内析构候选值死锁（含回归测试） | `/tmp/rutis-dev55` | done（pass，`68491a7`+`c0b6b41`） |

三个验收相互独立、并行执行；均为只读验收，不修改被审实现。用户裁决后主 agent 核实验收发现：越界 hits==0 一项不成立（hooks_match 测试事实上已断言），其余真实；仅 3 个测试缺口值得处理，派发 cordis-tests-04。

## 调度记录

- 2026-09-24：用户裁决先验收出报告、暂不合入 main。派发三个独立验收 agent（deepseek-v4-pro）。
- 2026-09-24：三个验收全部返回 pass，0 阻塞问题；测试分别为 209/213/223 passed、clippy 与 fmt 干净。非阻塞观察项共 12 条（主要是覆盖缺口），明细见各任务文件与验收记录。合入决策待用户裁决。
- 2026-09-24：主 agent 逐项核实验收发现：11 条真实、1 条（越界未断言钩子未执行）不成立——hooks_match 测试中越界读后 `hits == 1` 断言已覆盖该语义。用户裁决补 #53 重入测试与 #55 两条缺口；派发开发 agent 吴俊杰（cordis-tests-04，基于 0decf15 只加测试）。
- 2026-09-24：cordis-tests-04 完成：`795c165`（+141 行，仅测试），独立验收 pass（周文斌），226/226、clippy/fmt 干净、service_intercepts 20 次连续全过。全链路就绪，合入决策待用户裁决。
- 2026-09-24：远程 PR 评论到达：#53/#54 审核通过无问题；#55 报 P1——失败写入在框架锁内析构候选值，Drop 重入自死锁（评论者附探针复现）。主 agent 核实机制属实，且发现比评论更广的完整范围（三条失败路径：registration_open 失败、generation/state 检查失败、replace_mutable_if_current 拒绝），派发 cordis-fix-05（吴俊杰，TDD 先复现红再修绿）。
- 2026-09-25：cordis-fix-05 完成。独立验收 pass（周文斌），但复核发现主 agent 的三路径范围判断有误：Rust 逆声明序析构使路径 1/2 的候选值在锁释放后 drop，不死锁；真实 P1 仅路径 3，与远程评论一致。开发 agent 用 restart 实验实锤（`err.generation == 1` 证明路径 2 可达且旧代码不死锁），test 3 改为文档化该语义（`c0b6b41`）。修复 `68491a7` 对三条路径统一显式化锁边界（IIFE），红→绿证据成立（test 1/2 修复前死锁）。全量 229/229，clippy/fmt 干净，20 次稳定。本地链 `0decf15 → 795c165 → 68491a7 → c0b6b41` 就绪，合入决策待用户裁决。
- 2026-09-25：按用户裁决按分支重组：#53 分支补重入测试（`5bc64fc`）、#54 变基、#55 分支含两个缺口测试 + P1 修复 + 语义澄清（顶点 `8cac519`），推送后远程审阅通过并合并（main `f37f90d`）；#40 随合并自动关闭，#27、#29 经验收对照后关闭。设计文档状态更新为已实现，本文件归档进仓库。
