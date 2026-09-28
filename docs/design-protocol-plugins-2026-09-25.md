# #59 协议插件设计决策

> 2026-09-28。待实现规格；接口与消息名称为设计记号。实现状态和验证证据见[工作记录](protocol-plugin-implementation.md)，现有 API 用法见[开发指南](protocol-plugin-guide.md)。

## D1 兼容目标与边界

**决定：同一份业务插件源码通过普通插件入口运行；应用只改变装配和运行位置。**

| 项目 | 契约 |
| --- | --- |
| Cordis 入口 | inject、Context 服务、on/once/parallel/serial、effect |
| rutis 入口 | Plugin、原有 TypeKey、Arc<dyn Trait>、Ctx、EventBus |
| 支持类型 | 异步方法、声明的数据类型、接口对象、调用树内借用回调 |
| 兼容观察点 | await 后结果、装载就绪、声明的因果顺序、原生资源归属；不保证 await 前副作用时机 |
| 不支持 | 同步远端可变状态、同步决策事件、模式事件、任意反射/闭包传输、跨视图原始指针相等 |
| 不兼容处理 | 装载时校验可静态确定的契约；动态 this/filter 等在执行前校验，返回具体接口/成员及原因 |
| 范围排除 | 部署更新、升级回滚、进程自动恢复、管理诊断、布局比较、性能专项 |

公共接口独立于协议包；业务不导入 Client、CallContext、对象编号或事件 helper。同步接口需要迁移时，本地与远端共同迁移到同一公共接口；现有 SettingsScope 适配不作为零改动兼容证明。

## D2 责任与装配

| 责任方 | 输入/职责 |
| --- | --- |
| 业务插件 | 公共业务接口、原生依赖、apply、effect |
| 绑定包 | 契约版本/摘要；服务键与 codec；执行上下文策略；创建者解析；事件键、结果类型、作用域；原生依赖元数据 |
| 开发者应用 | 插件、绑定包、运行位置、服务路由；Host 即该应用 |
| 默认 SDK/driver | 兼容检查、握手、原生包装插件、对象交付、事件路由、发布与清理；完整安装后才宣告 capability |

```text
应用选择运行位置
  +-- 本地实现 ------------------+
  +-- 远端实现 <-- IPC <-- 代理 --+--> 原有服务槽 --> 业务插件
                              SDK 内部转换
```

复用现有 broker、对象图、权限与引用账本。绑定由生成器或一次性第三方适配提供；包元数据与进程启动使用现有模块/工厂和运行配置。默认入口返回原生插件句柄，验收不依赖 fixture 控制入口。

## D3 服务视图与执行上下文

### 读取与身份

```text
校验 wire 契约/接收者/权限
  -> 安装业务服务槽
  -> 原生 Ctx 读取并完成依赖/作用域检查
  -> 按实际读取者创建业务视图
  -> 原有 Arc<dyn Trait> / TS 接口
```

| 规则 | 决定 |
| --- | --- |
| Rust 读取 | 在 ctx/registry 共用读取路径增加受信绑定的内部上下文视图能力；覆盖 get_as 与 require_as，保留各自可选/严格语义 |
| 执行顺序 | 原生类型/可见性检查 → 上下文视图解析 → 原有 require 拦截；不持 registry 锁运行工厂 |
| Cordis 读取 | 通过真实读取 Context 和原生追踪机制创建业务视图，不捕获 runner root |
| 缓存 | 键包含对象身份、权限视图、读取者原生代及 Context 语义身份（shadow、isolate、intercept）；同视图重复返回和循环引用使用同一代理 |
| 引用转交 | 直接传递 Arc/JS 代理保持原归属；另一个插件通过自身 Ctx 读取才能建立独立视图 |
| owner 传回 | 保留授权视图，不解包成绕过授权的原始实现对象 |

现有 Rust intercept_require_as 不含读取者 Ctx 且不覆盖 get_as；现有 TS SDK facade 不提供 Service.tracker。两者不能直接充当上述业务视图。

### 提供者执行上下文

**决定：读取者代理归属与远端方法的执行 Context 分开处理；绑定必须声明上下文策略。**

| 策略 | 执行规则 |
| --- | --- |
| provider-owned | 在真实 provider 上执行；仅适用于本地契约本身由 provider 持有资源的服务 |
| caller-scoped | 绑定层在提供端建立绑定到消费者代/作用域的执行视图，通过 Cordis 原生追踪绑定方法接收者；该视图中的 effect 随消费者退出清理 |

caller-scoped 是依赖 `this.ctx.effect()` 的服务保持原生资源归属的必要能力，不能通过 creator resolver 事后补救。消费者身份由授权记录确定；不从任意载荷读取，不序列化整个 Context。执行视图是远端资源的清理所有者，不是第二个业务插件，不重复 apply。

Cordis 执行视图保留锁定版本 getTraceable 的 origin/shadow 规则：effect 归消费者 fiber；服务读取从 provider shadow 的 fiber/store 解析；intercept 配置沿执行 Context 的原生链解析。不能将这些能力统一映射到消费者，也不能合并 shadow、isolate 或 intercept 语义不同的视图。

