# CI 静态检查与最低版本矩阵（设计稿）

[English](design-ci-checks-matrix-2026-10-11.en.md)

状态：设计稿，待评审（#204）。日期：2026-10-11。基准：`main` `0941ef4`。
依据：[质量规范](quality-standard.md) Q5.3.7、Q10.2、Q10.3、§12；[CI 说明](ci.md)（#237）；#204 的"范围调整（2026-10-10）"。属于 #183 第一步。

硬约束（来自 #204）：

- 普通代码 PR 从推送到出结果 ≤ 10 分钟，排队也算在内；
- PR 上不新增 macOS、Windows 任务；
- 新检查放进并行的 Linux 任务，不让关键路径变长；
- 能替换就不新增。

范围外：E2E 任务（#186、#193）；夜间 TLC（`stress.yml`）；Windows 上的 `rutis-host` 测试（在 `runtimes-windows` 里加一行，另做）；actionlint；Go 的 `govulncheck`；Python 代码风格检查。

## 一、决策

| # | 决策 | 章节 |
| --- | --- | --- |
| D1 | 版本按机器分：Linux 任务用每种语言的最低支持版本，macOS 和 Windows 用最新版本 | 三 |
| D2 | websockets：Linux 装 `websockets==15.*`（声明的下限），其他地方装 `websockets>=15` | 三 |
| D3 | 新增一个 Linux 任务 `lint`，`code` 开关打开时运行：`cargo fmt --check`、`cargo clippy -D warnings`、版本一致性检查；`all` 打开时再加 `cargo deny check licenses bans sources` | 四、六 |
| D4 | clippy 基线写在代码里：`#[expect(<lint>, reason = "lint-baseline(#<issue>): …")]`。条目数只能减少，升级工具链的 PR 除外 | 四 |
| D5 | MSRV 按 crate 声明实际可用的版本（实测：1.85 / 1.88 / 1.90），新任务 `msrv` 在 `all` 打开时运行（main，以及改 `Cargo.lock`、根 `Cargo.toml`、CI 配置的 PR） | 五 |
| D6 | 漏洞检查（`cargo deny check advisories`、`npm audit`、`pip-audit`）放进每周运行的新 workflow `deps.yml`；失败时开一个 issue（已有就更新），不阻止合并 | 六 |
| D7 | 最低版本以包里的声明为准（`engines`、`requires-python`、`websockets>=`、`rust-version`、`rust-toolchain.toml`）；workflow 里写的版本由 `tools/check-ci-versions.mjs` 对照这些声明，在 `lint` 里检查 | 七 |
| D8 | `tools/ci-stats.mjs` 把 PR 运行分成"只有文档 / 普通代码 / dylib / 全部"四类；验收看"普通代码"这一类 | 九 |
| D9 | `checks` 里的 `cargo check --workspace --all-targets` 删掉：`lint` 的 clippy 已经编译了同样的代码 | 四 |

## 二、现状（实测）

### 2.1 PR 时间

`node tools/ci-stats.mjs 40 ci.yml`，2026-10-11 运行，覆盖 #237 合并（2026-10-10 12:58 UTC）之后的 40 次运行。

工具报告的 `pull_request` 中位数是 19.5 分钟，但这个数把三类运行混在一起：打开 `all` 的 PR（28 个任务，约 20–40 分钟）、普通代码 PR、重跑过的运行（重跑的那次从第一次创建时算起，例如 run `38056277205` 记为 659 分钟）。只看普通代码 PR（运行了 7–9 个任务，第一次运行，成功）：

| run | 分支 | 推送到结果 |
| --- | --- | ---: |
| 38097374545 | fix/238-multilang-go-flaky | 5.2 |
| 38056775967 | fix/233-exit-status-flaky | 5.6 |
| 38055889193 | fix/239-handshake-flaky | 7.4 |
| 38098778901 | fix/173-local-line-limit | 8.7 |
| 38098889402 | fix/184-concurrent-launch | 8.7 |
| 38100944545 | fix/247-row-self-dispose | 8.8 |

中位数约 8.0 分钟（6 次，样本少）。关键路径是 `rust`（约 4.5 分钟）和 `runtimes-windows`（4.2–5.1 分钟）。Linux 也会排队：run `38098889402` 里 `changes` 等了 1.8 分钟才开始，run `38055889193` 里 `ci-ok` 等了 2.3 分钟。原因是每次推送 main 都会同时占 28 个任务。也就是说，普通代码 PR 现在离 10 分钟只有约 2 分钟余量，而且这部分余量主要被 Linux 排队吃掉。

各任务执行时间的中位数（同一次统计）：`runtimes-windows` 5.6、`rust` 4.8、`network-macos` 3.9、`checks` 2.4、`runtimes-go`（Linux）1.7、`runtimes-bun`（Linux）1.1、`js-py` 0.6 分钟。

### 2.2 fmt

`cargo fmt --all --check`（工具链 1.98.1）：4 处差异，在 2 个文件里：`crates/rutis-xtask/src/main.rs`（3 处）、`examples/native-mount/tests/cordis_mount.rs`（1 处）。

### 2.3 clippy

`cargo clippy --workspace --all-targets --all-features`（1.98.1，macOS arm64，16 核，空的 target 目录，56 秒）：22 个警告，9 种 lint，10 个文件，没有错误。

