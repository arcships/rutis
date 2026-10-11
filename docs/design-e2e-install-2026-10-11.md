# S9 安装即用：安装冒烟与文档示例（设计稿）

[English](design-e2e-install-2026-10-11.en.md)

状态：设计稿，待评审。日期：2026-10-11。基准：`main` `0941ef4`。
Issue：#193（[质量跟踪 #183](https://github.com/arcships/rutis/issues/183) 第一步；#195、#196 的验收）。吸收已关闭的 #180 中的安装冒烟、文档示例、E7 与 `xtask dev` 项。
依据：[质量规范](quality-standard.md) Q6.15、Q6.6.5、Q2.13、Q10.2、Q12.1、Q13.1.6、Q13.2；[质量现状](quality-status.md) 风险 B1、A2、I5，控制手段 IN、MX、DOC，§8.4、§9.1；[CI](ci.md)；黑盒框架 `tests/e2e/`（#221）。

范围外：模板项目的测试与 `dev` 循环（S2 #186）；CI 的最低版本测试矩阵本身（#204）；rutis-dsh 的分发（#89）；跨版本相遇（#209）。

## 一、结论

| # | 决定 | 节 |
| --- | --- | --- |
| D1 | 安装冒烟是 `tests/e2e` 里的一组场景，以"已安装"模式运行：不注入仓库的运行时，不 checkout 仓库，只用构建任务上传的产物 | 三、八 |
| D2 | 本次构建的包只能从本地来源取得：npm 用本地注册表（`@arcships/*` 不设上游），PyPI 用本地索引（排在 PyPI 前面），二进制用构建出的归档 | 三 |
| D3 | 每个格子：安装 → 核对是本次构建 → `--version`、`new`、`check`、`run` 最小项目 → 按进程管理器的方式停止 → 卸载 → 残余检查 | 五 |
| D4 | 产物由与 release.yml 相同的构建步骤生成（抽成可复用 workflow），main 和发布共用 | 八 |
| D5 | 时机：main 上 3 个平台（建议 4 个）× npm / PyPI / 二进制；发布前 5 个平台全部作为发布门；发布后用注册表上的包再跑一遍，加 crates.io 与 Go 模块 | 四、九 |
| D6 | 最低版本格子放在 Linux：Node 22、Python 3.10（#196 合入前 3.12）、websockets 15.0；macOS、Windows 用中间和最新版本 | 六 |
| D7 | 文档示例：代码块前加 HTML 注释标记，脚本抽取成项目，用安装好的包运行；没有标记的代码块让检查失败 | 十 |
| D8 | E7 不再允许跳过；`cargo xtask dev` 尚不存在，SDK_ID 预检的测试移到实现它的 issue | 十一 |

## 二、现状：已有的与缺的

| 已有 | 位置 | 做到 | 缺 |
| --- | --- | --- | --- |
| `release-dry-run` | `.github/workflows/ci.yml:525` | 每个 crate 从包构建（`cargo package`）；4 个 npm 包 `npm pack --dry-run`；`uv build python/rutis`；`maturin build` | 不安装、不运行；只在 Linux |
| `release-windows` | `ci.yml:555` | 构建 Windows 二进制并运行 `help`；打 zip；平台包 `npm pack --dry-run`；构建 wheel | 只运行 `help`；不安装 |
| `release-wheel-aarch64` | `ci.yml:591` | 交叉编译 aarch64 wheel | 不安装 |
| 平台包测试 | `node/rutis-host/test/platform.test.mjs` | 平台包的名字、`os`、`cpu`、版本 | npm 实际选中哪个平台包没有验证 |
| 版本一致 | `scripts/train.mjs` | 源码里各包版本一致（Q6.15.4 的源码一侧） | 构建出的产物的版本没有核对 |
| 发布前冒烟 | `docs/release.md` "冒烟" | 两台机器的 WebSocket 冒烟；人工"用发布的包从零走一遍"两篇指南 | 人工，没有记录 |
| 黑盒框架 | `tests/e2e/`（#221） | `Scenario`、`Host`、probe、残余检查 | 总是注入仓库的运行时（`tests/e2e/src/lib.rs:320-322`）；宿主默认由 `cargo build` 得到（`lib.rs:84-112`） |
| E7 | `tools/test-sdk-bundle.sh:398-420` | 工作区的 toolchain pin 与 bundle 不一致时，`pack-plugin` 拒绝 | 机器上没有第二套 toolchain 时打印 "skipped" 并继续（`:406-407`） |
| #180 | 已关闭 | 没有合入代码。安装冒烟、文档示例、E7、`xtask dev` 并入 #193；模板并入 #186 | — |

读代码时发现、与本设计有关的问题：

