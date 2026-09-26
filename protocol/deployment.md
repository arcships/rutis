# 冻结计划中的根交付

Rust `deployment::DeploymentObjects` 持有完整 `PreparedDeployment`，从冻结的 bundle
原字节创建 `HostObjects`，选择每个消费者的完整 required 表。它是生产 Host 图的
路由装配层；完整 HostProxy、默认 service-capable driver 与监督者仍待接入。

## Host 顺序

1. `reserve(instance, activation)` 按 Host 顺序分配本代成员。每个 prepared group
   绑定一个 runtime/epoch，其他组不能借用同一 runtime 名称。当前只支持首代保留；
   监督者尚未给出 cleanup/reaping 证明，因此没有 unchecked rebind API。
2. native Ready 后，把 runner 报告的完整 `ServiceTable` 交给 `stage`。检查完整
   provides 名称、精确契约、真实 owner 和每项原始 source；不能把一个服务的图
   冒充另一个服务。未启用的 provides 仍须满足声明，但不进入授权路由。
3. Host 原生代理成为 Active 后才调用 `publish` 并完成远端 activate。两端各自的
   发布屏障仍独立；此对象层不会把简单的 stage 当作 Native Active 或 activate ACK。
4. 依赖可用时调用 `offer_required(consumer)`。缺少 route、尚未 staged 或尚未
   Published 的 provider 返回 Unavailable，不开始 issuance、不关闭消费者；完整
   Host native graph 应把这些业务依赖保持为 Pending。
5. 成功返回的 `RootDelivery` 通过 `receive` 或 `send` 接到实际 SDK。整张表只能
   签发一次，丢弃或失败后不能重新绑定这代。整表 Accept ACK 才允许业务构造。

native adapter 使用 `stage_native` 显式登记实际 own 根；每个名称必须在 frozen
`native_services` 中且精确契约一致。登记不自动 reserve 或 publish owner，也不把
已经导入的第三方 facade 当作 native-owned 对象。生成客户端的 native TypeKey /
Cordis slot 安装与最终 Host 图仍由适配层完成。

## 保留清单和权限视图

每个 native member 暂存一次完整 provides 表，保留其原始 staging pins 到 native
关闭。`object/route` 是 Host → owner 的私有 SDK 控制：选定 wire 服务名和冻结的
`PreparedRoute.source`，从保留清单创建一份独立暂存图。多个消费者使用同一个
真实 native 对象与 dispatcher，但各自有完整 source view 和独立 delivery pin。

SDK 不重新运行属性 getter，不构造替代对象。它先验证原始清单和 dispatcher，再
给全部 own 引用登记新 staging pins，保留原始 object identity、bundle SHA、interface、
snapshot 和 ownership，只为 Host 选择的路线登记 source alias。foreign proofs
原样保留，仍由 broker 检查 pass-back；这条路径不开放第三方转交。

Host 核对 owner 回复与已捕获的完整清单：除 own view 的选定 source 外，不允许改
root、引用、属性、SHA 或 owner。然后在一个 broker metadata 事务中签发所有 required
根，可跨多个 provider 和 bundle。每份图分别由其 owner 确认实际 pins；所有 commit
和必要 foreign pins 完成后，才返回拥有整个表的 receipt。

原 `StagedGraph.commit` 和完整服务 commit 仍逐项检查完整 view；没有忽略 source 或
放宽 whitelist。保留清单的多消费者 route 模式与既有一次性的整表 commit 分开使用：
后者转换了原始 staging pins，此后不能从它建立新的 route。

## 失败及证据

Owner 回复不匹配时，尚未签发 grants，Host 中止所有已取得的真实 stages。broker
事务失败时，本地候选图不是 delivery evidence，不交给 SDK 当成已接收 envelope。
后续 owner commit 失败则拒收整个已签发表，释放此前成功转换的 pins，并中止余下
stage。回滚会尝试整表清理，失败可观察；它不替代 native stop 或 OS reaping 确认。
issuance 在独立任务中继续推进，调用者丢弃等待也不能丢弃清理责任。

Linux `session_ipc` 用实际 managed rutis/Cordis Ctx、生成客户端和私有 Unix stream
验证两份精确 bundle。另加一对 native 消费者，分别从两位实际 provider 注入：同一
owner 的 RPC 根保持 object identity，消费者间 source 不同；其中一项经过 frozen
native adapter 选择。启动 apply 内执行 stateful query、循环对象和借用回调。

用例还验证发布前 Pending、重复 issuance 拒绝、错误 group/rebind 拒绝、篡改最后
清单的全表回滚，以及第二份实际 commit 失败后首份 native pin 回到原值。已有
消费者继续调用；有效 token 不能替换为另一消费者的 source。失败的保留成员不
构造业务插件，全部成员关闭后的 epoch 连续前缀仍完成回收。

测试的路由选择来自真正 prepared 元数据及冻结字节，原包在调用前删除。其管理
入口仍是标注 fixture，Rust actor 在 Host 测试进程中；计划内 Node entry 明确拒绝
装载，实际 Cordis 子进程使用 session fixture。该证据只支持冻结根装配，不支持
frozen runner → RuntimeReady → HostProxy Active 的完整发布链。事件能力在 metadata
中通过完整 bundle 检查，但本用例没有 broker 事件，不计作 M4。内部 child 精确
creator context、监督恢复、真实旧桥迁移和完整 T24 也仍待验收。
