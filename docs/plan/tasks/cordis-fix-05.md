# cordis-fix-05：修复失败写入在框架锁内析构候选值（PR #55 P1）

```yaml
id: cordis-fix-05
package: rutis
module: intercept
status: done
depends-on: []
```

## objective

修复 PR #55 远程审核发现的 P1：`ServiceWriter::set` 的失败路径在持有框架锁时析构候选 `StoredValue`，候选值 `Drop` 重入框架（如 `Ctx::effect()`）自死锁。附带回归测试。

## context

- 远程审核评论（PR #55，arcships/rutis）：旧 writer 绑定被替换后写入 Drop 中调用 `root.effect()` 的值，5 秒超时死锁；建议失败时交还候选值、锁外析构。
- 基线：`795c165`（0decf15 + 测试补充）；worktree `/tmp/rutis-dev55`。
- 主 agent 核实的完整 bug 范围（比评论多两条路径），`crates/rutis/src/intercept.rs` 的 `ServiceWriter::set`：

| # | 失败路径 | 持锁 | 候选值 drop 位置 |
|---|---|---|---|
| 1 | `registration_open()` 失败（`?` 提前返回） | admission | set 局部 drop |
| 2 | `transition.generation` / state 检查失败（`return Err`） | admission + transition | set 局部 drop |
| 3 | `replace_mutable_if_current` 返回 `None`（`?`） | admission + transition + bindings | registry 函数内 drop（`value` 为 owned 参数，槽位空 `?` 与 ptr_eq/removing `return None` 两处） |

成功路径（`0decf15`）已在锁外 `catch_unwind` drop 旧值——修复应对齐该模式。

## 修复方向

- `registry.replace_mutable_if_current` 改为失败时交还候选值（如 `Result<StoredValue /*old*/, StoredValue /*candidate*/>`），不在函数内 drop。
- `set` 将锁获取、检查、提交包进独立作用域，结果携带 owned 值（成功=旧值，失败=候选值+原因）返回到锁外，统一锁外 `catch_unwind` drop 并上报 sink，Drop panic 不影响返回的错误。

## 回归测试（tests/service_intercepts.rs）

1. 旧绑定被替换后，旧 writer 写入 Drop 中重入框架（`root.effect()`）的候选值 → 不死锁（timeout 保护），返回 `Stale`。
2. 摘除中绑定（removing 置位）同场景 → 不死锁，返回 `Stale`。
3. 覆盖 generation 失效路径（如 provider 关闭后 set）候选值 Drop 重入 → 不死锁。

## verification

- 先写测试 1 确认红（复现死锁），修复后转绿
- `cargo +1.98.1 test -p rutis` 全过；clippy `-D warnings` 干净；fmt `--check` 干净
- 新测试时序确定性（无 sleep 碰运气）

## 结果

- **结论：pass**（开发：吴俊杰 `68491a7` + `c0b6b41`；独立验收：周文斌，记录 [rev-cordis-fix-05.md](../reviews/rev-cordis-fix-05.md)；主 agent 复核 c0b6b41 并重跑全量）
- **范围更正**：主 agent 初核声称三条失败路径均死锁，**错误**——Rust 局部变量逆声明序析构，路径 1（`registration_open` 失败）与路径 2（generation/state 检查失败）的候选值在锁 guard 释放后才 drop，不会死锁（吴俊杰用 restart 场景实验实锤：`err.generation == 1` 证明进入路径 2 且旧代码不死锁）。真实 P1 仅路径 3（候选值 move 进 `replace_mutable_if_current` 后在函数内 drop，调用方持 admission + transition + bindings 三层锁）——与远程评论一致。
- **修复**：`registry.replace_mutable_if_current` 改返回 `Result<StoredValue, StoredValue>`（失败交还候选值）；`set` 用 IIFE 把候选值传出所有锁作用域，锁外 `catch_unwind` drop + sink 上报，与 `0decf15` 成功路径对齐。路径 1/2 一并被显式化（防未来重构把 value move 进锁内函数引入真死锁）。
- **红→绿证据**（独立复核）：测试 1（绑定被替换）、测试 2（摘除中）在修复前 5 秒死锁红，修复后绿；测试 3 文档化路径 2 可达但不死锁的语义并断言 `err.generation`。
- **验证**：全量 229 passed / 0 failed（主 agent 复跑确认）；clippy `-D warnings`、fmt `--check` 干净；service_intercepts 连续 20 次稳定。
- 提交序列 `0decf15` → `795c165` → `68491a7` → `c0b6b41` 经按分支重组推送（PR #55 分支顶点 `8cac519`），随三个 PR 合并进 main（`f37f90d`）。
