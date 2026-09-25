# cordis-effect-02：独立验收 PR #54（带标签 effect 清理树）

```yaml
id: cordis-effect-02
package: rutis
module: effect
status: done
depends-on: []
```

## objective

独立验收 PR #54（提交 `c9d4caf`，分支 `feat/cordis-effect-tree`）是否完整实现设计文档 §2「带标签的 effect 清理树」，结论 pass 或 blocked。

## context

- 设计文档（主仓库 main，被审分支上没有）：`/media/eric8810/fast-deliver/code/rutis/docs/design-cordis-observation.md`，验收以 §2 为准。
- 被审范围：`5e107bf..c9d4caf`（叠放在 PR #53 之上）；worktree `/tmp/rutis-rev54` 已 checkout 该顶点。
- 仅验收本层新增改动；PR #53 层由 cordis-dispatch-01 验收。

## path

- `crates/rutis/src/effect.rs`、`crates/rutis/src/fiber.rs`、`crates/rutis/src/ctx.rs`
- `crates/rutis/src/fiber/transient_tests.rs`
- `crates/rutis/tests/effect_tree.rs`

## verification

设计文档 §2 验收标准逐条核对：

- 自动标签（`anonymous` / 插件名）与显式标签（`effect_named`）
- `Effect::Many` 的真实嵌套生成 `children`；LIFO 清理顺序不变
- 并列 `ctx.effect()` 登记为兄弟项，不臆造父子关系
- 提前 dispose 后元数据消失；清理期间可见 `Draining`；记录完成后索引删除
- `EffectMeta` 不捕获清理闭包、不执行用户代码、不持子 fiber/服务值
- 现有 `Ctx::effect()` / `Plugin::apply()` 返回类型不变；错误聚合不变
- 1000 轮子树关闭后父 fiber 元数据与 effect 记录回到基线
- `FiberView::effects()` 入口满足 #27「由 fiber 获取」要求

命令（在 worktree 内执行）：

```sh
cargo +1.98.1 test -p rutis
cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings
cargo +1.98.1 fmt -p rutis -- --check
```

## 结果

- **结论：pass**（验收人：林晓雯，2026-09-24；记录：[rev-cordis-effect-02.md](../reviews/rev-cordis-effect-02.md)）
- 被审 `c9d4caf`（diff `5e107bf..c9d4caf`）；`cargo +1.98.1 test -p rutis` 213 passed / 0 failed；clippy、fmt 干净。
- 验收标准 8 项全部通过；无阻塞问题。
- 非阻塞观察项 5 条：sibling 项无专属测试、`effects()` 深拷贝整树 O(n)、纯值快照非引用视图、effect_index 弱引用 GC 依赖 drain 触发（与既有模式一致）、注册回滚路径 no-op。
