# 持续集成：设计与流程

[English](ci.en.md) · 规范：[质量标准](quality-standard.md) §12 · 测量与改造计划：[质量现状](quality-status.md) §8

本文说明仓库的持续集成**现在怎样运作**，以及**改动 CI 时要遵守的流程**：加一个任务、接入一种语言运行时、修改改动范围与任务的对应。规则的出处是质量标准 Q12；测量数据和后续改造的计划在质量现状 §8，这里不重复。

## 1. 原则

摘自 Q12，是本文所有规则的出发点：

1. **按改动选择**（Q12.3）：合入前只运行受这次改动影响的验证；主线上每次运行全部验证。
2. **取消被取代的运行**（Q12.4）：同一 PR 推送新提交后，旧的运行立即取消；主线上的运行不取消。
3. **一个合入条件**（Q12.5）：汇总检查 `ci-ok` 是分支保护唯一要求的检查。
4. **时间按开发者等待计**（Q12.6）：从推送到结果，包括排队。稀缺的执行机（macOS）每次运行占得越少越好。
5. **不重复**（Q12.7）：同一类检查在同一个层级只在一处运行。
6. **CI 配置是代码**（Q12.10）：修改它的 PR 运行全部验证，并像代码一样评审。

## 2. 层级与 workflow

| 时机 | workflow | 内容 |
| --- | --- | --- |
| PR | `ci.yml` | 按改动选择的任务（§3、§4），`ci-ok` 汇总 |
| 合并到 main | `ci.yml` | 全部任务，不取消 |
| PR 改了 dylib 的 Windows 部分；`v*` tag；手动 | `dylib-windows.yml` | Windows 上的 dylib 插件测试（25–40 分钟） |
| 每晚；手动 | `stress.yml` | 内核测试重复多轮；网络栈浸泡 |
| 每周；手动 | `ci-stats.yml` | CI 自身的度量（§8） |
| `v*` / `cli-v*` tag | `release.yml` / `release-cli.yml` | 发布 |

各层级的时间预算见质量现状 §8.4。

## 3. 按改动选择

`ci.yml` 的第一个任务 `changes` 用 `dorny/paths-filter` 判断改了什么，给出几个输出，后面的任务按输出决定是否运行。推送到 main 时所有输出都为 `true`。

| 输出 | 为 `true` 的条件（PR） | 含义 |
| --- | --- | --- |
| `all` | `Cargo.lock`、根 `Cargo.toml`、`rust-toolchain.toml`、`.github/workflows/**` | 影响范围无法判断（Q12.3.2、Q12.10）：其他输出也都为 `true` |
| `code` | 除 `docs/**` 和 `*.md` 以外的任何文件 | 代码改动 |
| `dylib` | 内核 `crates/rutis/**`，`rutis-cli`、`rutis-dylib*`、`rutis-sdk`、`rutis-dev`、`rutis-xtask`，`tools/*dylib*`、`tools/test-sdk-bundle.sh`、`tools/lib/**`、`tests/dylib-fixtures/**` | dylib SDK 及进入 SDK 的部分 |
| `packaging` | `scripts/train.mjs`、各 `pyproject.toml` / `package.json`、`crates/*/Cargo.toml`、`node/rutis-host/scripts/**` | 发布要打的包 |
| `docs` | `**/*.md` | 文档 |

对应关系写在 `ci.yml` 的 `changes` 任务里，是 CI 配置的一部分，按 §6.3 修改。

## 4. 任务目录

| 任务 | 平台 | 选择条件 | 做什么 |
| --- | --- | --- | --- |
| `changes` | Linux | 总是 | 判断改动范围（§3） |
| `links` | Linux | `docs` | Markdown 相对链接 |
| `test` | Linux | `code` | 全量：`cargo test --workspace`；loader 各种行（含 loopback）；bridge 全 feature；全目标与各 feature 组合的编译检查；Node、Python 包的测试 |
| `network-macos` | macOS | `code` | bridge 全 feature、loader 各种行、rutis-host 在 macOS 上的测试；工作区静态构建。**普通改动唯一的 macOS 任务**（Q12.6.2） |
| `runtimes-windows` | Windows | `code` | loader 各种行、bridge 全 feature、rutis-host 在 Windows 上的测试；工作区静态构建 |
| `runtimes-bun` | Linux、macOS × Bun 1.4.0、最新 | **无（每次都跑，见 §9）** | Bun 运行时的 `bun test`、bridge 与 loader 的 Bun 测试（含 loopback）、rutis-host 的 Bun 测试 |
| `semver-rutis` | Linux | `code` | 公开 API 相对上次发布的变化（只警告） |
| `dylib-linux-launcher` / `-repro` / `-sdk-bundle` | Linux | `dylib` | dylib 的宿主环境与插件替换、构建可重现（单机）、基于预构建 SDK 的外部插件构建，三个并行 |
| `dylib-macos` | macOS | `dylib` | 同上三项加 quarantine 与 hardened host，在一个任务里依次运行 |
| `release-dry-run` / `release-windows` / `release-wheel-aarch64` | Linux / Windows / Linux | `packaging` | 发布产物能构建、能打包 |
| `sdk-repro` / `sdk-repro-macos`（各 ×2）及比对 | Linux / macOS | `all` | 两台机器分别构建 SDK，比对哈希（不能缓存，只在主线和全量时运行，Q12.6.3） |
| `ci-ok` | Linux | 总是 | 汇总（§5） |

## 5. 合入条件：`ci-ok`

