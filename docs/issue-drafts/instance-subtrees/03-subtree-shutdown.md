# feat(core): 实现子树永久关闭与完整回收

状态：已发布为 [#24](https://github.com/arcships/rutis/issues/24)。合并 R4、R3，依赖 [实例键](01-instance-keys.md) 与 [实例事件](02-instance-events.md)。设计依据：[完整设计 §4](../../design-instance-subtrees.md#4-子树永久关闭与回收)。

## 问题

当前框架没有独立的子树永久关闭入口来同时保证同步停止准入、相关事件排干、driver 退出、错误归属和父级记录回收。已有普通 dispose 的通知点早于部分回收，不能直接作为新 shutdown 的完成屏障。

## 范围

1. 新增 FiberView::shutdown() -> BoxFuture<'static, Result<(), Arc<CordisError>>>。非 root 只关闭本子树，root view 委托现有 root shutdown；Ctx::shutdown 的 root 范围不变。
2. 调用点同步关闭子树准入并预取消 token；与登记、新代 token 发布互斥。协调任务独立拥有清理，丢弃等待方不停止它；成员不能再 restart/update/reload。
3. 保留在途 apply 的资源清理登记，等待 apply 与相关实例派发结束后，再按依赖/拥有关系卸载。晚到回滚任务也纳入完成屏障。
4. 子树内消费者直接终止，不经过新增的 Pending/load；外部消费者仍重查重载。服务移除晚于依赖消费者的清理。
5. 分离内部清理完成与公开关闭完成；子节点从父级自摘不通过 mount 清理等待自身。父子并发关闭认领同一任务，不产生互等。
6. 依赖边只排序，拥有边只汇总一次。父级认领与子节点脱离互斥；独立关闭并已脱离的失败结果不作为永久历史寄存在父级。
7. 复用已有 mount 自摘、inject_index 注销、空事件表删除与稀疏收缩，补齐 children 与实例在途记录回收。driver 真正退出并完成回收之后，才发布缓存结果。

## 验收

- [ ] shutdown 调用后即拒绝新插件、服务、监听器和实例事件；无需先 poll 返回 future。
- [ ] Loading、Pending、Active、Failed、已 dispose 的节点都能收敛，关闭不产生新的装载。
- [ ] 子树内消费者先于提供者结束，外部消费者可在后续 provider 恢复后重新装载；兄弟子树可继续工作。
- [ ] 在途回调阻塞时关闭不提前成功；释放回调后关闭完成。回调发起关闭然后返回无死锁。
- [ ] shutdown 与 dispose/restart/update、父级关闭、独立子级关闭竞争时，所有等待者均能结束。
- [ ] 迟到资源登记被清理；一处清理失败不跳过其他清理。并发等待者得到同一缓存错误 Arc，依赖边不重复汇总。
- [ ] 1000 轮创建/关闭后，释放外部句柄，mount/children/inject_index/绑定/实例事件条目和 driver 回到基线，容量不随轮数持续增长。
- [ ] 含常驻兄弟、关闭失败、重复关闭、丢弃等待 future 的回收流程同样通过。
- [ ] 原 root dispose/restart/shutdown、无实例事件、parity 和生命周期流程不回归。

## 验证和边界

执行完整设计中的 cargo +1.98.1 test/clippy/fmt 命令。内部计数可作为流程测试辅助；不把 RSS 必须精确恢复作为断言。vendor 独有的 lifecycle_diagnostics.rs 须明确哪些适用断言已迁入上游，不能报告未执行的消费仓库测试通过。

不承诺中断不协作的同步代码，也不负责业务终态事件、Session 装配迁移和真实业务压测。超时只结束等待，不伪报关闭成功。