1. **websockets 下限不一致**：声明是 `websockets>=15`（`python/rutis/pyproject.toml:17`，#218 改的），CI 装的是 `websockets>=13`（`ci.yml:247` 等五处），实际装到最新版；质量现状 §2.2 仍写 ≥ 13。下限 15.0 从未运行过。
2. **`cargo xtask dev` 不存在**：`crates/rutis-xtask/src/main.rs` 只有 `inspect`、`pack-sdk-bundle`、`pack-plugin`；`docs/design-host-dev-mode-2026-09-25.md` 的状态行写明 "`cargo xtask dev` 未做"。#193 第 5 项的 SDK_ID 预检测试无从写起（见十一）。
3. **Python 指南的示例不完整**：`docs/guide/python-plugin.md` §4 用到 `src/fake_llm.py`，但没有给出它的内容，照文档做不能运行。
4. **"PyPI 分发自带 Python 运行时"需要核实**：`docs/guide/rutis-host.md` 写 PyPI 分发自带 `rutis`，"项目里没有自己的运行时时用它们"。宿主找解释器的顺序是 `runtimes.py.python` → `$VIRTUAL_ENV` → `<project>/.venv` → `python3`（`crates/rutis-host/src/host.rs:492-497`）。`uvx rutis-host run` 时能否找到随包安装的 `rutis`，取决于 uvx 是否设置 `VIRTUAL_ENV`。S9 的 uvx 步骤会给出答案。
5. **PyPI 没有 musl Linux 的 wheel**：release.yml 只构建 manylinux wheel，`rutis-host` 也不发布 sdist，在 Alpine 等 musl 系统上 `pip install rutis-host` 会失败。支持声明只写了 "Linux"（`docs/guide/README.md:31`）。
6. **相对路径的 rutis.json（#226）**：文档的写法是在项目目录里执行 `rutis-host run`。S9 按文档的写法运行，所以依赖 #226 的修复；在那之前这一步标为已知失败，指向 #226（Q7.3）。
7. **发布后的验证不能用 `release` 事件触发**：release.yml 用 `GITHUB_TOKEN` 创建 GitHub Release，这样产生的事件不会触发其他 workflow。发布后验证要放在 release.yml 里（`needs: github`），或用 `workflow_run`。

## 三、"干净环境"的定义

### 3.1 所有渠道共同的规则

| # | 规则 | 做法 |
| --- | --- | --- |
| C1 | 不使用仓库源码 | 安装任务不 checkout。它只下载构建任务上传的产物，以及编译好的场景程序（`rutis-e2e` 的测试可执行文件）。场景程序里编译进去的仓库路径在这台机器上不存在，误用仓库的代码会直接失败 |
| C2 | 新的用户目录和缓存 | 每个场景设新的 `HOME`（Windows 另设 `USERPROFILE`、`APPDATA`、`LOCALAPPDATA`），`npm_config_cache`、`UV_CACHE_DIR` 指向场景目录，`PIP_NO_CACHE_DIR=1` |
| C3 | 环境变量白名单 | 用 `env_clear()` 后只加回白名单：`PATH`、`HOME` 等 C2 的变量、`TMPDIR`/`TMP`/`TEMP`、Windows 必需的 `SystemRoot`、`ComSpec`、`PATHEXT`，以及场景自己设置的 registry/index 变量。因此 `RUTIS_*`、`NODE_PATH`、`NODE_OPTIONS`、`PYTHONPATH`、`PYTHONHOME`、`VIRTUAL_ENV`、`CONDA_PREFIX`、`CARGO_*`、`RUSTUP_*` 都不会传进去 |
| C4 | PATH 最小 | 系统目录、任务指定版本的 `node`/`npm`、`python`、`uv`，以及安装渠道自己放进来的目录。不含 `~/.cargo/bin`、仓库的 `target/` |
| C5 | 语言环境版本是断言的一部分 | 场景开始时运行 `node --version`、`python --version`、`uv --version`，与任务要求的版本比对，写进日志 |
| C6 | 本次构建的包只能从本地来源取得 | 见 3.2：注册表上同名同版本的包取不到 |

Linux 的最低版本格子在官方的 slim 容器里运行（`node:22-bookworm-slim`、`python:3.10-slim-bookworm`、`debian:bookworm-slim`），除了镜像本身和下载的产物，机器上没有别的东西。为此 Linux 的场景程序编译为 `x86_64-unknown-linux-musl`（静态链接，任何容器里都能运行）。macOS、Windows 用 GitHub 的标准镜像，靠 C1–C5 隔离。

### 3.2 各渠道的安装来源与"确实是本次构建"

| 渠道 | 安装来源（main、发布门） | 第三方依赖从哪里来 | 怎样保证运行的是本次构建（Q6.15.2） |
| --- | --- | --- | --- |
| npm | 本地 npm 注册表（Verdaccio，固定版本），只在回环地址监听。`@arcships/*` 不设上游，里面只有本次构建发布进去的 tgz；其他包名代理 npmjs | npmjs（经本地注册表代理） | ① `@arcships/*` 没有上游，取不到注册表上的版本；② 安装后 `node_modules/@arcships/rutis-host-<平台>/bin/rutis-host` 的 sha256 等于构建清单里的值；③ `rutis-host --version` 等于列车版本 |
| PyPI | 本地 PEP 503 索引（脚本从 wheel 目录生成，`python -m http.server` 在回环地址提供），用 `--index` 放在 PyPI 前面；uv 默认的 `first-index` 策略下，`rutis`、`rutis-host` 只会从它取。pip 用 `--no-index --find-links` | `rutis`、`rutis-host` 没有第三方依赖；websockets（下限格子）和 hatchling（模板）从 PyPI | ① 同上，first-index / `--no-index`；② venv 里 `rutis-host` 可执行文件的 sha256 等于 wheel 里那一份；③ `rutis.__file__` 在 venv 内，`importlib.metadata.version("rutis")` 等于列车版本 |
| 二进制 | 构建任务上传的 release 归档（`.tar.gz`；Windows `.zip`） | 二进制本身没有；运行 Node 行时 `@arcships/rutis-runtime` 从本地 npm 注册表装进项目，Python 行时 `rutis` 从本地索引装进 venv（与文档"部署"一节相同） | 归档与它的 `.sha256` 一致；解压出的二进制 sha256 等于清单 |
| crates.io | 只在发布后（九）。发布前由已有的 `cargo package`（每个 crate 从包构建）覆盖，因为未发布的依赖 crate 无法从 crates.io 解析 | crates.io | `cargo install --locked rutis-host@<版本>` 后 `--version` |
| Go 模块 | main：`go mod edit -replace` 指向构建任务上传的 `go/rutis` 源码归档，`GOPROXY=off`；发布后：`proxy.golang.org` | 没有（SDK 只用标准库） | 发布后：`go list -m github.com/arcships/rutis/go/rutis@<版本>` 得到该版本 |