| lint | 个数 | 位置 |
| --- | ---: | --- |
| `clippy::await_holding_lock` | 7 | `crates/rutis-agent/tests/session_persist.rs`（测试里 `llm.calls.lock()` 的锁跨过了 `await`） |
| `dead_code` | 5 | `tests/dylib-fixtures/greeter-v2/src/lib.rs`（4，见下）；`crates/rutis-loader/tests/migration_example.rs`（1） |
| `clippy::doc_lazy_continuation` | 3 | `crates/rutis-agent/src/driver.rs`、`session.rs` |
| `clippy::missing_safety_doc` | 2 | `crates/rutis-sdk/src/lib.rs`、`crates/rutis-dylib/src/loader.rs` |
| `clippy::type_complexity` | 1 | `crates/rutis-bridge/tests/memory_mux.rs` |
| `clippy::map_flatten`、`unnecessary_mut_passed`、`items_after_test_module` | 各 1 | `crates/rutis-agent/src/driver.rs`、`tui.rs` |
| `clippy::redundant_closure` | 1 | `crates/rutis-cli/src/main.rs` |

按 crate：`rutis-agent` 13，greeter-v2 夹具 4，`rutis-sdk`、`rutis-dylib`、`rutis-loader`、`rutis-bridge`、`rutis-cli` 各 1。内核 `rutis` 没有警告。

测量时发现两个前提，CI 里也一样：

1. `--all-features` 会打开 `tests/dylib-fixtures/greeter-*` 的 `export` feature，它在编译时读取 `RUTIS_SDK_ARTIFACT_SHA256`，没有就编译失败。CI 里照 `sdk-repro` 的做法设成 64 个 `0`。
2. `examples/native-mount`、`examples/interop-experiments` 的 build.rs 要先 `npm --prefix node/rutis-runtime ci`；`rutis-dsh` 只有装了 dsh（`npm --prefix crates/rutis-dsh/dsh ci`）才编译 web 宿主。和 `checks` 任务一样，`lint` 要先装 Node 和这两个包。

greeter-v2 的 4 个 `dead_code` 只在 `--all-features` 下出现：`changed_identity`、`fail_once` 这几个 feature 是同一个夹具的不同版本，不是叠加关系，全部打开时有些代码用不到。

### 2.4 MSRV

声明：根 `Cargo.toml` `rust-version = "1.85"`，所有 crate 继承。本地装了 1.85.1、1.88.0、1.90.0（`--profile minimal`），用当前的 `Cargo.lock`（`--locked`）检查：

| 命令 | 结果 |
| --- | --- |
| `cargo +1.85 check --workspace` | 失败：依赖要求更高的 rustc，例如 `icu_*` 2.3 和 `darling` 0.24 要 1.88，`idna_adapter` 1.2.2 要 1.86，`tree-sitter-language` 0.1.8 要 1.90 |
| `cargo +1.85 check -p rutis -p rutis-bridge --all-features` | 通过 |
| `cargo +1.85 check -p rutis-dylib-meta -p rutis-sdk -p rutis-dylib -p rutis-dylib-launcher`（默认 feature） | 通过 |
| `cargo +1.85 check -p rutis -p rutis-bridge --all-features --all-targets` | 失败：测试用的依赖 `rcgen` 0.14、`time` 0.3.55 要 1.88 |
| `cargo +1.88 check -p rutis-loader -p rutis-host --all-features` | 通过 |
| `cargo +1.88 check --workspace --exclude rutis-agent --exclude rutis-cli` | 通过 |
| `cargo +1.88 check -p rutis-agent` | 失败：`tree-sitter-language` 0.1.8 要 1.90 |
| `cargo +1.90.0 check --workspace` | 通过 |

`rutis-loader`、`rutis-host`、`rutis-dsh`、`aimux-llm` 到 1.88 是因为 `url` → `idna` → `icu_*`；`rutis-agent`、`rutis-cli` 到 1.90 是因为 rutui 用的 `tree-sitter-language`。

没有验证：Linux 和 Windows 目标；`rutis-sdk`、`rutis-dylib*` 在 1.85 上的 `--all-features`；用户自己解析依赖（不用我们的 `Cargo.lock`）时会选到哪些版本。

### 2.5 依赖

- `cargo deny --all-features check advisories`（cargo-deny 0.20.2，没有 `deny.toml`）：没有漏洞；4 个"不再维护"：RUSTSEC-2025-0052（async-std）、RUSTSEC-2025-0141（bincode）、RUSTSEC-2024-0436（paste）、RUSTSEC-2024-0320（yaml-rust），都经 `rutis-agent` 和 rutui 引入。
- 许可证（`cargo deny list`）：MIT、Apache-2.0（含 LLVM-exception）、BSD-2/3-Clause、ISC、Unicode-3.0、Zlib、Unlicense、CC0-1.0、MIT-0、0BSD、BSL-1.0、CDLA-Permissive-2.0、MPL-2.0（`nucleo`、`nucleo-matcher`、`option-ext`）。`r-efi` 是 MIT / Apache-2.0 / LGPL-2.1-or-later 三选一。没有许可证的只有不发布的夹具和 `rutis-xtask`。`licenses` 检查还没有用实际的 `deny.toml` 跑过。
- `npm audit`：`node/rutis-runtime`、`node/baseline` 没有问题；`crates/rutis-dsh/dsh`（`private`，不发布）有 13 个（7 个 moderate、6 个 high，都在运行时依赖里，例如 `@modelcontextprotocol/client`、`fflate`、`http-cache-semantics`）。
- `pip-audit`（2.10.1）：`websockets==15.0.1` 没有已知漏洞。`python/rutis` 本身没有依赖。

