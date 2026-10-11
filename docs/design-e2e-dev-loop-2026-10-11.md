# S2 端到端：插件作者的开发循环（设计稿）

[English](design-e2e-dev-loop-2026-10-11.en.md)

状态：设计，待评审。日期：2026-10-11。基准：`main` `0941ef4`。
关联：[#186](https://github.com/arcships/rutis/issues/186)（[#183](https://github.com/arcships/rutis/issues/183) 第一步）。依据：[质量规范](quality-standard.md)（下称"规范"）Q6.6、Q7；[质量执行现状](quality-status.md)（下称"现状"）场景 A、B，§5，§9.1；E2E 框架 [#221](https://github.com/arcships/rutis/pull/221)（`tests/e2e/`）；[rutis-host 指南](guide/rutis-host.md)；[CI](ci.md)。

范围外：从 npm / PyPI 安装（S9 [#193](https://github.com/arcships/rutis/issues/193)）；跨语言崩溃（S3 #187）；长时间运行（S8 #192）；`rutis-host` 本身的行为修改。

## 一、结论

| # | 结论 | 节 |
| --- | --- | --- |
| C1 | 一个测试文件 `tests/e2e/tests/dev_loop.rs`，每种语言一个测试：`node`、`python`、`bun`、`go`；每个测试按同一串步骤走完 `new → 模板测试 → check → dev → 改 → 改坏 → 修好 → Ctrl-C → check（缺依赖）→ run → Ctrl-C → kill 后重启` | 三 |
| C2 | 插件的 v2、v3 和改坏版本由场景整份写入，v1 就是 `new` 生成的模板；v2、v3 在 apply 和清理时各打印一行，用来断言"旧实例卸载了且恰好一次" | 3.1 |
| C3 | 第 2 步用仓库里的包（链接 `node/rutis`、`bun/rutis-bun`、tsx；`RUTIS_PYTHON_PATH`；Go 的 `replace`），不连注册表；从注册表安装由 #193 负责 | 3.3、十四 |
| C4 | 断言分两类：退出码和 rutis 自己写的输出行用 golden；第三方工具的错误文字（tsx、Python、go 编译器）只断言"含文件名和行号" | 五 |
| C5 | 同步点全部是输出行或进程退出；改文件用"同目录点文件 + rename"并保证 mtime 变化；不用 sleep | 七 |
| C6 | Linux、macOS 先做；Windows 在 #232（Job Object）之后加，Ctrl-C 用一个附着到宿主控制台的小程序发出 | 八 |
| C7 | 新开一个并行的 Linux `e2e` 任务；macOS 放进 `network-macos`；A5 的循环版放进 nightly | 九 |

## 二、现状（核对过的代码）

**已有的测试**

| 测试 | 测了什么 | 没测什么 |
| --- | --- | --- |
| `crates/rutis-host/src/new.rs` `projects_are_created_with_their_names_filled_in` | 生成的文件内容 | 生成的项目能不能安装、测试、运行 |
| `crates/rutis-host/tests/project.rs` | `project::dev_config` 对各种项目的解析（库调用） | 二进制、重新加载 |
| `crates/rutis-host/tests/check.rs` | `check` 的 stdout / stderr / 退出码，逐字（手写的配置） | 模板生成的项目 |
| `crates/rutis-host/tests/signals.rs`（仅 Unix） | `run` / `dev` 在 SIGINT、SIGTERM、SIGHUP、超过截止时间、第二次 Ctrl-C、启动时被忽略的信号下的退出码与清理次数 | Windows；改文件后的重新加载；残余检查 |
| `tests/e2e/tests/cross_language.rs` | `run` 下跨语言调用；`kill` 后残余 | `new` / `dev` / `check` |

**`dev` 的行为**（`crates/rutis-host/src/main.rs` `dev`，`project.rs`）

- 启动后先取一次文件快照（`project::sources`），再打印 `rutis-host dev: running <id>; changes reload it (Ctrl-C ends)`。所以这一行之后的修改一定会被看到。
- 每 400 ms 重新取快照，比较 `(路径, mtime)` 列表。跳过 `node_modules`、`.git`、`.venv`、`venv`、`target`、`dist`、`build`、`__pycache__`、`.pytest_cache`，以及 `.` 开头的名字（`.env` 除外）。
- 有变化时（非 Go 项目）：`host.invalidate()`，然后对**所有**非 peer 行逐个 `loader.reload`，每行打印 `<id>: reloaded` 或 `<id>: cannot reload: <错误>`。`rutis.dev.json` 里加的行（包括场景的 probe）也会重新加载。
- `Loader::reload`（`crates/rutis-loader/src/loader/api.rs`）是全有或全无：新模块解析失败时，旧模块继续运行。
- Go 项目：重新 `go build` 到 `.rutis/go/<名字>-<n>`，成功时换二进制、重启运行时，打印 `<运行时>: rebuilt and restarted`，删掉上一个二进制；失败时打印 `<名字>: the build failed; the last build keeps running` 和编译器输出。Go 项目的其他行不重新加载。
- 状态行（`status::follow`，每 200 ms 比较一次）只在状态变化时打印，中间状态可能被跳过。场景不断言它们的顺序，只等稳定状态（`<id>: running`）。

**`check`**：在没有 `rutis.json` 的插件项目里检查项目本身（`dev_config`）；有不能运行的行时 stdout 写原因，stderr 写 `rutis-host: <n> row(s) or binaries cannot run`，退出码 1。Bun 运行时额外打印 `runtime bun: @arcships/rutis-bun <版本>, bun <版本>`（`runtime_line`）。`check` 只解析行，**不检查 `inject` 的服务有没有行提供**（见十四第 6 项）。

**退出码**（指南"停止与退出码"）：0 正常结束（含信号后清理完成），1 不能启动或运行、`check` 有不能运行的行，2 停止未完成。

## 三、场景

### 3.1 公共做法

- 场景目录 `<root>/<场景>-<pid>-<n>/project/`；`rutis-host new demo --lang <语言>` 在这里执行，生成 `project/demo/`。之后的命令在 `demo/` 里执行（用户的 `cd demo`）。
- 插件的四个版本：

| 版本 | 来源 | `greeter.hello("Ada")` | 打印 |
| --- | --- | --- | --- |
| v1 | `new` 生成的模板，不改 | `Hello, Ada!` | 无 |
| v2 | 场景整份写入 | `Hello, Ada! (v2)` | apply 时 `demo v2 applied <pid> <解释器>`，清理时 `demo v2 cleanup` |
| 改坏 | v2 在固定的第 5 行加一处语法错误（Go：编译错误） | — | — |
| v3 | 场景整份写入 | `Hello, Ada! (v3)` | `demo v3 applied …`、`demo v3 cleanup` |

  v2、v3 用 CRLF 换行写入（所有平台），覆盖"Windows 编辑器保存的文件"（A8）。整份写入而不是在模板上替换字符串：模板改了措辞不会让场景失效，模板本身由 v1 那几步测试。

- probe（`tests/e2e/probes/`）作为 `rutis.dev.json` 的额外行加入，在第 3 步之后、第 4 步之前写入，所以第 3 步检查的是原样的模板项目。

| 项目 | probe | 放在 | `rutis.dev.json` 加的 runtimes |
| --- | --- | --- | --- |
| node | TS（同一个 Node 运行时） | `demo/probe.ts` | 无 |
| python | Python（同一个 Python 运行时） | `demo/src/probe.py`（Python 运行时的项目是 `src/`，见 `project::python_row`） | 无 |
| bun | Python | `demo/dev/probe.py` | `"py": { "project": "dev" }` |
| go | Python | `demo/dev/probe.py` | `"py": { "project": "dev" }` |

  probe 文件在项目里，会进 `sources()` 的快照，但它不变，不触发重新加载。

### 3.2 步骤

"等"一律指等到某一行输出或进程退出（harness 的 `Host::expect` / `wait_for` / `wait_exit`），有防挂死超时。

| # | 动作 | harness 怎么做 | 断言 |
| --- | --- | --- | --- |
| 1 | `rutis-host new demo --lang <语言>` | `Scenario::host` 于 `project/`，`wait_exit` | 退出码 0；stdout 与 golden `new.txt` 一致（`created demo/`、`next: …`）；生成的文件列表与 golden `files.txt` 一致 |
| 2 | 模板自带的测试 | 先按 3.3 准备依赖，再 `Scenario::program` 运行模板里的命令 | 退出码 0 |
| 3 | `rutis-host check`（无参数） | `host_in("demo", ["check"])` | 退出码 0；stdout golden `check-project.txt`（行名、`inject`、`provides`、`config` Schema；Bun 有 runtime 行；Go 有 `go binaries:` 段）；stderr 为空 |
| 4 | `rutis-host dev` | 写 probe 和 `rutis.dev.json`；`host_in("demo", ["dev"])`；等 `rutis-host dev: running demo; …`（golden 行）和 probe `started` | `hello("Ada")` = `Hello, Ada!` |
| 5 | 改成 v2 | `Scenario::replace`；等 `demo: reloaded`、`<probe>: reloaded`、probe `started`、`demo v2 applied` | 结果 = `… (v2)`；Python：`applied` 行里的解释器在 `demo/.venv` 下（A8） |
| 6 | 改坏 | `replace`；等以 `demo: cannot reload: ` 开头的行、probe 重新 `started` | 错误含文件名（`index.ts` / `__init__.py` / `plugin.go`）和行号 5（A7）；结果仍 = `… (v2)`；到此为止 `demo v2 cleanup` 出现 0 次，`demo v2 applied` 1 次（旧实例没被卸载，A4） |
| 7 | 修好（v3） | `replace`；等 `demo: reloaded`、`demo v3 applied`、probe `started` | 结果 = `… (v3)`；`demo v2 cleanup` 恰好 1 次（A3） |
| 7a | dev 里 Ctrl-C | `Host::ctrl_c`；`wait_exit` | 退出码 0；stderr 行 `rutis-host: SIGINT: stopping; cleanups have 10s (again to exit at once)`（Windows：`Ctrl-C`）；`demo v3 cleanup` 恰好 1 次；probe `stopped`（B2） |
| 8 | 写一个缺依赖的 `rutis.json`，`check` | `rutis.json`：`demo` 行（各语言的写法见 3.3）+ `{ "id": "llm", "name": "fake-llm" }`；`host_in("demo", ["check", <绝对路径>])` | 退出码 1；stdout golden `check-missing.txt`：`demo (…): ok …`，`llm (fake-llm): no plugin named "fake-llm"` 和安装提示；stderr = `rutis-host: 1 row(s) or binaries cannot run` |
| 9 | `rutis-host run`，Ctrl-C | `rutis.json` 去掉 `llm`、加 probe 行；`host_in("demo", ["run", <绝对路径>])`；等 `demo: running`、probe `started`；调用；`ctrl_c` | 结果 = `… (v3)`；退出码 0；这个宿主的输出里 `demo v3 cleanup` 恰好 1 次；probe `stopped`（B2） |
| 10 | kill 后立即重启 | 再 `run`；等 `demo: running`；`Host::kill`（只杀宿主）；`wait_exit`；立刻第三次 `run`；等 `demo: running`、probe `started`；调用；`ctrl_c` | 第三次能启动、调用成功、退出码 0（B10）；Unix：被 kill 的那个宿主的输出里最终出现 `demo v3 cleanup` 1 次（指南：运行时发现通道断开后自己卸载各行，B3） |
| — | 结束 | `Scenario::finish()` | 残余检查全部通过（六） |

第 8、9、10 步传 `rutis.json` 的绝对路径，原因是 #226（相对路径的 `rutis.json` 让 `./` 行无法解析）；代码注释指向 #226。#226 修好后改成指南里的写法（无参数），见十四第 5 项。

### 3.3 各语言的差异

| | node | python | bun | go |
| --- | --- | --- | --- | --- |
| 第 2 步准备 | 链接 `node_modules/@arcships/rutis` → `node/rutis`，`node_modules/tsx` → `node/rutis-runtime/node_modules/tsx` | `python -m venv --without-pip .venv`（不装包；`rutis` 来自 `RUTIS_PYTHON_PATH`） | 链接 `@arcships/rutis`、`@arcships/rutis-bun` → `bun/rutis-bun` | `go.mod` 追加 `replace github.com/arcships/rutis/go/rutis => <repo>/go/rutis`（同 `project.rs` 的 `a_go_project_is_built_into_rows`） |
| 第 2 步命令 | `npm test`（Windows：`npm.cmd`），即模板的 `node --import tsx --test test/*.test.ts` | `.venv` 的 python `-m unittest discover -s tests`，`PYTHONPATH=src` + `python/rutis` | `bun test` | `go test ./...`，`GOTOOLCHAIN=local`、`GOPROXY=off`（不下载） |
| 运行时来源 | `RUTIS_NODE_RUNTIME`（harness 已设） | `.venv` 解释器 + `RUTIS_PYTHON_PATH` | 链接的 `node_modules/@arcships/rutis-bun`（`host::bun_runtime` 没有环境变量后备） | `dev` 自己构建 |
| 第 5–7 步的输出 | `demo: reloaded` / `demo: cannot reload: …` | 同左 | 同左 | `go-demo: rebuilt and restarted` / `demo: the build failed; the last build keeps running` + 编译器输出 |
| 第 6 步 | tsx 报错 | `SyntaxError`（含 `__init__.py`、`line 5`） | Bun 报错 | 编译错误 `plugin.go:5:…`；旧进程继续服务 |
| 第 7 步额外断言 | — | — | — | 旧运行时进程已退出（pid 不在进程表）；`.rutis/go/` 里只剩最新的二进制 |
| probe 在第 5–7 步 | 每次重新加载 | 每次重新加载 | 每次重新加载 | 不重新加载；Go 运行时重启时随 `greeter` 停下再启动，等它的 `stopped`、`started` |
| 第 8 步 `demo` 行 | `./src/index.ts`，`runtimes.node = {}` | `py:demo`，`runtimes.py = { "project": "src", "python": <.venv 的解释器> }` | `bun:./src/index.ts`，`runtimes.bun = {}` | 先 `go build -o plugins/demo ./cmd/demo`（指南的部署方式），`go:demo`，`runtimes.go = { "dir": "plugins" }` |
| 不做的 | — | `uv sync`（需要注册表，#193） | `bun run check`（tsc 需要 `@types/bun`，仓库里没有）；`bunx --bun @arcships/rutis-host`（需要发布的包）——都归 #193 | — |

Bun 运行时自己也比较入口文件的 mtime 和大小（Bun 设计 §3.8），所以 `replace` 保证 mtime 变化对 Bun 同样必要。

## 四、harness 要补的部分

都在 `tests/e2e/src/`，不依赖任何 rutis crate（Q7.6）。

| 新增 | 文件 | 作用 |
| --- | --- | --- |
| `Scenario::host_in(dir, args)` | `lib.rs` | 在项目的子目录里启动宿主（`cd demo`）；环境与 `host_with` 相同 |
| `Scenario::program(dir, program, args, env) -> Host` | `lib.rs` | 运行 npm / python / bun / go：同样的临时目录和凭据隔离、输出捕获、进程组、残余登记；用 `Host::wait_exit` 取退出码 |
| `Scenario::replace(relative, contents)` | `lib.rs` | 写到同目录的 `.<名字>.part`（`sources()` 跳过点文件），再 `rename`；如果新 mtime 与旧的相同，`File::set_modified(旧 + 1 s)` |
| `Scenario::probe_in(dir, id, lang, inject)` | `lib.rs` | probe 文件放在子目录；`Probe::row` 的 `name` 相对于 `rutis.dev.json` 所在目录 |
| `Scenario::link(project, package, target)` | `lib.rs` | `link_node_sdk` 的通用版：链接任意包到任意项目（Unix 符号链接，Windows 目录联接） |
| `Host::count(text)` | `host.rs` | 到目前为止含 `text` 的行数；"恰好一次"在宿主退出（输出结束）后再数 |
| `Host::ctrl_c()` | `host.rs` | Unix：`killpg(SIGINT)`（终端的做法）；Windows：见八 |
| `Host::ctrl_break()` | `host.rs`（仅 Windows） | `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 宿主 pid)` |
| `golden::check(name, text)`、`golden::normalize` | `golden.rs`（新） | 比对 `tests/e2e/golden/dev_loop/<语言>/<名字>.txt`；`RUTIS_E2E_BLESS=1` 时改写文件（Q7.7） |
| `rutis-e2e-console` | `src/bin/`（新，仅 Windows 有内容） | 见八 |

`windows-sys`（0.61，workspace 里已经由 `rutis-bridge` 使用）只作为 Windows 依赖加入，不新增 crate。

## 五、golden 输出

**锁定的**（rutis 自己的输出、写进文档的行为，Q3.2、Q5.6.3、Q6.6.6）：

| 输出 | 来源 |
| --- | --- |
| `new` 的 stdout、生成的文件列表 | `main.rs` `new_project`、`new.rs` |
| `check` 的 stdout、stderr（第 3、8 步） | `main.rs` `check` |
| `rutis-host dev: running demo; changes reload it (Ctrl-C ends)` | `main.rs` `dev` |
| `demo: reloaded`、`demo: cannot reload: ` 前缀、`go-demo: rebuilt and restarted`、`demo: the build failed; the last build keeps running` | `main.rs` `dev` |
| `rutis-host: SIGINT: stopping; cleanups have 10s (again to exit at once)` | `stop.rs` `stop` |
| 每一步的退出码 | 指南"停止与退出码" |

**不锁定的**：tsx、Bun、Python、go 编译器的错误文字（只断言含文件名和行号，A7）；状态行的顺序（Q3.3，实现细节）；`npm test` 等工具的输出。

**归一化**（`golden::normalize`）：场景目录 → `<dir>`（同时处理 `file://` URL 和 Windows 的 `\`）；`rutis-host` 版本 → `<version>`；Bun、Go 工具链版本和平台 → `<bun>`、`<go>`、`<platform>`。

**保存方式**：文件，`RUTIS_E2E_BLESS=1 cargo test -p rutis-e2e --test dev_loop` 重新生成（Q7.7）。`crates/rutis-host/tests/check.rs` 把期望写在代码里，没有再生命令；这里用文件，是因为四种语言各有几份，且需要可再生（十四第 2 项）。

## 六、残余检查

每个测试以 `Scenario::finish()` 结束，沿用 #221 的检查（`tests/e2e/src/residue.rs`）：

| 检查 | Linux | macOS | Windows |
| --- | --- | --- | --- |
| 宿主（含 `program` 启动的 npm、python、go）启动的进程全部退出 | 进程组 + 子进程收割（`PR_SET_CHILD_SUBREAPER`） | `ps` 按进程组 | #232 之前报告为"跳过"（Q7.8）；之后按 Job Object |
| 场景目录下没有 socket 文件 | 有 | 有 | 按 `*.sock` 文件名 |
| 宿主的临时目录为空（tsx 的 `tsx-<uid>` 除外） | 有 | 有 | 有 |
| 输出里没有凭据 | 有（本场景不用凭据，检查照常运行） | 有 | 有 |

场景本身另加的检查：

- 每个宿主退出后，`demo v2 cleanup` / `demo v3 cleanup` 在它的输出里恰好出现应有的次数（清理恰好一次，Q3.1）。
- Go：每次重建后旧运行时进程已退出；`.rutis/go/` 只剩当前的二进制。
- Bun 是否在临时目录留文件，实现时核对；需要例外时像 tsx 一样写明原因。

现状 §5 里需要进程内采样的项（测试进程的 fd、线程，tokio 任务数，内核与运行时内的注册表）不属于黑盒，本场景不做。

## 七、确定性（Q7.1）

| 可能不确定的地方 | 做法 |
| --- | --- |
| 改文件太早，`dev` 的第一次快照还没取 | 等 `rutis-host dev: running …` 再改（这行在快照之后打印） |
| 两次修改的 mtime 相同（文件系统时间粒度），变化被漏掉 | `replace` 检查并把 mtime 推后 1 s |
| 写到一半被 400 ms 轮询看到，加载了半个文件 | 写点文件再 `rename`，替换是原子的 |
| 两次修改落在同一次轮询里 | 每次修改后等到这次重新加载的全部结果行（每一行的 `reloaded` / `cannot reload`）和 probe `started`，再做下一次 |
| probe 在重新加载中，调用丢失 | 调用前等 probe 的 `started` 事件 |
| 状态行的中间状态被跳过 | 只等稳定状态 `<id>: running`，不断言顺序 |
| "某事没有发生"（旧实例没卸载） | 改写为"另一件事已发生（`cannot reload` 行、probe 重新 `started`）而它仍未发生"（Q7.1.1） |
| "恰好一次" | 宿主退出、输出关闭之后再数 |
| 超时 | 只有 harness 的防挂死超时（默认 30 s，`RUTIS_E2E_TIMEOUT`）；第一次 `go build` 在冷缓存的 CI 上可能接近，Go 测试的第 2 步 `go test` 先把缓存热起来 |
| 同一二进制里四个测试并行 | 残余里离开进程组的孤儿归属可能不准（#221 已写明）；失败是真的；要准确归属时 `--test-threads=1` |

不用 sleep。`Host::wait_exit` 和残余检查里的 20 ms / 50 ms 轮询是在等进程表变化（标准库没有带超时的 wait），不是用时间代替事件。

## 八、平台

| 平台 | 语言 | 信号 | 说明 |
| --- | --- | --- | --- |
| Linux | node、python、bun、go | `killpg(SIGINT)` | 全部检查 |
| macOS | node、python、bun、go | `killpg(SIGINT)` | 进程检查用 `ps` |
| Windows | node、python、go | Ctrl-C（主），Ctrl-Break（补充） | 在 #232 之后；bun 跳过：Bun 运行时的 Windows 支持在 B3（Bun 设计 §10），尚未声明支持（Q7.8 写明原因） |

**Windows 怎么发 Ctrl-C**：`GenerateConsoleCtrlEvent` 只能发给和调用者共用控制台的进程，而 `CTRL_C_EVENT` 只能发给整个控制台（进程组 0）。如果宿主和测试进程共用控制台，Ctrl-C 会打到 `cargo test` 自己。做法：

1. 宿主以 `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW` 启动，有自己的（不可见的）控制台；
2. `Host::ctrl_c` 运行 `rutis-e2e-console ctrl-c <宿主 pid>`：它 `FreeConsole`、`AttachConsole(宿主 pid)`、`SetConsoleCtrlHandler(NULL, TRUE)`（自己不响应），再 `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)`。这和用户在宿主的控制台里按 Ctrl-C 相同：运行时进程在各自的进程组里，不响应 Ctrl-C（指南），由宿主卸载各行。
3. `CREATE_NO_WINDOW` 下 `AttachConsole` 不可用时，改用 `CREATE_NEW_CONSOLE`（CI 没有桌面，窗口不可见）。实现时在 `runtimes-windows` 上确认。

Ctrl-Break 只发给宿主的进程组（`GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 宿主 pid)`），断言退出码 0 和清理一次。用户在控制台按 Ctrl-Break 会同时发给运行时进程，指南写明这时清理可能来不及——这种情况不断言（Q3.4，明确不保证）。

**Windows 额外覆盖**（A8）：`.venv\Scripts\python.exe` 被选中（第 5 步的解释器断言）；`file:///C:/…` 与 `\` 路径（归一化后的 golden）；CRLF 源文件；Go 二进制的 `.exe` 名字与旧二进制删除。

