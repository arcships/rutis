# 私有连接上的对象运行时

本层把既有的 broker、native exports/imports、生成绑定和执行后代屏障接到私有
`frame::Peer` / `Peer`。Rust `HostObjects` 是授权方，两端 `RuntimeObjects` 只保存
本进程的实际 native 表、暂存图与已接收代理；作者不分配 grant、object id 或 token。

这是 M1/M2 的可复用传输层。默认 Rust/Node native driver 已接命名服务传输并拒绝
事件能力；首代 [HostProxy 原生图](host.md) 已接入。
[内部 child 创建者上下文](native-context.md) 已保存在同一导出表并用于实际调度；
单对象远端撤销、更新恢复、事件和完整 T24 尚未验收。

## 接入顺序

Host 在发送独立 start 前按顺序 `reserve` activation，并把每条继承连接的 runtime /
epoch 固定到 `HostObjects.handler`。消息中的 recipient、stage owner 必须属于这条
连接；已关闭的连接不能重新绑定。成员到达可以乱序，Host 的 id 分配保持单调。

`DeploymentObjects` 从同一个 frozen plan 选择跨 provider 的完整 required 表，见
[冻结根交付](deployment.md)。私有 `object/route` 为保留的 native 清单创建每个
prepared source 的独立 stage；不改变原始 commit 对完整 view 的核对。

runtime driver 使用生命周期 `MountRequest` 的同一个 gate，在构造前 `reserve`。
把整个 required 根表接收到本代 scope 1，等待 Accept ACK，再绑定生成客户端和
登记 native providers。Rust/TS actor 的 `receive_services` / `receiveServices`
执行这条路径；所有绑定都归本代，Host 的原生键和 Rust TypeId 不进入 wire。

实际 apply 首先用原始 Ctx/Context 创建 `Exports.managed` 并 `bind`，然后业务才
可以调用 required 服务。每个 epoch 共用 ObjectIds、Imports 和交付回收前缀；每个
activation 保留独立 native 表、业务 context 和 gate。`NativePorts.stage` /
`NativeServices.stage` 复用绑定的这张表，`stage_services` / `stageServices` 注册
完整 named roots 的 dispatcher。不同 bundle 分别合并，完整 view source 不被省略。

Host `offer_services` 等待 owner 整表 commit 及必要 foreign pins，返回拥有新根的
`RootDelivery`。`receive` 接到本地 SDK；`send` 把完整 activation/contracts/graphs
交给指定接收 handler。必须取得整表 Accept ACK 才完成交付，不能凭普通成功响应
推断 receiver 已接受。丢弃 issuance waiter 或 receipt 会独立推进撤销和新根拒收，
关闭预留消费者；`start(instance)` 向默认 driver 发送完整 required 表，也核对整表
Accept。这些后台兜底不是 native cleanup 或进程回收的成功确认。

`lifecycle_handler` / `lifecycleHandler` 与 `session::serve` / `serve` 可以把对象和
生命周期组合到同一请求泵。默认 service-capable driver 完成 reserve/bind/整表暂存。
activate 成功后才开放普通 object execute；stop 意图同步关闭对象成员，断连同时
关闭全部成员。嵌入者仍持有 native root，等待 native cleanup 后独立监督进程。

## 调用和关闭

连接上的 `object/*` 方法是 SDK 控制协议，不是插件作者 API。

| 方法 | 方向 | 行为 |
| --- | --- | --- |
| call | runtime → Host | 目标 delivery、selector 与 owner draft；校验连接和已有 Accepted grant |
| controls | runtime → Host | runtime-wide Accept/Release；按 Host 已发出的 delivery 找到真实 recipient |
| open / pin | Host → owner | 排入单调 scope 和执行 pin，先于异步参数交付 |
| commit / services-commit | Host → owner | 核对原始 native manifest 后转换 staging pins |
| route | Host → owner | Host 选择保留的服务和 prepared source，产生独立 stage；原对象和 dispatcher 保持一致 |
| execute | Host → owner | 精确 bundle/view dispatcher，原始业务 context，参数接受先于用户代码 |
| end | Host → owner | 实际 handler 和登记后代结束后关闭 Borrow scope、释放执行 pin |
| reject / abort | Host → runtime | 未消费的新 envelope 拒收或暂存图中止；不释放以前成功接受的重传 token |
| closing | runtime → Host | native gate 的终态通知；撤销服务及其已绑定消费者，属于关闭意图 |
| revoke / release | Host → runtime | owner/member 失效和 native delivery pin 释放 |
| retire / retire-owner | 双向确认 | 整个连接 epoch 的连续收到/终态前缀，Host 独立检查后通知 owner |

