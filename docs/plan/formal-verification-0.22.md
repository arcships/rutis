# 0.22 形式化覆盖执行计划

Status：执行中。日期：2026-11-09。
范围决定：0.23 并入 0.22。0.23 里程碑的 #613、#622、#624、#630、#631、#634、#635、#639、#776 与 0.22 的 #1025 一起作为 0.22 的发布范围；#1040 继续延期，不因合并重新纳入。
依据：[形式化验证重新设计](../design-formal-verification-2026-11-08.md)、[0.22 路线图](../roadmap-0.22-2026-11-08.md)、[质量规范](../quality-standard.md) §3、§8、§9.4、§11。

本文只安排工作和缺口，不改变行为。开发者可见的变化为零；实现完成后按各项设计文档的“开发者看到什么”验收。

## 1. 工作流

| 编号 | 范围 | issue | 交付物 | 依赖 | 状态 |
| --- | --- | --- | --- | --- | --- |
| WS0 | 基线、路线图合并、缺口登记 | #776、#634 | 本文件、路线图与质量现状修订 | — | 进行中 |
| WS1 | #1025 事件关闭接纳：marker、发布、Entry::live、竞态测试、benchmark | #1025、#622 | 代码、确定性测试、benchmark 结果 | WS0 | 核心完成，验证中 |
| WS2 | 现行会话协议规格：帧、握手、调用、引用、取消、畸形帧、上限、未知字段 | #635 | `docs/protocol-session.md` 与 `.en.md`；旧设计改为引用；规格缺口清单 | WS0 | 第一版完成；未知字段已定（调用级失败） |
| WS3 | 规则清单与追踪表：每条规则的来源、人话承诺、证据、变异、状态、缺口 | #634 | `docs/formal-trace/` 与统计脚本；`guarantees.md` 接上 | WS2 部分输入 | 第一版完成，1817 行（含 1368 条协议规则）；未按现行规格逐条复核 |
| WS4 | 协议设计的 TLA+ 模型：事件关闭接纳、跨运行时同步、远程租约、行与调用生命周期 | #613 | `specs/` 新模型或对账后的旧模型、`expected.txt`、`run.sh`、README | WS2 | 九个模型全部扩到设计稿范围：event-close、events 四模块、cross-runtime-sync、remote-lease、row-lifecycle、call-lifecycle、peer-rows；约 215 个配置，新增配置都已实跑；剩余是实现修复与模型跟随 |
| WS5 | 真实执行 trace 与一致性：通道/帧 trace 验证、适用实现列 | #624 | 验证器、正反用例、缺口清单 | WS2、WS4 | 首块完成：16 个用例；上限、leases、host-gone、sync-cycles 待补 |
| WS6 | 非 Rust 运行时可控调度与静态检查 | #630 | 每个适用语言的证据；不适用写原因 | WS2 | 首块完成 |
| WS7 | 属性、模糊、Kani、Miri、虚拟时钟 | #631、#639 | 属性测试、模糊目标、计时用例、Kani/Miri 结论 | WS2 | 首块完成：引用回滚 6 + 值/帧/坏帧属性 5；Kani/Miri/fuzz 未跑 |
| WS8 | 收口：追踪表清零、发布门、CI 报告 | #634 | 发布检查结果与未验证清单 | 全部 | 进行中：追踪表检查已进 CI；发布门未开始 |

## 2. 顺序与门槛

第一段（现在，可并行）：

1. WS1 的竞态测试与模型先落地，再改 core；core 改动必须能单独回滚。
2. WS2 先写现行规格；规格与代码不一致时先改规格，不先改代码。
3. WS3 的表格先建骨架并接入现有 `guarantees.md`，规则从规格和保证表抽取，不从记忆里补。
4. WS4 先对账现有 `specs/cross-runtime-sync`、`specs/remote-lease`，再新增事件关闭接纳模型。
5. WS5 先做通道 trace 的允许序列验证；帧级验证与跨实现列随后。
6. WS6、WS7 先做本机可跑的语言和工具；Swift/Kotlin 与需要专用 runner 的项登记为环境缺口。

第二段（第一段有可运行证据后）：

- WS4 的事件、行生命周期、调用生命周期、节点托管行模型补齐；
- WS5 把真实 trace 接到 leases、host-gone、sync-cycles、AccessEvent 和跨语言一致性；
- WS7 的虚拟时钟覆盖所有计时承诺；
- WS8 按追踪表逐条清零。

发布门：按设计 §10 与质量规范 Q13.1，追踪表每条适用规则都有现行来源、实现证据、适用矩阵和状态；核心承诺未验证数为零；模型、trace、一致性、属性/模糊、虚拟时钟、平台和长期测试均有结果。

## 3. 缺口与阻碍

