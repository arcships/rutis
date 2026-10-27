# 0.20 的形式化覆盖

[English](design-formal-coverage-2026-10-28.en.md)

状态：草稿，未实现。日期：2026-10-28。基准：`main` `00b9fd3f`（0.18 收尾、0.19 实现中）。排期：0.20。

Issue：路线图 [#694](https://github.com/arcships/rutis/issues/694) 的 0.20；质量总 issue [#183](https://github.com/arcships/rutis/issues/183) 第四步。按本稿实施：[#634](https://github.com/arcships/rutis/issues/634)（规则清单与追踪表）、[#635](https://github.com/arcships/rutis/issues/635)（规格与保证表）、[#613](https://github.com/arcships/rutis/issues/613)（TLA+）、[#622](https://github.com/arcships/rutis/issues/622)（shuttle）、[#624](https://github.com/arcships/rutis/issues/624)（trace validation 与一致性套件补列）、[#630](https://github.com/arcships/rutis/issues/630)（非 Rust 运行时的可控调度）、[#631](https://github.com/arcships/rutis/issues/631)（属性测试、模糊测试、Kani、Miri）、[#639](https://github.com/arcships/rutis/issues/639)（一致性用例与普通测试）。

这 8 个 issue 已经写了每种手段具体做什么。本稿只定它们共同的事：范围按哪个版本算、什么时候开始、"不改行为"的界线、怎样算完成、检查在哪里跑、改 rutis 的人看到什么。

## 一、现在怎样

**用 rutis 的人**（插件作者、宿主持有者、用 `rutis-host` 装配的人）依赖的承诺写在[保证登记表](guarantees.md)里。登记表的"验证"一列只列测试，看不出哪条是在所有时序下检查过的，哪条只在测试碰巧走到的时序下成立。出过的事说明差别：统一事件的发出顺序与卸载规则在 0.13 之后由使用者先碰到（#513、#527、#536、#557、#587），都是只在特定时序下出现的问题。有些"不保证"只写在说明里，没有单独成行（#634"抽取时发现的问题"）。

**改 rutis 的人**：质量规范 Q9.1.2 要求多方交互的协议先做模型检查再定稿，Q11.2.3 要求设计改了模型一起改，但没有机制执行，统一事件就是没做模型检查定稿的。规则清单与追踪表现在在 #634 的评论里，没有人改代码时会同步它。

**追踪表的基准过时了**：#634 是按 `main` `3496669b`（0.16，2026-10-24）抽的，1368 条现行规则里应有形式化检查的 1114 条，形式化完整覆盖的 22 条。之后 0.17–0.19 加了 11 份有规则的设计稿（§3.1），改了会话帧（对象引用的 `objects` 能力）、调用（时限与取消）、远程运行时（一台与多台），这些都不在表里。

## 二、决策

### 2.1 范围按 0.19 发布时算

范围仍是 #634 定的：**保证登记表的全部条目 + 各协议规格的全部规则**，不按疑点挑。基准从 `3496669b` 改为 **0.19 的发布标签**：0.17–0.19 的设计稿按 §3.1 的前缀加进来，被它们改掉的旧规则标"已取代"。

不在范围内：

- 还没实现的设计：宿主自己写插件（#185，[设计](design-host-writes-plugins-2026-10-27.md)，实现排在 0.20 之后）；实现时再抽规则。
- 只有文档、没有行为的设计：[按层写指南](design-guides-by-layer-2026-10-27.md)、[设计文档约定](design-docs.md)。
- 示例项目 `rutis-agent`、`rutis-cli`。

不沿用 `3496669b`：0.18、0.19 改了的规则会按旧文字建模，模型检查的是已经不成立的规则。

### 2.2 0.19 发布之后开始

0.19 打标签之前，0.20 只做本稿和更新 issue 正文。这是路线图已定的理由：形式化覆盖不改行为，等 0.18、0.19 改完行为再做，规则不用改两遍。0.19 里改动的几乎覆盖每个分区（会话帧、调用、行、远程），没有可以提前安全开始的分区，所以不拆"先开始的部分"。

### 2.3 "不改行为"的界线

0.20 不加 API、不改配置格式、不改默认值、不升协议版本和 `PLUGIN_API`，没有迁移指南。检查中发现的问题这样处理：

| 发现 | 做法 |
| --- | --- |
| 代码不符合规格 | 在 0.20 修，加测试，规格不动。影响已经在用的人的，按补丁规则同时进 0.19.x |
| 规格写错，代码是对的 | 改规格（中英），旧规则标"已取代"，重新抽取 |
| 规格和代码都不能保证使用者以为的事，要保证就得做不兼容的改动 | 0.20 不改行为：登记表加一行"明确不保证"，规格写明；要不要改放到下一个不兼容的版本，补进已有 issue |
| 疑点不成立 | 对应的配置改为预期通过，README 写明 |

### 2.4 完成标准

0.20 发布时，追踪表里：

1. 应有形式化检查的规则全部"已覆盖"：检查通过，并有一个修复前的配置或故意引入的错误（变异）让它失败，证明它不是空的；
2. 形式化够不着的规则都有测试，状态"不适用"，备注写测试名；
3. 改为其他手段的规则写明理由；
4. 登记表每一条的"验证"列列出覆盖它的检查（§3.4）。

这四条进发布检查（§3.5）。0.20 之后加的规则同样要有检查，未覆盖的数目不得比上一次发布多。

### 2.5 追踪表落进仓库

追踪表从 #634 搬到 `docs/formal-trace/`：每个分区一个文件（`registry.md`、`events.md`、`session.md`、`runtime.md`、`remote.md`，以及 §3.1 新设计各自归入的分区），中英各一份；`README.md` 写范围、列的含义、统计。统计由 `scripts/formal-trace-stats` 从各分区的表算出，不手写。落进仓库之后以仓库为准，#634 关闭。

不放一份 JSON：评审要能在 PR 的 diff 里直接读每一行。

### 2.6 检查在哪里跑

- **新加的模型、shuttle 模型、模糊测试、trace validation**：每晚和完整运行里跑全量（#183 已定）。PR 上只在改了对应路径时跑短版本（TLA 跑全部配置，shuttle 每个模型 1000 次迭代），与现有 `specs` 任务、shuttle 模型的做法相同。
- **两个必跑的小检查**，每个 PR 都跑、设为合入必需，只读文件，不跑模型，各自一分钟内：
  - **模型漂移**（#635 里并入的 #612 第 7 项）：改了某个模型 README 里列出的实现文件、却没改那个 `specs/` 目录，PR 描述要写明为什么不涉及；
  - **追踪表引用**：追踪表和登记表里写的检查名确实存在，测试注释里写的编号确实登记了，统计是最新的。登记表开头写过核对表与测试"条目超过 300 条时再考虑"，现在登记表 374 条、规则一千多条，条件已到；只核对引用是否存在，不判断检查是否充分，没有误报。

PR 上 CI 的耗时目标（#577）不受影响：必跑的只多两个一分钟内的任务，按路径触发的跑在已有的并行任务里。

### 2.7 不新开 issue

0.17–0.19 新设计的规则按手段分到已有的 8 个 issue（§3.2），在各 issue 正文末尾"追踪表中归本 issue 的规则"补一节"0.17–0.19 加入的规则"。检查中新发现的问题先补进负责那条规则的 issue。

## 三、规则

### 3.1 新设计的前缀

规则编号仍是 `<前缀>-<章节>-<序号>`，登记表沿用原编号。新加的前缀：

| 前缀 | 设计 | 版本 |
| --- | --- | --- |
| `RAP` | [Rust API 收敛](design-rust-api-convergence-2026-10-24.md) | 0.17 |
| `DEF` | [插件定义跨语言](design-plugin-values-2026-10-25.md) | 0.17 |
| `DGN` | [诊断：错误、当前状态、时限](design-diagnostics-2026-10-26.md) | 0.18 |
| `CFG` | [配置 Schema](design-config-schema-2026-10-27.md) | 0.18 |
| `OBJ` | [对象按引用传](design-object-refs-2026-10-27.md) | 0.18 |
| `RRC` | [远程运行时一致性](design-remote-runtime-consistency-2026-10-26.md) | 0.19 |
| `LIM` | [管住插件](design-host-plugin-limits-2026-10-27.md) | 0.19 |
| `CTR` | [宿主契约类型](design-host-contract-types-2026-10-27.md) | 0.19 |
| `TRY` | [试用与测试插件](design-try-and-test-plugins-2026-10-27.md) | 0.19 |
| `PKG` | [插件安装](design-plugin-install-2026-10-27.md) | 0.19 |
| `ENT` | [SDK 入口收敛](design-sdk-entry-convergence-2026-10-27.md) | 0.19 |

抽取按设计稿在 0.19 标签时的文字，实施中的修订（设计稿状态行里的"同日修订""实施时改正"）以最后的文字为准。

### 3.2 新规则归哪个 issue

手段按 #634 的参与方定（跨进程用 TLA，进程内并发用可控调度，纯逻辑用属性测试 / 模糊测试，实现与模型对照用 trace validation 与一致性套件，其余用测试）。下表是每份设计主要的去向，具体到每条规则在抽取时定：

| 前缀 | 规则的内容 | 手段 → issue |
| --- | --- | --- |
| `OBJ` | 对象引用跨会话转发、持有方退出时释放、带对象的同步调用等待环 | TLA → #613（调用生命周期、跨运行时同步两个模型加对象）；引用计数 PT → #631；`objects.expected.json` → #639 |
| `LIM` | 调用时限、取消、`kill_on_timeout` | TLA → #613（调用生命周期模型加时限与超时结束进程） |
| `LIM` | 每个插件一个进程的监督 | SH → #622（Launcher 接缝的模型） |
| `LIM` | `allowed_services` 的判定 | PT → #631 |
| `LIM` | 问候时限 | 测试（虚拟时钟）→ #639 |
| `RRC` | 一台与多台共用发现与检查、`Unavailable`、`runtimes.remote` 三种写法 | TLA → #613（远程租约、节点托管行按新设计改）；接入点 TV → #624 |
| `DGN` | 当前状态快照与在途调用的一致 | SH → #622 |
| `DGN` | 错误结构经中继保持、`site`、错误码 | 一致性用例 → #639；格式化不能出错 → PT、FZ → #631 |
| `DGN` | 输出文字 | 测试 → #639（逐字比较，已有的验收测试写上编号） |
| `CFG` | 关键字、默认值、逐字一致的规则 | PT、FZ → #631；`specs/config-schema/cases.json` 记为一致性用例 → #639 |
| `CTR` | 生成的类型与契约描述一致、测试工具的严格检查 | 差分 PT → #631；一致性用例 → #639 |
| `PKG` | 安装记录与行的编辑、`remove` / `upgrade` | PT（对照参考模型）→ #631；其余测试 → #639 |
| `DEF` | 定义的值跨语言 | 一致性用例 → #639 |
| `TRY`、`ENT`、`RAP` | 命令行为、公开面、名字去向 | 测试 → #639（已有快照与验收测试写上编号） |

#635 负责把这些设计里的协议部分写进现行规格：`objects` 能力、`timeout` 码、调用取消进 `docs/protocol-session.md`；远程运行时的变化进远程与拨出的规格。规格先改，模型照规格建。

### 3.3 不是空的

每个标"已覆盖"的形式化检查，追踪表的"检查"列写两样东西：检查名，和让它失败的配置或变异名。没有后者的不算覆盖。做法同 `specs/remote-lease/expected.txt`（`lease-before-fix violated:RevokedStaysOut`）。

### 3.4 登记表的"验证"列

"验证"列除了测试，再列形式化检查，写法统一为 `<手段> <路径>::<名字>`：

| 手段 | 例子 |
| --- | --- |
| TLA | `TLA specs/events/EventsEmit.tla::EmitOrder` |
| SH | `SH rutis/src/shuttle_tests.rs::dispose_sees_cancel` |
| PT | `PT rutis-bridge/src/session/rpc/waits.rs::cycle_among_matches_dfs` |
| FZ | `FZ fuzz/fuzz_targets/frame_decode.rs` |
| KANI | `KANI rutis-loader/src/dylib/meta.rs::proof_header_bounds` |
| TV | `TV specs/events/EventsTrace.tla` |

路径规则同登记表 §4（相对 `crates/`，`crates/` 之外从仓库根起）。"状态"一列的含义不变。

### 3.5 发布检查

登记表 §1 加第 3 项，追踪表 README 写同样的话：

> 3. **从 0.20 起**：追踪表里应有形式化检查、状态不是"已覆盖"的规则数，0.20 为零，之后不得多于上一次发布。数字写在发布 PR 的描述里，与前两项放在一起。

### 3.6 以后改 rutis 的规则

- 改了多方交互规则的设计稿，定稿前要有"模型检查"一节（规范 Q9.1.1 第 5 项，#635）。
- 改了规格的 PR，同一 PR 里重新抽取受影响的规则（旧行标"已取代"）。
- 加检查的 PR，同一 PR 里把覆盖的规则改为"已覆盖"，写上检查名和让它失败的配置。

## 四、开发者看到什么

### 4.1 插件作者、用 `rutis-host` 装配的人、宿主持有者

行为没有变化，代码、配置、输出都不用改。0.20 中修掉的问题，修好之后的行为就是文档已经写的行为，没有新的出错文字。

能看到的是登记表。例如统一事件的发出顺序，0.20 之后在 `docs/guarantees.md` 里是两行：

```
| `events/emit-order` | 同一个发出方对同名事件的 emit，按发出的顺序开始各监听器调用 | 契约行为 | 已验证 | `TLA specs/events/EventsEmit.tla::EmitOrder`、`rutis-bridge/tests/event_hub.rs::emits_start_in_order` | 快速路径与经宿主两条路径都检查 |
| `events/no-total-order` | 不同发出方的 emit 之间没有先后；不承诺各进程看到同一个顺序 | 明确不保证 | 明确不保证 | `TLA specs/events/EventsEmit.tla::sees-all`（反例） | 协议规格 §4.4、§5.5 |
```

（编号与检查名以实施时为准；这里定的是形式：每条保证写出在所有时序下检查它的模型，不保证的写成单独一行并给出反例。）

0.20 的发布说明第一句写："这一版不改行为，不需要改代码"；后面列出修掉的问题，每个一行：现象、在哪些版本出现、修好之后怎样。

新概念：没有。"明确不保证"在登记表里早已有。

### 4.2 改 rutis 的人

这一节的角色不在 #663 的概念预算里（预算按插件作者、宿主持有者分层），这里列出新概念供维护者参考：规则编号、追踪表、六种手段的代号、"不是空的"。

**一切正常时**：改 `crates/rutis-bridge/src/events/hub.rs` 的 PR，同时改了 `specs/events/`，或者在 PR 描述里写一行

```
模型：events 不涉及（只改了日志文字）
```

英文写 `Model: events not affected (only log text changed)` 也认。两个必跑检查通过，`specs` 任务跑 `events` 的全部配置。

**出错时**，每种一行原因、输出原文、怎么改：

1. **改了模型列出的文件，没改模型，也没写理由**。`model-drift` 检查失败：

   ```
   model-drift: crates/rutis-bridge/src/events/hub.rs is listed in specs/events/README.md, but specs/events/ is unchanged.
   Change the model, or add this line to the PR description: 模型：events 不涉及（<理由>）
   ```

   改法：改模型，或者在 PR 描述里加那一行；改描述之后检查自动重跑。

2. **TLC 的结果与预期不同**。`specs` 任务失败，`run.sh` 的那一行（现有格式）之后多一行这个性质覆盖的规则：

   ```
   events-emit-healthy                violated:EmitOrder             expected ok                             (1,204,331 distinct states found; 212s; log /tmp/tmp.Xa81/events-emit-healthy.log)
   covers: EVP-4.4-2 EVP-5.5-1 events/emit-order (docs/formal-trace/events.md)
   ```

   反例的每一步在 log 里（TLC 原样输出）。改法：代码错了改代码；规格错了改规格和模型，按 §2.3。

3. **shuttle 模型失败**。测试失败的输出最后两行：

   ```
   shuttle model dispose_sees_cancel failed at iteration 48213 (rules: core/D24 core/features.4.2)
   replay: RUTIS_SHUTTLE_REPLAY='<schedule>' RUSTFLAGS="--cfg rutis_shuttle" cargo test -p rutis --lib shuttle_tests::dispose_sees_cancel -- --exact
   ```

   用第二行在本地按同一个调度重放。其他语言的可控调度（#630）照这个形式输出各自的重放命令。

4. **trace validation 不符**（每晚运行）：

   ```
   trace-validation events: line 1834 of host.log matches no action of EventsHost: {"op":"reply","emit":42,"row":"weather"}
   last matched: StartCall(emit=42, listener=3) at line 1833
   ```

   改法：实现做了模型不允许的事，按 §2.3 处理；如果是日志点写错，改日志点。

5. **追踪表引用不存在的检查，或测试注释写了没登记的编号**。`formal-trace` 检查失败：

   ```
   formal-trace: docs/formal-trace/events.md row EVP-4.4-2 names TLA specs/events/EventsEmit.tla::EmitOrder, which is not defined there.
   formal-trace: crates/rutis/tests/contract.rs:212 names guarantee core/D99, which is not in docs/guarantees.md.
   ```

   改法：改正名字或编号；检查被改名时同一 PR 里改追踪表。

6. **统计没更新**：

   ```
   formal-trace: the counts in docs/formal-trace/README.md are out of date; run scripts/formal-trace-stats --write
   ```

   改法：跑这条命令，提交结果。

7. **加了检查却没有让它失败的配置**（评审时看，不是 CI）：追踪表的"检查"列只有检查名，评审要求补上修复前的配置或变异，否则这一行不改为"已覆盖"。

## 五、兼容与迁移

不改代码、配置、协议和输出，没有迁移指南。发布说明照 §4.1 写。0.20 发布后照常重跑 #663 的角色评估，结果应与 0.19 相同；有不同就是 0.20 修问题时改了不该改的行为，按 bug 处理。

## 六、实施

0.19 打标签之后按下面的顺序做（与 #183 第四步"先做、再做、然后"一致）：

1. **重新抽取（#634）**：按 0.19 标签重抽全部分区，加上 §3.1 的前缀；旧行该取代的标"已取代"；更新统计；各 issue 正文补"0.17–0.19 加入的规则"。完成：每条现行规则有手段和 issue。
2. **先做：规格、登记表、追踪表进仓库（#635、#634）**：会话帧现行规格（含 §3.2 的 `objects`、`timeout`、取消）、远程与网络栈和多语言的规格修订、登记表修正、质量执行现状、Q9.1.1 第 5 项；`docs/formal-trace/`、统计脚本、两个必跑检查（§2.6）。完成：#635 的清单全部勾上，`model-drift` 与 `formal-trace` 是合入必需的检查，#634 关闭。
3. **再做：共用的基础**：shuttle 的三个替身与 Launcher 接缝（#622）、日志点（#624）、`EventsHost` 共用模块（#613）、shuttle 失败时的重放输出（§4.2 第 3 条）。
4. **然后：按手段并行**：#613、#622、#624、#630、#631、#639 各自按正文和补上的规则做。同一时间改同一个文件的，后合并的一方先同步 main。
5. **发布**：§2.4 的四条、§3.5 的数字写进发布 PR；登记表 §1 的前两项照常；发布说明按 §4.1。

每个 issue 的完成：正文末尾列出的规则（含补上的）在追踪表里全部符合 §2.4。开始实现时在 #694 写 PR 规划（每组一行），进度另开一个跟踪 issue，同 0.19 的 #771。