构建任务另外生成 `manifest.json`：每个产物的文件名、包名、版本、sha256，以及每个产物里二进制的 sha256。它同时核对所有产物的版本一致（tgz 的 `package.json`、wheel 的 `METADATA`、二进制的 `--version`），这是 Q6.15.4 的产物一侧。

## 四、平台 × 渠道矩阵

发布的目标来自 release.yml：二进制 5 个（`binaries` 矩阵），wheel 5 个（`wheels` 矩阵），npm 平台包与二进制一一对应。

| 平台（npm 名） | 运行机器 | npm | PyPI | 二进制 |
| --- | --- | --- | --- | --- |
| linux-x64 | `ubuntu-24.04` + slim 容器 | **main**：Node 22（最低） | **main**：Python 3.10（最低；#196 前 3.12），`python -m venv` + pip，websockets 15.0 | **main**：`debian:bookworm-slim`，Node 22、Python 3.10 从官方包装入 |
| linux-arm64 | `ubuntu-24.04-arm` | 发布门（建议 main，见十七-4） | 发布门 | 发布门 |
| darwin-arm64 | `macos-15` | **main**：Node 26（最新） | **main**：Python 3.14（最新），uv | **main** |
| darwin-x64 | GitHub 的 Intel macOS 镜像 | 发布门 | 发布门 | 发布门 |
| win32-x64 | `windows-2025` | **main**：Node 24 | **main**：Python 3.12，uv | **main**（`.zip`） |

发布后，5 个平台 × 3 个渠道全部用注册表上的产物再跑一次；另加 crates.io（linux-x64）和 Go 模块（linux-x64）。

各格子取舍的理由：

| 格子 | 状态 | 理由 |
| --- | --- | --- |
| 三个 main 平台 × 三个渠道 | 进 | B1 是 P0（质量现状 §4.2），规范 Q12.1 要求 main 上做干净环境安装。三个平台的打包方式不同（musl 静态二进制、Mach-O、`.exe` + `.zip`），各自可能坏 |
| 版本分布 | — | 三个平台各取一个版本，合起来覆盖 Node 22 / 24 / 26、Python 3.10 / 3.12 / 3.14。最低版本放 Linux，与 ci.md §6 "把 Linux 任务换成最低版本、macOS 保持最新"一致 |
| linux-arm64 | 发布门；建议 main | 与 linux-x64 同一份代码，差别只在交叉编译和 manylinux_2_28 的 wheel，这类问题在构建时就会出现。Linux 机器不稀缺，公开仓库的 arm64 机器免费，放 main 代价小 |
| darwin-x64 | 发布门 | Intel macOS 机器少、排队长，GitHub 正在淘汰 Intel 镜像；与 darwin-arm64 同一份代码。发布前必须通过（Q13.1.6） |
| crates.io | 只在发布后，只 linux-x64 | 发布前未发布的依赖 crate 解析不到；`cargo package` 已经让每个 crate 从包构建。源码包与平台无关，三个平台的编译 CI 每次都做 |
| Go 模块 | main 只 linux-x64 二进制格子；发布后 linux-x64 | Go SDK 只用标准库，与平台无关；平台相关的是 Go 插件二进制，由用户自己构建 |
| Bun（`@arcships/rutis-bun`） | main：linux-x64 npm 格子里加一个 `bun:` 行，Bun 1.4.0 | 走的是 npm 渠道，代价约 10 秒；其他平台的 Bun 列属于 #194（B2、B3 未完成） |
| win32-arm64、32 位、FreeBSD | 不进 | 没有发布，也没有声明支持 |
| musl Linux（Alpine）× PyPI | 不进 | 没有 musllinux wheel，装不上（二-5）。需要先决定支持声明（十七-9） |
| musl Linux × npm / 二进制 | 暂不进 | 二进制是静态 musl，预计能运行；没有声明，以后加一个 `node:22-alpine` 格子代价很小 |

## 五、每个格子走的路径

所有步骤都在场景目录里，用 3.1 的环境执行。"宿主命令"指这个渠道运行 `rutis-host` 的方式：npm 为 `npx rutis-host`，uv 为 `uv run rutis-host`，pip 为 `.venv/bin/rutis-host`，二进制为解压目录里的 `rutis-host`。

### 5.1 公共的最小路径

| 步骤 | 做什么 | 断言 |
| --- | --- | --- |
| 1 安装 | 按渠道（5.2–5.4） | 退出码 0 |
| 2 身份 | 3.2 的核对；`宿主命令 --version` | sha256 与清单一致；输出 `rutis-host <列车版本>` |
| 3 `new` | `宿主命令 new demo --lang node`（npm）或 `--lang python`（PyPI）；装依赖（`npm install` / `uv sync`，本次构建的包从本地来源）；`宿主命令 check` | 文件齐全；依赖装上的是本次构建；`check` 退出码 0，列出行 `demo`。模板的测试、`dev` 由 #186 负责 |
| 4 最小项目 | 场景程序里内置的固定项目（`tests/e2e/fixtures/install/`）：一个 `greeter` 行加一个 probe 行。npm：TS 行 `./greeter.ts`；PyPI：`py:greeter`；二进制：两者都有，外加 linux-x64 上的 Go 行 | `check` 退出码 0；`run`（在项目目录里执行，不带参数，即文档的写法）后 probe 调 `greeter.hello("Ada")` 得到 `"Hello, Ada"`；二进制格子里 TS 调 Python 得到结果 |
| 5 停止 | 像进程管理器那样，只给主进程发 SIGTERM：npm 是 Node 启动器（`node/rutis-host/bin/rutis-host.mjs`），uv 是 `uv` 进程，二进制是 `rutis-host` 本身 | 退出码 0；probe 输出 `stopped`；残余检查通过（七） |
| 6 卸载 | npm：`npm uninstall @arcships/rutis-host`；uv：`uv remove --dev rutis-host`；pip：`pip uninstall -y rutis-host rutis`；二进制：删除解压目录 | 卸载命令退出码 0；七中的卸载检查通过 |

