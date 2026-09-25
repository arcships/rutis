# cordis-tests-04：补三个验收覆盖缺口测试

```yaml
id: cordis-tests-04
package: rutis
module: tests
status: done
depends-on: []
```

## objective

在 `0decf15` 顶点（PR #55 叠放链顶点）补 3 个测试，填补独立验收发现的覆盖缺口。只加测试，不改实现代码。

## context

- 基线：`0decf15`（含 PR #53/#54/#55 三层全部代码）；worktree `/tmp/rutis-dev55`。
- 设计文档：`/media/eric8810/fast-deliver/code/rutis/docs/design-cordis-observation.md`
- 三个缺口（来源：[rev-cordis-dispatch-01.md](../reviews/rev-cordis-dispatch-01.md) 问题 1、[rev-cordis-intercept-03.md](../reviews/rev-cordis-intercept-03.md) 观察项 1、2）：

1. **观察器重入投递**（→ `tests/dispatch_observation.rs`）：观察器闭包内再次投递（emit 或 serial），断言嵌套投递也被观察、不死锁、内层业务监听器执行。设计 §1 验收清单明确列「重入」。
2. **不同键重入允许**（→ `tests/service_intercepts.rs`）：钩子 A 执行中读写另一个有钩子的键 B，应成功而非 `InterceptReentrant`；read 与 write 各覆盖或至少覆盖设计正文的场景。设计 §3：「同键同操作的同步重入返回明确错误；不同键重入允许」。
3. **摘除中绑定写入失败**（→ `tests/service_intercepts.rs`）：`binding.removing` 置位但槽位未替换时 `writer.set` 返回 `Stale`。设计 §3：「摘除中的绑定……一律失败」。

## path

- `crates/rutis/tests/dispatch_observation.rs`（缺口 1）
- `crates/rutis/tests/service_intercepts.rs`（缺口 2、3）

## verification

- 新测试各自通过且确实覆盖缺口语义（非空转断言）
- 全套回归：`cargo +1.98.1 test -p rutis` 全过
- `cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings` 干净
- `cargo +1.98.1 fmt -p rutis -- --check` 干净

## 结果

- **结论：pass**（开发：吴俊杰 `795c165`；独立验收：周文斌，记录 [rev-cordis-tests-04.md](../reviews/rev-cordis-tests-04.md)）
- 仅改两个测试文件（+141 行），未动实现；三个测试：`observer_reentry_observes_nested_dispatch_and_business_listener_runs`、`different_key_reentry_allowed_for_read_and_write`、`writer_set_fails_stale_during_binding_removal`
- `cargo +1.98.1 test -p rutis` 226 passed / 0 failed；clippy、fmt 干净；service_intercepts 连续 20 次全过（时序确定性确认）
- 提交 `795c165` 后经按分支重组推送，重入测试并入 PR #53（`5bc64fc`），随三个 PR 合并进 main（`f37f90d`）。
