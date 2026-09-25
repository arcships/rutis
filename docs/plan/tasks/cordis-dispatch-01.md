# cordis-dispatch-01：独立验收 PR #53（事件投递前观察）

```yaml
id: cordis-dispatch-01
package: rutis
module: bus
status: done
depends-on: []
```

## objective

独立验收 PR #53（提交 `5e107bf`，分支 `feat/cordis-dispatch-observation`）是否完整实现设计文档 §1「事件投递前观察」，结论 pass 或 blocked。

## context

- 设计文档（主仓库 main，被审分支上没有）：`/media/eric8810/fast-deliver/code/rutis/docs/design-cordis-observation.md`，验收以 §1 及「目的与边界」中三钩子作用域表为准。
- 被审范围：`7d7402d..5e107bf`；worktree `/tmp/rutis-rev53` 已 checkout 该顶点。
- 基准 Cordis 源码事实见设计文档表格（`events.ts` `_resolve` 的 `internal/dispatch`）。

## path

- `crates/rutis/src/bus.rs`、`crates/rutis/src/ctx.rs`、`crates/rutis/src/lib.rs`
- `crates/rutis/src/bus/transient_tests.rs`
- `crates/rutis/tests/dispatch_observation.rs`

## verification

设计文档 §1 验收标准逐条核对：

- 四种分发模式（Emit/Serial/Parallel/Waterfall）各触发观察器
- 动态限定名、实例键正确出现在 `DispatchAttempt`
- 零业务监听器时观察器仍被调用
- 观察器先于业务监听器快照（观察器内注册/移除监听器可影响随后投递）
- 注册/卸载竞态、重入投递、观察器 panic 隔离（ErrorSink 继续）
- 不同实例子树隔离：发射方祖先链筛选，兄弟实例互不可见
- 1000 轮注册/卸载无泄漏/无崩溃
- 无观察器时原有事件行为对拍不变
- 观察器不持锁运行（不持 admission、总线表、注册表、fiber 状态锁）
- 注册时校验 owner 属于该总线 root；观察器按 effect 卸载

命令（在 worktree 内执行）：

```sh
cargo +1.98.1 test -p rutis
cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings
cargo +1.98.1 fmt -p rutis -- --check
```

## 结果

- **结论：pass**（验收人：陈志远，2026-09-24；记录：[rev-cordis-dispatch-01.md](../reviews/rev-cordis-dispatch-01.md)）
- 被审 `5e107bf`（diff `7d7402d..5e107bf`）；`cargo +1.98.1 test -p rutis` 209 passed / 0 failed；clippy、fmt 干净。
- 验收标准 13 项全部通过；唯一覆盖缺口：重入投递无显式测试（实现 `bus.rs:366-371` 回调在锁外，审查确认无死锁，仅缺回归测试）。
- 非阻塞观察项 5 条：观察器线性扫描 O(n)、emitter 字段取自 Ctx 本身、注册双重点检、DispatchMode 未标 non_exhaustive、README 措辞一致。
