# 持续集成（CI）

[English](ci.en.md)

本文说明 CI 现在怎么运行，以及修改 CI 时要做什么。规则依据是[质量标准](quality-standard.md) §12，耗时数据在[质量现状](quality-status.md) §8。

## 1. 规则

1. PR 只运行和这次改动有关的任务；main 上每次运行全部任务。
2. PR 推了新提交，旧的那次运行自动取消；main 上的运行不取消。
3. 合并只看一个检查：`ci-ok`。
4. macOS 机器少，一次运行尽量只占一个 macOS 任务。
5. 同一项检查只在一个任务里跑。
6. 改 CI 配置的 PR 运行全部任务。

## 2. 有哪些 workflow

| workflow | 什么时候运行 | 做什么 |
| --- | --- | --- |
| `ci.yml` | 每个 PR；每次推送 main | 测试和检查（下文） |
| `dylib-windows.yml` | PR 改了 dylib 相关文件；打 `v*` tag；手动 | Windows 上的 dylib 测试 |
| `stress.yml` | 每晚；手动 | 重复运行内核测试；网络长时间测试 |
| `ci-stats.yml` | 每周；手动 | 统计 CI 的耗时和失败 |
| `release.yml`、`release-cli.yml` | 打 tag | 发布 |

## 3. PR 上怎么决定跑哪些任务

`ci.yml` 的第一个任务 `changes` 看这个 PR 改了哪些文件，输出下面几个开关。后面的任务根据开关决定跑不跑。推送 main 时所有开关都打开。

| 开关 | 改了这些文件时打开 |
| --- | --- |
| `all` | `Cargo.lock`、根目录的 `Cargo.toml`、`rust-toolchain.toml`、`.github/workflows/**`。打开时其他开关也全部打开 |
| `code` | 除 `docs/**` 和 `*.md` 以外的任何文件 |
| `dylib` | 内核 `crates/rutis`，以及 `rutis-cli`、`rutis-dylib*`、`rutis-sdk`、`rutis-dev`、`rutis-xtask`、`tools/*dylib*`、`tools/test-sdk-bundle.sh`、`tools/lib/**`、`tests/dylib-fixtures/**` |
| `packaging` | `scripts/train.mjs`、各个 `pyproject.toml` 和 `package.json`、`crates/*/Cargo.toml`、`node/rutis-host/scripts/**` |
| `docs` | 任何 `.md` 文件 |

## 4. 每个任务

| 任务 | 机器 | 开关 | 内容 |
| --- | --- | --- | --- |
| `links` | Linux | `docs` | 检查 Markdown 里的相对链接 |
| `test` | Linux | `code` | 全部 Rust 测试和编译检查，Node、Python 包的测试 |
| `network-macos` | macOS | `code` | bridge、loader、rutis-host 在 macOS 上的测试；整个工作区的编译检查 |
| `runtimes-windows` | Windows | `code` | bridge、loader、rutis-host 在 Windows 上的测试；整个工作区的编译检查 |
| `runtimes-bun` | Linux、macOS；Bun 1.4.0 和最新版 | 无，每次都跑（见 §7） | Bun 运行时的全部测试 |
| `semver-rutis` | Linux | `code` | 公开 API 和上次发布相比有没有不兼容的改动（只提示） |
| `dylib-linux-launcher`、`dylib-linux-repro`、`dylib-linux-sdk-bundle` | Linux | `dylib` | dylib 测试，分三个任务同时跑 |
| `dylib-macos` | macOS | `dylib` | 同样的 dylib 测试，在一个任务里依次跑 |
| `release-dry-run`、`release-windows`、`release-wheel-aarch64` | Linux、Windows、Linux | `packaging` | 发布要用的包能构建 |
| `sdk-repro`、`sdk-repro-macos` 及比对 | Linux、macOS | `all` | 在两台机器上分别构建 SDK，比较结果是否相同 |
| `ci-ok` | Linux | 总是 | 汇总（§5） |

## 5. 合并条件

- `ci-ok` 是 main 分支唯一要求通过的检查。
- `ci-ok` 只看列在它 `needs` 里的任务：这些任务成功或被跳过，它就通过；有任务失败或被取消，它就失败。
- 没有列进 `ci-ok` 的任务，失败了也不会阻止合并。

## 6. 修改 CI 时要做的事

### 加一个任务

1. 给它加 `needs: changes` 和 `if: needs.changes.outputs.<开关> == 'true'`。没有合适的开关就在 `changes` 里加一个，并写进 §3。
2. 把它加进 `ci-ok` 的 `needs`。
3. 看已有任务里是不是已经在做同样的检查；能放进已有任务的，不新开任务。
4. 要用 macOS 时，先看能不能放进 `network-macos`。普通改动最多占 1 个 macOS 任务，改 dylib 时最多 2 个。
5. 写进 §4。

### 加一种语言运行时

1. `changes` 加一个这种语言的开关，包括它的运行时目录，以及 `crates/rutis-bridge`、`crates/rutis-loader`、`crates/rutis-host`。
2. 在 `test` 和 `network-macos` 里安装这种语言，在已有的 bridge、loader、host 测试步骤里加上它的 feature。
3. 如果要测多个版本，加一个 `runtimes-<语言>` 任务：按第 1 步的开关运行，加进 `ci-ok`，PR 上只在 Linux 跑；macOS 上的旧版本只在 main 上跑。
4. 发布相关：`release-dry-run`、`scripts/train.mjs`、`release.yml` 都加上它的包。
5. 更新 §3、§4。

### 改开关对应的文件

- 改 `changes` 等于改了 `.github/workflows/**`，这个 PR 会运行全部任务。
- 如果某个任务在 PR 上被跳过，合并后在 main 上失败了，说明开关漏了某些文件：修好失败后，把漏掉的文件加进开关。

### 改 CI 配置

- 单独开 PR，提交类型用 `ci:`。
- 合并后看 main 上第一次运行的结果。

## 7. 失败怎么处理

- main 上失败优先处理：找出是哪次合并引起的，修好之前不再合并可能受影响的改动。
- main 上的运行被手动取消后，要重新运行，每次合并都要有完整结果。
- 偶尔失败的测试：先想办法稳定复现，再修。不用自动重试来掩盖。

## 8. 现在和本文不一致的地方（2026-10-10，main `45506e8`）

| 问题 | 怎么改 |
| --- | --- |
| `runtimes-bun` 没有开关，也不在 `ci-ok` 里。结果是每个 PR（包括只改文档的）都会跑它，占 2 个 macOS 任务；它失败了也不会阻止合并。原因是 #200 和 #213 同时合并，#200 加的这个任务没有接上 #213 的开关 | 按 §6 加 `bun` 开关；`runtimes-bun` 按开关运行，加进 `ci-ok`；PR 上只在 Linux 跑 |
| Bun 的测试在 macOS 上跑了三遍：`network-macos` 一遍，`runtimes-bun` 两个版本各一遍 | `network-macos` 的 loader 测试加上 `bun`；macOS 上的 Bun 1.4.0 只在 main 上跑 |
| main 上最近 4 次运行被取消，`45506e8` 没有完整结果 | 重新运行 main 最新提交 |
| 质量现状 §8.4 写着合并到 main 时要跑 `dylib-windows`，但 `dylib-windows.yml` 推送 main 时不会运行 | 二选一：给它加上推送 main 时运行；或者把 §8.4 改成只在打 tag 时运行 |

这些修改单独开一个 `ci:` PR。