### 2.6 版本现在写在哪里

| 位置 | Node | Python | websockets | 其他 |
| --- | --- | --- | --- | --- |
| `ci.yml` `rust`、`js-py`、`checks` | 24 | 3.12 | `>=13` | Bun 1.4.0、Go oldstable |
| `ci.yml` `runtimes-go`、`runtimes-bun` | 24 | 3.12 | `>=13`（只有 go） | 各自的矩阵 |
| `ci.yml` `network-macos` | 26 | 3.12 | `>=13` | Bun latest、Go stable |
| `ci.yml` `runtimes-windows` | 24 | 3.12 | `>=13` | Go stable |
| `ci.yml` `release-*` | 24 | — | — | 与 `release.yml` 相同 |
| `release.yml` | 24 | — | — | Rust 1.98.1、Go stable |
| `release-cli.yml` | — | — | — | `dtolnay/rust-toolchain@stable`（其他地方都是 1.98.1） |
| `stress.yml`（#245 的 `multiprocess`） | 24 | 3.12 | 有 | 写明"和 `rust` 任务相同" |
| 声明 | `engines.node` `>=22`（#219 已合并） | `requires-python >=3.12`（#196 未合并） | `network = ["websockets>=15"]`（#218 已合并） | `rust-version = "1.85"`；`rust-toolchain.toml` 1.98.1；`engines.bun` |

声明已经改了，CI 还停在旧值：Node 最低已经是 22，CI 跑 24；websockets 下限已经是 15，CI 装 `>=13`。`ci.yml` 的注释里还写着"#219 合并后改 22""#218 合并后锁 15.*"。这正是 D7 要防的问题。

## 三、版本矩阵

规则（D1）：每种语言，Linux 任务用最低支持版本，macOS 和 Windows 用最新版本。Linux 机器多，最低版本在 PR 上每次都验证；macOS 和 Windows 每次运行只占一个任务，在那里验证最新版本。不新增任务，只换版本号。

| 任务 | 机器 | Node | Python | websockets | Bun | Go |
| --- | --- | --- | --- | --- | --- | --- |
| `rust` | Linux | 24 → **22** | 3.12（#196 合并后 **3.10**） | `>=13` → **`==15.*`** | 1.4.0 | oldstable |
| `js-py` | Linux | 24 → **22** | 同上 | `>=13` → **`==15.*`** | — | — |
| `checks` | Linux | 24 → **22**（只用来编译） | — | — | — | — |
| `runtimes-go`（矩阵） | Linux；main 上加 macOS | 24 → **22** | 同 `rust` | `>=13` → **`==15.*`** | — | 矩阵 |
| `runtimes-bun`（矩阵） | Linux；main 上加 macOS | 24 → **22** | 同 `rust` | — | 矩阵 | — |
| `network-macos` | macOS | 26（不变） | 3.12 → **`3.x`** | `>=13` → **`>=15`** | latest | stable |
| `runtimes-windows` | Windows | 24 → **26**（待定，见十三第 3 项） | 3.12 → **`3.x`**（待定） | `>=13` → **`>=15`** | — | stable |
| `release-dry-run`、`release-windows` | Linux、Windows | 24（不变，跟 `release.yml`） | — | — | — | — |
| `stress.yml` `multiprocess` | Linux | 跟 `rust` | 跟 `rust` | 跟 `rust` | 跟 `rust` | 跟 `rust` |

说明：

- `runtimes-go`、`runtimes-bun` 只变化自己的语言；Node、Python 只是跨语言行要用的对端，用最低版本，在 macOS 的矩阵项里也一样。
- Python 的"最新"写成 `3.x`（`actions/setup-python` 取最新的稳定版），不写死。新版本发布后第一个 PR 就会用到它；如果它让 PR 失败，那是 Q10.2 要找的问题，单独开 PR 修，修好之前可以临时写死上一版并开 issue。
- Node 的"最新"继续写主版本号（现在是 26），新的偶数主版本发布后手动改。
- `release.yml` 和 `release-*` 任务里的 Node 是发布工具的版本（`npm publish --provenance`），不属于支持矩阵，保持 24，三处一致。
- `release-cli.yml` 的 `dtolnay/rust-toolchain@stable` 改成 `@1.98.1`。仓库根有 `rust-toolchain.toml`，cargo 实际用的本来就是 1.98.1，`@stable` 只是多装了一个用不到的工具链。
- #196 合并时，Linux 的 Python 从 3.12 改成 3.10。由于 D7 的检查，#196 改了 `requires-python` 却不改 workflow，`lint` 会失败；所以这一行改动由 #196 的 PR 一起做（这是"`ci.yml` 只由 #203、#204 修改"约定的例外，需要维护者同意），或者 #196 合并后立即由本 issue 跟进。
- 对关键路径的影响：没有。换版本不增加步骤；`ubuntu-24.04` 镜像自带 Node 22 和 Python 3.10，`pip install "websockets==15.*"` 几秒。

## 四、fmt 与 clippy：任务 `lint`

### 4.1 任务

```yaml
lint:
  needs: changes
  if: needs.changes.outputs.code == 'true'
  runs-on: ubuntu-24.04
```

步骤，按顺序：