绑定须声明执行视图所需的 Context 能力，并按上述原生规则逐项映射和验证。未实现的映射拒绝装载，不能静默改用 provider Context。**执行视图及清理联动尚需最小原型验证，是服务实现的前置门槛；不得以仅支持 provider-owned 宣告本目标完成。**

### 对象与回调

- 返回对象由已登记创建者导出；仅在绑定明确声明时继承 provider。子插件或 caller-scoped 资源通过 creator resolver 指向实际所有者；不明归属拒绝导出。
- 回调和借用对象可用于当前调用及已登记后代；整个调用树完成后失效。持久订阅须由公共接口定义独立句柄与关闭语义。
- 业务 close/dispose 保持公共接口；协议 release 仅回收引用，不能代替提交事务、刷新或业务资源关闭。

## D4 原生生命周期与清理

**决定：MemberHandle 对应一次原生安装，Generation 对应一次真实 apply。业务 fiber 决定代变化，代理只映射可用性。**

### 依赖与启动

Member 期间维护原生依赖可用性镜像；它是调度信号，不是可调用服务或对象 grant。镜像独立于消费者 activation，避免“等待依赖才能申请身份，等待身份才能交付依赖”。

```text
安装 Member -> 缺必需依赖：Pending
  -> 远端 provider 发布：更新依赖镜像
  -> 原生框架进入包装 apply
  -> OpenGeneration：新 activation
  -> 校验依赖仍有效并交付本代对象，安装服务/事件绑定
  -> 业务 apply
  -> Stage / Accept + 订阅 ACK
  -> Publish：对外可用

依赖失效 -> 关闭旧代并完成清理
依赖恢复 -> 原生框架再次进入包装 apply -> 全新一代
显式 dispose -> Member 终态，禁止再开代
```

依赖在镜像确认与实际交付间失效时，本代退出且不进入业务 apply；后续重试由原生调度决定。Cordis 保留 required/optional；rutis 保留 Plugin::injects/get_as 语义。元数据不新增另一套业务依赖声明。

| 操作 | 不变量 |
| --- | --- |
| OpenGeneration | 绑定 epoch/member/本地代令牌，重复申请幂等；每代独立 activation、导出表和准入状态 |
| Stage / Accept / Publish | 完整表验证、业务 apply 成功、订阅 ACK 齐备后发布 |
| CloseGeneration | 进入下述关闭流程；旧代清理完成前不开始新代 |
| DisposeMember / stop | 成员终态；关闭当前代，禁止依赖恢复重开 |
| 迟到消息 | 按 epoch/member/activation 校验；不得影响新代或复活旧引用 |

### 关闭顺序

```text
停止外部新业务准入，撤销服务根和新事件订阅
  -> 启动原生取消与依赖清理
  -> 执行 effect / caller-scoped 视图清理，促成在途调用结束
  -> 汇合：已准入执行及后代结束 + 本代清理结束 + 依赖消费者清理确认
  -> 退休出站引用、导出与代身份
  -> Closed（保留清理错误）
```

**排干不是开始清理的前置条件。** 清理与在途执行可以重叠；pending read 可由 disposer 的 close 结束。原生取消不等于执行结束，调用 pin 保留到真实完成。caller-scoped 视图按消费者原生清理顺序释放，不因仍有调用而推迟启动清理。

| 关闭规则 | 决定 |
| --- | --- |
| 清理准入 | 原生清理入口授予本代既有引用及清理调用树权限；可调用公共 close 等方法，禁止发布根、订阅或向外泄漏新资源 |
| 正常 provider 撤销 | 根不可见并进入 Closing 后，保留既有依赖消费者的清理准入及所需对象；消费者清理确认前，不退休其导出/执行视图，不先销毁清理所需资源 |
| 依赖屏障 | 复用原生依赖清理观察；确认绑定双方 activation，包含消费者 effect 及其清理调用树的实际完成；迟到旧代确认无效 |
| 权限隔离 | 普通后台任务不因持有同一代理而取得清理权限；接收端校验既有授权，Closing 清理准入不开放普通业务调用 |
| 失败 | 断连或目标已 Closed 返回清理错误，不复活对象；失败保留到关闭结果，不伪造成功确认或提前释放在途 pin |

本代原生资源销毁遵循框架依赖清理顺序；屏障仅覆盖已登记原生依赖，不扩展到任意引用图。两端原生清理入口传播内部调用作用域，现有单一 gate 拆分业务准入与清理准入；异步传播与跨成员屏障必须通过 A0/B 验收。

## D5 原生事件

### 接入与记录

| 项目 | 决定 |
| --- | --- |
| Cordis | Context.extend/getTraceable 安装作用域事件服务，覆盖 ctx.on 与 ctx.events.on 等入口；不修改全局原型或私有监听表 |
| rutis | EventBus 增加按声明事件键选择的后端，覆盖注册/注销/异步分发；内核保留准入、作用域和 effect 归属 |
| 路由 | 声明事件的本地和远端监听进入同一协调列表；其他事件保持原生路径 |
| 分发 | 调用指定监听，不重新广播；不持列表锁等待回调，支持 A→B→A 重入 |
| this/filter | 绑定声明可编码的业务 this/授权范围身份；任意远端 Context 身份过滤拒绝执行 |