Windows 上第 5 步暂用 `kill`，残余检查中的进程一项按 #232 报告为跳过（框架已有此行为）；Windows 上给 npm 启动器发 Ctrl-C 的方式随 #232 一起做。

### 5.2 npm 格子另加的步骤

| 步骤 | 做什么 | 断言 | 风险 |
| --- | --- | --- | --- |
| npx 直接运行 | 在新的空目录里 `npx --yes @arcships/rutis-host@<版本> --version`（文档的第一种写法） | 输出列车版本 | B1 |
| 平台包选择 | 安装后列出 `node_modules/@arcships/` | 只有本平台的 `rutis-host-<平台>`，没有其他平台的包 | B1 |
| 缺平台包 | `npm install --omit=optional` 后运行宿主命令 | 退出码 1，输出 `no binary for <平台>`（`bin/rutis-host.mjs:15`） | B1、B6 |
| 自带的 Node 运行时 | 最小项目里不装 `@arcships/rutis-runtime` | 行照常运行：运行时来自 `@arcships/rutis-host` 的依赖（`bin/rutis-host.mjs:19-21`） | B1 |
| Bun（只 linux-x64） | 项目里装 `@arcships/rutis-bun`，加一个 `bun:` 行 | 行运行，probe 调用得到结果 | B1 |

### 5.3 PyPI 格子另加的步骤

| 步骤 | 做什么 | 断言 | 风险 |
| --- | --- | --- | --- |
| 项目方式（uv） | `uv init --bare`，`uv add --dev rutis-host==<版本>`，之后都用 `uv run rutis-host`（文档写法） | 同 5.1 | B1 |
| uvx | `uvx rutis-host@<版本> --version`；在一个没有 `.venv` 的 `py:` 项目里 `uvx rutis-host@<版本> run` | 第一条输出列车版本。第二条按文档应使用随包安装的 `rutis`；如果失败，修代码或修文档（二-4），在实现 PR 里决定 | B1、Q2.13 |
| pip（只 linux-x64） | `python -m venv .venv`，`.venv/bin/pip install --no-index --find-links <wheel 目录> rutis-host==<版本>` | 同 5.1 | B1 |
| wheel 内容 | 安装后 `python -c "import rutis.runtime"` 等运行时用到的模块 | 能导入；wheel 缺文件时失败 | B1 |
| websockets 下限（只 linux-x64） | `pip install websockets==15.0`；`python -m rutis listen:ws://127.0.0.1:<端口>/rutis --id gpu --peer main <目录>`（`docs/guide/nodes.md` 的写法，去掉 TLS，回环地址），宿主用节点行连上并运行 `gpu:greeter` | probe 调用得到结果；token 不出现在任何输出里 | MX、P11 |
| 缺运行时包 | 用一个没有装 `rutis` 的解释器（`runtimes.py.python` 指向它） | 退出码 1，输出给出安装命令（`host.rs:511-516`） | B6 |

### 5.4 二进制格子另加的步骤

| 步骤 | 做什么 | 断言 | 风险 |
| --- | --- | --- | --- |
| 归档 | 校验 `.sha256`，解压 | 归档里有 `rutis-host`（`.exe`）、`README.md`、`LICENSE` | B1 |
| 系统依赖 | Linux 上 `ldd rutis-host`（或 `file`） | 静态链接，不依赖 glibc | B1 |
| 缺运行时包 | 项目里没有 `@arcships/rutis-runtime`，运行一个 Node 行 | 退出码 1，输出 `npm install @arcships/rutis-runtime`（`host.rs:448-452`） | B6 |
| Go 行（只 linux-x64） | `rutis-host new weather --lang go`，`go mod edit -replace` 到源码归档，`GOPROXY=off go build`，放进 `runtimes.go.dir` | `check` 列出这个二进制，`run` 后 probe 调用得到结果 | B1 |

## 六、最低版本格子

| 依赖 | 声明 | 格子 | 用的版本 | 说明 |
| --- | --- | --- | --- | --- |
| Node | `engines.node >=22`（三个 npm 包，#219） | linux-x64 npm、二进制 | 声明的最低版本 | Node 的次版本会加功能，所以 Q10.2.1 的"最低"应是次版本级别：`>=22` 就是 22.0.0。用 22 的最新补丁还是 22.0.0，见十七-8 |
| Python | `requires-python >=3.12`；#196 后 `>=3.10` | linux-x64 PyPI、二进制 | 3.12，#196 合入后改 3.10 | 补丁版本不加功能，用该次版本的最新补丁。只改一处版本号 |
| websockets | `websockets>=15`（`python/rutis/pyproject.toml:17`） | linux-x64 PyPI | `==15.0` | 声明的下限（Q10.2.2）。CI 测试任务的 websockets 列由 #204 负责 |
| Bun | `engines.bun >=1.4` | linux-x64 npm | 1.4.0 | 与 `runtimes-bun` 任务相同 |
| Go | `go 1.24`（`go/rutis/go.mod`） | linux-x64 二进制 | 1.24 的最新补丁 | 用于构建 Go 插件 |
| Rust | `rust-version = "1.85"` | 发布后的 crates.io | stable | MSRV 由 #204 负责，不在安装冒烟里重复（Q12.7） |