## 九、CI 与时间

**预计耗时**（估计，实现时在 CI 上实测并写进 PR 描述）：`cross_language` 场景在 Linux 上约 1 s（#221 实测）。S2 每种语言约 10–15 s（npm test 约 1–2 s；建 venv 约 1 s；`check` 两次、`dev`、`run` 三次，每次启动约 0.5–1 s；三次修改各约 0.5–1 s）；Go 再加 `go test` 和三次构建，约 10–20 s。四个测试并行，Linux 上整个文件约 20–30 s。A5 循环版（200 次重新加载）每种语言约 2–3 分钟，只在 nightly。

**需要的 CI 改动**（`ci.yml` 由 #203 / #204 修改，本 PR 不改，写在 PR 描述里）：

| 改动 | 级别 | 时间 |
| --- | --- | --- |
| 新任务 `e2e`（Linux，`code` 开关）：与 `rust` 相同的最低版本 Node、Bun、Python、Go；`npm --prefix node/rutis-runtime ci`；`cargo test -p rutis-e2e`；失败时上传 `RUTIS_E2E_DIR`。加进 `ci-ok` | PR | 与 `rust` 并行，不在关键路径上：构建 `rutis-host`（读 main 缓存）加场景，预计 3–5 分钟 |
| `rust` 任务的 `cargo test --workspace` 加 `--exclude rutis-e2e` | PR | 关键路径缩短约 E2E 的时长（Q12.7，一项检查只在一处跑） |
| `network-macos`：`cargo test -p rutis-e2e` | PR（`code`） | 约 +1 分钟；不新增 macOS 任务（ci.md §6） |
| `runtimes-windows`：`cargo test -p rutis-e2e`（#232 之后） | PR（`code`） | 约 +1–2 分钟；不新增 Windows 任务 |
| `stress.yml`：`cargo test -p rutis-e2e --test dev_loop -- --ignored`（A5） | 每日 | 约 10 分钟 |