```text
Subscription = activation, localId, contract, scope, sourceSequence,
               prepend, oncePolicy, nativeOwner
Dispatch     = activation, dispatchId, contract, scope, afterSequence, mode, payload
Invoke       = dispatchId, invocationId, subscriptionId, payload
Result       = NoValue | Value(encodedValue) | Error(encodedError)
```

### 顺序与快照

1. on/once 同步返回本端句柄，顺序发送 Subscribe；协调端按接纳顺序建表，prepend 插到表头。
2. Dispatch 等待该来源 afterSequence 之前的登记/注销生效；原生就绪等待本代已有订阅 ACK。发送失败使分发/装载失败。
3. 分发在协调端选快照。普通注销移除未来快照中的登记，不撤销原生语义允许继续执行的已选调用；tombstone 只防迟到登记复活。代关闭另行拒绝未准入调用。
4. 同一次 invocation 去重；不同 dispatch 不合并。跨来源并发注册不承诺全局先后，确定顺序由装载依赖建立。
5. 兼容不包含调用返回 Promise 前的本地副作用时机，不引入分布式同步快照。

### 结果与 once

serial 由发布端解释结果；once 由监听端决定认领时点，策略来自可信绑定。

| 线上结果 | Cordis 发布者 | rutis 发布者 |
| --- | --- | --- |
| NoValue（undefined / None） | 继续 | 继续 |
| Value(false) | 继续 | Some(false)，短路 |
| Value(null) | 继续 | 类型允许时 Some(null)，短路 |
| Value(0) | 返回 0，短路 | 类型允许时 Some(0)，短路 |
| Error | 拒绝并停止 | Err 并停止 |

结果必须满足契约类型。parallel 等待全部已选调用后返回发布端原生聚合错误；业务错误码/字段保持，跨语言异常类和堆栈不保证相同。

rutis once 在选快照时认领，被前置短路跳过也会消耗；Cordis once 在回调入口注销，保留锁定版本的并发快照行为，不另加全局 at-most-once。

### 初始化事件

```text
OpenGeneration -> 安装事件绑定 -> 业务 apply
                                  on -> await serial -> 返回
  -> Stage / Accept + 订阅 ACK -> Publish
```

初始化资格绑定成员、代、事件契约和授权范围，仅允许已授权 Loading/Published 发布者调用。它不开放普通服务根，也不将整个 activation 提前标为 Published。

载荷对象复用契约、创建者和借用检查；可在已准入调用及已登记后代中传递，派生引用继承原有上限，不得逃出调用树或提升为已发布根。控制消息泵独立于业务 apply。失败撤销初始化资格，按 D4 排干已准入工作并清理；业务自身的循环等待通过既有取消/失败通道退出。

## D6 错误与取消

| 情况 | 处理 |
| --- | --- |
| 业务失败 | 公共业务错误类型/字段 |
| 契约不匹配 | 装载失败；动态不支持参数在执行前失败 |
| 越权、旧代、断连 | 公共接口约定的不可用错误，保留原因；不返回成功默认值 |
| 丢弃等待者 | 不代表远端停止；执行结束后释放 pin |
| 取消/重试 | 取消由公共契约显式定义；不自动重试有副作用调用，不将未知执行状态报告为未执行 |

## D7 实施与验收

每种语言各保留一份普通业务插件源码，分别运行本地基准、Rust 消费 Node、Node 消费 Rust。比较 await 后结果、因果事件轨迹、apply/effect 次数和资源归属；混合语言结果按 D5 验证。

| 顺序 | 修改模块 | 必须通过的验收 |
| --- | --- | --- |
| A0 上下文原型 | Cordis 执行视图；两端原生清理入口 | 双方同名依赖不同实现与不同 intercept 下，服务解析/缓存符合原生，this.ctx.effect 归消费者；effect 的 close 结束 pending read；后台任务不能借清理权限继续调用 |
| A 服务 | ctx/registry；两端 services/sdk；codegen | 原生键/类型；get/require 区别；回调真实归属；重复/循环身份；子插件资源；越权/旧引用拒绝 |
| B 生命周期 | managed；driver；lifecycle/session | 缺依赖→提供→失效→恢复；镜像与交付竞争；optional 不错误阻塞；卸载 provider 时消费者 effect 完成 close 后才退休目标；清理失败保留；旧代清理完成；迟到 ACK 无效；dispose 终态 |
| C 事件 | EventBus；TS 事件服务；两端 events | 混排/prepend；false/null/0；错误；once 快照/并发/重入；首监听注销后监听；apply 内 await；初始化对象嵌套/借用过期/失败撤销 |
| D 装配 | 默认 driver；元数据；指南 | 普通加载入口、完整 capability、同源码切换，无业务 helper 或 fixture 控制 |
