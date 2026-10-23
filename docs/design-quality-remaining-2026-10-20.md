# 质量控制剩余项：保证登记尾项、agent 写插件与资源上限（方案）

[English](design-quality-remaining-2026-10-20.en.md)

状态：草案，待评审。日期：2026-10-20。基准：`main` `4e23339`。
关联：[质量控制总 issue](https://github.com/arcships/rutis/issues/183)。依据：[质量规范](quality-standard.md)、[质量执行现状](quality-status.md)、[保证登记表](guarantees.md)。

## 一、范围

质量控制总 issue 真正没做完的是两类：

1. 保证登记表里 8 条还没钉死的行为；
2. agent 写插件、接上、迭代的端到端主线。

另外有一项不在总 issue 的勾选里，但决定 agent 写的坏插件会不会拖垮宿主：一个会话里引用与调用数的上限。本方案把它作为**并列的安全轨道**一起设计，不阻塞 agent 主线；agent 场景使用受控测试插件，资源上限单独排 PR。

已经随统一事件完成、只需在总 issue 里勾掉的三项：跨边界事件携带分发方式、Cordis 转发到 rutis 的同步 bail、internal 事件跨边界。

不在本方案：`cargo xtask dev`、dsh 的发布分发，以及质量现状里其它独立缺口（actionlint、许可证、安装冒烟、Swift / Kotlin 扩展等）。这些各自开 issue，不塞进质量总 issue。

## 二、8 条登记尾项

每条都要有能重复跑的测试；测试通过后，登记表从“部分验证 / 未验证”改为“已验证”。不能补的，降为“明确不保证”，并写一条锁定当前行为的测试。

| # | 行为 | 现在缺什么 | 要加的测试 |
| --- | --- | --- | --- |
| 1 | 保存队列与多写者：保存严格串行、按提交顺序；持续冲突后返回 `Conflict`，待保存队列保留 | 保存的串行与顺序、持续冲突后的 `Conflict` 没有测试 | `save_is_serial_and_in_submission_order`：两个并发保存，断言存储写入顺序与调用顺序一致；`persistent_conflict_keeps_the_queue`：存储永远冲突，3 次后返回 `Conflict`，队列仍在，`flush` 再试 |
| 2 | 保存的层恢复：改过隔离范围或注入后，用存下的层在新 loader 里得到相同的期望树与运行态 | 只断言了存下的 patch；改 `isolate` / `inject` 后没有在新 loader 里恢复 | `isolate_and_inject_changes_restore_in_a_new_loader`：改动、持久化、新建 loader、比较期望树；再创建实例比较运行态 |
| 3 | 实例编辑全有或全无：多个实例上一起重命名、重载，任一失败则全部回到旧模块，不写可编辑层 | `rename_module`、`reload` 在多个实例上一起应用、一起回滚没有测试 | `rename_and_reload_apply_to_every_instance_or_none`：两个实例，一个副本故意失败；断言两个都留在旧模块，可编辑层未变。这条是核心承诺，另加一条黑盒或另一路径的独立验证 |
| 4 | Go 运行时的 eager 模式：全部启动并一直运行，空闲也不停 | `eager` 没有测试 | `go_runtimes_in_eager_mode_start_all_and_stay`：两个二进制、`eager`；断言两个进程都起来，空闲超过 `idle` 后仍在，停止宿主后都退出 |
| 5 | 停止截止时间：超时后逐个打印还在停止的插件；启动失败时，已经起来的部分也按同样规则关闭 | 只断言了 `did not finish`，没有逐项断言；启动失败（不是信号）的关闭没有测试 | `deadline_names_every_stopping_plugin`：两个插件清理不返回，断言两行都出现；`startup_failure_closes_what_started`：第二个运行时启动失败，断言第一个已关闭且退出码正确 |
| 6 | 退出码：配置错误、行加载失败、运行时启动失败退出 1；运行时没按时退出退出 2 | 配置错误 / 行加载失败退出 1、运行时没按时退出退出 2 没有测试；启动失败退出 1 只在发布流程里断言 | `run_exits_1_on_config_and_row_failures`、`run_exits_2_when_a_runtime_misses_the_exit_deadline`、`run_exits_1_when_a_runtime_fails_to_start`（从 `install.yml` 移进普通测试） |
| 7 | run / dev 的行状态输出：`starting`、`failed: …`、`disabled`、`removed` 与实例副本的写法 | 只测了 `running` 和 `waiting for its services` | `row_state_lines_cover_every_state`：一份配置依次触发这几种状态，逐行断言输出；实例副本断言 `<行 id> in PluginId(n)` |
| 8 | `rutis-host go add`：用本机 Go 工具链把插件二进制装到 `runtimes.go.dir` | 完全没有测试 | `go_add_runs_go_install_in_the_configured_dir`：用 PATH 上的 stub `go` 记录参数并在目标目录写一个假二进制，断言宿主传的模块、工作目录与提示；不依赖网络与真实 Go 安装 |

验收：8 条全部“已验证”，或明确降级并带锁定测试；总 issue 的对应勾选更新。

8 条里 7 条的产品行为已经在现有代码里，主要是补测试；`go add` 的宿主命令也在，但真正的 Go 安装需要本地文件 GOPROXY 的测试，stub 只验证宿主传参。在真安装测试通过前，这一条保持“部分验证”，不写“已验证”。若测试发现行为不符，按缺陷修。

## 三、agent 写插件主线

### 3.1 产品决定：两个通用工具，不是每个方法一个工具

给 agent 两个工具：

- `rutis_services`：无参数，返回当前可用的服务列表。每项是 `{ name, provider, methods, callable }`；`provider` 是提供它的行或宿主，`methods` 是 `{ 方法名: "sync" | "async" }`（运行时报了就有，没报就是 `null`），`callable` 表示能否从 agent 这一侧按名字调用。
- `rutis_call`：参数 `{ service, method, arguments }`，按名字调用一个服务方法，返回 JSON 或模型可见的 `error: ...`。

不做“每个服务方法生成一个工具”。原因：

- 服务方法没有跨语言的 JSON Schema；为每个方法造 schema 只能猜；
- 工具列表会随插件加载、卸载、热替换不停变化，agent 的 prompt 也跟着变，难复现；
- 两个通用工具在插件换实现、换语言、换节点时都不用改，agent 从 `rutis_services` 读当前状态，从错误里学参数。

方法的详细说明由插件自己的文档提供；运行时只保证名字、种类和调用结果。

### 3.2 数据与调用路径

新增 `Loader::services()`，返回：

```rust
pub struct ServiceInfo {
    pub name: String,
    pub provider: ProviderInfo, // 行 id，或宿主
    pub kind: ServiceKind, // 插件行 / 宿主 / 远端 / 实例
    pub methods: Option<serde_json::Value>, // { method: "sync" | "async" }
    pub callable: bool,
}
```

实现从 loader 当前的行与宿主服务目录取名字；对每个名字，尝试 `ctx.get_as::<dyn HostDispatch>(host_key(name))`：

- 拿得到：`callable = true`，`methods` 取 `HostDispatch::methods()`；
- 拿不到：列出来但 `callable = false`，`rutis_call` 返回“这个服务没有按名字对外”。

`kind` 用来做默认权限过滤：`rutis_services` 默认只返回插件行提供的全局服务；宿主、远端、实例内服务只有出现在 `allow` 里才返回。

`Loader::services()` 的名单只有一个来源：当前行与宿主目录；`callable` 只由 `HostDispatch` 是否可达决定。两处不一致时列出来并标 `callable = false`，不猜。先加一条 `service_names_come_from_rows_and_host_catalog` 的清单测试，再加工具。

新增 `ServiceToolsPlugin`（`rutis-agent` 的可选 `loader` feature，依赖 `rutis-loader` 与 `rutis-bridge` 的 session；不会形成循环依赖）：

- `inject = [tools]`，从 `ToolRegistry` 注册两个工具；
- `ToolRegistry` 增加 `register_scoped`（返回 token）与 `unregister(token)`；卸载时注销这两个工具，token 保证只移除自己注册的，不会误删同名工具；driver 每轮都读一次 schema，所以注册晚一轮也能在下一轮出现；
- `rutis_call` 通过 `HostDispatch::invoke` 调用；返回值是 future 时用现有的 settle 辅助等待，sync 方法直接返回；
- 未知服务、未知方法、不可按名调用、参数错误都变成 `error: ...` 文本，不 panic，不影响其他工具。

归属：`rutis-agent` 是示例 crate。`ServiceToolsPlugin` 先放在它的可选 feature 里，作为 agent 示例的一部分；`Loader::services()` 是核心 API。若以后要作为产品 API，再移到独立 crate。

**实现约束。** 这些是第一版就定死的：

- 同步方法放到后台阻塞线程执行；异步结果用现有的等待辅助；取消只停止等待，已经开始的同步调用可能在后台跑完，不重试。
- 从 agent 工具发起调用没有“调用方插件”身份；需要调用方上下文的方法返回明确错误，不假装能调。
- 参数和结果只支持普通数据；日期、二进制、对象引用等特殊值返回明确错误，不把无法使用的句柄交给 agent。
- 每次调用重新查一次服务；服务被替换或撤下时，下一次调用看到新实现或明确错误，不缓存旧对象。

### 3.3 权限与隔离决定

- **插件（包括 agent 写的）默认可信，与手写插件同一信任级别。** rutis 不提供沙箱；需要更强隔离时，把插件放到另一个进程、另一个用户或容器里，或只通过 remote 连接。这与质量现状的信任假设一致。
- `ServiceToolsPlugin` 只暴露宿主已经能看到的服务；它不绕过行的 `isolate` / `inject`，也不改变服务的可见范围。
- 默认只暴露**由插件行提供的服务**；宿主自带服务、远端服务与实例内服务需要显式 `allow([...])`。这样 agent 能立刻使用自己刚写的插件服务，但不会默认拿到宿主能力。列表之外的服务不出现在 `rutis_services`，`rutis_call` 返回“未允许”。
- allowlist 只管 agent 工具的可见面，不改变插件本身的信任级别：插件仍默认可信、无沙箱。agent 写的插件若要用宿主服务，宿主必须先把它加进 allow。
- 远端连接沿用现有 TLS、身份与接入点规则；agent 工具不新增远程入口。
- agent 的文件、命令权限由它现有的 bash / 文件工具决定，不由 rutis 增加或削弱。

### 3.4 场景与断言

用脚本化模型，固定六轮：

1. 第 1 轮：agent 用文件工具在临时项目里写一个 Python 插件，提供 `weather` 服务。
2. 宿主加载插件；`rutis_services` 出现 `weather`，`rutis_call` 的 `today("Oslo")` 返回旧值。
3. 第 2 轮：agent 把插件改坏（语法错误）。宿主报告重载失败，旧版本继续服务；第 3 轮的 `rutis_services` 仍是旧方法，`rutis_call` 仍返回旧值。
4. 第 4 轮：agent 修好并改变行为（返回新值）。宿主重载；第 5 轮的 `rutis_call` 返回新值。
5. 第 6 轮：agent 删除插件。`rutis_services` 不再有 `weather`，`rutis_call` 返回明确错误；没有其他行时 Python 运行时退出。

断言：

- 每一轮的工具列表（`rutis_services` 的结果）与调用结果；
- 重载失败期间服务不中断，旧版本继续；
- 修好后行为变化，没有“两份实现同时服务”；
- 删除后服务消失，调用显式失败，不是挂起或旧值；
- 结束后残余检查通过：没有残留的 Python 进程、socket、临时目录。

### 3.5 测试落点

- 新增 fixture 宿主：装配 loader、`ToolsPlugin`、`ServiceToolsPlugin`、`AgentDriverPlugin` 与脚本化模型；用文件工具写插件，用测试用的小轮询（50 ms）把项目变化交给 loader。生产应用用 `rutis-host dev` 或自己的 watcher，fixture 只负责确定性。
- fixture 沿用 `tests/e2e` 的宿主启动与残余检查：设置场景标记，结束后检查 fixture、Python 运行时、socket 与临时目录；不另造一套。失败重载与删除用 loader 的同一套编辑 API，删除行后确认 Python 运行时退出。
- 新增 E2E 场景 `tests/e2e/tests/agent_writes_plugin.rs`，Linux、macOS、Windows 都跑。用测试专用的写文件工具而不是 bash，避免 Windows 上没有 bash。
- 另加 `rutis-agent` 内的单元测试：`rutis_services` 的列表随服务出现/消失；`rutis_call` 对未知服务、未知方法、不可调用、allowlist 拒绝各一条；工具注销后不再出现在列表里。

## 四、资源上限（并列的安全轨道）

agent 写的插件可能一次授予大量引用或发起大量调用。现在只有帧大小和 64 层嵌套上限，没有引用数与调用数上限。这一节与 agent 主线并列，不阻塞第三节；资源上限单独排 PR。

理由：agent 场景用的是受控测试插件，不会故意耗尽资源；资源上限是协议改动，放进主线会拖慢 issue。这个顺序不改变今天的暴露面，单独验收。

设计：

- hello 增加可选 `limits: { refs, calls }`；缺席表示不限制，兼容旧对端。
- 默认每会话 4096 个存活引用、1024 个在途调用；宿主与 SDK 可配置。
- 超限返回结构化错误，只让那次调用失败：不关闭会话、不静默丢弃、不重试。
- 引用按每次授予计数；进来的调用在执行前检查；事件参数或结果里的引用只让这次分发失败。
- Rust 与六种语言实现各一组边界测试（`limit - 1`、`limit`、`limit + 1`），失败后会话继续、计数回到基线。

不做：按字节 / 按插件的配额、准入控制、队列长度。本方案不引入两套资源模型。

## 五、实施顺序

| PR | 内容 | 依赖 |
| --- | --- | --- |
| 1 | 保存队列、保存层恢复、实例编辑全有或全无 3 条 loader 测试（核心承诺带第二验证） | — |
| 2 | Go eager 与 `go add` 2 条测试 | — |
| 3 | 停止截止时间、退出码、行状态输出 3 条宿主测试 | — |
| 4 | `Loader::services()` + 单元测试 | — |
| 5 | `ServiceToolsPlugin` + 工具注销 + 单元测试 | 4 |
| 6 | agent 写插件 E2E fixture 与场景（三平台） | 5 |
| 7 | allowlist、权限文档、登记表 8 条改“已验证” | 1–6 |
| 8 | 资源上限：协议字段与 Rust 侧 | — |
| 9 | 资源上限：Node、Python、Go、Bun | 8 |
| 10 | 资源上限：Swift、Kotlin 与跨版本 | 8、9 |

1–7 是 issue 主线；8–10 是并列轨道，不阻塞 1–7。每个 PR 只做一项；测试与文档随实现合入。PR 描述写明关闭哪条行为、在哪个平台跑过。

PR 1–3 先确认失败注入点：永久冲突存储、单副本失败、清理不返回。如果现有 API 没有注入口，先加测试用的公开接口，再写测试；不要靠窥探内部状态。PR 1 还要先确认实例编辑是否真的是事务性的；若不是，把产品改动拆出来，不混在“补测试”里。PR 3 的截止时间测试用可控时钟或宽裕余量，避免真实长等待导致偶发失败。

## 六、完成标准

- [ ] 8 条登记尾项全部“已验证”，或明确降级并带锁定测试；核心承诺那条有第二验证；
- [ ] 总 issue 勾掉三项已完成的事件工作，关掉剩余勾选；
- [ ] `rutis_services` 与 `rutis_call` 随插件加载、重载失败、修复、删除正确变化；
- [ ] 重载失败期间旧版本继续服务；删除后调用显式失败；残余检查通过；
- [ ] 默认只暴露插件行提供的服务；宿主、远端、实例内服务需要显式 allow；这条决定写进文档并有测试；
- [ ] 同步 / 异步调用、取消、没有调用方身份、普通数据限制都有测试，错误对模型可见；
- [ ] 三平台 E2E 通过；Windows 用写文件工具，不依赖 bash。

资源上限是并列轨道，单独验收，不作为这个 issue 的完成条件。

## 七、风险与处理

| 风险 | 处理 |
| --- | --- |
| `Loader::services()` 看不到只在 Rust 类型系统里的宿主服务 | 列出来但 `callable = false`；文档写明需要按名字对外（`HostDispatch` / export）才能被 agent 调用 |
| `Loader::services()` 的名单来源不一致 | 以当前行与宿主目录为准；`callable` 只看 `HostDispatch` 是否可达；不一致时列出来标 `false`，并加清单测试 |
| 实例编辑其实不是真事务 | PR 1 先确认；若不是，把产品改动拆出来，不当作“补测试” |
| 失败注入没有现成钩子 | PR 1–3 先加测试用的公开接口（永久冲突存储、单副本失败、清理不返回），不窥探内部状态 |
| 截止时间测试靠真实等待，容易偶发失败 | 用可控时钟或宽裕余量，不睡真实长秒数 |
| `go add` 的 stub 只验证宿主传参 | 另加本地文件 GOPROXY 的真安装测试；通过前保持“部分验证” |
| 工具列表随热替换变化，脚本化测试难稳定 | `rutis_services` 是数据，不是工具；工具 schema 固定，脚本按服务名断言 |
| 同步方法把 agent 卡住 | 放到后台阻塞线程；取消只停止等待，已经开始的调用可能在后台跑完，不重试 |
| agent 没有调用方插件身份 | 需要调用方上下文的方法明确拒绝，错误对模型可见，文档写明限制 |
| 特殊值或引用交给 agent 后用不了 | 第一版只支持普通数据；日期、二进制、引用明确报错，不返回句柄 |
| 默认把宿主能力全给 agent | 默认只暴露插件行提供的服务；宿主、远端、实例内服务需要显式 allow |
| 重载失败时 loader 的行为与 `rutis-host dev` 不完全一样 | fixture 用 loader 的同一套编辑 API；生产 watcher 的行为在另一条 E2E 里已有覆盖，agent 场景不假装覆盖它 |
| 资源上限默认值误伤正常插件 | 先用现有事件矩阵与浸泡数据复核；只能上调；提供配置项 |
| 把权限做成半个沙箱 | 明确不做沙箱；只做可见范围与 allowlist，不新增能力模型 |

## 八、不在本方案

- 完整沙箱、能力权限、按插件的文件 / 网络限制；
- 为每个服务方法生成 JSON Schema 或独立工具；
- 实例内服务、远端服务的 agent 工具（第一版只做宿主可见的全局服务）；
- `cargo xtask dev`、dsh 发布分发；
- actionlint、许可证、安装冒烟、Swift / Kotlin 扩展等其它质量缺口；
- 历史设计文档的批量回填。
