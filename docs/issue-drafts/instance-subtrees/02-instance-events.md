# feat(core): 增加实例事件及在途派发归属

状态：已发布为 [#23](https://github.com/arcships/rutis/issues/23)。对应 R2，依赖 [实例键](01-instance-keys.md)。设计依据：[完整设计 §3](../../design-instance-subtrees.md#3-实例事件)。

## 问题

现有动态事件通道可以分流，但没有实例子树注册/发送限制，也没有能让子树关闭等待相关在途派发的归属记录。仅删除监听器和尾链表项不能停止已取得快照的回调。

## 范围

1. 新增 on_instance、emit_instance、serial_instance、parallel_instance。注册者与发射者均须处于实例子树内并使用该 root 的总线。
2. emit_instance 同步返回 Result，Ok 表示已接纳，异步回调错误走 ErrorSink；serial/parallel 在首次 poll 时接纳，结果沿原分发语义返回。
3. 保留现有 TypeKey 索引及 named 通道。实例变体构造无 qualifier 的实例键；不增加实例 waterfall、once 或 named+instance 组合入口。
4. 校验、快照和 emit 尾链链接有统一提交顺序。相同类型和实例的 emit 保序，不同实例独立。serial 只保证单次调用内的监听顺序。
5. 为已接纳派发记录目标实例、发射 fiber、监听器注册 fiber。按 fiber 保守排干跨代派发，不按代次拆分在途计数。提供给子树关闭使用的停止准入与排干能力；关闭子节点不关闭祖先实例供兄弟使用的整个通道。
6. 明确 drop/panic/error 下的归属：emit 后台任务有所有者；借用 serial 的 future 析构后释放凭据；parallel 子任务全部结束或取消并 join 后才释放凭据。
7. 实例监听器注销/重载须排干其旧代在途引用。文档说明回调可以发起关闭，但不能等待包含自身的关闭或自身注销。

对回调参数 Ctx 的既有语义不变：来自发射者，监听器的资源归注册者。不将用户任意 spawn 的任务纳入总线保证。

## 验收

- [ ] 无实例、不同实例、原 named 通道互不串扰；越界注册和发送均失败且没有副作用。
- [ ] A 阻塞时 B 继续；同实例 emit 的并发接纳顺序确定；serial 短路和 parallel 错误聚合保持一致。
- [ ] 关闭准入与发送/注册竞争：请求要么进入在途集合被等待，要么得到 Closed，没有遗漏。
- [ ] serial future 被 drop、parallel future 被 drop、回调 panic/失败不泄漏计数或后台回调。
- [ ] 排干完成前监听器捕获和尾链仍有明确所有者，完成后可释放。
- [ ] 原有 event_keys、dispatch_chain_probe、parity 等流程继续通过。

本项建立事件侧关闭协议；与公开 FiberView::shutdown 的完整联动在第三个 issue 验收。
