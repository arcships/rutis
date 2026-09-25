# 验收记录：cordis-intercept-03（PR #55 严格服务读取与提供者写入拦截）

## 被审版本与 diff 范围

- 被审提交：`4c1e160`（feat: intercept strict reads and mutable service writes）+ `0decf15`（fix: drop replaced service values outside framework locks）
- diff 范围：`c9d4caf..0decf15`（叠放在 PR #54 之上）
- worktree：`/tmp/rutis-rev55`，checkout 顶点 `0decf15`
- 本层新增：约 1123 行（intercept.rs 新增 481 行；ctx.rs +140、registry.rs +64、error.rs +45、lib.rs +7、README +2；tests/service_intercepts.rs 新增 434 行）

## 结论

**pass**

实现完整覆盖设计文档 §3 的读取拦截与写入拦截两组标准。所有验证命令通过，测试覆盖设计验收场景（少数正行为缺专门断言，见观察项，不阻塞）。

## 验收标准逐条核对表

### 读取拦截

| 标准 | 覆盖证据（测试 / 代码） | 结论 |
| --- | --- | --- |
| `get/get_as` 不进拦截链（绕过钩子） | [ctx.rs:445-475] `get_as` 直接走 `read_binding_as`，不调用 `apply_read_interceptors`；测试 `strict_reads_chain_and_locator_bypass_with_recorded_denial`（`get_as` 后 `seen==0`） | 通过 |
| `require/require_as` 钩子 `Continue \| Replace(Arc<T>) \| Deny` 语义正确 | [intercept.rs:22-27] `ServiceIntercept<T>` 三变体；[intercept.rs:162-175] `call` 闭包按变体映射；测试覆盖 replace（`11`）、deny（`InterceptDenied`）、continue（重入钩子返回 Continue） | 通过 |
| 未声明、越界、未就绪先于钩子拒绝；钩子不能使不可读服务变得可读 | [ctx.rs:520-579] 顺序为 OutOfScope→TypeMismatch→Inactive→Undeclared→Unavailable，全部在 `apply_read_interceptors`（[ctx.rs:639]）之前；拦截只在 `value == Some` 时运行；测试 `strict_read_checks_reject_before_interceptor_runs`（Undeclared / Unavailable 且 `hits==0`） | 通过 |
| 钩子按完整 TypeKey + 有效 isolate scope 匹配；注册 fiber 位于读取方祖先链 | [intercept.rs:31-33] `HookKey=(TypeKey, Option<ScopeId>)`；注册时 `(key, scope_for(&key))`（[intercept.rs:233]），读取时 `(key, scope.cloned())`（[intercept.rs:283]）；[intercept.rs:88-110] `select` 沿 `weak_fiber` 父链构建 ancestry 并过滤 owner；测试 `hooks_match_full_key_scope_and_reader_ancestry`（兄弟 fiber 隔离、isolate left/right 隔离、instance key 越界） | 通过 |
| 替换只影响本次返回值，不改绑定身份；`ServiceAccess` 记录原绑定身份 | [ctx.rs:639-647] 拦截结果仅作为返回值，不写回 `Binding`；[ctx.rs:648-661] `record_access` 使用 `found`（原 provider/gen）；测试断言 `access.provider.is_some()` 且 `failure==InterceptDenied` | 通过 |
| 同键同操作同步重入明确报错；不同键重入允许 | [intercept.rs:137-158] `ReentryGuard` 用 thread-local `ACTIVE_HOOKS` 按 `(kind, (TypeKey, scope))` 判重；测试 `strict_reads_chain...` 重入段（`InterceptReentrant`）与写重入段（`InterceptReentrant`） | 通过（不同键重入见观察项 1） |
| 钩子 panic 转为明确错误；shutdown 等已接纳钩子完成；卸载后自摘 | panic：`catch_unwind`→`InterceptPanicked`（[intercept.rs:118-123]）；shutdown 等待：`HookFlight` 对 owner 调 `begin_event`/`finish_event`（[intercept.rs:130-150]），测试 `subtree_shutdown_waits_for_selected_read_hook`；自摘：effect `AsyncDisposer` 在 admission 锁下 `remove`（[intercept.rs:246-259]），测试 `repeated_hook_disposal_reclaims_tables`、`thousand_child_shutdowns_reclaim_instance_hook_keys` | 通过 |

### 写入拦截