required 服务可以在消费者 native apply 中调用；发送者尚未发布时准入仍由其本代
绑定决定。普通服务 owner 必须已发布。借用回调依赖 Host 已授权的本次执行证明，
可在启动 apply 中回入；不会提前发布消费者的普通 provides。

管理锁仅覆盖 metadata、编号分配和完整请求排队。用户 handler、peer await 和
cleanup 不持有 broker/管理锁。方法查找绑定完整 raw bundle SHA；嵌套回调按完整
签名匹配，未知签名或 selector 拒绝，TS 不使用 prototype 成员查找。

Accept/Release 队列共用连接级串行 ACK 屏障；空队列 flush 也等待已经发出的前一批。
不能把另一调用取走 Accept 后留下的空队列当作确认成功。Rust 的独立发送任务在
flush 等待者被丢弃后继续保留屏障；revoke/reject/end 也经过同一屏障才响应。

Rust handler 在独立任务内捕获 panic，再等待 CallContext 的登记后代；TS 对 throw
和后代 rejection 同样等待。Owner 结果编码失败标记 execution unknown。丢弃 Rust
caller waiter 不取消实际执行，未消费的 object-valued result 会拒收，保持交付编号
前缀连续。运行中的执行 pin 保留到实际完成，不凭 deadline 或 waiter drop 释放。

runtime 跟踪已 pin、运行中和完成待确认的执行。断连关闭 native gate 和所有 root
scopes，释放尚未执行的 admission pins；已经运行的任务仍等待后代完成后释放本地
native pin。Host 丢失 owner ACK 时保留执行记录，后续 supervisor 的 reaping /
StopUnconfirmed 管理尚待 M3。epoch 断连可以凭 Host 的撤销证据向存活的 owner
释放 delivery pins，这不构成执行完成证据。

native 必要服务或依赖失效触发单次 closing 通知。接收 revoke 时同步关闭被撤销
成员以及已绑定的依赖成员，旧原始 context 随即拒绝新 effect；不会在旧 id 重建。
回收使用连接身份，允许所有成员已关闭但 epoch 仍连接时提交前缀；不创建虚拟业务
activation。owner pins 确认前不推进 SDK watermark。

## 实际证据与边界

`tests/session_ipc.rs` 启动实际 Node 子进程，继承匿名 Unix fd 3；Rust Host 内另一条
匿名私有流承载 Rust runtime actor。两 Rust、两 Cordis 原生成员运行两份精确 bundle
的双向 named DI，根由冻结的 prepared routes 选择；另加一对消费者验证多次 route、
跨 provider required 表和 native adapter 选择。额外第三个 Node provider 验证丢弃完整根交付：两个真实 owner
pins 从 2 降到 0，预留 Rust 接收者没有构造业务成员。

用例执行 connect/query、重复对象、循环属性、owner passback、两层借用回调及原始
member context 检查。handler throw / Rust panic 后仍等待实际 child；丢弃 value 和
object-valued waiter 后仍完成、释放并回收。伪造跨连接 sender 在执行前拒绝。
移除 Rust 必要服务的实际 Disposer 会关闭 provider、通知 Host、关闭 Node consumer，
另一组成员此前保持运行。全部成员关闭后仍完成 epoch 回收，native 清理计数和
实际 Node 正常退出都被检查。两端另验证未 execute 的 native admission 不阻塞关闭。
两端用真实 broker 控制交付延迟 Release ACK，验证后续空队列 flush 不提前完成；
Rust 还丢弃第一位 flush 等待者，再确认第二位仍等待同一实际 ACK。

该用例使用 SDK actor 和权威 broker，没有手工根 id/token；管理入口仍是明确标注的
fixture，并非 frozen plan 到完整 HostProxy 的生产装配。它不替代整个 T01–T24、
同组多个独立包的双语言最终验收、Linux descendant supervisor、真实旧桥迁移或
最终 family/version 冻结。