1. `actions/checkout@v4`（`fetch-depth: 0`，基线比较要用 PR 的 base 提交）。
2. `dtolnay/rust-toolchain@1.98.1`，`components: rustfmt, clippy`。
3. `Swatinem/rust-cache@v2`，`save-if: github.ref == 'refs/heads/main'`（同其他任务）。
4. `node tools/check-ci-versions.mjs`（§七，约 1 秒）。
5. `cargo fmt --all --check`（约 5 秒）。放在 clippy 前面，格式错了马上失败。
6. 基线条目数检查（§4.3，约 1 秒）。
7. `actions/setup-node@v4`（Node 22），`npm --prefix node/rutis-runtime ci`、`npm --prefix crates/rutis-dsh/dsh ci`（§2.3 前提 2）。
8. `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`，环境变量 `RUTIS_SDK_ARTIFACT_SHA256` 设成 64 个 `0`（§2.3 前提 1）。`-D warnings` 写在 `--` 后面，不写进 `RUSTFLAGS`：改 `RUSTFLAGS` 会让依赖全部重新编译，缓存失效。
9. 只在 `all` 打开时：`cargo deny --all-features check licenses bans sources`（§六）。

把 `lint` 加进 `ci-ok` 的 `needs`。

只用 clippy 默认的 lint 组，不打开 `pedantic`、`nursery`。

用 `code` 开关，不另设"只改 Rust"的开关：只改 Python 或 Node 的 PR 也会跑 `lint`，多占一台 Linux 机器，但不增加等待时间；少一个开关，少一处要维护的路径表。

### 4.2 时间

- 有缓存（main 保存过之后）：估计 3–4 分钟。参照：`checks` 有缓存时 2.4 分钟，内容是一次全工作区 `cargo check` 加 15 次 feature 组合的检查；`lint` 是一次全工作区、全部 feature 的 clippy，再加 Node 和两次 `npm ci`（约 30 秒）。
- 没有缓存（合并后第一次在 main 上运行，或缓存被挤掉）：估计 5–7 分钟，会超过 `rust`，成为那一次的关键路径。
- 对普通代码 PR 关键路径的影响：有缓存时没有影响（`rust`、`runtimes-windows` 约 4.5–5 分钟）；多占一台 Linux 机器（每次 PR 运行从 8 个任务变成 9 个）。
- 合并后用 `ci-stats` 看 `lint` 的执行时间中位数，如果超过 `rust`，按这个顺序处理：先确认缓存是否命中；再把 clippy 拆成两个并行任务（例如 `rutis-agent`、`rutis-cli`、`rutis-dsh` 一组，其余一组）。

D9：`checks` 里的 `cargo check --workspace --all-targets` 删掉。clippy 已经以全部 feature 编译了工作区所有目标；默认 feature 下的编译由 `rust` 的 `cargo test --workspace` 覆盖，bridge、loader 的单 feature 组合仍由 `checks` 检查。这样 main 上少约 1 分钟。`network-macos`、`runtimes-windows` 里的同一条命令保留，它们检查的是各自平台的编译。

### 4.3 基线

现状只有 22 个警告（§2.3），都能改掉。推荐第一阶段全部改掉，基线从 0 开始（十三第 2 项）；下面的机制仍然要有，因为升级工具链时会出现新的 lint。

**怎么写。** 基线条目是代码里的一个属性，放在能放的最小范围上（一个函数、一条语句，而不是整个文件或 crate）：

```rust
#[expect(clippy::await_holding_lock, reason = "lint-baseline(#NNN): 测试里的锁跨过 await")]
```

- 用 `expect` 而不用 `allow`：代码改好、lint 不再出现时，`expect` 会产生 `unfulfilled_lint_expectations` 警告，`-D warnings` 让 `lint` 失败，提醒把这一条删掉。所以基线不会留下已经没用的条目。`expect` 和 `reason` 在 Rust 1.81 稳定，低于 MSRV。
- `reason` 以 `lint-baseline(#<issue>)` 开头，issue 是跟踪这一批条目的 issue。
- 注意：clippy 的 lint（`clippy::…`）只在 clippy 运行时检查 `expect`；rustc 自己的 lint（如 `dead_code`）在每次编译都检查。只在某个 feature 组合下出现的 rustc lint 不能用无条件的 `expect`，要用 `cfg_attr(<那个组合>, expect(...))`，或者直接改代码。

**怎么防止增加。** `lint` 的第 6 步数 `git grep -c 'lint-baseline('`：PR 上同时数 PR 的 base 提交和当前提交，当前提交的条目多于 base 时失败；但 PR 改了 `rust-toolchain.toml` 时允许增加（新工具链带来的新 lint）。推送 main 时只把条目数写进任务摘要。

**基线以外的 `allow`。** 新代码确实需要关掉某条 lint 时，用普通的 `#[allow(<lint>, reason = "…")]`，写明原因，和代码一起评审。它不带 `lint-baseline(` 标记，不计入基线。例如 greeter-v2 夹具的几个 feature 是互斥的版本，全部打开时有代码用不到，这属于正常的 `allow`，不是基线。

**谁负责、怎么收紧。** 每一条属于它所在的 crate，由改这个 crate 的人顺手删掉；跟踪 issue 列出全部条目。条目数写在 main 上每次 `lint` 的任务摘要里。目标是 0；升级工具链的 PR 带进来的新条目，在下一次升级工具链之前清掉。

## 五、MSRV：任务 `msrv`

### 5.1 1.85 不通过怎么办

实测（§2.4）1.85 不能编译整个工作区。按 Q10.3，声明必须改成实际验证过的版本。两种做法：