普通代码 PR 仍在 10 分钟内：E2E 在并行任务里，macOS、Windows 只在已有任务里加一步。

## 十、覆盖的风险

| 风险 | 级 | 本场景怎么覆盖 | 余下的 |
| --- | --- | --- | --- |
| A2 模板项目不能安装、测试、运行 | P1 | 第 1–4 步：生成的项目通过自带测试、`check`、`dev`（用仓库里的包） | 从注册表安装：#193 |
| A3 重新加载后旧代码仍在运行 / 旧实例没卸载 | P0 | 第 5、7 步：结果变化；旧实例的清理恰好一次；Go：旧进程退出 | — |
| A4 改坏后 `dev` 崩溃或旧版本停止服务 | P1 | 第 6 步：`cannot reload`、旧版本继续服务且未被卸载、`dev` 继续运行 | 真正与失败的重新加载**同时**进行的调用：probe 本身也被重新加载，做不到（十四第 9 项） |
| A5 反复重新加载数百次后资源增长 | P1 | 第 5–7 步循环 200 次（nightly）：每次清理一次；运行时进程数不变；Linux 上宿主和运行时进程的 fd 数（`/proc/<pid>/fd`）不超过开始时 + 16（与现有浸泡相同的容差） | macOS、Windows 的 fd 采样 |
| A6 `check` 误判 | P1 | 第 3 步（能运行的通过）、第 8 步（缺包的失败），模板项目上 | `inject` 无人提供时 `check` 仍通过：见十四第 6 项 |
| A7 错误信息看不出文件、行 | P1 | 第 6 步：错误含文件名和行号；第 8 步：指出行 id 和名字 | — |
| A8 Windows 上路径、venv、换行 | P1 | Windows 一列（八）；CRLF 源文件在所有平台 | #232 之前 Windows 不跑 |
| B2 信号下不清理、退出码不符 | P0 | 第 7a、9 步：Ctrl-C（Windows Ctrl-C 与 Ctrl-Break），清理一次、退出码 0、probe `stopped`、残余为零 | SIGTERM、SIGHUP、截止时间、第二次 Ctrl-C 已由 `signals.rs` 覆盖（Unix），不重复（Q12.7）；Windows 关闭控制台不做 |
| B3 宿主被 SIGKILL 后运行时成孤儿 | P0 | 第 10 步 + 残余检查 | — |
| B10 重启时上一次的 socket / 端口没释放 | P1 | 第 10 步：kill 后立即重启能起来 | 带 `listen` 的配置（本场景没有监听） |

