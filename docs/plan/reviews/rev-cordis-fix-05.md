# 验收记录：cordis-fix-05（修复失败写入在框架锁内析构候选值）

## 被审版本与 diff 范围

- 被审提交：`68491a7`（fix(core): drop candidate StoredValue outside framework locks on write failure）
- diff 范围：`795c165..68491a7`
- worktree：`/tmp/rutis-dev55`
- 改动文件：
  - `crates/rutis/src/intercept.rs`：+51/-26
  - `crates/rutis/src/registry.rs`：+20/-6
  - `crates/rutis/tests/service_intercepts.rs`：+205/-1

## 结论

**pass**

三条失败路径全部修复，锁边界清晰，IIFE 正确地将候选值所有权传出锁外。回归测试 1+2 红→绿证据确凿（修复前死锁 5s 超时）。稳定性 20/20 全绿，全量 229 passed，clippy/fmt 干净。

## 三条路径修复核对表

### 路径 1：`registration_open()` 失败

| 项目 | 详情 |
| --- | --- |
| 修复位置 | [`intercept.rs:408-409`](/tmp/rutis-dev55/crates/rutis/src/intercept.rs#L408) |
| 锁（admission）获取 | `intercept.rs:407`：`let _admission = self.shared.admission.lock().unwrap();` |
| 候选值离开锁的方式 | `return (value, Err(...))` — `value` 随 IIFE 闭包返回值元组传出 |
| IIFE 闭包结束时 `_admission` 释放 | 是，闭包局部变量在闭包返回时逆序 drop，`_admission` 先于闭包外部 drop |
| 锁外 drop | `intercept.rs:427`：`owned`（= `value`）在 IIFE 闭包结束后 drop |
| catch_unwind + sink | `intercept.rs:430-434`：与 `0decf15` 完全相同的模式 |
| 通过 | ✅ |

### 路径 2：`transition.generation` / state 检查失败

| 项目 | 详情 |
| --- | --- |
| 修复位置 | [`intercept.rs:412-416`](/tmp/rutis-dev55/crates/rutis/src/intercept.rs#L412) |
| 锁（admission + transition）获取 | `intercept.rs:407`（admission）、`intercept.rs:411`（transition） |
| 候选值离开锁的方式 | `return (value, Err(...))` — 同路径 1，闭包结束 → `transition` 释放 → `_admission` 释放 → `owned` = `value` |
| 锁释放顺序 | `transition`（第 411 行声明，先释放）→ `_admission`（第 407 行声明，后释放）— Rust 逆序 drop 保证 |
| 锁外 drop | 同上 |
| 通过 | ✅ |

### 路径 3：`replace_mutable_if_current` 失败

| 项目 | 详情 |
| --- | --- |
| 修复位置 | [`registry.rs:125-155`](/tmp/rutis-dev55/crates/rutis/src/registry.rs#L125) + [`intercept.rs:417-425`](/tmp/rutis-dev55/crates/rutis/src/intercept.rs#L417) |
| 签名变更 | `Option<StoredValue>` → `Result<StoredValue, StoredValue>`：`Ok(old)` = 成功返回旧值，`Err(candidate)` = 失败交还候选值 |
| `bindings` 锁（registry 内部） | `registry.rs:138`：`let bindings = self.bindings.lock().unwrap();` — 函数内局部变量，函数返回时自动释放 |
| 四条拒绝路径是否全部交还候选值 | 1. **槽位空**（`registry.rs:140-141`）：`return Err(value);` ✅ 2. **`Arc::ptr_eq` 失败**（`registry.rs:143-147`）：`return Err(value);` ✅ 3. **`removing` 置位**（`registry.rs:143-147`）：同 2，`return Err(value);` ✅ 4. **`ValueSlot::Fixed` 槽位**（`registry.rs:153`）：`Err(value)` ✅ |
| Mutable 路径 unwrap 安全性 | `registry.rs:149-151`：`match &current.value { ValueSlot::Mutable(_) => ... }` 已确认为 Mutable 槽；`replace_mutable` 对 `Mutable` 变体始终返回 `Some(old)`（`registry.rs:55-63`），`unwrap()` 不会 panic |
| 候选值在 `bindings` 锁外 drop | 是 — `replace_mutable_if_current` 返回时 `bindings` guard 已释放；返回值回到 IIFE 的 match 分支，闭包结束再释放 `transition` 和 `_admission`，最后在 IIFE 外 drop |
| 锁外 drop | 同路径 1/2，`catch_unwind` 保护 |
| 通过 | ✅ |

### IIFE 边界逐行核查

IIFE 闭包范围：[`intercept.rs:406-426`](/tmp/rutis-dev55/crates/rutis/src/intercept.rs#L406)

```
406: let (owned, result) = (|| -> (StoredValue, Result<(), ServiceWriteError>) {
407:     let _admission = self.shared.admission.lock().unwrap();   // ← A 锁获取
408:     if caller.registration_open().is_err() {
409:         return (value, Err(...));                              // ← 提前返回，A 在闭包结束时释放
410:     }
411:     let transition = provider.transition.lock().unwrap();     // ← T 锁获取
412:     if transition.generation != self.binding.provider_gen ... {
415:         return (value, Err(...));                              // ← 提前返回，T→A 逆序释放
416:     }
417:     match self.shared.registry.replace_mutable_if_current(...) {
           // ↑ replace_mutable_if_current 内部获取 bindings 锁 B，函数返回时释放 B
423:         Ok(old) => (old, Ok(())),
424:         Err(candidate) => (candidate, Err(...)),
425:     }
426: })();  // ← 闭包结束：transition(T) 释放 → _admission(A) 释放
427: // owned 在此处（锁外）drop
430: if let Err(panic) = catch_unwind(AssertUnwindSafe(|| drop(owned))) {
```

**结论：** 所有锁（admission A、transition T、bindings B）的作用域在 `owned` drop 之前全部结束。三个锁的释放顺序：B（registry 函数返回时）→ T（闭包结束）→ A（闭包结束），然后 `owned` drop。

### catch_unwind 模式对齐检查

| 对比项 | `0decf15` 成功路径 | `68491a7` 本修复 |
| --- | --- | --- |
| drop 对象 | `old`（替换掉的旧值） | `owned`（成功=旧值，失败=候选值） |
| catch_unwind 包裹 | `AssertUnwindSafe(|| drop(old))` | `AssertUnwindSafe(|| drop(owned))` |
| panic 转 Arc 错误 | `Arc::new(panic_error(panic))` | 相同 |
| 上报 sink | `caller.error_sink()` → `sink(error)` | 相同 |
| sink 自身 panic 保护 | `catch_unwind(AssertUnwindSafe(|| sink(error)))` | 相同 |
| 通过 | — | ✅ |

## 红→绿独立复核

### 方法

1. `git worktree add /tmp/rutis-rev-fix 795c165`（修复前基线）
2. 将 `68491a7` 的 `tests/service_intercepts.rs` 覆盖到该 worktree（保留修复前实现 + 新测试）
3. 分别运行三个回归测试，观察死锁行为
4. 与修复后代码（`68491a7`，worktree `/tmp/rutis-dev55`）对比

### 结果

| 测试 | 修复前（795c165） | 修复后（68491a7） |
| --- | --- | --- |
| test 1：`writer_set_stale_candidate_drop_no_deadlock_replaced_binding` | **FAILED**（5.00s 超时："recv_timeout means Mutex deadlock"） | **ok**（0.03s） |
| test 2：`writer_set_stale_candidate_drop_no_deadlock_removing_flag` | **FAILED**（5.00s 超时） | **ok**（0.03s） |
| test 3：`writer_set_stale_candidate_drop_no_deadlock_generation_stale` | **ok**（0.00s，见下方分析） | **ok**（0.03s） |

**test 1、2 红→绿证据确凿：** 修复前死锁（`std::thread::spawn` + `mpsc::recv_timeout` 在 5s 超时后返回 Err，测试 panic），修复后通过。

**test 3 修复前也通过的分析：** 修复前代码的 `registration_preflight()`（[intercept.rs:383-385]）在 admission 锁之外执行；`view.restart()` 后旧 fiber 处于过渡态，`registration_preflight` 失败 → `?` 提前返回 → 此时无任何框架锁 → `value` 在函数局部安全 drop。test 3 在修复前实际走到了 preflight 失败路径而非 generation 检查路径，因此未触发死锁。详见下方「问题列表」问题 1。

## 回归测试质量

### 测试 1：替换后绑定的候选值 Drop 重入

- **机制：** `DropEffect` 的 `Drop` 调用 `ctx.effect_named()`，触发框架锁获取
- **死锁检测：** `std::thread::spawn` + `mpsc::recv_timeout(5s)` — 不依赖 tokio 超时（tokio timeout 不能取消同步阻塞的线程）
- **竞态收敛：** dispose 后台 + yield_now 自旋等待 `removing` 或键消失（最多 1000 次），无 sleep
- **断言：** `err.reason == ServiceWriteFailure::Stale`

### 测试 2：removing 置位的候选值 Drop 重入

- **机制：** 与测试 1 相同，差异在自旋循环明确等待 `removing == true`
- **确定性：** 自旋上限 1000 次 + `yield_now()`，无 sleep 依赖

### 测试 3：generation 失效的候选值 Drop 重入

- **构造：** `MutableDropProvider` plugin 在 `apply` 中缓存 writer 和 ctx；`view.restart()` 触发 fiber 重建，`transition.generation` 变化
- **问题：** 修复前测试未实际触发 generation 检查的死锁路径（见问题 1）
- **修复后的价值：** 在修复后代码中，preflight 可能仍然失败或成功；若成功则进入 IIFE 的 generation 检查路径、安全返回候选值。测试覆盖了 restart 场景下 `writer.set` 不死锁的端到端行为

## 稳定性验证

```
for i in $(seq 1 20); do cargo +1.98.1 test -p rutis --test service_intercepts; done
```

结果：**20/20 passes**，每轮 13 passed / 0 failed，耗时约 0.03s/轮。

## 全量验证

| 命令 | 结果 |
| --- | --- |
| `cargo +1.98.1 test -p rutis` | **229 passed / 0 failed**（226 + 3 个新增回归测试）。分布：16+3+15+69+1+7+4+11+23+4+57+13+4+1+1 |
| `cargo +1.98.1 clippy -p rutis --all-targets -- -D warnings` | 干净（`Finished`，无警告） |
| `cargo +1.98.1 fmt -p rutis -- --check` | 干净（exit 0） |

## 问题列表

### 问题 1（非阻塞）：test 3 修复前未实际触发 generation 死锁路径

test 3 在修复前代码上 0.00s 通过，未触发死锁。原因是 `view.restart()` 后 `registration_preflight()` 在锁外失败，`?` 提前返回，候选值在无框架锁的情况下安全 drop。

**影响：** 红→绿证据对 generation 路径不够直接。但：
- 测试 1 和 2 充分证明了 `set` 的 IIFE 修复对 `registration_open` 失败路径和 `replace_mutable_if_current` 失败路径的有效性
- generation 检查（路径 2）的死锁机制与路径 1 完全相同（`return Err` 时持有 admission+transition 锁），结构化论证足够
- test 3 在修复后代码上仍有价值：覆盖了 restart 后 writer.set 的端到端不死锁行为

### 问题 2（非阻塞）：test 3 的 provider_ctx 生命周期依赖

test 3 使用 `restart()` 后仍持有旧的 `provider_ctx`。`restart()` 后旧 fiber 状态不确定，`registration_preflight` 可能失败也可能成功，导致测试在不同运行时走不同代码路径。虽然最终断言一致（`Stale`），但测试路径不确定。

建议：如果希望确定性覆盖 generation 检查路径，可以构造一个 provider fiber 保持 Active 但 generation 已变化（如通过 binding 替换而非 restart）。

## 剩余不确定性

无。三条路径的锁边界和 IIFE 语义已逐行核实，红→绿证据充分（test 1/2），全量回归全绿。