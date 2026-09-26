# 协议插件语言扩展路线图（2026-09-26）

> 状态：后续验证方向，尚未实现，不属于基础版本的交付门槛。
> 核心协议、M0–M5 阶段及 T01–T24 见[协议插件设计](design-protocol-plugins-2026-09-25.md)。
> 执行模型为候选方案；每个 runner 实施前需根据核心原型结果复核，不因列入路线图而冻结 API。

## 一、 语言路线图与发布顺序

协议不维护封闭的语言白名单。新增语言主要增加 runner、契约适配/生成物与互通测试，不在 rutis 核心增加语言分支。
下面的先后顺序表示验证依赖与投入顺序，不是日期承诺；全部尚未实现。L1 的脚本原型可在 M2 后开始，
正式发布需通过 M3/M4 的生命周期与取消验收以及相应 OS 后端验收。

| 批次 | 语言/运行环境 | 执行形态 | 主要目标与进入条件 |
| --- | --- | --- | --- |
| L0 基础 | Node.js / TS、Python、Go、Rust | 共享或单成员组；Go 见核心设计 §4.3，Rust 见核心设计 §十二 | 按 M0–M5 验证统一协议、依赖图和组恢复，逐语言验证默认分组 |
| L1 系统自动化 | Shell（先 Bash/Linux） | 共享协议 runner，管理每插件或每次调用的子 shell/命令 | 优先覆盖已有脚本、文件/进程/系统工具；不要求作者手写协议循环 |
| L1 系统自动化 | PowerShell（先 PowerShell 7） | 一个 runner 进程内，每个 activation 有专属 Runspace | 复用 PowerShell 引擎与程序集，保留插件会话；按已通过的 OS 逐个平台发布 |
| L2 macOS 自动化 | AppleScript、JXA | macOS automation runner；初版管理脚本执行子进程 | macOS 传输/回收验证完成，并验证自动化授权和目标应用行为 |
| L3 按需求扩展 | Lua / LuaJIT | Lua runner，在共享进程内管理插件环境；具体 VM 布局单独验证 | 有轻量嵌入/脚本扩展用例后开始 |
| L3 按需求扩展 | Ruby、PHP、Perl | 各自的常驻语言 runner，兼容依赖环境成组 | 有具体生态需求后实现；PHP 按常驻 CLI 生命周期编写 |
| L3 替代运行环境 | Bun、Deno | 分别实现 JS/TS runner，与 Node 分组 | 通过同一语料和生命周期测试后支持，不由“能执行 JS”推定 Node 等价 |

语言 runner 复用同一契约和管理协议，但可以有不同执行模型。共享的最小单位是协议宿主，
是否复用解释器、Runspace 或仅复用脚本进程管理，由语言能力决定；独立进程也不自动等于完整权限沙箱。
基础版本的完成不要求 L1–L3 全部交付，后续每个 runner 独立记录实验/可用状态、运行环境与已通过测试。

## 二、 系统自动化 runner 的设计边界

**统一作者体验**：清单声明方法、配置及返回数据结构；作者实现脚本函数/入口，runner 承担协议收发、
参数传递、schema 校验、错误映射和任务登记。清单指定解释器/语言及显式入口，不从用户提供的方法名拼接可执行源码。
协议端口和结果通道不占用插件普通日志输出；系统命令退出码、脚本错误与协议错误分别保留。

**Shell / Bash**：