| 编号 | 缺口/阻碍 | 影响 | 处理 |
| --- | --- | --- | --- |
| G1 | 未知字段的旧行为：Rust 整会话结束，其他六种忽略 | 错误范围过大或跨实现不一致 | 已定 B：拒绝，但只失败该调用；七种运行时都已实现（`bad-frame`，`hello` 失败握手，`release`/`cancel` 忽略） |
| G11 | Swift、Kotlin 仍静默忽略未知字段，没有 `bad-frame` 调用级拒绝 | 同一份带新字段的帧在不同实现上结果不同 | 已解决：Swift/Kotlin 已实现并进 CI；七种运行时一致 |
| G2 | #634 的 1368 条规则清单只在 issue 评论里，仓库没有可抽取来源 | 追踪表无法声称完整 | WS3 先建 schema 和 `guarantees.md` 全量映射；规则清单抽取做成脚本或独立数据文件；完成前状态只能是“部分” |
| G3 | `release_transient` 在 `Shared.admission` 外摘除 child | marker 集合可能漏成员 | WS1 把摘除移进 admission；若保留无锁摘除，必须补“alive=false 后不可接纳”的证明和并发用例 |
| G4 | 事件关闭接纳目前没有 shuttle 模型；新 marker 若用 std 原子，shuttle 没有切换点 | 无法穷举 marker 发布与 `Entry::live` 的竞态 | WS1 的 marker 使用 `crate::sync::AtomicBool`，或在读取点加 `switch_point()`；模型必须能在顺序错误时失败 |
| G5 | 本机没有 Swift、Kotlin 工具链；Fray/Lincheck、TSan、模拟器需要对应 runner | WS6 的 Swift/Kotlin 证据无法在本机产生 | WS6 已完成 Node 帧泵 4 个用例；Bun/Python/Go 仅做现有测试盘点，未新增；Swift/Kotlin 登记为 CI/设备缺口，不写成通过 |
| G6 | `formal-trace.md` 只有通道级 trace（方向、长度、结束），没有帧负载 | WS5 的帧级 trace 验证不能直接复用生产 trace | 测试夹具解码帧字节；生产 trace 保持只记大小，按设计 §1 执行 |
| G7 | 现有 TLA+ 模型只覆盖 `cross-runtime-sync`、`remote-lease`，事件与行生命周期模型不存在 | WS4 的覆盖不能声称完整 | 先对账旧模型，再新增最小 `specs/event-close/`；事件模型按 issue #613 的范围分批 |
| G8 | 0.22 发布范围从 #1025 扩到完整四层，发布日期不再由单一性能改动决定 | 发布门时间不可预估 | 路线图改为分阶段 gate；WS8 只按追踪表判断，不按“测试跑过”判断 |
| G9 | Node/Bun 会话关闭时若有已接纳、未执行的业务帧，`drain()` 可能永不结算 | 运行时留下悬挂 promise，与 Python 行为不一致 | WS6 已复现；按 `close()` 对每个被丢弃任务 `#finish()` 或结算 `#draining` 另开修复项，并补回归测试 |
| G10 | `formal-trace.md` §1/§3.0 仍把 `objects` 能力写成对象传递的不保证；新会话规格认定协议 4 没有 `objects` 能力、六种实现都收对象 | 两份现行文档对同一行为给出不同规则，追踪表无法判定状态 | WS2 先按代码复核；确认后改 `formal-trace.md`、`guarantees.md` 和追踪表，不能同时保留两种说法 |
| G12 | 异步调用没有“之后是否 await 结果”的协议信号，854bef5d 只能保守记边，误拒不会死但会失败；去掉会漏真实等待边 | 已标记链上的异步调用可能被误判成环，或相反地漏掉真实环 | 已开 issue #1098；在协议定信号前保留保守行为，模型 `async-policy-b` 记为已知误报 |
| G13 | 排队中被取消的调用被调方不回复，`Calls.cancelled` 项只由迟到回复清理，留到会话结束 | 长会话里反复取消排队调用会无界涨内存；直接清表项会把合法迟到回复当悬空而断会话 | 已开 issue #1099；等取消确认帧/迟到回复规则定下再改代码 |
| G14 | 模型按 `ctx.instance()` 建 peer row，代码更像 `plugin#n` 每次新键 | 同一实例跨重启/两会话时模型与实现不是同一身份，覆盖不能直接算数 | 已开 issue #1100；先定 #625/#640 的行身份语义，再对齐代码和模型 |

## 4. 文件所有权（并行时避免冲突）

