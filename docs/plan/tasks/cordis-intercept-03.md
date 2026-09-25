# cordis-intercept-03：独立验收 PR #55（严格服务读取与提供者写入拦截）

```yaml
id: cordis-intercept-03
package: rutis
module: intercept
status: done
depends-on: []
```

## objective

独立验收 PR #55（提交 `4c1e160` + `0decf15`，分支 `feat/cordis-service-intercepts`）是否完整实现设计文档 §3「严格服务读取与提供者写入拦截」，结论 pass 或 blocked。

## context

- 设计文档（主仓库 main，被审分支上没有）：`/media/eric8810/fast-deliver/code/rutis/docs/design-cordis-observation.md`，验收以 §3 为准。
- 被审范围：`c9d4caf..0decf15`（叠放在 PR #54 之上）；worktree `/tmp/rutis-rev55` 已 checkout 顶点 `0decf15`。
- 仅验收本层新增改动；下层由 cordis-dispatch-01、cordis-effect-02 验收。
- 设计文档已声明：拦截器是可信扩展点，不要求防不可信拦截器的值来源证明；#29 验收表述中相应项按此修正。

## path

- `crates/rutis/src/intercept.rs`（新增）
- `crates/rutis/src/ctx.rs`、`crates/rutis/src/registry.rs`、`crates/rutis/src/error.rs`、`crates/rutis/src/lib.rs`
- `crates/rutis/tests/service_intercepts.rs`

## verification

设计文档 §3 验收标准逐条核对：

读取拦截：
- `get/get_as` 不进拦截链（绕过钩子）
- `require/require_as` 的钩子 `Continue | Replace(Arc<T>) | Deny` 语义正确
- 未声明、越界、未就绪先于钩子拒绝；钩子不能使不可读服务变得可读
- 钩子按完整 TypeKey + 有效 isolate scope 匹配；注册 fiber 位于读取方祖先链
- 替换只影响本次返回值，不改绑定身份；`ServiceAccess` 记录原绑定身份
- 同键同操作同步重入明确报错；不同键重入允许
- 钩子 panic 转为明确错误；shutdown 等已接纳钩子完成；卸载后自摘

写入拦截：
- `provide_mut_as` 返回 `ServiceWriter<T>`；旧代句柄、摘除中绑定、非 owner、类型/实例越界写入一律失败
- `ServiceWriter::set` 校验调用方为创建绑定的提供 fiber；提交时重查 Arc 身份、provider、generation 后原子替换
- 旧 `Arc<T>` 不被原地改写，旧持有者保持旧值
- 写入保持 provider、generation、依赖四元组与驱逐关系，不自动重载消费者
- 写入钩子按提供者祖先链筛选；panic 不提交候选值
- 普通不可替换服务不为可变槽增加读取锁
- `0decf15` 修复：替换的旧值在框架锁外 drop

命令（在 worktree 内执行）：

```sh
cargo +1.98.1 test -p rutis
cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings
cargo +1.98.1 fmt -p rutis -- --check
```

## 结果

- **结论：pass**（验收人：周文斌，2026-09-24；记录：[rev-cordis-intercept-03.md](../reviews/rev-cordis-intercept-03.md)）
- 被审 `4c1e160` + `0decf15`（diff `c9d4caf..0decf15`）；`cargo +1.98.1 test -p rutis` 223 passed / 0 failed（service_intercepts 8/8）；clippy、fmt 干净。
- 读取拦截 8 项、写入拦截 7 项全部通过；无阻塞问题。
- 非阻塞观察项 6 条：不同键重入无专门测试、摘除中绑定写入无专门测试、越界先拒未断言 hits==0、select 偶发 spurious begin/finish、has_any 注册/读取竞态（设计未要求线性化）、同键重入含 scope 维度。
