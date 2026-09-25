# 验收记录：cordis-tests-04（补三个验收覆盖缺口测试）

## 被审版本与 diff 范围

- 被审提交：`795c165`（test(core): add observer reentry, cross-key reentry, and eviction-write tests）
- diff 范围：`0decf15..795c165`
- worktree：`/tmp/rutis-dev55`
- 改动文件（仅测试，无实现代码）：
  - `crates/rutis/tests/dispatch_observation.rs`：+48 行
  - `crates/rutis/tests/service_intercepts.rs`：+93 行
  - 合计：2 files, +141 insertions

## 结论

**pass**

三个新测试均真实覆盖此前验收记录的缺口语义，断言与实现路径一致，不空转。稳定性验证（service_intercepts 连续 20 次）全部通过。全量回归、clippy、fmt 均干净。测试风格与既有文件一致。

## 逐测试核对表

### 缺口 1：观察器重入投递（来源：rev-cordis-dispatch-01 问题 1）

| 项目 | 详情 |
| --- | --- |
| 测试名 | `observer_reentry_observes_nested_dispatch_and_business_listener_runs` |
| 文件 | `crates/rutis/tests/dispatch_observation.rs` |
| 覆盖语义 | 观察器闭包内再次 `emit`，断言嵌套投递也被观察、不死锁、内层业务监听器执行 |
| 断言证据 | 1. `attempts == vec![1, 2]`：外层 `serial(Ping(1))` 和内层 `emit(Ping(2))` 均被观察器记录 2. `result.unwrap().is_none()`：外层 serial 发现 hooks 已被内层 emit 取走，返回 None（非空转） 3. `inner_hits == 1`：业务监听器 Count 在嵌套 emit 尾任务中执行 4. `tokio::time::timeout(Duration::from_secs(5), ...)`：防止死锁 |
| 代码路径验证 | `observe_attempt`（`bus.rs:366-371`）在锁外调用观察器回调；观察器内 `emit`→`emit_keyed_inner`（`bus.rs:667-722`）可重新获取 admission+inner 锁，不会死锁。外层 serial 的 `take_hooks`（`bus.rs:872`）在观察器返回后执行，此时 hooks 已被内层 emit 取走，正确返回 None |
| 通过 | ✅ |

### 缺口 2：不同键重入允许（来源：rev-cordis-intercept-03 观察项 1）

| 项目 | 详情 |
| --- | --- |
| 测试名 | `different_key_reentry_allowed_for_read_and_write` |
| 文件 | `crates/rutis/tests/service_intercepts.rs` |
| 覆盖语义 | 钩子 A 执行中读/写另一个有钩子的键 B，应成功而非 `InterceptReentrant`；read 与 write 各覆盖 |
| 断言证据 | **Read 场景**：1. 键 A 的 require hook 内 `require_as::<u64>(key_b)` 成功（不报 InterceptReentrant） 2. `b_read_hits == 1`：键 B 的钩子确实执行（替换 +10） 3. `b_value_seen == Some(12)`：键 B 原始值 2 + 钩子替换 10 = 12 **Write 场景**：4. 键 C 的 set hook 内 `writer_d.set(30)` 成功 5. `d_write_hits == 1`：键 D 的钩子确实执行（替换 +100） 6. `*root.get_as(key_d) == 130`：30 + 100 = 130 |
| 代码路径验证 | `ReentryGuard::enter`（`intercept.rs:202-210`）按 `(HookKind, HookKey)` 精确判重，不同键有不同 `HookKey`，不会匹配同一条目，实现正确 |
| 通过 | ✅ |

### 缺口 3：摘除中绑定写入失败（来源：rev-cordis-intercept-03 观察项 2）

| 项目 | 详情 |
| --- | --- |
| 测试名 | `writer_set_fails_stale_during_binding_removal` |
| 文件 | `crates/rutis/tests/service_intercepts.rs` |
| 覆盖语义 | `binding.removing` 置位但槽位未替换时 `writer.set` 返回 `Stale` |
| 断言证据 | `err.reason == ServiceWriteFailure::Stale`：无论竞态落在「removing 置位」还是「槽位已空」，均返回 Stale |
| 稳定性分析 | 1. `dispose()` 触发 `evict_and_finalize`，其首条同步语句 `mark_removing_if`（`ctx.rs:1144`）在首个 `.await` 前执行，确保 removing 快速置位 2. 测试用 `yield_now()` 自旋最多 1000 次检测 diagnostics 中 `removing` 或键消失，无 sleep，时序确定性高 3. 两种竞态结局（removing=true → `replace_mutable_if_current` line 138 拦截；键消失 → `Arc::ptr_eq` line 137 拦截或 `registration_preflight` line 384 拦截）均映射到同一 `Stale` 错误码，断言不依赖特定微时序 |
| 代码路径验证 | `replace_mutable_if_current`（`registry.rs:128-143`）先 `Arc::ptr_eq` 比对，再检查 `removing` 原子标记；任一条不满足返回 None → `set` 路径 `ok_or_else(|| Stale)`（`intercept.rs:415-416`） |
| 通过 | ✅ |

## 稳定性验证

service_intercepts 测试文件连续运行 20 次，全部通过（10/10 passed × 20 runs），无偶发失败。

```
for i in $(seq 1 20); do cargo +1.98.1 test -p rutis --test service_intercepts; done
→ 20/20 passes, 0 failures
```

## 验证命令与实际结果

工具链：`1.98.1-x86_64-unknown-linux-gnu`

| 命令 | 结果 |
| --- | --- |
| `cargo +1.98.1 test -p rutis` | **226 passed / 0 failed**（16+3+15+69+1+7+4+11+23+4+57+10+4+1+1 doc-test）。其中 `service_intercepts` 10/10、`dispatch_observation` 7/7 |
| `cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings` | 干净（`Finished`，无警告） |
| `cargo +1.98.1 fmt -p rutis -- --check` | 干净（exit 0） |

## 代码质量

- 命名风格：snake_case 异步测试函数，与既有测试一致（`observer_reentry_...`、`different_key_reentry_...`、`writer_set_fails_stale_...`）
- 辅助类型：复用文件已有定义（`Ping`/`Count`、`Capture`），未引入新类型
- 断言方式：`assert_eq!` + `unwrap_err().reason` 匹配模式与既有测试一致
- 竞态处理：使用 `yield_now()` 自旋循环 + 上界保护，与既有测试（如 `shutdown_waits_for_selected_synchronous_observer` 的 `for _ in 0..100` 循环）模式一致
- 依赖：未引入新 crate 依赖，所有类型已在文件级 `use` 中导入
- 死锁防护：重入测试用 `tokio::time::timeout` 包裹，防止实现 bug 导致测试挂死，合理的防御性实践

## 观察项（非阻塞）

无。

## 剩余不确定性

无。三个缺口均已由对应测试覆盖，断言与实现代码路径吻合，验证命令全绿。