- **A：整个工作区一个值，改成 1.90。** 最简单；但内核 `rutis` 和 `rutis-bridge` 明明能在 1.85 上编译，README 和 guide 里"内核需要 Rust 1.85"的说法也会变成 1.90，只在 Rust 里嵌入内核的用户被多要求了 5 个版本。
- **B：按 crate 声明（推荐）。** 根 `Cargo.toml` 保持 `rust-version = "1.85"`，这些 crate 在自己的 `Cargo.toml` 里覆盖：

  | rust-version | crate |
  | --- | --- |
  | 1.85（继承） | `rutis`、`rutis-bridge`、`rutis-sdk`、`rutis-dylib`、`rutis-dylib-meta`、`rutis-dylib-launcher`、`rutis-dev`、`rutis-xtask` |
  | 1.88 | `rutis-loader`、`rutis-host`、`rutis-dsh`、`aimux-llm` |
  | 1.90 | `rutis-agent`、`rutis-cli` |

  README 里"内核需要 Rust 1.85"不变；`docs/guide/README.md` 和英文版"Rust 1.85 或更高（只在 Rust 里嵌入时需要）"改为按 crate 写（嵌入内核和 bridge 1.85，用 loader 或 host 1.88）。

MSRV 只承诺库和二进制能编译，不包括测试：测试依赖（`rcgen`、`time`）要 1.88，检查时不加 `--all-targets`。

### 5.2 任务

```yaml
msrv:
  needs: changes
  if: needs.changes.outputs.all == 'true'
  runs-on: ubuntu-24.04
```

- 一个步骤 `node tools/msrv-check.mjs`：用 `cargo metadata --no-deps` 读出每个工作区成员的 `rust-version`，按版本分组；每组先 `rustup toolchain install <版本> --profile minimal`，再 `cargo +<版本> check --locked --all-features -p <这一组的 crate>…`。版本只写在 `Cargo.toml` 里，workflow 不重复写。
- 环境变量 `RUTIS_SDK_ARTIFACT_SHA256` 同 `lint`；装 Node 和 `node/rutis-runtime`、dsh 的 `npm ci`（`rutis-dsh` 的 build.rs）。
- `Swatinem/rust-cache@v2`，`save-if` 同其他任务。
- 加进 `ci-ok` 的 `needs`。
- 时间：每组装工具链约 20 秒；没有缓存时每组编译 2–3 分钟，共估计 5–8 分钟。只在 `all` 时运行，不在普通 PR 上。

**为什么用 `all`，不只放 main。** 依赖的 rust-version 变高几乎都来自 `Cargo.lock` 或 `Cargo.toml` 的改动（加依赖、`cargo update`），这些 PR 本来就打开 `all`、运行全部任务（约 25 分钟），`msrv` 和别的任务并行，不增加它们的时间，却能在合并前拦住。只有"代码用了新版本才有的标准库 API"这一类，要到合并后在 main 上发现。

**以后不通过时。** 在同一个 PR 里二选一，并写进 PR 描述：把那个依赖锁在旧版本（`cargo update -p <依赖> --precise <版本>`）；或者提高这个 crate 的 `rust-version`，同时改 README、guide 里对应的说法。提高 `rust-version` 是用户能看到的变化，由维护者决定。

## 六、依赖检查

| 检查 | 放在哪 | 什么时候 | 失败时 |
| --- | --- | --- | --- |
| `cargo deny check licenses bans sources` | `ci.yml` `lint` 的一个步骤 | `all` 打开时：main，以及改 `Cargo.lock`、根 `Cargo.toml`、CI 的 PR | 和其他检查一样，`ci-ok` 失败，阻止合并 |
| `cargo deny check advisories` | 新 workflow `deps.yml` 的任务 `cargo-deny` | 每周；手动；改 `deny.toml` 或 `deps.yml` 的 PR | 开 issue，不阻止合并 |
| `npm audit --omit=dev` | `deps.yml` 的任务 `npm-audit`，三个 lockfile 一个矩阵项 | 同上 | 同上 |
| `pip-audit` | `deps.yml` 的任务 `pip-audit` | 同上 | 同上 |

**为什么这样分。** `licenses`、`bans`、`sources` 的结果只取决于 `Cargo.lock`，`Cargo.lock` 不变结果就不变，所以只在 `Cargo.lock` 可能变化的运行（`all`）里检查，可以阻止合并。漏洞库每天都在更新，`advisories`、`npm audit`、`pip-audit` 的结果会在代码没变的情况下变化；放在 PR 上，一条新公告会让所有无关的 PR 突然失败。所以放在每周的运行里，由维护者处理。

**`deny.toml`（新文件，在仓库根）。** 要点：

- `[graph] all-features = true`。
- `[licenses]`：`allow` 列出 §2.5 里出现的许可证；MPL-2.0 是否允许由维护者决定（十三第 7 项）；`private.ignore = true`，不检查不发布的 crate（夹具、`rutis-xtask`、示例）。
- `[advisories]`：`ignore` 列出现有的 4 个"不再维护"（§2.5），每条写原因和跟踪 issue，例如 `{ id = "RUSTSEC-2024-0320", reason = "yaml-rust，经 rutui 引入；跟踪 #NNN" }`。新出现的"不再维护""不健全"和所有漏洞都会让每周的检查失败。
- `[bans]`：`multiple-versions = "allow"`（现在有很多重复版本，例如 `windows-sys`，不是这个 issue 要处理的）；`wildcards = "deny"`，`allow-wildcard-paths = true`（工作区内的 path 依赖）。
- `[sources]`：只允许 crates.io；出现未知的 registry 或 git 来源时失败。
- 这份配置还没有实际跑过；实现时先在本地跑通，再进 CI。

