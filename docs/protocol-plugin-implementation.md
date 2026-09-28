# #59 工作记录

> 2026-09-28 按用户要求补正原生使用体验目标。当前范围以[修订设计](design-protocol-plugins-2026-09-25.md)为准。
> 本记录替代旧逐阶段日志；历史细节保存在 Git 中。

## 当前目标与状态

实现分支：`feat/protocol-plugins-59`。原设计基线：`0d8a5ac`；`d134015` 为本轮收尾前的实现基线。本轮收尾改动随实现分支提交。

原 C01–C10 已验证对象协议基础，但同一业务插件在本地与协议服务之间保持原生用法的目标尚未完成。服务业务类型、原生事件语义、依赖恢复与默认加载入口的适配属于当前必需工作。既有测试通过不能替代这项验收。部署更新、恢复编排、布局比较和性能专项继续排除。

此前写出的清理观察、ManagedCleanup 与 epoch supervisor 改动随实现分支保留。本轮没有继续扩展部署/监督实现；已有实验代码不因为投入过开发就成为本轮交付要求。它们存在于实现差异中，不能把范围简化理解为已经从代码中删除。

## 本轮收尾改动

- 适配发行包 `SettingsProvider` 的真实 SettingsScope：异步读写、原生 schema、稳定身份、owner 传回、借用回调及卸载，见[运行示例与迁移调用点](../protocol/settings-example.md)。只替换测试存储，不模拟原生服务行为。
- 新增 `events::HostEvents`，补齐统一监听顺序、parallel/serial、ready、once、scope、注销、在途工作与原生卸载。复用已有对象通道；Rust/Node 双向发布真实对象载荷，见[事件 API 与边界](../protocol/events.md)。
- TS 原生端口允许明确声明 `localInjects`，使实际设置插件保留本进程 settings 依赖。用例验证缺依赖 Pending、实际提供后 Active，以及失去依赖封闭旧上下文。
- 新设置 bundle 及两端生成绑定纳入重生成核对；CI 安装锁定的 host 依赖，以运行真实设置服务测试。
- 补充[跨进程插件开发指南](protocol-plugin-guide.md)：应用与 Host 的关系、完整示例、服务/回调/事件写法、装配顺序和当前入口限制。文档从 README 与开发手册可直接进入。

事件端点按应用显式接线；默认通用 driver 未安装端点，仍在 hello 拒绝事件 capability。事件结果限于声明的 JSON 值，支持对象载荷；不支持的模式和类型明确拒绝。纯本地事件保留原行为。本轮不提供任意旧插件零修改迁移。

## 核心用例证据

| 用例 | 已运行的验证入口与结论 |
| --- | --- |
| C01 原生双向注入 | `session_ipc.rs`、`native_runner_ipc.rs`：真正 rutis/Cordis 从原始上下文取得对方服务；`managed.rs`、TS `services.test.ts` 验证缺依赖不 apply |
| C02 状态对象 | `objects_ipc.rs`、`session_ipc.rs`：两端对象返回、继续调用和 owner 传回；`settings_ipc.rs` 验证发行包原生状态与 schema |
| C03 身份与授权 | `objects.rs`、`graphs.rs`、`drafts.rs`：重复/循环身份、独立权限视图、伪造和第三方转交拒绝 |
| C04 作用域与释放 | `objects.rs`、`graphs.rs`、`exports.rs`：别名、独立 child scope、独占 disposer 一次及失败；设置用例卸载后注册归零、旧属性失效 |
| C05 借用回调 | `bindings.rs`、`objects_ipc.rs`：A→B→A、实际创建者与登记后代；设置用例保存的借用回调到期拒绝 |
| C06 发布与失效 | `lifecycle.rs`、`native_runner_ipc.rs`、`managed.rs`：发布/activate ACK、必要服务失效、Loading stop、旧 Ctx 与迟到 ACK 封闭 |
| C07 等待与执行 | `bindings.rs`、`exports.rs`、`frames.rs`：丢弃等待者、别名释放与执行 pin 独立；`events.rs` 注销和卸载等待真实监听结束 |
| C08 基础事件 | `events.rs`：并行等待/错误、serial 顺序和 0/false/null、ready、重入 once、scope、参数拒绝、注销与原生清理；`settings_ipc.rs`：真实双向发布、对象继续调用与传回 owner |
| C09 契约与断连 | Rust `contracts.rs`、`graphs.rs`、`frames.rs` 与 TS 共享语料；`session_ipc.rs` 的断连撤销/迟到消息，默认 driver 拒绝未实现 capability |
| C10 真实插件适配 | `settings_ipc.rs` 使用已发行 SettingsProvider；异步、属性、事件、清理改动见设置示例；旧桥 TCP e2e 纳入 workspace 回归 |