**明确不覆盖**：A1（#206）；A9、A10（单元测试已有）；A11（Python 只重新导入入口模块，是写明的限制；锁定"确实不提供"可以加，但不在本场景）；A12（actionlint，#204）；B1、注册表安装、`bunx --bun`（#193）；B4（`run` 不监视配置文件，现状 §10 第 2 项）；B5、B6（#205 已做）；`go add`。

## 十一、规范要求的对应（Q9.1.1）

- **定义的行为**：本设计不定义新行为，只验证已写进文档的行为：`dev` 改文件后重新加载、改坏时旧版本继续运行（指南"命令"、各语言指南）——契约行为（Q3.2）；信号后清理恰好一次、结束后无残余——核心承诺（Q3.1）；退出码——契约行为。
- **核心承诺的要求**：每个支持的平台（Linux、macOS 本 PR，Windows 在 #232 之后）；两种独立方式（Q8.3）：loader 的库级重新加载测试、`signals.rs` 与本黑盒场景。故障注入的部分是改坏文件和 SIGKILL。
- **组件类型**：Q6.6 面向用户的入口。Q6.6.1 黑盒（只用二进制和文件，`rutis-e2e` 不依赖 rutis crate）；Q6.6.2 `new` / `check` / `dev` / `run` 的成功路径、常见错误路径（改坏、缺包）、退出码；Q6.6.3 每个平台的终止信号；Q6.6.5 模板项目能测试、运行（干净环境安装在 #193）；Q6.6.6 输出 golden。
- **风险评估**：十；风险编号沿用现状 §4.2，不改现状文档。