**`deps.yml`。**

- 触发：`schedule`（每周一 02:00 UTC，在 `ci-stats.yml` 的 01:00 之后）；`workflow_dispatch`；`pull_request`，只在 `paths` 为 `deny.toml`、`.github/workflows/deps.yml` 时，用来在合并前验证配置的改动。
- `cargo-deny`：`taiki-e/install-action@cargo-deny`，`cargo deny --all-features check advisories`。约 1 分钟。
- `npm-audit`：矩阵 `node/rutis-runtime`（发布的 `@arcships/rutis-runtime` 的依赖，`@arcships/rutis-host` 经它引入）、`node/baseline`（测试用）、`crates/rutis-dsh/dsh`（不发布）。`npm audit --omit=dev --audit-level=high`。dsh 现在有 6 个 high；在它被处理之前，dsh 这一项只把结果写进摘要，不让任务失败（十三第 6 项）。
- `pip-audit`：Python 用 `3.x`，`pip install "./python/rutis[network]"` 后运行 `pip-audit`；再用 `websockets==15.*` 跑一次，检查声明的下限。
- `report`：`needs` 以上三个任务，`if: failure()`；权限 `issues: write`。用 `gh issue list --label dependencies --state open` 找已有的 issue，有就加一条评论，没有就新建，标题"每周依赖检查失败"，内容是失败的任务、发现的条目（从各任务的摘要取）和运行链接。工具本身出错（例如下载漏洞库失败）和发现了问题，在评论里分开写。

**通知谁、多久处理。** 新建 issue 时指派给维护者（具体账号写在 `deps.yml` 里，十三第 5 项）；之后的评论通知所有订阅这个 issue 的人。处理办法：发布的包（训练列车里的 crate、`node/rutis-runtime` 的依赖、`websockets`）有漏洞时，7 天内升级依赖，或在 `deny.toml` / 审计命令里加一条带原因和到期日期的忽略；高危漏洞是否需要补丁版本，由维护者按质量规范 §13 的发布门决定。`deps.yml` 不列入 `ci-ok`，不阻止合并。

## 七、版本写在哪里，怎么保持一致

**唯一的来源是包里的声明：**

| 什么 | 声明在哪 |
| --- | --- |
| Node 最低版本 | `node/rutis`、`node/rutis-runtime`、`node/rutis-host` 的 `package.json` `engines.node` |
| Bun 最低版本 | `bun/rutis-bun/package.json` `engines.bun` |
| Python 最低版本 | `python/rutis/pyproject.toml` `requires-python` |
| websockets 下限 | `python/rutis/pyproject.toml` `[project.optional-dependencies] network` |
| Rust 工具链 | `rust-toolchain.toml` `channel` |
| MSRV | 各 crate 的 `rust-version`（`msrv` 任务直接读，§5.2） |
| 发布用的 Node | `release.yml` `npm` 任务 |

GitHub Actions 的 `uses:` 和 `with:` 里不能直接读文件，所以 workflow 里仍然写字面值。新脚本 `tools/check-ci-versions.mjs`（只用 Node 标准库，逐行读 YAML）检查这些字面值和声明是否一致，在 `lint` 第 4 步运行：

1. `ci.yml`、`stress.yml`、`release.yml`、`release-cli.yml`、`dylib-windows.yml` 里每一个 `dtolnay/rust-toolchain@<版本>` 等于 `rust-toolchain.toml` 的 `channel`。
2. "最低版本任务"的 `node-version` 等于 `engines.node` 的下限主版本。哪些任务算最低版本任务，写在脚本里的一个列表中（`rust`、`js-py`、`checks`、`runtimes-go`、`runtimes-bun`，以及 `stress.yml` 的 `multiprocess`），和代码一样评审。
3. 最低版本任务的 `python-version` 等于 `requires-python` 的下限；它们装 websockets 时写的是 `==<下限>.*`；其他任务写的是 `>=<下限>`；任何地方都不出现比下限低的版本。
4. `rust` 任务和 `bun-matrix` 的最低 Bun 等于 `engines.bun` 的下限。
5. `release-dry-run`、`release-windows` 的 `node-version` 等于 `release.yml` 的。
6. 不符合时打印文件、行号、期望值和声明的位置，退出码非 0。

这样，以后改了声明（例如 #196 把 Python 降到 3.10）却忘了改 CI，`lint` 会失败，不会再出现"声明 22、CI 跑 24"的情况。

没有选的做法：把版本放进每个 workflow 顶部的 `env`。`env` 不能用在 `runs-on` 和矩阵里，三个 workflow 也还是各写一份，仍然要靠人保持一致。做一个 composite action 统一安装各语言，可以减少重复，但改动面大（每个任务都要改），也不能代替上面的检查；以后需要时再做。

`stress.yml`：#245 的 `multiprocess` 说明它用 `rust` 任务的版本。#245 和本 issue 的实现谁先合并，后合并的一方负责让两者一致；脚本把 `multiprocess` 列为最低版本任务，不一致时 `lint` 会失败。

## 八、`docs/ci.md` 的修改

中英文同时改（`docs/ci.md`、`docs/ci.en.md`）：