这些证据对应已实现的传输与生命周期基础，不证明同源码的原生使用兼容。新验收复用同一份提供者、消费者和事件插件源码，分别运行本地和真实跨进程模式；不扩展为进程布局或组合故障矩阵。

## 验证记录

- 2026-09-28 同步 main 后完整 Rust workspace：**536 passed / 0 failed / 2 ignored**，命令终态 exit 0。包含设置服务真实 Node IPC、双向服务与事件、保留的实验组件及旧桥真实 Node TCP e2e。
- TS 协议：**77 passed / 0 failed**，check/build 通过。
- workspace all-targets check、rutis/协议 all-targets clippy `-D warnings`、fmt 通过；host 原有 build 与新适配器/fixture 的严格 TS 类型检查通过。
- 开发指南的 Rust 消费者片段编译、TS 提供者、事件监听和发布片段严格类型检查通过；19 份本轮 Markdown 文档的 139 个本地链接及代码围栏检查通过。
- 第一次当前 workspace 回归在旧 prepare 测试出现 8 个磁盘配额失败；错误均为复制临时文件的 `QuotaExceeded`，单项复跑通过。将 `TMPDIR` 指向工作区 `target/protocol-test-tmp` 后完整复跑通过。
- 同步 main 后修正协议依赖版本、原生事件键调用和真实运行器 catalog/清单的版本声明；完整复跑通过。更新前回归中的 runner metadata 不匹配属于这一兼容性问题。同步前收尾实现为 Rust **509 passed / 0 failed / 2 ignored**。
- 两个 ignored 分别依赖外部 min-cordis/dsh 检出与真实模型后端，不计入通过。上一次提交基线为 Rust **502 passed / 0 failed / 2 ignored**，TS **75 passed / 0 failed**。

当前日志：`/tmp/rutis59-workspace.log`、`/tmp/rutis59-ts-test.log`。本机复现完整回归使用 `TMPDIR="$PWD/target/protocol-test-tmp" cargo test --workspace`，先建立该临时目录；TS 使用 `npm --prefix protocol/ts test`。

## 来源与维护责任

Rust 使用本仓库 rutis 源码，已同步 main 的待发布 **0.4.0**；Cordis 使用锁定的发行包 **4.0.1**，不维护 fork。设置服务使用 **0.1.1-rc.2** 发行包。

Rust 适配使用本仓库新增的公开 `FiberView.seal_effects`、`Ctx.is_within` 和 `Ctx.track_dependency_cleanup`，需要随相应 rutis 发行版本提供；它们不属于未修改的外部 rutis 0.3.0。接口与适配层由本仓库维护。

当前 TS 适配器和生成接口是实验源码 API，尚未作为独立稳定安装接口发布；跨进程方法异步，反射、类实例和持久业务回调需明确适配。

## 阅读入口

- [开发指南](protocol-plugin-guide.md)：面向应用和插件作者的接入步骤、代码与限制。
- [修订设计](design-protocol-plugins-2026-09-25.md)：原生使用体验目标、接入方式与同源码验收。
- [设置服务示例](../protocol/settings-example.md)、[基础事件](../protocol/events.md)：本轮实际接入与运行方式。
- [协议实现约定](../protocol/README.md)、[服务绑定](../protocol/services.md)、[生命周期](../protocol/lifecycle.md)：已有互通基础。

其他协议目录文件是已有实验 API 参考，不增加本轮任务。

## 当前必须补齐的原生接入

按修订设计依次补齐公共服务接口绑定、原生装载代和依赖恢复、原生事件入口、默认运行器与普通插件加载入口。业务不导入协议 Client/CallContext，不调用专用事件 helper，不自己登记回调创建者或装配 SDK。应用只选择插件、契约与运行位置。

设计已细化到服务读取者上下文、每代状态/消息、事件顺序与结果映射、初始化准入及 A–D 同源码验收，见修订设计第 8–9 节。接口名称仍为提案，以上运行时代码尚未按新目标完成。Cordis 的本地可行性试验已验证公开 Context.extend/getTraceable 能保持原始监听上下文及原生事件入口；该试验没有 IPC，不能计作原生接入验收。MR 应明确保留这一完成缺口。
