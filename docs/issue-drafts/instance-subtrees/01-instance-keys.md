# feat(core): 增加 fiber 实例键与子树可见性

状态：已发布为 [#22](https://github.com/arcships/rutis/issues/22)。对应 R1；设计依据：[完整设计 §2](../../design-instance-subtrees.md#2-身份和服务键)。

## 问题

现有 TypeKey 能表达类型和限定名，但不能表达一个服务只允许某个 fiber 子树提供和读取。调用方必须自行配置 isolate，框架无法从实例键核对访问边界。

## 范围

1. 新增私有字段的 InstanceId(NonZeroU64)，Copy/Eq/Hash/Debug。创建 fiber 时分配，进程内不复用，重载保持稳定；Ctx::instance 返回本 fiber 身份，isolate 派生上下文共用身份。
2. TypeKey 增加实例字段，保持 Clone；新增 instance::<T>(id)、with_instance(id)、instance_id()。保留所有既有限定名 API。Eq/Hash 包含实例，describe 展示实例。
3. get_as、依赖门控、提供入口共用实际祖先链校验；越界提供返回 InstanceOutOfScope，越界读取/门控视为不可解析。跨 root、旧 ID 不能绕过限制。
4. 建立根共享准入设施和可清理的 Weak children，服务提交与生命周期检查在同一同步边界。元数据等用户代码不在准入锁中执行。
5. `get_as` 越界时返回 `None`，依赖门控视为未满足；`require_as` 直接返回结构化越界错误。不维护读取历史或诊断快照。

不引入 SessionId/BranchId，不禁止 TypeKey::of::<T>()，不声称漏写 ID 会编译失败。保留原 provider/generation/key/scope 驱逐身份。

## 验收

- [ ] 两个实例提供同类型服务，各自子树可读，外部和兄弟不可读；嵌套实例可读取合法祖先实例。
- [ ] 越界 provide_as/provide_as_with_check 不插入绑定、不唤醒消费者；门控越界不调用外部 check。
- [ ] 声明依赖中的越界保持 Pending，实际严格读取返回 OutOfScope，且不泄露外部 provider 信息。
- [ ] 静态/动态限定名与实例字段组合的 Eq/Hash/describe 正确，旧键行为不变。
- [ ] ID 跨重载不变，跨新建 root/fiber 不复用；关闭后旧 Ctx 的 instance() 不 panic、不复活 fiber。
- [ ] 进程服务重载触发两个实例中的依赖消费者；实例服务重载不影响兄弟实例。
- [ ] Weak children 不因创建后失败或普通 dispose 留下历史条目。
- [ ] 原 contract/parity/config_update 流程通过，test/clippy/fmt 按设计文档执行并记录结果。

## 关联与交付

交付上游代码、流程测试、rustdoc；消费仓库的 vendor 回灌与来源记录由其迁移任务负责。