- 共享 runner 可运行多个插件，但不把无关脚本全部 source 到同一个 shell；各自的变量、cwd、trap 和退出归属需分开。
  Bash 的子执行环境规则见 [Bash 手册](https://www.gnu.org/software/bash/manual/html_node/Command-Execution-Environment.html)。
- 首轮采用按调用启动脚本的命令式入口；需要持久状态时显式选择每 activation 常驻脚本模式，后者须另验装卸和清理。
  命令执行完成不销毁代理 fiber，后续请求仍能调用同一插件声明的服务。
- 参数用 argv 或约定的 JSON 输入传入，绝不把业务字符串插进 `sh -c` 的脚本内容。
  正常 stdout/stderr 用于日志，结构化结果由 helper 通过专用结果管道/fd 返回；helper 负责 JSON 编码，不能解析人类日志冒充结果。
- 子 shell、pipeline 和后台任务都属于其 activation 的执行记录。取消先请求停止并确认受管进程退出；
  未回收则不得发送任务完成确认。runner 崩溃仍按 runtime 组故障处理。
- 首版只承诺已测试的 Bash/Linux 组合；POSIX sh、其他 shell、其他 OS 分别声明并验收，不混称为全平台 Shell 支持。

**PowerShell**：

- 每个插件 activation 使用专属 Runspace，在同一 runner 进程复用引擎；不把有状态插件请求随机分配到任意池内 Runspace。
  多 Runspace 托管能力见 [PowerShell 托管文档](https://learn.microsoft.com/en-us/powershell/scripting/developer/hosting/creating-multiple-runspaces)。
- Runspace 分开管理会话变量和模块，但程序集、进程环境及部分静态状态仍共享；它不是进程隔离。
  不兼容模块/运行版本或要求独立故障范围时拆 runtime 组。
- 用参数绑定调用明确的命令/函数，不用拼接后的脚本文本承载用户数据。
  输出、错误、警告、信息和原生命令输出由 runner 分别处理，协议连接保持独立。
- 返回值先投影成契约 DTO，再编码；不承诺任意 .NET/系统对象可以无损转成 JSON。
  显式测试 null、空数组、单元素数组、深层对象和错误信息；[ConvertTo-Json](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.utility/convertto-json)的深度规则不能被当成完整对象序列化保证。
- 停止 pipeline 后还要确认关联任务和子进程完成；无法确认时 StopUnconfirmed，不把 Runspace.Dispose 当成任意后台工作的强杀保证。

**AppleScript / JXA**：

- 二者共享 macOS automation runner 的管理/传输层。初版可由 runner 调用脚本执行工具，
  后续再验证常驻 Cocoa/OSA 宿主是否值得引入；不承诺初版共享所有脚本对象或执行上下文。
  Apple 对两种语言的定位见 [Mac 自动化指南](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/)。
- 业务参数作为 handler/run 参数或由宿主包装器提供的结构化数据传入，不把参数插值进 AppleScript/JXA 源码。
  结果由包装器按契约显式转换；应用对象引用/Apple Event descriptor 不是通用 JSON，无法表示时返回明确转换错误。
- 声明依赖的应用标识及自动化能力，验证应用未安装、未运行、用户拒绝授权等路径。
  以实际打包后的 runner/执行链测试 macOS 自动化授权，不以开发终端已授权推定发布后仍可用，见 [Apple 授权说明](https://support.apple.com/en-mz/guide/mac-help/mchl108e1718/mac)。
- 取消会停止等待并尝试终止脚本执行，但已发给目标应用的动作可能继续或已经完成；返回执行结果未知，不自动重放。
  用户自己的目标应用不属于 runtime 进程树，不能为停止脚本而杀 Finder、浏览器或其他用户应用。
- 第一批样例先使用应用提供的脚本接口；依赖 GUI 操作的脚本另行声明和验证所需系统权限、交互会话及应用版本。

## 三、 扩展能力与验收

runner 在 runtime/hello 中声明已实现的调用能力，插件在清单中声明所需能力；具体字段与控制 schema 一起冻结。
初版脚本适配可以只实现 unary，但仍须实现生命周期、契约验证、取消/退出确认与错误路由。
缺少 server_stream 就拒绝流方法；缺少反向服务调用能力就拒绝需要使用 requires 的插件，
不能以“脚本不方便”为由把这些声明忽略。脚本语言不需要生成不存在的静态类型，使用同一 schema 的校验包装与数据 helper。

每个扩展 runner 的发布除适用的 T01–T24 外，还须通过以下验收；L1–L3 不通过的能力明确标 unsupported：

| 编号 | 验收场景 | 必须断言 |
| --- | --- | --- |
| E01 | 两个脚本插件、配置更新及单插件 stop | 代理/参数/状态不串用，声明为独立执行的成员正常清理不结束同组其他成员 |
| E02 | 空格、引号、换行、Unicode 及看似 shell/脚本代码的参数 | 原样作为数据到达，不被执行；结果与日志不会混入协议帧 |
| E03 | 子命令失败、脚本异常、错误返回类型、深层/空/单项数据 | 明确区分业务、执行和契约错误，不把格式化输出当结构化结果 |
| E04 | pipeline、后台任务、取消与退出竞态 | 完成确认晚于受管任务退出；不协作时如实报告，强停仍遵守组边界 |
| E05 | PowerShell 两个专属 Runspace、模块冲突、同一插件连续调用 | 会话归属稳定；共享进程状态的边界明确，冲突者可拆组 |
| E06 | macOS 正式 runner 授权允许/拒绝、目标应用缺失、取消已发出的应用操作 | 原因可读，不自动重放，不关闭用户应用，不宣称操作回滚 |
| E07 | 仅 unary runner 装载流/反向调用插件，换另一 OS 或解释器 | 不支持的能力在装载前拒绝；通过对应环境测试才标可用 |

路线图不会重新引入预设的资源配额。性能与内存对照仍作为测量任务，流控优化、池规模和默认时限待数据支持后独立讨论。