- 列在 `ci-ok` 的 `needs` 里的任务，结果为 `success` 或 `skipped` 才算通过；`failure` 或 `cancelled` 让 `ci-ok` 失败。
- `ci-ok` 是分支保护唯一要求的检查。**不在它 `needs` 里的任务，失败也不挡合入。**
- 因此每个新任务都必须同时：有选择条件，并列进 `ci-ok` 的 `needs`（§6.1）。

## 6. 流程

### 6.1 加一个任务

1. **定层级**：它能发现的问题要多快发现（Q12.1）？只在发布时才可能坏的放到主线或 tag；不能用缓存、耗时长、检出率低的不放在合入前的关键路径（Q12.2、Q12.6.3）。
2. **定选择条件**：`needs: changes`，`if: needs.changes.outputs.<输出> == 'true'`。现有输出都不合适时，在 `changes` 里新增一个输出（§6.3），不要省略条件。
3. **列进 `ci-ok` 的 `needs`。**
4. **查重复**（Q12.7）：同一类检查是否已经在别的任务里跑？在同一台机器上能依次完成的，并进已有任务。
5. **稀缺平台**：要占 macOS 时，先看能否并进 `network-macos`；普通改动在 macOS 上最多 1 个任务，改 dylib 时最多 2 个（质量现状 §8.4）。
6. **写进本文 §4 的任务目录。**

### 6.2 接入一种语言运行时

语言运行时（Node、Python、Bun，以及今后的 Go 等）的测试分三类，各放一处：

| 测试 | 放在哪里 |
| --- | --- |
| 运行时自身的单元测试（`bun test`、`npm test`、`unittest`） | Linux：`test`；macOS：`network-macos`；需要多版本时放该语言的版本任务（下一行） |
| Rust 侧的契约、行、宿主测试（`--features <语言>`） | 同上：`test` 与 `network-macos` 的 bridge、loader、host 步骤加上这个 feature；Windows 支持后加进 `runtimes-windows` |
| 最低与最新版本的矩阵 | 一个 `runtimes-<语言>` 任务：**PR 上只在 Linux**，选择条件为该语言的输出（§6.3）；macOS 的最低版本放主线 |

清单：

- [ ] `changes` 新增 `<语言>` 输出：运行时包目录，以及 `crates/rutis-bridge/**`、`crates/rutis-loader/**`、`crates/rutis-host/**`。
- [ ] `test`、`network-macos`（以及支持时的 `runtimes-windows`）安装该语言，并在已有步骤里加上 feature，而不是另起任务。
- [ ] 版本矩阵任务 `runtimes-<语言>`：`needs: changes` 与选择条件；列进 `ci-ok`；PR 上不占 macOS。
- [ ] `release-dry-run` 打它的包；`scripts/train.mjs` 与 `release.yml` 加上它。
- [ ] 本文 §3、§4 更新。

### 6.3 修改改动范围与任务的对应

- 修改 `changes` 里的路径或新增输出，也就是修改 `.github/workflows/**`，所以这个 PR 跑全部任务（`all`）。
- 新增输出时，在 §3 的表里写明它为 `true` 的条件。
- **合入前被跳过、在主线上失败**，说明对应关系漏了这类改动：修复失败之后，补上路径（Q12.3.3）。

### 6.4 修改 CI 配置

- 单独的 PR，类型为 `ci:`；改动的理由写在提交说明或本文里。
- 合入前跑全部验证（`all`）。
- 合入后看主线上的第一次运行；度量（§8）有明显变化时，更新质量现状 §8。

## 7. 失败的处理

- **主线失败**是最高优先级的缺陷（Q12.8）：查明是哪次合入引入的；修复之前，不合入可能受影响的改动。
- **主线上的运行不能被取消后不管**：每次合入都要有完整的结果（Q12.4）。被手动取消的，重新运行。
- **偶发失败**：先复现（本地重复运行，或在 CI 里临时加重复步骤并在合入前删掉）；不能用重试掩盖。确认是测试本身的问题就修测试，是产品的问题就修产品；一时修不了的，开 issue 并在测试里注明。

## 8. 度量

`ci-stats.yml` 每周统计最近的运行：从推送到结果的时间（含排队）、各任务耗时、失败与取消。结果写在该次运行的摘要里；超出质量现状 §8.4 的预算时，按 Q12.2、Q12.6 调整。

## 9. 当前偏差与修正（2026-10-10）

对照 `main`（`45506e8`）：

| 偏差 | 违反 | 修正 |
| --- | --- | --- |
| `runtimes-bun` 没有选择条件，也不在 `ci-ok` 的 `needs` 里：每个 PR（包括只改文档的）都跑，占 2 台 macOS；失败也不挡合入。#200 与 #213 同时合入，Bun 的任务没有接上选择 | Q12.3、Q12.5、Q12.6.2 | 按 §6.2：新增 `bun` 输出；`runtimes-bun` 加条件并列进 `ci-ok`；PR 上只在 Linux |
| Bun 的测试在 macOS 上跑三遍：`network-macos`（bridge 全 feature、host）和 `runtimes-bun` 的两个版本；Linux 上 `test` 与 `runtimes-bun` 也重叠 | Q12.7 | `network-macos` 的 loader 步骤加上 `bun`；macOS 的 Bun 最低版本只在主线运行 |
| main 上 `45506e8` 等 4 次运行被取消，没有完整结果 | Q12.4、Q12.8 | 重新运行 main 的最新提交 |
| 质量现状 §8.4 要求合并到 main 时运行 dylib-windows，`dylib-windows.yml` 没有 `push: main` 触发 | §8.4 | 加上 `push` 到 main 的触发，或在 §8.4 改为只在 tag 时运行（需要决定） |

修正按 §6.4 单独提交。