#195（Node 22，已合入）的安装验收由 linux-x64 的 npm 格子给出；#196（Python 3.10）的安装验收由 linux-x64 的 PyPI 格子在 #196 合入后给出。

## 七、残余检查

沿用 `tests/e2e/src/residue.rs` 的检查（宿主启动的进程都已退出、socket 文件已删除、端口能重新绑定、临时目录已清空、凭据不在输出里），另加三项：

| 检查 | 做法 | 发现什么 |
| --- | --- | --- |
| 包装进程 | npm 的 Node 启动器、`uv run` 的 `uv` 进程也登记为宿主启动的进程，停止后必须退出 | 启动器收到信号后没有转发，或转发了自己不退出 |
| 用户目录 | 场景结束（卸载后）`HOME` 下除了白名单（被重定向的缓存目录）以外没有新文件 | 宿主或安装脚本往用户目录写了东西 |
| 卸载干净 | npm：`node_modules/@arcships/rutis-host*`、`node_modules/.bin/rutis-host*` 不存在；uv / pip：venv 的 `bin`（`Scripts`）里没有 `rutis-host`，`site-packages` 里没有 `rutis_host*`；二进制：解压目录之外没有新文件 | 卸载留下文件 |

每项检查都要有自测，证明它能发现问题（与 #221 的残余检查自测相同）。

## 八、产物从哪里来：构建与运行分开

```text
package（可复用 workflow，release.yml 与 main 共用）
  ├─ build-<目标>：与 release.yml 的 binaries / wheels 相同的步骤
  │    → 归档 + .sha256、wheel、平台包目录
  │    → 场景程序：cargo test -p rutis-e2e --test install --no-run（Linux 用 musl 目标）
  ├─ pack：npm pack 四个包和各平台包；uv build python/rutis；go/rutis 的源码归档
  └─ manifest：manifest.json（每个产物与其中二进制的 sha256），核对所有产物版本一致
         │  actions/upload-artifact
         ▼
install（可复用 workflow；输入：来源 local | registry，版本）
  └─ install-<平台>-<渠道>：不 checkout；下载产物和场景程序；
       启动本地注册表 / 索引（来源为 local 时）；
       RUTIS_E2E_ARTIFACTS=<目录> RUTIS_E2E_CHANNEL=<渠道> <场景程序> --ignored
       失败时上传场景目录（日志、残余报告）
```

- 构建步骤只有一份（Q12.7）：现在 release.yml 的 `binaries`、`wheels` 移进 `package`，release.yml 调用它。main 上 `package` 取代 ci.yml 的 `release-windows`、`release-wheel-aarch64`（这两项只构建、不验证，`package` 构建同样的东西）。`release-dry-run` 留在 PR 的 `packaging` 开关下，作为便宜的打包检查。
- 场景放在 `tests/e2e/tests/install.rs`，标 `#[ignore = "needs this build's packages: run by install.yml with RUTIS_E2E_ARTIFACTS"]`，`cargo test --workspace` 不运行它们，跳过原因写明（Q7.8）。
- 框架需要的改动：`Scenario::installed(name, channel)`：按 3.1 设置环境，不设 `RUTIS_NODE_RUNTIME` / `RUTIS_PYTHON_PATH`；宿主命令由渠道给出（`npx rutis-host`、`uv run rutis-host` 等），可以带前缀参数；一次性命令（`--version`、`check`、`npm install`）的辅助函数，带防挂死超时；七中的新检查。Windows 上 `npx`、`npm` 是 `.cmd`，经 `cmd /C` 调用。
- 本地注册表与索引的脚本放 `tools/install/`：`registry.mjs`（启动固定版本的 Verdaccio，配置写在场景目录，发布 tgz，等待端口就绪）、`simple-index.mjs`（从 wheel 目录生成 PEP 503 目录）。Verdaccio 本身在 3.1 的环境之外安装，场景里的 npm 只和它通信。

## 九、运行时机

| 时机 | 内容 | 时间预算 | 依据 |
| --- | --- | --- | --- |
| PR（所有改动） | 不增加任务。`docs` 开关下，`links` 任务加 `node tools/doc-examples.mjs --check`（几秒，十） | 不变 | ci.md §1-8：普通 PR ≤ 10 分钟 |
| PR（`packaging` 开关） | 建议：linux-x64 的 npm、PyPI 两格（十七-5） | 约 8 分钟，与测试并行，不在关键路径上 | Q12.1 "打包错误合并前就能发现" |
| main | `package`（main 平台的目标）+ `install`（四中标 main 的格子）+ 文档示例 | ≤ 25 分钟（main 预算 60 分钟，质量现状 §8.4） | Q12.1、Q6.15.1 |
| 发布门 | release.yml：`verify` → `package`（全部 5 个目标）→ `install`（来源 local，5 个平台 × 3 个渠道）→ 各 publish 任务 | 在发布前增加约 10 分钟 | Q13.1.6 |
| 发布后 | release.yml 最后一个任务（`needs: github`）：`install`（来源 registry，5 × 3）+ crates.io + Go 模块。先轮询注册表直到能取到该版本（每 15 秒一次，最多 15 分钟，防挂死，不是同步手段）。失败时自动开 issue，标题含版本 | 约 15–20 分钟，每次发布一次 | Q6.15.3、Q13.2 |
| 手动 | `install.yml` 支持 `workflow_dispatch`，输入版本与来源，用于排查 | — | — |

