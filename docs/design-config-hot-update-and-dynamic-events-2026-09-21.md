# 设计:配置热更新(D32)与动态事件键(D33)

> 2026-09-21。范围:两件独立功能,可并行实施、独立提交、独立回滚。
> D32 = cordis `fiber.update(config)` 的 rutis 化(M4 核心);D33 = TypeKey 限定名放宽 + 事件总线 keyed 面 + 桥事件链路(即旧路线的 M3 事件链路)。
> 参照物:dsh fork 的懒 config 解析(vendor README 修改 #15,port cordiverse/cordis#41)与 `internal/update` 语义;上游 cordis 字符串事件。

## 〇 动机

1. **D32**:cordis 生产最依赖的能力——运行中改插件配置、自动重启生效。dsh 的整条"改 YAML → 插件热更新"链路建立在它上面。rutis 现状:config 烘死在实例里(D19),改配置只能卸载重装。
2. **D33**:dsh 宿主经 `evt/emit` 发来的事件,名字是运行时字符串(`session/event`、`agent/*`),Rust 侧目前只能打 stderr 日志(rutis_dsh.rs:136),无法按名字订阅。事件总线注册面键是裸 `TypeId`(bus.rs:184/189/231/238),同类型多通道不存在。

两者正交:动态插件装载(loader)需要的"名字→工厂"注册表**不在本设计内**(见 §五)。

---

## 一 D32 配置热更新

### 1.1 语义总表(cordis → rutis 映射)

| cordis 语义 | rutis 映射 | 成本 |
|---|---|---|
| `fiber.update(config)`:validate 后重启 | 存 config + 复用 `Intent::Restart`,现有状态机保证恰好一次清理、等清理完再重载 | 零新事务逻辑 |
| config 可引用注入服务,注入激活后才解析(fork `internal/config` waterfall) | 工厂是纯函数(config→实例,不接 ctx);**服务引用在 apply 内解析**——apply 已在依赖门控之后,时序等价 | 不需要 internal/config 对应物 |
| provider 替换 → config 重新解析 | 依赖驱动重载已实现:unload 清 last_deps → load 重新走工厂 = 用当前 config 重造实例 | 免费 |
| pending update 保留 raw config | config 存 fiber,Pending 态等门控放行时自然用新值 load | 免费 |
| update 前校验失败不动现状 | dry-run(见 D32b)失败返回 Err,不存不重启 | 新增 |
| `internal/update` waterfall(持久化钩子 noSave) | 不做;宿主层监听 `FiberStatusChanged` + 自管持久化 | 明确裁剪 |

### 1.2 状态矩阵(update 在各态的行为)

统一路径:**存 config → cancel_current → post_join(Restart) → join**。

| 当前态 | 行为 | 依据 |
|---|---|---|
| Active | unload(清 last_deps)→ refresh_deps → load(新 config) | Restart 分支(fiber.rs:505-508) |
| Loading | cancel 当前代 → apply 协作退出 → unload → Pending → load(新 config) | 同上 |
| Pending | 跳过 unload,refresh_deps:依赖满足则 load(新 config);不满足则继续 Pending,**config 已存,provider 到位后自动用新值** | Restart 分支(fiber.rs:505) |
| Failed | 同 Active 路径(restart 从 Failed 重载,新 config 生效)——修好配置热修复 Failed 插件 | 同上 |
| Unloading(进行中) | 排队等驱动串行处理,收敛同上 | mailbox FIFO |
| Disposed / terminal_task 已登记 / 驱动已退出 | 拒绝 `InactiveEffect` | 同 restart 现有检查(fiber.rs:685-688) |

**等值合并不吃掉 update**:unload 清 last_deps,refresh_deps 在 `last_deps == None` 时必不等值、必 load(fiber.rs:305-315)。config 不参与依赖身份(D32e),其变化经 restart 的 generation+1 体现;依赖该 fiber 服务的消费者看 provider_gen,自动驱逐重载——免费。

### 1.3 API 草案

```rust
// plugin.rs 新增
/// 工厂:每代从当前 config 构造插件实例。build 必须是纯构造(D32b 契约):
/// 无副作用或幂等——dry-run 与 load 各调用一次。
pub trait PluginFactory<C: Send + Sync + 'static>: Send + Sync + 'static {
    /// 依赖门控声明(工厂模式无实例可问,由工厂从 config 派生)。
    fn injects(&self, _config: &C) -> Vec<TypeKey> { Vec::new() }
    /// config 级校验(不构造实例)。
    fn validate_config(&self, _config: &C) -> Result<(), CordisError> { Ok(()) }
    /// 构造插件实例。失败 = config 无法产出可用实例。
    fn build(&self, config: &C) -> Result<Box<dyn Plugin>, CordisError>;
}

// ctx.rs 新增(plugin() 现有路径不变,零迁移成本)
impl Ctx {
    pub fn plugin_with<C: Send + Sync + 'static>(
        &self, factory: impl PluginFactory<C>, config: C,
    ) -> FiberView;

    /// 闭包便捷形态(等价于单方法工厂)。
    pub fn plugin_from<C: Send + Sync + 'static>(
        &self,
        build: impl Fn(&C) -> Result<Box<dyn Plugin>, CordisError> + Send + Sync + 'static,
        config: C,
    ) -> FiberView;
}

// fiber.rs FiberView 新增
impl FiberView {
    /// 热更新:dry-run 通过后存入并重启。join 返回 restart 终态。
    pub fn update<C: Send + Sync + 'static>(
        &self, new_config: C,
    ) -> BoxFuture<'static, Result<(), Arc<CordisError>>>;

    /// 当前 config 快照(诊断用;类型不符返回 None)。
    pub fn current_config<C: Send + Sync + 'static>(&self) -> Option<Box<C>>;
}
```

### 1.4 内部形态

- `FiberInner` 增两字段:
  ```rust
  factory: Option<Arc<dyn ErasedFactory>>,          // 工厂模式才有
  config: Mutex<Option<Box<dyn Any + Send + Sync>>>, // 同上
  ```
- `ErasedFactory`(内部):`config_type_id() / injects_erased(&dyn Any) -> Vec<TypeKey> / validate_config_erased(&dyn Any) / build_erased(&dyn Any) -> Result<Box<dyn Plugin>>`。泛型 `PluginFactory<C>` 由包装结构擦除实现(同 registry 的 `StoredValue` 手法)。
- **两形态并存**(D32a):`plugin()` 静态模式(`FiberInner.plugin: Some(..)`)不动;工厂模式 `plugin` 字段为 None。`load()` 取实例改为 `this.current_plugin()`:静态模式克隆既有实例,工厂模式 `factory.build(当前 config)`(失败走 `fail_load`,保持装配失败原子性)。validate/apply 下游路径完全复用。
- **update 流程**(1.2 矩阵的展开):
  1. transition 锁内检查 terminal_task / Disposed / 驱动退出 → 拒绝;
  2. `TypeId::of::<C>()` 与 `factory.config_type_id()` 匹配,不符 → `Validation` 错;
  3. dry-run:`validate_config(&new)` → `build(&new)` → 实例 `validate()`(D32b);任一失败返回 Err,**不存不重启**;
  4. 存入 config;
  5. `cancel_current()` + `post_join(Restart)` + `join_task`。

### 1.5 并发语义

| 场景 | 收敛 |
|---|---|
| update × update | mailbox FIFO,后者覆盖 config;两个 join 各自等到自己的 Restart 处理完 |
| update × dispose | terminal_task 先登记则 update 拒绝;反之 dispose 排队等 restart 完成后卸载 |
| update × 驱逐(RefreshDepsJoin) | mailbox 序决定先后;两者都收敛到"用当前 config 重载",无竞态窗口 |
| update × 运行中 apply | cancel_current 协作退出(与 restart 现有语义一致,D27 限制照旧:不观察 token 的 apply 无限等待) |

### 1.6 决策点

- **D32a 双形态并存**:`plugin()` 不变(零迁移、115 测试零改动);统一为 identity 工厂留后续简化批次评估。不做进本期。
- **D32b dry-run = `validate_config` + `build` + 实例 `validate`**,造出的实例丢弃;`build` 纯构造是工厂契约(违反 = dry-run 与实跑不一致,自担)。错误尽早暴露,避免重启到一半才发现 config 造不出实例;代价是 build 跑两次。
- **D32c 状态矩阵**如 1.2(对齐 cordis + fork 的非 ACTIVE 态行为)。
- **D32d 不做 `internal/update` waterfall**:持久化归宿主层。`noSave` 语义无对应物。
- **D32e config 不进依赖身份**:变化经 restart 换代体现,消费者看 provider_gen。与 cordis 四元组语义一致。

### 1.7 测试计划(契约测试,目标 ~10 条)

1. Active 态 update:新 config 生效(提供的服务值变)、generation+1、旧代清理恰好一次(LIFO 顺序断言);
2. dry-run 失败三种(validate_config / build / 实例 validate):返回 Err,状态与服务不变;
3. Pending 态 update:依赖未到时 update,provider 到位后装载用**新** config;
4. Failed 态 update:修复配置后重启成功回 Active;
5. update 后消费者重载:provider update → 依赖它的消费者自动驱逐并用其当前 config 重载;
6. 并发 update × dispose / update × update(FIFO、join 均正确落定);
7. 工厂模式 `injects(config)` 门控:依赖缺失时 Pending,不 apply;
8. config 类型不匹配的 update 报 Validation;
9. 工厂 build 失败(依赖已满足):Failed,config 保留,再次 update 修复;
10. 与 parity 对拍:`update config while injected service reloads` 的内核(原列"部分对拍",载体 update config——本设计落地后可全拍)。

---

## 二 D33 动态事件键

### 2.1 现状与思路

bus 注册面键是裸 `TypeId`:`hooks: HashMap<TypeId, ..>`、`wf_hooks`、`dispatch_tail`(bus.rs:107-113)。思路:**不抄 cordis 的字符串事件面**(四分发做一套字符串版本 = 语义面翻倍、类型安全丢失),而是把服务键已有的 TypeKey 机制搬进事件总线——限定名支持动态字符串后,一个类型化事件类型 + 动态键即可承载"运行时才知道名字的事件",四分发、fiber 生命周期清理、once/prepend 全部免费继承。

### 2.2 TypeKey 限定名放宽(key.rs)

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Qualifier {
    Static(&'static str),   // 现有路径,零分配
    Dynamic(Arc<str>),      // 运行时名字
}

pub struct TypeKey {
    type_id: TypeId,
    qualifier: Option<Qualifier>,
}

impl TypeKey {
    pub fn keyed<T: ?Sized + 'static>(qualifier: &'static str) -> Self;          // 不变,走 Static
    pub fn keyed_dynamic<T: ?Sized + 'static>(name: impl Into<Arc<str>>) -> Self; // 新增,走 Dynamic
}
```

- Hash/Eq 按字符串内容:Static("foo") == Dynamic("foo"),互查互通;
- **放弃 Cow / 全量 Arc<str> 的理由**:Cow<String> 每次克隆 memcpy、Arc<str> 从静态串构造也要分配;双轨 enum 让现有 `keyed::<T>("name")` / `Key<T>` 常量零变化零分配,只有动态路径付 Arc 成本;
- **TypeKey 失去 `Copy`**(enum 含 Arc)。受影响点:`provided` 遍历(fiber.rs:372-378 的 `map(|(k,_)| *k)`)、`notify_key_changed` 传值等改 `clone()`。TypeKey 克隆在装载/注册路径,频率低,可接受。逐点清点在实施时做,`clippy -D warnings` 保证无漏网。

### 2.3 事件总线 keyed 面(bus.rs / event.rs)

`BusInner` 三个注册面键 `TypeId` → `TypeKey`(hooks / wf_hooks / dispatch_tail)。**现有 API 签名一律不动**(115 项测试零改动),新增 keyed 变体:

```rust
impl EventBus {
    // 注册面
    pub fn on_keyed<E: Event>(&self, ctx: &Ctx, name: impl Into<Arc<str>>,
                              l: impl Listener<E>) -> Result<Disposer, CordisError>;
    pub fn on_keyed_opt<E: Event>(.., opts: EventOptions) -> ..;   // prepend/once 同现有
    pub fn once_keyed<E: Event>(..) -> ..;
    pub fn on_waterfall_keyed<E: Event>(.., l: impl WaterfallListener<E>) -> ..;

    // 分发面(四语义全套)
    pub fn emit_keyed<E: Event>(&self, ctx: &Ctx, name: impl Into<Arc<str>>, e: Arc<E>);
    pub async fn parallel_keyed<E: Event>(..) -> Result<(), CordisError>;
    pub async fn serial_keyed<E: Event>(..) -> Result<Option<E::Value>, CordisError>;
    pub fn waterfall_keyed<'a, E: Event, T: Terminal<E> + 'a>(..) -> BoxFuture<'a, ..>;
}
```

- 键构造即烙类型:`keyed_dynamic::<E>(name)` 的 TypeId 来自 E,分发侧 `take_hooks` 校验 `key.type_id == TypeId::of::<E>()`,不符 → `PluginFailed`(fail fast,防 downcast 灾难);
- keyed 变体内部一律转 TypeKey 走同一条注册/快照/派发代码路径(与非 keyed 共享实现,非 keyed = qualifier None 的特例),不复制四套逻辑;
- **D31 尾链键跟随 TypeKey**:同名(跨 Static/Dynamic 拼写)同类型共享一条派发链,跨名不保序——边界声明照旧。

### 2.4 HostEvent 与桥事件链路(rutis-cordis / rutis-dsh)

HostEvent 定义在 **rutis-cordis**(内核零 serde 红线不破):

```rust
/// 宿主透传事件:evt/emit 翻译产物。name 冗余存于载荷(qualifier 在注册键里,
/// 监听器从事件值内取名字)。serial/waterfall 的 Value 用 Value,宿主可短路。
pub struct HostEvent {
    pub name: String,
    pub payload: serde_json::Value,
}
impl Event for HostEvent {
    const NAME: &'static str = "host/*";
    type Value = serde_json::Value;
}
```

- `InboundHooks` 增 evt 转发缝:`on_event(ctx, name, payload)`;`Bridge::start` 需持有 `Ctx`(runner 侧传入,rutis_dsh.rs 组装时已有);
- 泵的 `dispatch_notify` 对 `evt/emit` 调 `ctx.events().emit_keyed::<HostEvent>(name, HostEvent{ name, payload: params })`;其余 Ntf 维持现状;
- 三字段透传(scopeId/sessionId/turnId, rpc.rs:41-46 预留)进 payload 或独立字段,实施时随 proto 定;
- 订阅方用法:`ctx.events().on_keyed::<HostEvent>("session/event", |ctx, e| ..)`,随注册方 fiber 卸载自动清理(D28 照旧)。

### 2.5 决策点

- **D33a 双轨 Qualifier enum**(Static 零分配)而非 Cow/Arc 全量;TypeKey 失去 Copy 的清点责任在实施提交内。
- **D33b HostEvent 放 rutis-cordis**,内核不引 serde;payload 用 `serde_json::Value`(桥线已是它的形状)。
- **D33c 现有 API 不改签名**,keyed 全家桶为增量;内部共享一条 TypeKey 路径。
- **D33d Context.filter 事件作用域过滤仍不做**(D29 维持):跨 isolate 事件不过滤;宿主事件过滤由订阅方在监听器内自查(与 dsh 桥 v1"只收纯 emit"的现状一致)。
- **D33e loader(名字→工厂注册表)不在本设计**:动态插件装载是另一个课题,D32 的 PluginFactory 是它的底座。

### 2.6 测试计划(契约 + e2e,目标 ~9 条)

1. keyed 注册/分发基础:同类型不同名互不串扰,静态/动态同名互通;
2. 动态名(运行时 `format!` 构造)注册并 emit 命中;
3. D31 回归:keyed emit 同名保序、不同名不进同链;
4. `serial_keyed` 短路值 / `waterfall_keyed` veto / `parallel_keyed` 聚合(复用现有四语义断言模式);
5. once_keyed 恰好一次、prepend_keyed 顺序;
6. 类型不符(手工构造的 key 与分发 E 错位)fail fast;
7. 监听器随注册方 fiber 卸载自动摘除(D28 keyed 路径);
8. parity 补拍:events.spec 的 `ctx.on()/ctx.once()/ctx.waterfall()` 字符串名用例,原判"部分对拍(JS 载体)",keyed 落地后内核可全拍(名字 → keyed_dynamic);
9. 桥 e2e(tcp_e2e 模式):宿主发 `evt/emit` → Rust 侧 `on_keyed::<HostEvent>` 收到,含 payload 断言。

---

## 三 里程碑

| 阶段 | 内容 | 交付判据 |
|---|---|---|
| M1 | D32:plugin.rs + ctx.rs + fiber.rs + 契约测试 1.7 | 全绿 + clippy/fmt 清零 |
| M2 | D33:key.rs + bus.rs + event.rs + 契约测试 2.6(1-8) | 同上;现有 115 项零改动 |
| M3 | 桥:HostEvent + InboundHooks evt 缝 + rutis_dsh 组装 + e2e | tcp_e2e 扩展全绿 |

M1/M2 可并行(不同文件为主,fiber.rs 仅 M1 触及)。每阶段独立提交、独立可回滚。

## 四 风险

- **TypeKey 失 Copy 的连锁**:编译器全量暴露,无静默风险;工作量在逐点 clone,预计 ≤ 20 处。
- **D32 工厂双形态**:`load()` 分支多一条取实例路径;fail_load 复用保证原子性不破。
- **bridge 持 Ctx 的所有权环**:Bridge(Arc) → Ctx(Arc<CtxInner>) → fiber(Weak),无强环;实施时以现有 `Shared` 结构为准核对。
- **事件风暴**:宿主高频 evt/emit 下,emit_keyed 每次构造 Arc<str> 键(一次分配);dsh 现实测事件频率低(session/agent 级),不构成瓶颈,先不做缓存。

## 五 不做清单

- `internal/update` waterfall 与 noSave(D32d);
- config schema / schemastery 对应物(D32e 连带);
- Context.filter 事件过滤(D33d);
- loader(插件名→工厂注册表、配置文件驱动装载)(D33e)——D32 的 PluginFactory 是其底座,本期不建注册表;
- `internal/*` 协议面整体(维持原对拍裁决);
- 事件名通配符/glob 订阅(宿主侧真需要再加,当前无需求证据)。

## 六 实施顺序建议

M1 → M2 → M3。M1 先行的理由:独立收益最大、不动总线;M2 改 TypeKey 触碰全仓签名,单独一个提交便于 review 与回滚。

## 七 实施记录(2026-09-21 执行完毕)

**全部落地,全 workspace 261 项测试全绿(新增 22:config_update 10 + event_keys 11 + host_events 1)**,clippy 对新增代码零警告(存量警告未动)。

- **M1(D32)**:`PluginFactory`(plugin.rs)+ `Ctx::plugin_with/plugin_from`(ctx.rs,与 `plugin()` 共享 mount 收尾)+ `FiberView::update/current_config`(fiber.rs)。内部:`FiberInner.factory/config` 双字段,`ErasedFactory` 擦除适配;`load()` 经 `current_plugin()` 统一取实例,工厂模式每代重造。**实施中补的一个设计缺口**:`resolve_deps` 原从 `plugin` 实例读 injects,工厂模式 plugin 为 None → 门控失效(直接装载);已改为工厂模式从当前 config 派生(config 先 clone 出锁再进用户回调,防重入死锁)。`update` 的 dry-run 用户回调裹 catch_unwind。测试 10 条:tests/config_update.rs。
- **M2(D33)**:`Qualifier` 双轨 enum(key.rs,Static 零分配 / Dynamic `Arc<str>`;**Eq/Hash 手写按字符串内容**,derive 会按变体判等——测试抓出);bus 三个注册面键 `TypeId` → `TypeKey`,keyed API 全家桶(`on/once/on_waterfall/emit/parallel/serial/waterfall` 的 `_keyed` 变体,内部共享一条 TypeKey 路径)。TypeKey 失 `Copy` 的连锁由编译器全量暴露,registry 的 lookup/consumers_of/notify_key_changed 顺势改吃借用。测试 11 条:tests/event_keys.rs(含 parity 补拍:cordis events.spec 字符串事件名内核,原判"部分对拍"升级为可全拍)。
- **M3**:rutis-cordis 新模块 events.rs(`HostEvent{name, payload}` + `forward_host_events(ctx, observe)`——observe 收全部 Ntf 与转发并行,恶形 evt 静默丢弃);rutis_dsh runner 的 on_notify 换装(stderr 摘要降为观察者)。e2e:tests/host_events.rs(MemoryWire 双名订阅 + 恶形不进总线 + 观察者全量)。

**与设计的偏差**:无实质偏差。一个 API 形态微调:`current_config` 返回 `Option<Arc<C>>`(设计草案写 `Option<Box<C>>`;Arc 存储使快照零拷贝,且 update 的存入也走 Arc)。