## 十二、分阶段

都在本 PR 内，按提交分开；Windows 一阶段如果 #232 还没合入，拆成后续 PR。

| 阶段 | 内容 |
| --- | --- |
| E1 | harness 补充（四，除 Windows 部分）；`golden.rs`；`dev_loop.rs` 的 node、python 两个测试；Linux、macOS 通过 |
| E2 | bun、go 两个测试 |
| E3 | A5 循环版（`#[ignore = "nightly: stress.yml runs it with --ignored (A5)"]`） |
| E4 | Windows：`rutis-e2e-console`、宿主启动标志、Ctrl-C / Ctrl-Break；在 #232 之后 |

## 十三、验收条件

每条都能自动检查：

1. `cargo test -p rutis-e2e --test dev_loop` 在 Linux、macOS 上通过，四个测试都运行（没有 `#[ignore]`，A5 循环版除外）；每个测试以 `finish()` 结束且没有残余。
2. 每一步的退出码与 3.2 一致：`new` 0、模板测试 0、`check` 0、`check`（缺包）1、dev 的 Ctrl-C 0、run 的 Ctrl-C 0。
3. golden 文件与输出一致；`RUTIS_E2E_BLESS=1` 重新生成后 `git diff --exit-code tests/e2e/golden` 为空。
4. 在 `RUTIS_LOCAL_HANDOVER=loopback` 下（Unix）同样通过。
5. 本机连续运行 5 次全部通过（PR 描述里记录）。
6. 场景代码里没有 `sleep`（`grep -n "sleep" tests/e2e/tests/dev_loop.rs` 为空）。
7. A5 循环版用 `--ignored` 运行通过：200 次重新加载，清理计数 = 200，运行时进程数不变，Linux fd 增长 ≤ 16。
8. E4 之后：Windows 上 node、python、go 三个测试通过，残余检查的进程项不再报告"跳过"。