- §2 workflow 表：加 `deps.yml`（每周；手动；改 `deny.toml` 的 PR；漏洞检查）。
- §4 任务表：加 `lint`（Linux，`code`，main 上多做 `cargo deny check licenses bans sources`）和 `msrv`（Linux，只在 `all` 时）；`rust`、`js-py`、`network-macos`、`runtimes-windows` 的版本说明按 §三 改，并删掉"#219 合并后改 22"这类已经过时的说法；`checks` 的内容去掉 `cargo check --workspace --all-targets`。
- §4"每种改动在 PR 上跑什么"：内核、bridge、loader、host、Python 包几行都加上 `lint`；预计时间不变。
- 新增一节"版本"：§三 的规则（Linux 最低、macOS 和 Windows 最新）、§七 的声明表、`tools/check-ci-versions.mjs` 检查什么。文档里只写规则和声明的位置，不抄具体版本号，避免文档本身过时。
- §6"新增 CI 检查的流程"加一条：结果会因外部数据（漏洞库、新版本发布）而变化、代码不变也可能失败的检查，不放 PR，放每周。
- §6 加"clippy 基线"：§4.3 的写法、计数规则、升级工具链时怎么处理。
- §6"加一种语言运行时"第 2 步：同时在 `tools/check-ci-versions.mjs` 里加上这种语言的声明。
- §7 失败处理：`deps.yml` 的 issue 怎么处理（§六）；`msrv` 失败时的两种做法（§5.2）。

不改 `docs/quality-status.md`（按协作约定，每一步结束时统一更新）。

## 九、测量

**改 `tools/ci-stats.mjs`：**

1. 把 `pull_request` 运行分成四类，用这次运行里实际跑了哪些任务来判断：
   - 只有文档：`rust` 没跑；
   - 普通代码：`rust` 跑了，`checks` 没跑，没有名字以 `dylib-` 开头的任务跑；
   - dylib：有 `dylib-` 任务跑了，`checks` 没跑；
   - 全部：`checks` 跑了。

   "推送到结果"表格按这四类各出一行，加上 `push`。
2. 重跑过的运行（`run_attempt > 1`）从这一次尝试的 `run_started_at` 算起，而不是从第一次创建时算起；同时在表格里单独计数。
3. 新参数 `--since <日期>`：只统计这个日期之后创建的运行。
4. 输出一行结论：`普通代码 PR：中位数 X 分钟，p90 Y，共 N 次；目标 ≤ 10`。

**基准：** 改造前（§2.1）普通代码 PR 中位数约 8.0 分钟（6 次）。

**验收：** 第一阶段合并的日期记为 T。T + 7 天，运行 `node tools/ci-stats.mjs 100 ci.yml --since T`（每周的 `ci-stats.yml` 也会跑），普通代码 PR 中位数 ≤ 10.0 分钟。样本少于 10 次时再延长一周。结果（中位数、p90、`lint` 和 `rust` 的执行时间中位数、Linux 任务的排队时间）作为评论写进 #204。

**超出时：** 先看 `lint` 是不是变成了关键路径（§4.2 的处理）；再看是不是 Linux 排队变长了（每次推送 main 占 28 个任务，`lint`、`msrv` 又各加一个）。排队的问题不在本 issue 里解决，记下数据，交给 #183。

## 十、风险

| 风险 | 后果 | 处理 |
| --- | --- | --- |
| Linux 并发上限 | main 上一次运行占 28 个任务，PR 的任务排队（§2.1 已经看到 1.8–2.3 分钟）。`lint` 让每次 PR 多一个任务，`msrv` 让 main 多一个 | `msrv`、漏洞检查不放普通 PR；验收时单独看排队时间 |
| `lint` 没有缓存时变成关键路径 | 那一次 PR 多 1–3 分钟 | 缓存在 main 上保存；持续超出时拆成两个任务（§4.2） |
| 多一份 Rust 缓存 | 仓库 10 GB 缓存空间更紧，main 的其他缓存可能被挤掉 | 合并后看 Actions 缓存用量；必要时 `lint` 设 `cache-targets: false`，只缓存依赖 |
| clippy 只在 Linux 上跑 | `cfg(windows)`、`cfg(target_os = "macos")` 的代码没有 lint | 接受；不在稀缺的 macOS、Windows 机器上加 clippy |
| macOS、Windows 用浮动的"最新"（`3.x`、Bun latest、Go stable） | 新版本发布后，无关的 PR 失败 | 这就是 Q10.2 要发现的问题；单独 PR 修，必要时临时写死上一版并开 issue |
| 工具链升级带来新的 clippy lint | 升级 PR 变大 | 升级 PR 允许增加基线条目（§4.3），下次升级前清掉 |
| MSRV 只按我们的 `Cargo.lock` 验证 | 用户自己解析依赖时可能选到要求更高 rustc 的版本 | 在 guide 里写明：旧版 Rust 用户可用 `cargo update` 配合 Cargo 的 `incompatible-rust-versions = "fallback"` |
| 代码用了更新的标准库 API | 只在 main 上发现 | 按 `docs/ci.md` §7 处理 main 失败 |
| 每周检查的失败没人看 | 漏洞一直留着 | 开 issue 并指派（§六）；每周 `ci-stats` 的那次也能看到 |
| 和进行中的 PR 冲突 | #196 改 Python 下限、#245 改 `stress.yml` | §三、§七 的做法：检查脚本会在不一致时让 `lint` 失败 |

## 十一、分阶段

设计评审通过后，实现加在本 PR 里，每个阶段一个提交，按顺序：