| 工作流 | 独占/主要文件 |
| --- | --- |
| WS1 | `crates/rutis/src/fiber.rs`、`crates/rutis/src/ctx.rs`、`crates/rutis/src/bus/table.rs`、`crates/rutis/src/fiber/shuttle_tests.rs`、`crates/rutis-loader/tests/close_cpu.rs`、新增事件关闭测试 |
| WS2 | `docs/protocol-session.md` 与 `.en.md`、`docs/design-protocol-plugins-2026-09-25.*`、`docs/design-protocol-plugin-mount.*` |
| WS3 | `docs/formal-trace/**`、`docs/guarantees.md` 的链接列、统计脚本 |
| WS4 | `specs/**` |
| WS5 | `crates/rutis-bridge/tests/trace_validation.rs`、`crates/rutis-bridge/src/channel/` 的 trace 读取辅助（不改协议） |
| WS6 | `node/`、`bun/`、`python/`、`go/` 的测试与脚本 |
| WS7 | 新增属性测试文件、`fuzz/fuzz_targets/`、虚拟时钟测试文件 |

冲突规则：同一 crate 的 `Cargo.toml`、`session/protocol.rs`、`bus.rs` 由 WS1 或主线程改；其他工作流需要改这些文件时先报告，不直接改。

## 5. 执行记录

- 2026-11-10：六个模型目录扩到设计稿范围：events 33 配置、cross-runtime-sync 44、remote-lease 32、row-lifecycle 36、call-lifecycle 35、peer-rows 28；新增配置全部实跑通过，`specs/README` 与计划状态同步；剩余是实现修复与验证收口。
- 2026-11-10：三个设计缺口开 issue：#1098（异步调用“是否等待结果”信号）、#1099（取消表清理与取消后回复）、#1100（peer row 键身份）；当前 PR 只做不改变这些语义的实现 bug 修复。- 2026-11-09：`events::link::accept` 旧提供回插已修（`gone` 边界 + 插入同锁重检，2 个确定性回归测试）；`cross-runtime-sync` 的“已标记链异步调用”疑点发现帧上无法区分“发起即不管”和“之后 await”，先回退代码改动，待协议加信号后再改。
- 2026-11-09：TLA+ 九块全部落地：`specs/events/` 四模块 21 配置、`specs/peer-rows/` 7 配置、`row-lifecycle` 22 配置、`call-lifecycle` 16 配置、`remote-lease` 21 配置、`cross-runtime-sync` 新配置；模型发现 bail 分发盲区、本地放弃不发 cancel、已标记链异步调用误判、link accept 旧代次回插等疑点。
- 2026-11-09：第三批：shuttle 增至 9 个模型（新增 root dispose/shutdown、依赖驱逐）；`specs/call-lifecycle` 7 个配置按预期结束；trace 验证器 23 个用例；cargo-fuzz 实际跑 session_frame/websocket_upgrade/error_graph/local_lines 共 4 个目标、无 crash。
- 2026-11-09：第二批代码落地：shuttle 增至 7 个模型（新增 effect 登记/dispose、在途卸载、并发子树关闭）；`specs/row-lifecycle` 9 个配置按预期结束；trace 验证器 16 个用例；值/帧/坏帧属性 5 个；Node/Bun/Python/Go 实现未知字段调用级 `bad-frame`；#634 协议规则 1368 条导入追踪表（1817 行），rule tracker 检查进 CI。
- 2026-11-09：未知字段规则定为“拒绝、但只失败该调用”（`hello` 失败握手，`release`/`cancel` 忽略）。Rust 已实现并以 `bad-frame` 返回；其他六种仍忽略，记为 G11。
- 2026-11-09：0.23 并入 0.22；建立本计划；WS1、WS2、WS3、WS4、WS5、WS6、WS7 并行启动。
- 2026-11-09：WS1 的 marker 发布、`Entry::live`、root/子树路径和 `release_transient` 锁已实现；4 个确定性竞态用例和 1 个 shuttle 模型通过；把发布顺序写错时模型失败。本机 release `close_cpu`：1000 个 instance 创建 17.55 s、关闭 11.15 s，阈值通过，issue 的 ≤30 秒创建目标在本机达到；`queued_emits_memory` 通过，每个排队 emit 约 1938 B。
- 2026-11-09：WS2 的会话协议规格第一版完成（82 条 `SESS-*` 规则，中英一致）；未知字段冲突仍未决定。
- 2026-11-09：WS3 的追踪表第一版完成：264 行（登记表 210 + 手写 54），`node tools/formal-trace.mjs --check` 零问题；协议 1368 条规则仍未抽。
- 2026-11-09：WS4 的 `specs/event-close/` 首块模型完成；7 个配置按 `expected.txt` 结束（healthy 2,263,040 distinct/21s、tree 688,580/12s、drain 31,488/1s；4 个变异分别违反预期不变式）。
- 2026-11-09：WS5 的通道/帧 trace 验证器落地，11 个测试通过；WS7 的引用授予回滚属性测试落地，6 个测试通过，变异可被捕获；WS6 的 Node 帧泵 4 个用例落地，并复现 Node/Bun `drain()` 悬挂（G9）。
- 2026-11-09：七个工作流的首块都已完成或进入验证；WS8（收口、CI 接入、全量规则抽取）未开始。
