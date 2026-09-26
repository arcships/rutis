# 实验对象协议实现约定

此目录与 `rutis-protocol` crate 实施 [#59 设计](../docs/design-protocol-plugins-2026-09-25.md)。
当前 M1 与 M2 的传输验证在推进，尚未形成可部署的 Rust↔Cordis 协议插件系统。已有两端生成绑定、对象图、私有 socket 上的真实跨语言调用与回收 ACK，以及只读 prepare 和 Rust 延迟 factory 注册；生产 runner、监督恢复和 broker 事件仍在开发。
实施证据及完整验收范围见[验收记录](../docs/protocol-plugin-implementation.md)。旧 `rutis-cordis` 桥继续独立存在。

包/部署 JSON 格式、原字节文件库存、精确路由、静态 ELF catalog 与只读 CLI 见[prepare 约定](package-format.md)。prepare 冻结计划；实际启动仍需固定 artifact 快照、RuntimeReady 与成员发布屏障。

## JSON 与描述符

所有入站 JSON 使用 `json::decode` / `decodeJson`，然后解码消息或 bundle 的结构。
UTF-8 必须严格有效，不移除 BOM；字符串只包含 Unicode scalar value，拒绝孤立 surrogate。
对象的所有层级拒绝重复键，转义后相同的键也算重复。拒绝第二份文档、尾逗号、非 JSON 空白以及非有限数字。
JSON 数字使用 IEEE-754 binary64；整数结果必须处于 JS 安全整数范围，较大整数通过声明的十进制字符串传递。
`1`、`1.0`、`1e0` 和 `-0` 按同一 JSON 数字语义处理，不以某端打印形式判定 enum/const 相等。

解码器安全边界为 **16 MiB 原始 UTF-8 字节、64 层子值深度**：根的深度是 0，对象成员和数组元素各加 1。
空容器不增加子值深度。边界内的有效输入可接受，超过边界返回 `InvalidParams`，解析帧头时也必须在分配 body 前检查长度。
这两个边界保护解码器的内存和调用栈，不是插件对象/调用配额；后续资源策略另行设计。
`frame::Peer` / TS `Peer` 使用 u32 大端长度和严格 JSON，独立接收泵支持重入，独立写队列持有完整帧。Rust 等待者被丢弃不会中断半帧写入；断连的未完成请求报告 execution unknown。共享帧语料覆盖分片 UTF-8、重复字段、BOM、坏长度和截断；接收任务拒绝重用/倒序 request id，避免重复执行。

bundle 原始字节 SHA-256 保持精确，不做 JSON 重排、空白归一化或 semver 宽松匹配。
先检查完整结构，再检查 capability 和契约；禁止用某个已知不支持的外层类型掩盖内部结构错误。
值 schema 使用受限 2020-12 子集：`oneOf` 的每个分支必须为 object，并有同名、required、string const 判别字段，const 各不相同。
一般重叠 union、递归值 schema、外部 `$ref`、正则等不支持。对象接口图允许有环。
重复 capability、event mode、required 字段或 enum 值拒绝；数值边界不能倒置。

回调签名的 SHA-256 输入是下面的 UTF-8 编码，避免两端 JSON 打印数字、Unicode 排序产生差异。
完整 `TypeExpr`（包含 ownership 和所有嵌套契约）参与编码；前缀为 `$callback:`。

| 值 | 编码 |
| --- | --- |
| null / true / false | `z` / `t` / `f` |
| number | `n` + 16 个小写十六进制字符（binary64 大端位模式）+ `;`；正负零统一为 0 |
| string | `s` + UTF-8 字节数十进制 + `:` + 原 UTF-8 内容 |
| array | `a` + 元素数十进制 + `:` + 各元素编码 |
| object | `o` + 成员数十进制 + `:` + 各 key 编码、value 编码；key 按 UTF-8 字节序排序 |

共享语料分别覆盖 JSON（35 项）、描述符（50 项）、wire value（32 项）、回调签名（9 项）、对象图（31 项）、帧（10 项）。
两端消费同一文件；语料中的非法 UTF-8、重复键以原字节保存，不先经过会覆盖字段的普通 JSON parser。

## 真实对象持有与交付准入

owner SDK 的导出表为真实 Arc / JS 对象分配稳定身份；相同字段的不同对象不合并。
同一个 runtime epoch 的全部 activation 共用 object id 分配器；id 不复用。
不同 native 插件装载和发布的完成顺序可以不同；broker 检查编号唯一性，不要求对象登记按分配顺序到达。
本地对象仍存活时，shared 对象在无 pin 的间隔后再次导出保持身份。
broker 接受 owner 分配的身份，重复登记保持现有接口视图，不能换对象或扩大现有视图的方法。宿主准入计划可为同一身份增加独立视图，先前 grant 的 whitelist 保持不变。

临时 staging pin 覆盖导出交付提交前的间隔；同一 owner 的多个编码器共用 staging 编号空间，每份 delivery 和实际 execution 独立持有真实对象。
`GraphExporter` 先编码未授权 `DraftGraph`，提交真实 owner 身份与显式快照；foreign 引用提交原 accepted delivery 证明。broker 按宿主选择的 bundle/type/source 校验整图，并原子签发 grants；拒绝任一引用时不改变账本或交付序列。owner 用 `StagedGraph.commit` 确认对应 delivery pin 后，接收方才见到图。借用属性继承借用范围；snapshot 错误或 panic 释放 staging。owner pin 失败时仍向接收 SDK 提交完整拒绝清单，避免连续前缀出现缺口。
释放 delivery 或取消用户等待不能释放 execution pin。
shared 策略只释放协议的强引用，不调用业务 close；独占策略只用于没有合法本地使用者的外部资源，最后 pin 释放后执行一次 disposer。
独占注册立即接管资源，首次 pin 前暂存强引用；未发布就回滚也会清理，不能让注册到交付之间成为泄漏窗口。
独占 disposer 一旦开始，该身份永不复活；清理在独立任务内推进，停止 waiter 被丢弃也不取消它。
panic/rejection 会成为可观察的清理失败，不能计作成功卸载。

Rust `Exports::managed` 与 TS `Exports.managed` 把表交给真实 native ctx 的 effect。
每次准入同步检查原生代的 gate/uid；清理观察任务不能成为失效判定的唯一依据。
native 卸载关闭准入、撤销 delivery/staging pin，等待仍执行的对象和 disposer。
这是原生清理接入证据；整体 StopUnconfirmed / supervisor 恢复屏障仍待 M3。

导入表 `receive_batch` 先检查整份引用清单，再在一个临界区附加 token。
类型/图验证失败使用 `reject`，只拒绝这次新增的交付；以前已经成功交给作者的 token 和别名保持有效。
同 delivery id 的重传不增加 pin；相互矛盾的记录拒绝。新交付不能复活已经 release 的旧包装。
对象图先校验整个引用表，再建齐代理和不可变身份关系；每个引用必须可达，快照关系继承父引用的 scope，借用关系不能延长子对象寿命。关系边不持有代理，scope 关闭清空包装缓存。显式释放子包装后，导航该关系失败；新交付可建立新的子包装，已释放别名仍关闭。相同活视图的快照变化拒绝。scope 继承 native gate，失效后缓存属性与方法同样拒绝；owner 撤销记录还拒绝尚未接收的旧 handoff。

## 生成绑定与内存调度

生成器先准入完整原字节 bundle，输出包含该 SHA-256 的 Rust/TS 源码：

```sh
cargo run -p rutis-protocol --bin rutis-protocol-bindgen -- \
  protocol/fixtures/database.bundle.json \
  protocol/generated/database.rs protocol/ts/generated/database.ts
cargo run -p rutis-protocol --bin rutis-protocol-bindgen -- \
  protocol/fixtures/binding-types.bundle.json \
  protocol/generated/binding-types.rs protocol/ts/generated/binding-types.ts
cargo run -p rutis-protocol --bin rutis-protocol-bindgen -- \
  protocol/fixtures/rpc.bundle.json \
  protocol/generated/rpc.rs protocol/ts/generated/rpc.ts
```

TS 输出默认相对导入 `../src` 的 SDK；当前目录结构是实验包布局，并未发布稳定的安装接口。
字段排序由生成器固定，不能依赖 serde_json 的 feature 联合结果。三份产物在 Rust/TS 编译，并由 workspace 测试核对重生成结果；`rpc.bundle.json` 用于真实私有 socket 互通。
Rust 生成 `InterfaceDatabaseService`、`InterfaceDatabaseClient` 等类型：业务实现返回真实 `Arc<dyn …Service>`，调用者得到门控客户端；参数和 JSON 数据 DTO 由 SDK 编码。
可选 DTO 字段用 `OptionalField::Missing/Present` 保留缺失与 null 的区别，字符串 enum/判别 union 生成 Rust enum，开放 JSON 数据保持 Value/map。
TS 用冻结的生成 facade 保持对象身份，属性只读本地快照；未知选择器拒绝。业务 `then` 通过 `then$` 暴露，避免 Promise 自动调用它。Caller 属于原生 activation，运行器应为该 activation 保持稳定实例。
两端导出适配器只读取声明的 selector 和属性，不枚举业务对象或原型；dispatch 必须取得该实际对象的 execution pin。
`CallContext` 保存原始 native Ctx/Context，并提供登记子任务的 API；子任务及后代完成前，借用 scope 不能闭合。

Rust `memory::Network` 是使用真实 broker 的内存传输测试入口，支持图交付、状态对象、owner pass-back、嵌套借用回调及 native endpoint effect。
所有执行离开 broker 临界区后运行；owner-returned facade 也走 broker 调度。丢弃调用 waiter 只丢弃结果，owner handler 和登记子任务继续跟踪，晚到未消费图由 envelope 释放新增交付。
Linux `tests/objects_ipc.rs` 启动独立 Node 进程，仅继承私有 Unix stream 的 fd 3。双方用同一原字节 bundle 的生成绑定，Rust 权威 broker 根据固定连接身份准入，运行双向 connect/query、同一对象重复返回、循环属性、owner pass-back、重入 callback 和登记子任务。参数 grant 在交给业务代码前确认；callback 的 native Ctx 来自其 creator。stdout 诊断保持独立。测试还覆盖借用到期、丢弃 Rust waiter 后的真实 Node 执行 pin，以及回收 ACK。
这是真实对象调度的 conformance fixture。多 bundle prepare/权限路由计划已有独立准入测试；生产 runner 的实际路由绑定、RuntimeReady 发布屏障、旧插件迁移及 supervisor 尚未实现，不能据此部署该协议。

## 连续前缀回收的含义

delivery id 属于**接收 runtime epoch 的整条连接**，覆盖其中所有 activation。
代理 cache key 不含 delivery id；交付账本与包装身份分开。
同一个连接内的 accept/release 幂等，终态不复活。闭合 epoch 后，未知旧编号也不能成为新授权。

控制帧必须遵守以下顺序；私有 socket fixture 已运行两端前缀提议、owner 通知和 ACK，生产多 activation 连接与故障交错仍待验收：

1. 接收 runtime SDK 记录每份已处理的交付 envelope（包括取消/解码失败后拒绝的 envelope）。
   `received_through` 只推进到没有缺口的连续前缀；`terminal_through` 同时要求每个 id 已 Released/Revoked。
   单个业务 activation 或作者对象不能替整条连接声明前缀。
2. SDK 向 broker 提出这两个前缀。broker 从已认证的连接取得 runtime/epoch，独立确认没有超出已分配序号，且 terminal 前缀没有 Offered/Accepted。
   release 必须先被 broker 处理；拒绝未经确认、倒退、有缺口或活 token 的回收，不按时钟推断。
3. broker 保存高水位、回收其终态 tombstone，再返回确认的 `terminal_through`。
   接收 SDK 收到 ACK 后才调用 `acknowledge_retirement` 删除自己的终态记录；ACK 不能超出已提出的前缀。
   低于确认水位的迟到交付拒绝；迟到 accept/release 忽略，不重新登记对象。
4. broker 给 owner 的 pin 撤销先于该前缀的 owner 回收通知。owner `retire_deliveries` 仅接收 broker 确认，不依据本地稀疏 id 推断连续前缀。
   owner 独立要求该前缀没有活 delivery pin，保留水位并拒绝迟到重 pin；execution pin 不受这一前缀影响。
5. 连接断开则 broker 闭合整个 recipient epoch，撤销全部 pending/accepted delivery。
   owner `close_recipient_epoch` 可回收其 delivery 终态记录，并永久拒绝该旧 epoch 的新 pin；仍 executing 的 pin 继续跟踪到实际完成或监督者确认 owner 被回收。

目前 API 中的身份参数是宿主/SDK 内部可信参数。实现真实消息路由时，不能直接把作者提交的身份字段作为 authenticated caller。
断线后的进程回收与 consumer cleanup 双屏障尚未实现，这些前缀方法不代替监督者。