发布后验证失败时，按 Q13.2 由维护者决定撤回或标记（`npm deprecate`、PyPI yank）并发布修正版本；步骤写进 `docs/release.md`。

## 十、文档示例

### 10.1 标记

`docs/guide/*.md` 中每个代码块前一行必须有一个 HTML 注释标记（GitHub 上不显示）：

| 标记 | 含义 |
| --- | --- |
| `<!-- example: <名字> file=<路径> -->` | 把这个代码块写到示例 `<名字>` 的 `<路径>` |
| `<!-- example: <名字> run -->` | 按顺序执行这个 shell 代码块的每一行，`cd` 改变后续命令的目录 |
| `<!-- example: <名字> run until="<文本>" -->` | 长时间运行的命令（`dev`、`run`）：启动，等输出出现 `<文本>`，发 SIGINT，断言退出码 0 |
| `<!-- example: skip reason="<原因>" -->` | 不运行，原因必须写（例如"需要另一台机器"、"片段：rutis.json 里的一行"） |

`npm publish` 发布到本地注册表（Verdaccio 配置里允许示例包名），后面的 `npm install greeter` 从那里装，所以"发布 → 被宿主使用"两节也能运行。`uv publish` 标 skip，原因写明。

Rust 代码块（`rust-host.md`、`cordis.md`）是函数体里的片段：抽取后按 `tests/doc-examples/<名字>/template.rs` 中 `// {{blocks}}` 的位置拼接，编译（不运行）；main 上依赖工作区的 crate（path），发布后依赖 crates.io 的版本。Go 代码块写进模板项目，运行 `go test`、`go build`。

### 10.2 防止漂移

`tools/doc-examples.mjs` 有两种用法：

- `--check`（PR，`docs` 开关，`links` 任务里，几秒）：
  1. 每个代码块都有标记；没有标记就失败，指出文件和行号。
  2. 中文与英文文件的标记序列相同；同一标记下的代码去掉行注释（`//`、`#`）后相同（代码注释按语言翻译，代码本身不能不同）。
  3. 文档里出现的版本范围（`"^0.8.0"`、`rutis>=0.8,<0.9`、`rutis = "0.8"` 等）与 `scripts/train.mjs` 的列车版本一致。
  4. 自测：对一份带未标记代码块的样例文件必须报错。
- `--out <目录>`（main）：抽取出示例项目和步骤文件 `steps.json`；`tests/e2e/tests/doc_examples.rs` 读它，在 3.1 的环境里用安装好的包（linux-x64 的 npm、PyPI 格子同一台机器）执行。

规则：改文档里的代码块，必须同时让它能运行或标 skip 并写原因；新增指南文件自动被检查，不需要登记。第一次标记时发现的不能运行的示例（如二-3），在同一 PR 里修文档。

只检查 `docs/guide/`。`docs/development-handbook.md` 已经作为内核 crate 的 doctest 编译（`crates/rutis/src/lib.rs:57`），迁移指南由 `crates/rutis-loader/tests/migration_example.rs` 一类测试覆盖，不重复。

## 十一、E7 与 `xtask dev`

### 11.1 E7

`pack-plugin` 在编译前比较插件工作区和 bundle 的 `rust-toolchain.toml` 的 channel（`crates/rutis-xtask/src/main.rs:502-508`），这是纯字符串比较，不需要第二套 toolchain 存在。现在的脚本从已安装的 toolchain 里找一个不同的（`tools/test-sdk-bundle.sh:405`），找不到就跳过。两种改法：

| 做法 | 改动 | 代价 | 覆盖 |
| --- | --- | --- | --- |
| A：不存在的 pin | 工作区 pin 写成 `0.0.0-e7`，设 `RUSTUP_AUTO_INSTALL=0`，断言完整的拒绝信息（含两个 pin），且没有出现 rustup 安装或 E0514 | 不下载，确定 | 字符串比较这一分支；如果检查被删，构建会在 rustup 处失败，信息与断言不符，测试失败 |
| B：安装第二套 toolchain | CI 在 dylib 任务里 `rustup toolchain install <pin 的上一个版本> --profile minimal` | 每个 dylib 任务约 30 秒下载 | 同上 |

两种都去掉跳过分支：找不到条件时失败，不再打印 "skipped"。推荐 A（十七-7）。

### 11.2 `xtask dev` 的 SDK_ID 预检

设计（`docs/design-host-dev-mode-2026-09-25.md` §六第 1 条）要求 `cargo xtask dev` 在编译前与宿主握手，SDK_ID 或 toolchain 不一致时立即报错。这个命令还不存在，测试无法写。现有的编译前检查只有 `pack-plugin` 的 pin 比较（E7 覆盖）。建议把这项移出 #193，作为实现 `cargo xtask dev` 的 issue 的验收条件；质量现状里 I5 的现状在第一步结束统一更新时注明（本 PR 不改 `docs/quality-status.md`）。

## 十二、CI 时间与成本

以下是估计，实现后用 `node tools/ci-stats.mjs` 测量并写进 PR。