1. **版本**：§三 的版本替换（不含 Python 3.10）；`release-cli.yml` 改为 `@1.98.1`；`tools/check-ci-versions.mjs`。
2. **fmt、clippy**：修好 2 个文件的格式；修好 clippy 警告（按十三第 2 项的决定）；新任务 `lint`，加进 `ci-ok`；基线计数；`checks` 删掉被覆盖的那条命令。
3. **MSRV**：按十三第 1 项的决定改 `rust-version`；`tools/msrv-check.mjs`；新任务 `msrv`，加进 `ci-ok`；README、guide 里的 Rust 版本说法。
4. **依赖**：`deny.toml`；`lint` 里的 `licenses bans sources` 步骤；`deps.yml`；用 `workflow_dispatch` 在本分支上跑一次。
5. **测量与文档**：`tools/ci-stats.mjs` 的修改；`docs/ci.md`、`docs/ci.en.md`。

之后：

- #196 合并时：Linux 的 Python 改为 3.10（§三）。
- T + 7 天：§九 的验收，结果写进 #204。
- 如果基线不为 0：开跟踪 issue，按 §4.3 收紧。

本 PR 改了 `.github/workflows/**`，会运行全部任务；只在普通 PR 上才有的部分（`lint` 在普通 PR 上的时间），合并后看之后的第一个普通 PR。

## 十二、验收条件（都可以用命令检查）

1. `git grep -n 'websockets>=13' .github` 没有输出。
2. `node tools/check-ci-versions.mjs` 退出码为 0；把任意一个最低版本任务的 `node-version` 改成 24 后再运行，退出码非 0。
3. `ci.yml` 里 `runs-on` 为 macOS 或 Windows 的任务名单和 `0941ef4` 相同（用脚本对比两个版本的任务名和 `runs-on`）。
4. `ci.yml` 有 `lint` 和 `msrv` 两个任务，都在 `ci-ok` 的 `needs` 里；`lint` 的条件是 `needs.changes.outputs.code == 'true'`，`msrv` 的条件是 `needs.changes.outputs.all == 'true'`。
5. 合并后 main 上第一次运行：`lint`、`msrv` 成功；`cargo fmt --all --check` 和 `cargo clippy … -- -D warnings` 在本地也通过。
6. `git grep -c 'lint-baseline('` 的数目等于跟踪 issue 里列出的条目数（推荐的做法下为 0）。
7. `tools/msrv-check.mjs` 检查的版本集合等于 `cargo metadata --no-deps` 里所有 `rust_version` 的集合。
8. `deps.yml` 有 `schedule` 和 `workflow_dispatch`；在本分支上手动运行一次，结果是成功，或者开出了一个带 `dependencies` 标签的 issue。
9. `links` 任务通过（`docs/ci.md`、`docs/ci.en.md` 及本设计的链接）。
10. T + 7 天：`node tools/ci-stats.mjs 100 ci.yml --since T` 的"普通代码 PR"中位数 ≤ 10.0 分钟，样本 ≥ 10 次。

## 十三、需要维护者决定

| # | 问题 | 选项 | 推荐 |
| --- | --- | --- | --- |
| 1 | MSRV 1.85 不能编译整个工作区 | A：全部改为 1.90；B：按 crate 1.85 / 1.88 / 1.90（§5.1） | **B**。内核和 bridge 实测能在 1.85 上编译，README 的承诺可以保持；代价是 `msrv` 任务装 3 个工具链，只在 `all` 时运行 |
| 2 | clippy 的 22 个现有警告 | 全部改掉，基线为 0；或者全部写成基线条目，再逐步删 | **全部改掉**。都是机械的改动（锁的作用域、文档缩进、`# Safety` 说明等），约 10 个文件；基线机制保留给以后升级工具链 |
| 3 | Windows 上的 Node、Python | 用最新版本（Node 26、Python `3.x`）；或者保持 24、3.12 | **最新版本**，规则统一为"Linux 最低，macOS 和 Windows 最新"。保持不变则 Windows 验证的是一个既不是最低也不是最新的版本 |
| 4 | `cargo deny` 放哪 | 全部每周（#204 的表）；或者 `licenses bans sources` 放在 `all` 上阻止合并，`advisories` 每周 | **后者**。前者结果只取决于 `Cargo.lock`，在改 `Cargo.lock` 的 PR 上就能拦住，不增加普通 PR 的时间 |
| 5 | 每周依赖检查失败通知谁 | 开 issue 并指派给某个账号；只加标签不指派 | **开 issue 并指派**，请指定账号 |
| 6 | dsh 的 13 个 npm 漏洞（不发布的包） | 先只报告，另开 issue 升级依赖；或者现在就让它失败 | **先只报告**，另开 issue |
| 7 | MPL-2.0（`nucleo`、`option-ext`） | 允许；不允许（要替换依赖） | **允许**。MPL-2.0 按文件生效，不影响我们自己的代码 |
| 8 | 4 个"不再维护"的公告（都经 rutui） | 写进 `ignore` 并在 rutui 那边跟踪；或者现在就替换 | **写进 `ignore`**，在 rutui 开 issue |
| 9 | #196 合并时改 Linux 的 Python 版本 | 由 #196 的 PR 改 `ci.yml`（约定的例外）；或者 #196 合并后由 #204 跟进 | **由 #196 的 PR 改**。否则 #196 的 `lint` 会失败（§七），两个 PR 要按顺序合并 |
| 10 | `checks` 删掉 `cargo check --workspace --all-targets`（D9） | 删；保留 | **删**。和 `lint` 重复（Q12.7） |
