# 原生 Host 依赖图

Rust `host::{HostGraph,HostProxy}` 把同一个冻结计划的 instance 变为实际 rutis
fiber。`HostGraph` 使用 `DeploymentObjects` 的权威 broker，并为 Host 自己建立
一条真实私有帧连接和 `RuntimeObjects` SDK；原生 TypeKey 下保存真实 `ObjectProxy`。
它实现首代成员的发布与关闭，并可挂载实际 [Host 原生适配器](native-adapters.md)；
尚不拥有 Linux 进程监督、快照租约或恢复换代。

## 挂载与发布

1. `HostGraph::new(objects, host_identity)` 创建 Host SDK epoch；runtime 名必须
   与远端成员不同。`mount(parent, instance, hello_identity)` 校验完整 frozen group
   身份，注册原生依赖：精确 RuntimeReady key 与每条 prepared required route key。
   可以在 hello/RuntimeReady 前挂载。缺路由和未发布的 provider 保持 Pending，
   不分配 activation，不发送 start，不运行远端业务代码。
2. 原生依赖齐备后，apply 捕获实际注入的 `Arc<ObjectProxy>`，核对 frozen provider
   的 owner、原始对象、bundle、interface 和完整 source。随后按 Host 顺序分配
   remote activation 与独立 Host SDK activation。回滚 effect 在原始本代 Ctx 中
   注册，早于任何根签发或 start。
3. `DeploymentObjects.offer_required` 交付整张表；start 请求同步入队，receipt
   等待真实 Accept ACK 和远端 native Ready。完整 provides 表必须通过 stage 校验。
   各个成员独立推进，同组某个成员 Pending/Loading 不构成组级发布屏障。
4. Host 为启用的 exports 复制保留清单的独立 mirror stage，经过同一个 broker
   签发和真实 SDK Accept。mirror 保持实际对象和 dispatcher，只使用 Host 选择的
   独立 source；普通调用仍要求 owner Published。导入整表后才 bind 原始 Host Ctx。
   实际原生 export key 注册 `ObjectProxy`，availability 保持 false。
5. 独立发布任务观察 native 状态到 Active，再发远端 activate。初次 native await
   的 Pending 结果不是启动失败。只有精确 activate ACK、gate 仍开放及 Host native
   仍 Active 同时成立，才发布 broker/SDK 成员、设置 availability 并 `ctx.refresh()`。
   消费者由真实原生依赖通知启动；不手工触发另一成员 apply。

`HostProxy::ready()` 等待这条发布链；`service::<GeneratedClient>(wire_name)` 从实际
原生 ObjectProxy slot 绑定生成客户端与本代 SDK caller。`captured()` 可核对本代
注入的实际 Arc 身份。旧客户端、返回对象及原始 Ctx 都保留自己的代，不重新查找
新 provider。

rutis 的可选 `get_as` 可以读到尚未可用或仍待 disposer 摘除的注册值。因此 mirror
代理的 `delivery()`、方法和缓存属性另检查同一个 availability；属性中的对象也
继承检查。原生依赖检查决定 Pending，SDK 检查阻止通过可选读取绕过发布屏障。

## 同步关闭与确认

`HostProxy::stop()` 在调用点关闭 availability 和 gate，封闭原始 Host Ctx 的 effect
及 child 准入，并同步排入远端 stop；返回 future 只负责 join。即使 Host apply 正
等待 start、原生 cleanup 尚未排干或 stop waiter 被丢弃，远端关闭也已开始。
同一配对状态锁保证 stop 不越过尚未发送的 start；没有管理锁跨 peer await 或业务
清理。Loading start 的关闭结果保存为 ready 失败，实际 cleanup 仍独立确认。

权威 Host member 关闭在 broker 锁外同步调用弱租约：provider 原生代理与已经捕获
它的消费者同时封闭，不等 revoke/stop ACK。runtime epoch 断连也封闭尚未进入 apply
的 Pending 代理。迟到 activate ACK 不得重开 gate、availability 或原生依赖。
停止结果核对精确 activation/instance 与 Stopped phase，等待 broker 撤销和本地 SDK
Release ACK，并缓存供重复 join。已断连的 epoch 使用其同步撤销证据，不等待它
回复新的 revoke；远端 native stop 未确认仍是失败，不能据此放行恢复。

`HostGraph::shutdown()` 先关闭所有成员，再 join 远端确认和实际原生清理；成功后
关闭 Host SDK 私有连接。失败仍可观察，不作为恢复证明。同一 instance 不能重新
mount；监督者的 cleanup/reaping 屏障与新代 API 尚待 M3。嵌入者须独立持有快照，
直到原生清理和实际子进程退出都确认，不能把关连接当作 OS reaping。

## 实际证据与剩余范围

Linux `native_runner_ipc` 从已删除源包的 frozen snapshot 启动真实 Rust image 和
编译后的通用 Node runner，每个 runtime 有独立 provider/consumer 包。HostGraph
挂载四个原生代理；真实 Rust activate ACK 被延迟时，Host provider 虽 Active，
原生键与 SDK 仍不可调用，Node consumer 保持 Pending。另一对成员独立发布，放行
ACK 后 native refresh 启动消费者；捕获 Arc 与实际原生 slot 相同。

另一用例分别延迟两端 required Accept ACK，在 Host Loading 时发 stop，观察远端
Closing 且无构造/import；丢弃首个 stop waiter 后，重复 stop 仍 join 同一清理。
它还在 Node activate ACK 被延迟时完成 stop，再投递旧 ACK，
确认旧 Ctx 和 availability 不恢复；断连封闭 missing-route Pending 代理。用例核对
实际 native 清理计数与两个子进程正常退出，最后释放 snapshot。

这些用例覆盖首代 instance 路由的 Host 发布与关闭；另一个 native adapter 用例
验证实际原生 slot 安装。内部 child 对象的精确 creator Ctx、共享/单独完整拓扑、快照监督租约、更新
恢复、Linux descendant reaping、broker 事件、真实旧桥迁移与完整 T24 仍需验收。