| 项 | 机器 | 估计 | 次数 |
| --- | --- | --- | --- |
| `build-<目标>`（release 构建 + wheel + 场景程序） | 每个目标一台 | 有缓存 4–8 分钟，没有缓存 10–15 分钟 | main 3–4 个目标；发布 5 个 |
| `pack` + `manifest` | Linux | 2–3 分钟 | 1 |
| `install-<平台>-<渠道>` | 每格一台 | 3–5 分钟（启动注册表、安装、约 10 个场景） | main 9 格（建议 12）；发布门 15；发布后 15 + 2 |
| 文档示例 | Linux | 3–5 分钟 | main 1 |
| `--check` | Linux（`links` 任务内） | < 5 秒 | 改文档的 PR |
| E7 | — | A：0；B：每个 dylib 任务约 30 秒 | 改 dylib 的 PR |

- main：关键路径是 build（≤ 15 分钟）+ install（≤ 5 分钟），约 20 分钟，在 main 的 60 分钟预算内。每次 main 推送增加 macOS 任务：1 个 build + 3 个 install。三个渠道可以在同一台 macOS 上依次运行（每个渠道一个新的场景目录、新的 `HOME`），合成 1 个 install 任务，macOS 任务数从 4 降到 2。Linux、Windows 每格一个任务，便于看清是哪一格失败。
- 去掉 ci.yml 在 main 上的 `release-windows`、`release-wheel-aarch64` 后，Windows 和 Linux 的任务数净增约 4 个。
- PR：默认不增加；`packaging` 开关下（建议）增加 1 个 Linux 任务，与测试并行。
- 发布：增加约 10 分钟发布门；发布后验证约 20 分钟，不阻塞发布。

## 十三、覆盖的风险与不覆盖的

| 风险 / 条款 | 怎样覆盖 | 时机 |
| --- | --- | --- |
| **B1**（P0） | 三渠道 × main 的平台，本次构建的产物；其余平台在发布门；发布后用注册表上的包 | main、发布门、发布后 |
| Q6.15.1 | 安装 → 最小路径 → 卸载，干净环境 | 同上 |
| Q6.15.2 | 本地来源没有上游；sha256 与清单比对 | 同上 |
| Q6.15.3、Q13.2 | 发布后验证 | 发布后 |
| Q6.15.4 | `manifest` 核对所有产物版本一致 | main、发布 |
| Q13.1.6 | 发布门 | 发布前 |
| A2（P1），部分 | 模板 `new` + 用本次构建的包装依赖 + `check`；模板的测试与 `dev` 归 #186 | main |
| B6，部分 | 缺平台包、缺 Node 运行时包、缺 `rutis` 的错误信息 | main |
| B2、B3，部分 | 包装进程（npm 启动器、`uv run`）收到 SIGTERM 后清理并退出 | main（Unix） |
| MX，部分 | 安装场景下的 Node 22、Python 3.10、websockets 15.0、Bun 1.4.0 | main |
| Q2.13、DOC | 文档示例运行；标记、中英文、版本号的检查 | PR（检查）、main（运行） |
| I5，部分 | E7 不再跳过；`xtask dev` 见 11.2 | 改 dylib 的 PR |
| Q6.6.5，部分 | 模板在干净环境中装依赖、`check` | main |

不覆盖：

| 不覆盖 | 原因 |
| --- | --- |
| 浏览器下载的 macOS 二进制被隔离（quarantine）后的提示（`crates/rutis-host/src/main.rs:75-94`） | CI 下载的文件没有隔离属性；可以以后用 `xattr -w com.apple.quarantine` 加一个场景 |
| musl Linux（Alpine） | 未声明支持；PyPI 没有 wheel（十七-9） |
| pnpm、yarn、`bun install` 作为包管理器；全局安装（`npm i -g`、`uv tool install`） | 文档没有写这些方式 |
| 从上一版本升级、多版本并存 | J2、E13，#209 |
| 公司代理、私有镜像、离线安装 | 没有声明 |
| Windows 的进程残余与 Ctrl-C | #232 |
| 真实的跨机器环境 | Q13.1.7，人工（`docs/release.md`） |
| 最小路径之外的功能正确性 | IN 的定义就不覆盖（质量现状 §3） |
| linux-arm64（若不放 main）、darwin-x64 在两次发布之间的打包问题 | 发布门才发现，不是合并时 |

## 十四、新增与修改的文件（实现时）

| 文件 | 内容 | 阶段 |
| --- | --- | --- |
| `tests/e2e/src/install.rs` | 渠道（npm / uv / pip / 二进制）、3.1 的环境、身份核对、一次性命令 | 1 |
| `tests/e2e/src/residue.rs` | 七中的三项检查及自测 | 1 |
| `tests/e2e/tests/install.rs` | 五中的场景（`#[ignore = …]`） | 1 |
| `tests/e2e/fixtures/install/` | 最小项目（编译进场景程序） | 1 |
| `tools/install/registry.mjs`、`simple-index.mjs`、`manifest.mjs` | 本地注册表、索引、产物清单与版本核对 | 1 |
| `tools/test-sdk-bundle.sh` | E7 去掉跳过 | 1 |
| `.github/workflows/package.yml`、`install.yml`；`release.yml` | 可复用的构建与安装；发布门；发布后验证（谁改见十七-3） | 1、2 |
| `docs/release.md`（中英） | 发布门、发布后验证失败时的处理 | 2 |
| `tools/doc-examples.mjs`、`tests/e2e/tests/doc_examples.rs`、`tests/doc-examples/` | 文档示例 | 3 |
| `docs/guide/*.md`（中英） | 标记；修不能运行的示例 | 3 |

## 十五、分阶段计划