| 标准 | 覆盖证据（测试 / 代码） | 结论 |
| --- | --- | --- |
| `provide_mut_as` 返回 `ServiceWriter<T>`；旧代句柄、摘除中绑定、非 owner、类型/实例越界一律失败 | [ctx.rs:775-786] `provide_mut_as`；[intercept.rs:367-388] `set` 依次校验 shared 同一、provider 升级、`Arc::ptr_eq(actor,provider)`、scope、`in_instance_key`、preflight；测试 `mutable_writer_respects_owner_generation_and_write_hooks`（非 owner、旧代 Stale）、`hooks_match_...`（instance 越界 WrongOwner） | 通过（摘除中见观察项 2） |
| `set` 校验调用方为创建绑定的提供 fiber；提交时重查 Arc 身份、provider、generation 后原子替换 | [intercept.rs:376-386] `Arc::ptr_eq(&actor, &provider)`；[intercept.rs:402-419] admission 下 `registration_open` + `transition.generation` + state 复查；[registry.rs:128-143] `replace_mutable_if_current` 用 `Arc::ptr_eq` + `removing` 判定后 `mem::replace` | 通过 |
| 旧 `Arc<T>` 不被原地改写，旧持有者保持旧值 | [registry.rs:55-63] `replace_mutable` 用 `std::mem::replace` 换槽，返回旧值；测试 `mutable_writer_...`（`*old==1` 写入后不变） | 通过 |
| 写入保持 provider、generation、依赖四元组与驱逐关系，不自动重载消费者 | [intercept.rs:367-427] `set` 仅改值槽，不触碰 `provider_id/provider_gen/last_deps`，不调 `notify_key_changed`；测试断言 `view.state().state == Active`（写入不驱逐） | 通过 |
| 写入钩子按提供者祖先链筛选；panic 不提交候选值 | [intercept.rs:393-401] `run(Write, (key, scope), caller, ...)`，actor=provider（已校验），`select` 按 provider 祖先链筛选；测试 `hooks_match_...`（root 写不入 a 的钩子，`write_hits==0`）、`mutable_writer_...`（panic→`InterceptPanicked` 且值仍 13） | 通过 |
| 普通不可替换服务不为可变槽增加读取锁 | [registry.rs:42-64] `ValueSlot::Fixed`（无锁）/`Mutable`（有锁）二分；`provide_as` 走 `mutable=false`（[ctx.rs:762-763]） | 通过 |
| `0decf15` 修复：替换的旧值在框架锁外 drop | [intercept.rs:412-427] `old` 在 `drop(transition); drop(_admission)` 之后 drop，并 `catch_unwind` 捕获 user Drop panic 上报 sink；测试 `replaced_value_drop_can_read_registry_after_commit`、`replaced_value_drop_panic_reports_to_sink_after_commit` | 通过 |

## 验证命令与实际结果

工具链：`1.98.1-x86_64-unknown-linux-gnu`（已安装；仓库无 `rust-toolchain.toml`，用 `+1.98.1` 显式指定）。

| 命令 | 结果 |
| --- | --- |
| `cargo +1.98.1 test -p rutis` | 通过：**223 passed / 0 failed**（222 单元+集成 + 1 doc-test）。其中 `tests/service_intercepts.rs` 8/8 通过 |
| `cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings` | 干净，无警告（`Finished`） |
| `cargo +1.98.1 fmt -p rutis -- --check` | 干净（exit 0） |

## 问题列表

无阻塞问题。

## 观察项（非阻塞）

1. **「不同键重入允许」无专门测试**：设计/任务验收标准明确列出「不同键重入允许」，但 `service_intercepts.rs` 只覆盖同键重入报错（read/write 各一处）。[intercept.rs:137-158] 的 `ReentryGuard` 按 `(kind, key)` 判重，不同键天然不冲突，实现正确，仅缺正向断言。
2. **「摘除中绑定写入失败」无专门测试**：`set` 中摘除中绑定经 `replace_mutable_if_current` 的 `removing` 检查（[registry.rs:137-139]）返回 Stale；现有 `old_writer_cannot_commit_after_binding_is_replaced_during_hook` 覆盖的是「钩子运行期间绑定被替换」路径，未单独覆盖「binding 置 `removing` 但尚未被替换」路径。逻辑正确，缺专门断言。
3. **「越界先于钩子拒绝」无显式断言 `hits==0`**：`hooks_match_...` 测了越界读返回 `OutOfScope`，但未断言钩子未执行（代码顺序保证 OutOfScope 在拦截前返回）。非阻塞。
4. **`select` 存在一次偶发的 spurious begin/finish**：当 `has_any` 为真但该键无匹配钩子时，`select` 仍会对 actor fiber `begin_event` 后立即 `finish_event`（[intercept.rs:95-110]、[intercept.rs:113-116]）。仅轻微开销，不影响正确性。
5. **`has_any` 与表锁的注册/读取竞态**：`has_any` 为 false 时 `run` 早退，不锁表；若并发插入恰好落在 `push` 与 `fetch_add` 之间，本次读取可能漏跑新钩子。与 EventBus 监听器注册同属「注册与并发读取不强一致」的边界，设计未要求线性化，非阻塞。
6. **「同键重入」按 `(TypeKey, scope)` 判定**：`HookKey` 含 scope，同 TypeKey 在不同 isolate scope 下的重入不算「同键」。与 scope 作为有效匹配键的语义一致，非阻塞。

## 关键设计点核对（附证据）

- 拦截器是可信扩展点：`Replace` 可注入兄弟实例同类型值，是设计接受的边界（[design §3 第 99 行]），实现未做来源证明，符合设计声明。
- 写入比 Cordis `internal/get/set` 更严格：rutis 拦截器不能越过类型/实例/依赖声明边界（[ctx.rs:520-579] 检查先于钩子）。
- 钩子注册/撤销遵守实例子树边界：注册走 `check_instance`（[intercept.rs:226]），effect 归 owner fiber 所有（[intercept.rs:242-259]）。
