# Cordis 检查与拦截能力在 rutis 中的设计

状态：设计草案；未实现。基准为 rutis `7d7402d`、Cordis [`56b3d4f`](https://github.com/cordiverse/cordis/tree/56b3d4f725681cf4556c1a8695a709cc3b6eed74)。关联 rutis [#27](https://github.com/arcships/rutis/issues/27)、[#29](https://github.com/arcships/rutis/issues/29)。

## 目的与边界

消费方已经使用 `Ctx::diagnostics()` 分析插件依赖图、初始化超时和作用域装配；保留这个按需读取的架构快照。`FiberView::watch()` 与 `FiberStatusChanged` 继续承担现有状态观察。已关闭的 [#28](https://github.com/arcships/rutis/issues/28) / [#37](https://github.com/arcships/rutis/pull/37) 所提 root 诊断变动流没有消费方，本设计不恢复它；[删除快照的 #38](https://github.com/arcships/rutis/pull/38) 也已关闭。

Cordis 的检查能力分散在运行对象和同步钩子中，没有单一的 `diagnostics()` 快照。这里分别设计三件事：投递前观察、清理项树、服务读写拦截。每个接口在实际动作边界工作，不让后台订阅流推测已经发生了什么。

| Cordis 源码中的事实 | rutis 现状 | 本设计的处理 |
| --- | --- | --- |
| [`events.ts` 的 `_resolve`](https://github.com/cordiverse/cordis/blob/56b3d4f725681cf4556c1a8695a709cc3b6eed74/packages/core/src/events.ts#L72-L81) 在选监听器前同步触发 `internal/dispatch`，包括零监听器；返回值没有放行/拒绝语义 | 只有业务 `on`、`emit`、`serial`、`parallel`、`waterfall` | 增加同步、只读的投递尝试观察钩子 |
| [`events.ts` 的 `on`](https://github.com/cordiverse/cordis/blob/56b3d4f725681cf4556c1a8695a709cc3b6eed74/packages/core/src/events.ts#L154-L165) 可由 `internal/listener` 改写监听器注册 | 注册直接进入总线表 | 记录为独立差异；目前没有替代注册的消费需求，不混入投递观察 |
| [`fiber.ts` 的 `effect` / `getEffects`](https://github.com/cordiverse/cordis/blob/56b3d4f725681cf4556c1a8695a709cc3b6eed74/packages/core/src/fiber.ts#L275-L346) 保存标签与实际嵌套清理关系 | `EffectRecord` 只保存待执行清理，`Effect::Many` 在登记时拍平 | 增加按 fiber 读取的清理树，沿实际拥有关系构造 |
| [`reflect.ts` 的属性访问](https://github.com/cordiverse/cordis/blob/56b3d4f725681cf4556c1a8695a709cc3b6eed74/packages/core/src/reflect.ts#L71-L123) 经 `internal/get` / `internal/set` waterfall；显式 `ctx.get` 绕过属性读取拦截 | `require` 严格检查已实现，`get` 是可选定位器；绑定值不可替换 | 在严格读取后加类型安全拦截；为可替换服务增加提供者持有的写入句柄 |
| `internal/plugin`、`internal/status`、`internal/service` 通知运行变化 | 全树快照、单 fiber watch、状态事件已有各自用途 | 不因此增加第二份 root 事件日志；需要新的实时消费者时另行定义 |

Cordis 的 `parallel` 在 `_resolve` 中也传入 `emit` 模式；rutis 的新模式字段按真实调用区分 `Emit` 和 `Parallel`，不照搬这个字符串细节。Cordis 的 `internal/dispatch` 监听器抛错可能中断投递，但那不是一个明确的审核决定接口。

## 1. 事件投递前观察

### 公共接口草案

```rust
pub enum DispatchMode { Emit, Serial, Parallel, Waterfall }

pub struct DispatchAttempt<'a> {
    pub key: &'a TypeKey,       // 含类型、限定名与实例号
    pub mode: DispatchMode,
    pub emitter: PluginId,
    pub emitter_instance: InstanceId,
    pub event: &'a (dyn std::any::Any + Send + Sync),
}

impl EventBus {
    pub fn observe_dispatch(
        &self,
        owner: &Ctx,
        observer: impl for<'a> Fn(&DispatchAttempt<'a>) + Send + Sync + 'static,
    ) -> Result<Disposer, CordisError>;
}
```

观察器借用事件，只能在回调期间下转型读取；接口不复制事件、不保存载荷。观察器能够读取事件内容，属于可信的框架扩展点，不能用于运行不可信插件。注册归 `owner` 的 fiber effect 所有。注册时确认 `owner` 属于该总线所在 root；投递时只调用位于发射 fiber 祖先链上的观察器。因此 root 可以观察全树，Session 作用域插件只观察自己的子树，兄弟实例互不可见。实例事件仍先执行现有的实例号与关闭准入检查。

每次合法投递尝试调用一次观察器，零业务监听器也调用。`emit` 在当前线程入队前调用；`serial`、`parallel`、`waterfall` 在 future 首次 poll、选择业务监听器前调用。按观察器注册顺序同步调用，不持有 admission、总线表、注册表或 fiber 状态锁。观察器中新增/移除的监听器可影响随后取得的业务监听器快照，与 Cordis 的先观察后选择顺序一致。观察器重入投递按普通嵌套调用处理，不声明跨线程的全局观察顺序；同键 `emit` 的原有尾链保序仍以实际入队顺序为准。

观察器返回 `()`，没有拒绝通道。其 panic 被捕获并交给 ErrorSink；ErrorSink 自身的 panic 也隔离，业务投递继续。观察器按 effect 卸载，新投递不再选择它；已选中的同步回调计入所属 fiber 的在途回调，子树 shutdown 等它退出。不能持锁等待观察器，也不能让观察器保留借用的事件。并发关闭后失败的实例投递不触发观察器；已经观察到的尝试仍可能在重新校验时因关闭而未被接纳，因此名称是 `DispatchAttempt`，不是 `DispatchAccepted`。

**审核边界**：这个钩子能在投递前检查和记录，不能保证记录持久化，更不能放行/拒绝。若业务需要“审核失败则不投递”，应另外设计显式策略入口及可返回拒绝的 `try_emit`；现有返回 `()` 的 `emit` 不能假装具有这个契约。`internal/listener` 式注册替换同样另立需求，不与此钩子合并。

验收：四种分发模式、动态限定名、实例键、零监听器、观察器先于监听快照、注册/卸载竞态、重入、panic、不同实例隔离及 1000 轮注册/卸载；无观察器时原事件对拍结果不变。

## 2. 带标签的 effect 清理树（#27）

```rust
pub enum EffectPhase { Live, Draining }
pub struct EffectMeta {
    pub label: String,
    pub phase: EffectPhase,
    pub children: Vec<EffectMeta>,
}

impl Ctx {
    pub fn effect_named(
        &self,
        label: impl Into<String>,
        f: impl FnOnce() -> Effect,
    ) -> Result<Disposer, CordisError>;
}
impl FiberView {
    pub fn effects(&self) -> Vec<EffectMeta>;
}
```

现有 `Ctx::effect()` 和 `Plugin::apply()` 的返回类型不改；默认标签分别为 `anonymous` 和插件名。plugin mount、service provide、listener register 使用框架生成的类型/键/实例标签，不含服务值、配置或事件载荷。`Effect::Many` 的原有嵌套结构生成 `children`；每个子项先用稳定的序号和种类标识。并列的 `ctx.effect()` 登记仍为兄弟项，不通过调用栈臆造父子关系。需要用户自定义子项标签时再引入明确的组合 API，避免给公开 `Effect` 枚举贸然增加变体。

`EffectRecord` 登记时同时保存纯元数据树和现有 LIFO 清理列表；元数据不捕获清理闭包。当前 `FiberInner.effects` 在卸载时整表取出，因此还需可清理的弱引用索引，让 `effects()` 在清理期间看见 `Draining`，在记录进入 `Done` 后删除索引。读取只复制标签、阶段和树结构，不执行用户代码，不持有子 fiber 或服务值。先提供 `FiberView::effects()`；架构图确实需要时才把它纳入全树 `Ctx::diagnostics()`。

验收：自动/显式标签、`Many` 的真实嵌套与 LIFO 清理顺序、提前 dispose、清理中可见、完成即消失、错误聚合不变，以及 1000 轮子树关闭后父 fiber 的元数据与 effect 记录回到基线。

## 3. 严格服务读取与提供者写入拦截（#29）

### 读取

`get/get_as` 保留显式可选定位器语义，不进拦截链。`require/require_as` 先按现有顺序完成实例可见性、类型、活跃状态、依赖声明和绑定可用性检查，再在锁外运行当前 `(完整 TypeKey, 有效 isolate scope)` 的同步、类型化钩子。只选择注册 fiber 位于读取方祖先链上的钩子，防止 Session 中的钩子接管兄弟 Session 的进程级服务读取。钩子依注册顺序接收已解析的 `Arc<T>`，可继续、替换本次读取结果为另一个 `Arc<T>`，或拒绝；拒绝使 `ServiceReadError` 得到明确的拦截原因，panic 转为明确错误。钩子不能使未声明、越界或未就绪的服务变得可读。替换只影响本次返回值，不改绑定及 `(provider, generation, key, scope)` 身份；`ServiceAccess` 仍记录原绑定身份，并可另记本次结果是否被拦截。

拦截器是可信的框架扩展点。`Arc<T>` 的类型不能证明值来自哪个实例：拦截器若预先持有兄弟实例的值，仍能把它作为同类型替代结果返回。因此实例键和 scope 校验只保证**被读取的绑定**以及**允许注册钩子的 fiber**没有越界，不能证明钩子制造的内容来源。若要求对不可信拦截器强制防串用，必须禁用 `Replace`，或改成只能返回带私有来源证明的绑定句柄；当前草案不声称实现这一点，#29 的相应验收表述也应随实现 PR 修正。

候选入口为 `Ctx::intercept_require_as::<T>(key, hook) -> Result<Disposer, CordisError>`，决策类型为 `Continue | Replace(Arc<T>) | Deny`。同键同操作的同步重入返回明确错误；不同键重入允许。钩子是 owner fiber 的 effect，注册与撤销遵守实例子树边界。读取先快照钩子，再离锁调用；shutdown 等已接纳的钩子完成。

### 写入

现有 `provide_as` 注册的服务保持不可替换。新增 `provide_mut_as` 返回清理句柄和代次绑定的 `ServiceWriter<T>`；只有该句柄可以写回其创建时的绑定。旧代句柄、摘除中的绑定、非 owner、类型或实例越界一律失败。这样旧代异步任务即使持有相同的 `Ctx`，也不能改写新代的同键服务。

`ServiceWriter::set(Arc<T>)` 在锁外运行该键、scope 的同步写入钩子；只选择注册 fiber 位于提供者祖先链上的钩子，防止兄弟子树干预写入。钩子可继续、替换同类型候选值或拒绝。提交时重新检查绑定的 Arc 身份、provider 与 generation，然后原子替换值。可替换绑定单独持有一个可变值槽；普通不可替换服务不必为此增加读取锁。成功写入保持 provider、generation、依赖四元组和驱逐关系；旧 `Arc<T>` 不会被原地改写，之后的读取取得新值。写入不自动重载消费者；健康谓词的外部条件变化仍由 `refresh()` 触发门控重查。钩子 panic 返回写入错误，不提交候选值。

这个顺序比 Cordis 的 `internal/get/set` 更严格：Cordis 的 get 钩子位于最终依赖读取之前；rutis 的拦截器不能越过类型、实例和依赖声明边界。同步入口也不能复用目前异步的 `EventBus::waterfall`，只能复用它的链式顺序思想。

验收：`get_as` 绕过、`require_as` 的 continue/replace/deny、未声明与越界先拒绝、按完整键及 isolate 限制钩子匹配、同代写入与旧代句柄失效、旧 Arc 保持旧值、写入不驱逐、重入和 panic、卸载后钩子自摘且无旧代回调；原服务与生命周期对拍通过。

## 实施顺序与交付边界

1. 先实现投递前观察：这是现有 Cordis 行为中明确提出要核对的投递动作入口。单独 PR，独立于快照与服务拦截。
2. 按 #27 实现 effect 树；先在单 fiber 上读取，不扩大全树 DTO。
3. 按 #29 先实现严格读取拦截，再实现可替换服务及写入拦截；后者需要独立验证代次、旧 Arc 和更新竞态。

每一步都保留现有 `Ctx::diagnostics()` 的架构分析用途，并运行 `cargo +1.98.1 test -p rutis`、`clippy -p rutis --all-targets -- -D warnings`、`fmt -p rutis -- --check`。消费仓库的构建、架构图和 Session/Branch 诊断流程在 API 变更后另行验收。没有实现代码前，本文件不表示 rutis 已提供这些 Cordis 钩子。