另外在实现 PR 里手动做一次反向验证并记在描述里：临时让 `dev` 在重新加载失败时卸载旧实例、或在 Ctrl-C 时不调用清理，确认场景失败（说明断言真的能发现 A3、A4、B2）。

## 十四、需要维护者决定

| # | 问题 | 选项 | 建议 |
| --- | --- | --- | --- |
| 1 | 第 2 步用仓库里的包还是从注册表安装 | a. 链接仓库里的包（3.3）；b. 真的 `npm install` / `uv sync` | **a**。未发布的提交上，模板依赖的版本在注册表上还不存在；联网安装也不确定。从注册表安装是 #193 的职责 |
| 2 | golden 放哪里 | a. `tests/e2e/golden/` 文件 + `RUTIS_E2E_BLESS=1`；b. 像 `check.rs` 那样写在代码里 | **a**，满足 Q7.7（可由命令再生），四种语言的期望文本也不挤在代码里 |
| 3 | Windows 的信号 | a. Ctrl-C（附着控制台的小程序）为主，Ctrl-Break 发给宿主进程组为补充；b. 只发 Ctrl-Break（issue 原文） | **a**。用户实际按的是 Ctrl-C，指南也让用户用 Ctrl-C；只测 Ctrl-Break 测不到用户的路径 |
| 4 | Windows 什么时候做 | a. 等 #232，E4 拆成后续 PR；b. 先在 Windows 跑、进程检查报告"跳过" | **a**。没有进程检查，B2、B3 在 Windows 上恰好验证不到；b 会让 Windows 看起来已覆盖 |
| 5 | #226 | a. 现在传绝对路径，#226 修好后改成无参数；b. 等 #226 | **a**，注释指向 #226；如果 #226 先合入，直接用无参数 |
| 6 | `check` 不检查 `inject` 有没有行提供（读代码发现，A6 里"不能运行的通过"的一种） | a. 另开 issue，属于行为修改，需要先定输出；b. 在本 PR 里改 | **a**。本场景只断言现在写明的行为（缺包） |
| 7 | CI 放置 | a. 新 Linux `e2e` 任务 + `rust` 排除 rutis-e2e + macOS、Windows 加进已有任务（九）；b. 继续在 `rust` 的 `--workspace` 里跑 | **a**。E2E 会随 S3、S9 等场景变长，不应在关键路径上；ci.md §6 要求新检查放在并行的 Linux 任务 |
| 8 | Bun 的 `bun run check`（tsc）和 `bunx --bun @arcships/rutis-host` | a. 归 #193（需要注册表上的 `@types/bun` 和发布的包）；b. 本 PR 联网安装 | **a**；在 #193 的 issue 里补上这两项 |
| 9 | 第 6 步"期间调用不失败"的含义 | a. 失败的重新加载之后，旧版本继续服务且没被卸载；b. 与失败的重新加载**同时**进行的调用不失败 | **a**。`dev` 每次重新加载所有行，probe 也被重新加载，所以 b 需要一个不被重新加载的调用方（例如另一个宿主经 peer 调用），成本高。Go 项目的 probe 不被重新加载，Go 测试在构建失败期间照常调用，算作 b 的一部分 |