| 阶段 | 内容 | 依赖 | 完成时 |
| --- | --- | --- | --- |
| 1 | 框架的"已安装"模式；npm / PyPI / 二进制场景；本地注册表与索引；产物清单；`package` 与 `install` workflow；main 上的格子；E7 | #221（已合入）；`run` 的相对路径一步依赖 #226 | main 上安装冒烟全部通过；#195 的安装验收 |
| 2 | 发布门（5 个平台）；发布后验证（注册表、crates.io、Go 模块）；`docs/release.md` | 阶段 1 | 下一次发布经过发布门，发布后验证通过 |
| 3 | 文档示例：标记全部指南；`--check` 进 PR；示例在 main 上运行；修文档 | 阶段 1（用它的安装环境） | 全部示例通过；未标记的代码块让 PR 失败 |
| — | Python 最低版本格子改 3.10 | #196 | #196 的安装验收 |

阶段 3 对应跟踪 issue #183 的"文档示例运行在第三步"。

## 十六、验收（都可以自动检查）

1. main 上 `install` workflow 的每个格子通过；每格日志里有：语言环境版本、`rutis-host <列车版本>`、sha256 与清单一致的记录。
2. 检查能发现问题（自测，作为场景的一部分运行）：
   - 去掉 `rutis` 包的 wheel → PyPI 格子失败；
   - `cpu` 写错的平台包 → npm 格子在"平台包选择"一步失败；
   - sha256 与清单不同的二进制 → 身份一步失败；
   - 从本地注册表请求 `@arcships/rutis-host@<上一个发布版本>` → 取不到（证明没有上游）。
3. 每格的残余报告为空（Windows 的进程一项按 #232 报告为跳过）。
4. release.yml 的任务图中，每个 publish 任务都 `needs` 全部发布门 install 任务。
5. 发布后验证在发布后自动运行；人为制造一次失败（`workflow_dispatch` 指定不存在的版本）时自动开出 issue。
6. `node tools/doc-examples.mjs --check` 在 main 上通过，对未标记代码块的样例报错；全部带标记的示例在 main 上运行通过。
7. `tools/test-sdk-bundle.sh` 中 E7 没有跳过分支；dylib 任务日志中 E7 执行并通过。
8. 最低版本格子的日志显示 Node 的声明最低版本、Python 3.10.x（#196 后）、websockets 15.0。
9. 普通 PR 的关键路径不变（`tools/ci-stats.mjs` 前后对比）；main 上 `package` + `install` ≤ 25 分钟。

## 十七、需要维护者决定

| # | 问题 | 选项 | 推荐 |
| --- | --- | --- | --- |
| 1 | npm 的本地来源 | A：Verdaccio，`@arcships/*` 无上游；B：直接 `npm install ./*.tgz` | **A**。只有经注册表安装，npm 才按 `optionalDependencies` 选平台包（B1 的主要情况）；文档里的 `npx @arcships/rutis-host …` 能原样运行；注册表里只有本次构建，身份由结构保证 |
| 2 | 安装任务是否 checkout | A：不 checkout，下载编译好的场景程序；B：checkout 后在仓库外的目录运行 | **A**。"不含仓库源码"由结构保证，误用仓库路径会直接失败；代价是多上传一个测试可执行文件 |
| 3 | workflow 由谁改 | A：新文件 `package.yml`、`install.yml` 和 `release.yml` 的改动在本 PR 的实现提交里，`ci.yml` 不动，去掉 `release-windows`、`release-wheel-aarch64` 并入 #203 / #204；B：全部交给 #203 / #204 | **A**。协作规则只限定 `ci.yml`；构建步骤从 release.yml 抽出与安装场景是同一件事，分开改容易不一致 |
| 4 | main 上的平台 | A：linux-x64、darwin-arm64、win32-x64；B：再加 linux-arm64 | **B**。Linux 机器不稀缺，公开仓库的 arm64 机器免费；darwin-x64 只在发布门 |
| 5 | `packaging` 开关的 PR 上跑不跑 | A：不跑，main 上发现；B：跑 linux-x64 的 npm、PyPI 两格 | **B**。改打包文件的 PR 少，这一格约 8 分钟、与测试并行，不拉长关键路径；打包错误在合并前发现 |
| 6 | 发布门 | A：5 个平台安装冒烟不通过就不发布；B：只在 main 上验证 | **A**（Q13.1.6）。增加约 10 分钟，避免发布坏包后撤回 |
| 7 | E7 | A：不存在的 pin + `RUSTUP_AUTO_INSTALL=0` + 断言完整信息；B：安装第二套 toolchain | **A**。覆盖同一个分支，不下载、结果确定；两种都去掉跳过 |
| 8 | Node 最低版本的格子 | A：用 22.0.0（`>=22` 的字面最低）；B：用 22 的最新补丁 | **A**；如果 22.0.0 不能通过，把 `engines` 提到实际通过的最低次版本（Q10.3：不声明没验证过的） |
| 9 | PyPI 在 musl Linux | A：文档写明 PyPI 分发只支持 glibc Linux；B：增加 musllinux wheel | **A**，现在只改支持声明；有用户需要时再做 B |
| 10 | `xtask dev` 的 SDK_ID 预检测试 | A：移出 #193，作为实现 `cargo xtask dev` 的验收；B：#193 里先实现最小的 `xtask dev` | **A**。命令不存在，实现它是功能工作，不属于安装冒烟 |
| 11 | PR 拆分 | A：本 PR 做阶段 1 与 E7，阶段 2、3 各一个 PR，#193 在阶段 3 合入时关闭；B：全部在本 PR | **A**（保持 PR 小）。采用 A 时本 PR 的 `Closes #193` 改为 `Part of #193` |
| 12 | 发布后验证失败的处理 | A：任务失败并自动开 issue，撤回由人决定；B：自动 `npm deprecate` / yank | **A**。撤回不可逆，需要人判断（Q13.2） |
