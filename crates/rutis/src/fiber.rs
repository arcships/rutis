use std::any::{Any, TypeId};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::ctx::{Ctx, Shared};
use crate::effect::{Effect, EffectRecord};
use crate::error::{aggregate_arcs, panic_error, CordisError};
use crate::event::{CatchUnwind, Event};
use crate::key::{ScopeId, TypeKey};
use crate::{BoxFuture, Plugin, PluginFactory};

/// 六态状态机(§四:保留 TS 六态)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberState {
    /// 等待声明的依赖就绪。
    Pending,
    /// apply 执行中。
    Loading,
    /// 装配完成,提供中。
    Active,
    /// validate/apply 失败。
    Failed,
    /// 已移除,不可重启(root 除外)。
    Disposed,
    /// 清理执行中。
    Unloading,
}

/// fiber 快照(watch 载荷,generation 兼作 sequence,D24)。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub generation: u64,
    pub state: FiberState,
    pub error: Option<Arc<CordisError>>,
}

/// 插件身份(D10:注册返回的显式 id,非闭包指针)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginId(pub u64);

/// fiber 状态迁移事件(D24:锁内 FIFO 入队、锁外分发;
/// `seq` 保证提交顺序可识别,不保证 listener 完成顺序)。
#[derive(Debug, Clone)]
pub struct FiberStatusChanged {
    pub plugin_id: PluginId,
    pub seq: u64,
    pub generation: u64,
    pub from: FiberState,
    pub to: FiberState,
}

impl Event for FiberStatusChanged {
    const NAME: &'static str = "rutis::FiberStatusChanged";
    type Value = ();
}

pub(crate) enum TaskDone {
    Running,
    Done(Option<Arc<CordisError>>),
}

/// 恰好一次的转换任务(D6/D20):锁内单 `Arc`,同代 join 同一个;
/// 完成结果缓存 `Arc<CordisError>`(identity 归这里,D25)。
pub(crate) struct TransitionTask {
    pub done_tx: watch::Sender<TaskDone>,
    /// 常驻 receiver 保活(§八 7 同坑):无 receiver 时 watch 通道视为关闭,
    /// `send` 静默失败——消费者快速完成(如等值合并直接跳过)而 join 方
    /// 尚未订阅时,完成值丢失、join 永等(对拍 isolate 用例实锤)。
    _keepalive: watch::Receiver<TaskDone>,
}

impl TransitionTask {
    pub(crate) fn new() -> Arc<Self> {
        let (done_tx, done_rx) = watch::channel(TaskDone::Running);
        Arc::new(Self {
            done_tx,
            _keepalive: done_rx,
        })
    }

    pub(crate) fn complete(&self, err: Option<Arc<CordisError>>) {
        let _ = self.done_tx.send(TaskDone::Done(err));
    }
}

pub(crate) enum Intent {
    RefreshDeps,
    /// 带完成信号的依赖重查(驱逐 join 用,D14):处理完成后任务收到终态。
    RefreshDepsJoin(Arc<TransitionTask>),
    /// 稳定性栅栏(简化 Settle):FIFO 排到它时,此前入队的意图都已处理完,
    /// 按当时状态完成任务——"稳定了吗"由 mailbox 顺序直接回答。
    Settle(Arc<TransitionTask>),
    Restart(Arc<TransitionTask>),
    Dispose,
}

impl Intent {
    fn task(&self) -> Option<&Arc<TransitionTask>> {
        match self {
            Intent::RefreshDepsJoin(task) | Intent::Settle(task) | Intent::Restart(task) => {
                Some(task)
            }
            _ => None,
        }
    }
}

/// 意图携带的完成信号即刻完成(驱动不存在时防 join 永等)。
fn complete_intent(intent: &Intent, err: Option<Arc<CordisError>>) {
    if let Some(task) = intent.task() {
        task.complete(err);
    }
}

/// 哨兵键:`injects(config)` panic 时的占位声明——永远无 provider,
/// fiber 因此留 Pending(不就绪),错误已路由 ErrorSink。
#[derive(Debug)]
struct InjectsUnavailable;

enum NextState {
    Pending,
    Disposed,
}

pub(crate) struct Trans {
    pub generation: u64,
    pub state: FiberState,
    pub error: Option<Arc<CordisError>>,
    pub seq: u64,
    pub status_queue: Vec<FiberStatusChanged>,
    pub terminal_task: Option<Arc<TransitionTask>>,
}

pub(crate) struct FiberInner {
    pub id: PluginId,
    pub name: String,
    pub is_root: bool,
    pub plugin: Option<Arc<dyn Plugin>>,
    /// 工厂模式(D32):与 `plugin` 互斥(工厂模式 plugin 为 None)。
    pub factory: Option<Arc<dyn ErasedFactory>>,
    /// 工厂模式的当前 config(Arc 存储便于快照;update 原子替换)。
    pub config: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
    /// 工厂模式 spawn 时注册的依赖声明快照(D32 评审:防 injects 随
    /// config 漂移——registry 的 inject_index 只注册一次,漂移会让
    /// notify/驱逐静默失效。update 时新 config 派生的声明必须与
    /// 之集合相等,否则 Validation 拒绝,fail fast)。
    pub spawn_injects: Mutex<Vec<TypeKey>>,
    pub ctx: Ctx,
    pub parent_fiber: Option<Weak<FiberInner>>,
    /// 当前 fiber 代的取消 token(D27):每次 load 新建一代;卸载第②步取消。
    /// 意图发送方(dispose/restart/驱逐)在入队前预取消,使运行中的 apply
    /// 经 `ctx.cancelled()` 协作退出——驱动串行,不预取消则 apply 永远等不到。
    pub token: Mutex<CancellationToken>,
    pub transition: Mutex<Trans>,
    pub snapshot_tx: watch::Sender<Snapshot>,
    /// 常驻 receiver:保活 watch 通道(无 receiver 时通道视为关闭,send 静默失败)。
    pub snapshot_rx: watch::Receiver<Snapshot>,
    pub effects: Mutex<Vec<Arc<EffectRecord>>>,
    pub intents_tx: mpsc::UnboundedSender<Intent>,
    /// 驱动存活标志:置 false 后 post 拒绝投递并即刻完成携带的任务
    ///(评审 #2/#3:驱动退出后的排队意图不得让 join 永等)。
    pub alive: AtomicBool,
    /// 本 fiber 提供的 (key, scope)。
    pub provided: Mutex<Vec<(TypeKey, Option<crate::key::ScopeId>)>>,
    /// 等值合并键:上次成功装载解析到的依赖四元组集(§〇 epoch 语义)。
    /// 亦是驱逐判定的唯一事实源(D21:消费者 = last_deps 含该四元组者;
    /// 含作用域——同一 fiber 在不同作用域提供的同键绑定不可混淆)。
    pub last_deps: Mutex<Option<HashSet<(PluginId, u64, TypeKey, Option<crate::key::ScopeId>)>>>,
}

/// 工厂擦除面(D32):泛型 `PluginFactory<C>` 的类型擦除适配层内部协议。
pub(crate) trait ErasedFactory: Send + Sync + 'static {
    fn config_type_id(&self) -> TypeId;
    fn name(&self) -> &str;
    fn injects_erased(&self, config: &dyn Any) -> Vec<TypeKey>;
    fn validate_config_erased(&self, config: &dyn Any) -> Result<(), CordisError>;
    fn build_erased(&self, config: &dyn Any) -> Result<Box<dyn Plugin>, CordisError>;
}

/// 泛型工厂的擦除包装(同 registry `StoredValue` 手法)。
struct FactoryAdapter<F, C> {
    factory: F,
    _marker: std::marker::PhantomData<fn() -> C>,
}

impl<F, C> ErasedFactory for FactoryAdapter<F, C>
where
    F: PluginFactory<C>,
    C: Send + Sync + 'static,
{
    fn config_type_id(&self) -> TypeId {
        TypeId::of::<C>()
    }

    fn name(&self) -> &str {
        self.factory.name()
    }

    fn injects_erased(&self, config: &dyn Any) -> Vec<TypeKey> {
        config
            .downcast_ref::<C>()
            .map(|c| self.factory.injects(c))
            .unwrap_or_default()
    }

    fn validate_config_erased(&self, config: &dyn Any) -> Result<(), CordisError> {
        config
            .downcast_ref::<C>()
            .ok_or_else(|| CordisError::Validation {
                issues: vec!["config type mismatch".into()],
            })
            .and_then(|c| self.factory.validate_config(c))
    }

    fn build_erased(&self, config: &dyn Any) -> Result<Box<dyn Plugin>, CordisError> {
        config
            .downcast_ref::<C>()
            .ok_or_else(|| CordisError::Validation {
                issues: vec!["config type mismatch".into()],
            })
            .and_then(|c| self.factory.build(c))
    }
}

impl FiberInner {
    /// 当前代插件实例(D32):静态模式克隆既有实例;工厂模式用当前
    /// config 构造(每代一个新实例 = 配置热更新生效点)。
    /// `build` 是用户回调且 panic 概率高于 validate(评审:三处用户回调
    /// 唯独此处漏边界会杀驱动 → fiber 卡 Loading、join 永等)——panic
    /// 转 `fail_load`,与 validate 的边界对称。
    fn current_plugin(&self) -> Result<Arc<dyn Plugin>, CordisError> {
        if let Some(plugin) = &self.plugin {
            return Ok(plugin.clone());
        }
        let factory = self
            .factory
            .as_ref()
            .ok_or_else(|| CordisError::PluginFailed("fiber has no plugin or factory".into()))?;
        let config = self
            .config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| CordisError::PluginFailed("factory fiber has no config".into()))?;
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            factory.build_erased(config.as_ref())
        }))
        .unwrap_or_else(|p| Err(panic_error(p)));
        built.map(Arc::from)
    }

    /// 当前代 token(`ctx.cancellation_token()`/`ctx.cancelled()` 暴露给插件)。
    pub(crate) fn current_token(&self) -> CancellationToken {
        self.token.lock().unwrap().clone()
    }

    pub(crate) fn cancel_current(&self) {
        self.token.lock().unwrap().cancel();
    }

    fn new_generation_token(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.token.lock().unwrap() = token.clone();
        token
    }

    /// 投递无完成信号的意图。驱动已退出时返回 false。
    pub(crate) fn post(&self, intent: Intent) -> bool {
        if !self.alive.load(Ordering::SeqCst) {
            complete_intent(&intent, None);
            return false;
        }
        match self.intents_tx.send(intent) {
            Ok(()) => true,
            Err(err) => {
                complete_intent(&err.0, None);
                false
            }
        }
    }

    /// 投递携带完成信号的意图。发送后复查 alive:若发送落在驱动排空之后
    /// (该意图无人处理),任务即刻自完成——与退出排空的重复完成同值,无害
    ///(封住"查过 alive → 驱动退出排空 → send 才落地"的竞态,简化红线)。
    pub(crate) fn post_join(
        &self,
        task: Arc<TransitionTask>,
        make: impl FnOnce(Arc<TransitionTask>) -> Intent,
    ) -> bool {
        if !self.alive.load(Ordering::SeqCst) {
            task.complete(None);
            return false;
        }
        match self.intents_tx.send(make(task.clone())) {
            Ok(()) => {
                if !self.alive.load(Ordering::SeqCst) {
                    task.complete(None);
                }
                true
            }
            Err(_) => {
                task.complete(None);
                false
            }
        }
    }

    pub(crate) fn state(&self) -> FiberState {
        self.transition.lock().unwrap().state
    }

    pub(crate) fn state_snapshot(&self) -> Snapshot {
        let tr = self.transition.lock().unwrap();
        Snapshot {
            generation: tr.generation,
            state: tr.state,
            error: tr.error.clone(),
        }
    }

    /// 锁内:改状态 + FIFO 入队 + watch 发布(绝不持锁跨 await,D5)。
    fn set_state(&self, tr: &mut Trans, new: FiberState) {
        let old = tr.state;
        if old == new {
            return;
        }
        tr.state = new;
        tr.seq += 1;
        tr.status_queue.push(FiberStatusChanged {
            plugin_id: self.id,
            seq: tr.seq,
            generation: tr.generation,
            from: old,
            to: new,
        });
        let _ = self.snapshot_tx.send(Snapshot {
            generation: tr.generation,
            state: new,
            error: tr.error.clone(),
        });
    }

    /// 锁外:排干状态事件队列,fire-and-forget 分发(D24)。
    fn flush_status(&self) {
        let queue: Vec<FiberStatusChanged> = {
            let mut tr = self.transition.lock().unwrap();
            std::mem::take(&mut tr.status_queue)
        };
        for event in queue {
            self.ctx.shared().bus.emit(&self.ctx, Arc::new(event));
        }
    }

    fn resolve_deps(
        &self,
    ) -> (
        HashSet<(PluginId, u64, TypeKey, Option<ScopeId>)>,
        Vec<TypeKey>,
    ) {
        let mut satisfied: HashSet<(PluginId, u64, TypeKey, Option<ScopeId>)> = HashSet::new();
        let mut missing: Vec<TypeKey> = Vec::new();
        // 依赖声明来源:静态模式取实例;工厂模式从当前 config 派生(D32)。
        // config 先 clone 出锁再进用户回调,防重入 update 死锁。
        // 用户回调 panic 视为"依赖不就绪"(同 check() 的既有语义,
        // registry.rs):哨兵键永远无 provider → fiber 留 Pending,不杀驱动。
        let inject_keys: Vec<TypeKey> = if let Some(plugin) = &self.plugin {
            plugin.injects().to_vec()
        } else if let Some(factory) = &self.factory {
            match self.config.lock().unwrap().clone() {
                Some(config) => {
                    let derived = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        factory.injects_erased(config.as_ref())
                    }));
                    match derived {
                        Ok(keys) => keys,
                        Err(p) => {
                            self.ctx.error_sink()(Arc::new(panic_error(p)));
                            vec![TypeKey::of::<InjectsUnavailable>()]
                        }
                    }
                }
                None => Vec::new(),
            }
        } else {
            return (satisfied, missing);
        };
        let registry = &self.ctx.shared().registry;
        for key in inject_keys {
            let scope = self.ctx.scope_for(&key);
            match registry.resolve_dep(&key, scope.as_ref()) {
                Some((provider_id, provider_gen)) => {
                    satisfied.insert((provider_id, provider_gen, key, scope));
                }
                None => missing.push(key),
            }
        }
        (satisfied, missing)
    }

    async fn refresh_deps(this: &Arc<Self>) {
        if this.plugin.is_none() && this.factory.is_none() {
            return;
        }
        if this.state() == FiberState::Disposed {
            return;
        }
        let (satisfied, missing) = this.resolve_deps();
        // 被取消的 Loading 代视同"已装载":排干后再定去留
        let loaded = !matches!(this.state(), FiberState::Pending | FiberState::Disposed);
        if !missing.is_empty() {
            // 依赖缺失:已装载则卸载回 Pending;缺依赖长期 Pending 不报错(D22)
            if loaded {
                Self::unload(this, NextState::Pending).await;
            }
            return;
        }
        // 等值合并:依赖三元组集未变则跳过(§〇 epoch 内容派生相等键)
        let unchanged = {
            let last = this.last_deps.lock().unwrap();
            last.as_ref() == Some(&satisfied)
        };
        if unchanged {
            return;
        }
        if loaded {
            Self::unload(this, NextState::Pending).await;
        }
        Self::load(this, satisfied).await;
    }

    async fn load(this: &Arc<Self>, deps: HashSet<(PluginId, u64, TypeKey, Option<ScopeId>)>) {
        let shared = this.ctx.shared().clone();
        this.new_generation_token();
        {
            let mut tr = this.transition.lock().unwrap();
            tr.generation += 1;
            tr.error = None;
            Self::set_state(this, &mut tr, FiberState::Loading);
        }
        this.flush_status();

        // 当前代实例(D32):静态模式既有实例;工厂模式从当前 config 构造,
        // 构造失败按装载失败回滚(fail_load 保持装配原子性)。
        let plugin = match this.current_plugin() {
            Ok(p) => p,
            Err(e) => {
                Self::fail_load(this, e).await;
                return;
            }
        };

        // validate-before-store(D12):validate 失败 → Failed;
        // validate 是用户回调,panic 同样转入 Failed(评审 #6:不杀驱动)
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plugin.validate())) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                Self::fail_load(this, e).await;
                return;
            }
            Err(p) => {
                Self::fail_load(this, panic_error(p)).await;
                return;
            }
        }

        // 依赖快照(D21:装载窗口内即"绑定中",驱逐判定读它)
        *this.last_deps.lock().unwrap() = Some(deps.clone());

        // apply:直接等待退出(D7 第③步"等 apply 退出",不中止)。
        // 预取消(dispose/restart/驱逐)使观察 token 的插件经 ctx.cancelled()
        // 协作返回;不观察则 dispose 无限等待(协作取消限制,D27)。
        let ctx = this.ctx.clone();
        let outcome = CatchUnwind::new(plugin.apply(&ctx)).await;
        let result: Result<Effect, CordisError> = outcome.unwrap_or_else(|p| Err(panic_error(p)));
        match result {
            Ok(effect) => {
                let record = EffectRecord::new(effect);
                this.effects.lock().unwrap().push(record);
                // 惯性锁 2(fiber.spec:LOADING 期同 fiber 被重新 provide,in-flight
                // 加载直接完成进 ACTIVE):装载窗口内依赖集若已整体翻新且无缺失,
                // 就地采纳新三元组集——排队的重查意图将看到"未变化"而跳过,
                // 不触发换代重载。仍有缺失则不采纳,按原语义卸载(惯性锁 1)。
                let (fresh, missing) = this.resolve_deps();
                if missing.is_empty() {
                    *this.last_deps.lock().unwrap() = Some(fresh);
                }
                {
                    let mut tr = this.transition.lock().unwrap();
                    tr.error = None;
                    Self::set_state(this, &mut tr, FiberState::Active);
                }
                // 通知本 fiber 提供的服务键:后到消费者激活
                let provided: Vec<TypeKey> = this
                    .provided
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in provided {
                    shared.registry.notify_key_changed(&key);
                }
            }
            Err(e) => Self::fail_load(this, e).await,
        }
        this.flush_status();
    }

    /// 失败装载的回滚(评审 #1,对齐 TS fiber.ts:749-779 的失败路径):
    /// 经 UNLOADING 排干 apply 半注册的资源(监听/服务/子插件),再进 Failed。
    /// 装配失败保持原子性(支柱 1);清理错误路由 ErrorSink,Failed 携带装载错误。
    async fn fail_load(this: &Arc<Self>, error: CordisError) {
        {
            let mut tr = this.transition.lock().unwrap();
            Self::set_state(this, &mut tr, FiberState::Unloading);
        }
        this.flush_status();
        this.cancel_current();

        let cleanup_errors = Self::drain_effects(this).await;
        let sink = this.ctx.error_sink();
        for e in cleanup_errors {
            sink(e);
        }

        let arc = Arc::new(error);
        {
            let mut tr = this.transition.lock().unwrap();
            tr.error = Some(arc);
            Self::set_state(this, &mut tr, FiberState::Failed);
        }
        this.flush_status();
    }

    /// ④ EffectRecord 严格 LIFO 串行清理 + 消费边/依赖快照/提供表复位。
    async fn drain_effects(this: &Arc<Self>) -> Vec<Arc<CordisError>> {
        let handle = this.ctx.handle().clone();
        let effects: Vec<Arc<EffectRecord>> = std::mem::take(&mut *this.effects.lock().unwrap());
        let mut errors: Vec<Arc<CordisError>> = Vec::new();
        for record in effects.into_iter().rev() {
            if let Err(e) = record.drain(&handle).await {
                errors.push(e);
            }
        }
        *this.last_deps.lock().unwrap() = None;
        this.provided.lock().unwrap().clear();
        errors
    }

    /// 卸载五步(D7):①标 unloading → ②cancel 当前代 → ③(驱动串行,apply 已退出)
    /// → ④EffectRecord LIFO 清理 → ⑤发布终态。
    async fn unload(this: &Arc<Self>, next: NextState) {
        {
            let mut tr = this.transition.lock().unwrap();
            Self::set_state(this, &mut tr, FiberState::Unloading);
        }
        this.flush_status();

        // ② 取消当前代 token(插件后台任务经 ctx.cancelled() 协作退出)
        this.cancel_current();

        // ④ EffectRecord 严格 LIFO 串行清理
        let errors = Self::drain_effects(this).await;
        let err = aggregate_arcs(errors);

        match next {
            // 非终止卸载(restart/依赖刷新):清理错误路由 ErrorSink,
            // 不改变下一代状态(评审 P2:不得静默吞掉)
            NextState::Pending => {
                if let Some(e) = err {
                    (this.ctx.error_sink())(e);
                }
                let mut tr = this.transition.lock().unwrap();
                Self::set_state(this, &mut tr, FiberState::Pending);
            }
            NextState::Disposed => {
                let mut tr = this.transition.lock().unwrap();
                tr.error = err;
                Self::set_state(this, &mut tr, FiberState::Disposed);
            }
        }
        this.flush_status();
    }
}

/// fiber 驱动任务:意图串行处理(Loading→Unloading→Loading 不丢唤醒;
/// apply 与卸载天然互斥,跨代结果不会串染)。
pub(crate) async fn drive(this: Arc<FiberInner>, mut rx: mpsc::UnboundedReceiver<Intent>) {
    while let Some(intent) = rx.recv().await {
        match intent {
            Intent::RefreshDeps => FiberInner::refresh_deps(&this).await,
            Intent::RefreshDepsJoin(task) => {
                FiberInner::refresh_deps(&this).await;
                complete_task(&this, task);
            }
            // 稳定性栅栏:FIFO 排到这里时,此前入队的意图已全部处理完。
            // 错误只认 Failed(红线):dispose 聚合错误经 dispose() 的
            // 任务通道返回,不从 settle 漏出。
            Intent::Settle(task) => {
                let err = {
                    let tr = this.transition.lock().unwrap();
                    (tr.state == FiberState::Failed)
                        .then(|| tr.error.clone())
                        .flatten()
                };
                task.complete(err);
            }
            Intent::Restart(task) => {
                let state = this.state();
                if state == FiberState::Disposed {
                    // 仅 root 可重启(§五 root_restart);换新代 token,
                    // 并清除终态任务:重启后的 dispose 必须真正再卸载一轮
                    if this.is_root {
                        this.new_generation_token();
                        {
                            let mut tr = this.transition.lock().unwrap();
                            tr.error = None;
                            tr.terminal_task = None;
                            FiberInner::set_state(&this, &mut tr, FiberState::Active);
                        }
                        this.flush_status();
                    }
                    complete_task(&this, task);
                    continue;
                }
                if !matches!(state, FiberState::Pending) {
                    FiberInner::unload(&this, NextState::Pending).await;
                }
                FiberInner::refresh_deps(&this).await;
                complete_task(&this, task);
            }
            Intent::Dispose => {
                if this.state() != FiberState::Disposed {
                    FiberInner::unload(&this, NextState::Disposed).await;
                }
                let (task, err) = {
                    let tr = this.transition.lock().unwrap();
                    (tr.terminal_task.clone(), tr.error.clone())
                };
                if let Some(task) = task {
                    let _ = task.done_tx.send(TaskDone::Done(err));
                }
                if !this.is_root {
                    // 退出:先置 false(此后 post_join 的迟到投递自完成),
                    // 单遍排空已入队的残留并完成其任务(评审 #2/#3)
                    this.alive.store(false, Ordering::SeqCst);
                    while let Ok(intent) = rx.try_recv() {
                        complete_intent(&intent, None);
                    }
                    return; // 非 root 终态后驱动退出(句柄仍可 join 缓存终态)
                }
            }
        }
        this.flush_status();
    }
}

/// 以 fiber 当前终态错误完成转换任务(join 者收到同一 `Arc`,D25)。
fn complete_task(this: &Arc<FiberInner>, task: Arc<TransitionTask>) {
    let err = {
        let tr = this.transition.lock().unwrap();
        tr.error.clone()
    };
    task.complete(err);
}

pub(crate) fn spawn_fiber(
    shared: &Arc<Shared>,
    parent_ctx: Option<&Ctx>,
    plugin: Option<Arc<dyn Plugin>>,
    is_root: bool,
) -> Arc<FiberInner> {
    spawn_fiber_inner(shared, parent_ctx, plugin, None, None, is_root)
}

/// 工厂模式 spawn(D32:`Ctx::plugin_with` 入口)。
pub(crate) fn spawn_factory_fiber<F, C>(
    shared: &Arc<Shared>,
    parent_ctx: Option<&Ctx>,
    factory: F,
    config: C,
    is_root: bool,
) -> Arc<FiberInner>
where
    F: PluginFactory<C>,
    C: Send + Sync + 'static,
{
    let erased: Arc<dyn ErasedFactory> = Arc::new(FactoryAdapter {
        factory,
        _marker: std::marker::PhantomData,
    });
    // spawn 时派生依赖声明:用户回调 panic 视为不就绪(哨兵键,
    // fiber 留 Pending)——同 resolve_deps 的既有语义,不杀调用方。
    let injects = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        erased.injects_erased(&config)
    })) {
        Ok(keys) => keys,
        Err(_p) => vec![TypeKey::of::<InjectsUnavailable>()],
    };
    let boxed: Arc<dyn Any + Send + Sync> = Arc::new(config);
    let this = spawn_fiber_inner(shared, parent_ctx, None, Some(erased), Some(boxed), is_root);
    // 工厂模式门控声明从 config 派生,spawn 时注册一次。声明快照留档:
    // update 时新 config 派生的声明须集合相等(fail-fast 校验,
    // 见 FiberView::update),防 inject_index 陈旧导致 notify/驱逐静默失效。
    *this.spawn_injects.lock().unwrap() = injects.clone();
    for key in injects {
        shared.registry.register_inject(key, &this);
    }
    this
}

fn spawn_fiber_inner(
    shared: &Arc<Shared>,
    parent_ctx: Option<&Ctx>,
    plugin: Option<Arc<dyn Plugin>>,
    factory: Option<Arc<dyn ErasedFactory>>,
    config: Option<Arc<dyn Any + Send + Sync>>,
    is_root: bool,
) -> Arc<FiberInner> {
    let id = PluginId(
        shared
            .next_plugin_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
    );
    let initial = if is_root {
        FiberState::Active
    } else {
        FiberState::Pending
    };
    let (snapshot_tx, snapshot_rx) = watch::channel(Snapshot {
        generation: 0,
        state: initial,
        error: None,
    });
    let (tx, rx) = mpsc::unbounded_channel();
    let token = CancellationToken::new();
    let parent_fiber = parent_ctx.map(|p| p.weak_fiber());

    let this = Arc::new_cyclic(|weak: &Weak<FiberInner>| {
        let ctx = match parent_ctx {
            Some(parent) => Ctx::new_child(shared.clone(), parent, weak.clone(), None),
            None => Ctx::new_root(shared.clone(), weak.clone()),
        };
        FiberInner {
            id,
            name: plugin
                .as_ref()
                .map(|p| p.name().to_string())
                .or_else(|| factory.as_ref().map(|f| f.name().to_string()))
                .unwrap_or_else(|| "root".to_string()),
            is_root,
            plugin,
            factory,
            config: Mutex::new(config),
            spawn_injects: Mutex::new(Vec::new()),
            ctx,
            parent_fiber,
            token: Mutex::new(token),
            transition: Mutex::new(Trans {
                generation: 0,
                state: initial,
                error: None,
                seq: 0,
                status_queue: Vec::new(),
                terminal_task: None,
            }),
            snapshot_tx: snapshot_tx.clone(),
            snapshot_rx,
            effects: Mutex::new(Vec::new()),
            intents_tx: tx.clone(),
            alive: AtomicBool::new(true),
            provided: Mutex::new(Vec::new()),
            last_deps: Mutex::new(None),
        }
    });

    if let Some(plugin) = &this.plugin {
        for key in plugin.injects() {
            shared.registry.register_inject(key.clone(), &this);
        }
    }

    shared.handle.spawn(drive(this.clone(), rx));
    this
}

/// fiber 句柄:注册返回,`IntoFuture` = 等待进入稳定态(启动错误经 Err 返回)。
pub struct FiberView {
    /// 插件身份(D10)。
    pub id: PluginId,
    inner: Arc<FiberInner>,
}

impl Clone for FiberView {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            inner: self.inner.clone(),
        }
    }
}

impl FiberView {
    pub(crate) fn from_inner(inner: Arc<FiberInner>) -> Self {
        Self {
            id: inner.id,
            inner,
        }
    }

    /// 当前快照。
    pub fn state(&self) -> Snapshot {
        self.inner.state_snapshot()
    }

    /// 插件显示名。
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// 订阅状态变化(watch:last-value,晚订阅先 `borrow()` 再 `changed()`,D6)。
    pub fn watch(&self) -> watch::Receiver<Snapshot> {
        self.inner.snapshot_rx.clone()
    }

    /// dispose:恰好一次;重复/并发调用 join 同一 `Arc<CordisError>`(D6/D20)。
    /// 入队前预取消当前代 token(运行中的 apply 协作退出,D27 第②步前置)。
    pub fn dispose(&self) -> BoxFuture<'static, Result<(), Arc<CordisError>>> {
        // 登记在调用点同步完成(评审 #2):dispose() 返回后,并发的
        // restart() 立刻可见终态任务并拒绝,不依赖本 future 被 poll
        let task = {
            let mut tr = self.inner.transition.lock().unwrap();
            match &tr.terminal_task {
                Some(task) => task.clone(),
                None => {
                    self.inner.cancel_current();
                    let task = TransitionTask::new();
                    tr.terminal_task = Some(task.clone());
                    self.inner.post(Intent::Dispose);
                    task
                }
            }
        };
        Box::pin(async move { join_task(&task).await })
    }

    /// restart:干净卸载后重装配(无状态迁移,Erlang code_change 省略版)。
    /// 已 Disposed(或 Dispose 已入队/驱动已退出)的非 root fiber返回
    /// `InactiveEffect`——不得让 join 永等(评审 #2)。
    /// 经转换任务 join:等待本次 restart 排干并回传终态错误。
    pub fn restart(&self) -> BoxFuture<'static, Result<(), Arc<CordisError>>> {
        let this = self.inner.clone();
        Box::pin(async move {
            {
                let tr = this.transition.lock().unwrap();
                if !this.is_root
                    && (tr.terminal_task.is_some() || !this.alive.load(Ordering::SeqCst))
                {
                    return Err(Arc::new(CordisError::InactiveEffect));
                }
            }
            this.cancel_current(); // 预取消:运行中的 apply 协作退出
            let task = TransitionTask::new();
            this.post_join(task.clone(), Intent::Restart);
            join_task(&task).await
        })
    }

    /// 配置热更新(D32):dry-run 通过后存入并按状态矩阵重启。
    ///
    /// - dry-run = `validate_config` + `build` + 实例 `validate`,任一失败
    ///   返回 Err,**不存不重启**(现状不动);
    /// - 通过后存 config,经 `Intent::Restart` 收敛:Active/Loading/Failed
    ///   → 卸载重载(新 config 构造新实例);Pending → 直接重查,依赖就绪
    ///   即用新 config 装载,未就绪则存着等门控;
    /// - 非工厂 fiber(静态 `plugin()` 装载)返回 `Validation` 错;
    /// - 与 restart 同款终态拒绝(Disposed/终态已登记/驱动已退出)。
    pub fn update<C: Send + Sync + 'static>(
        &self,
        new_config: C,
    ) -> BoxFuture<'static, Result<(), Arc<CordisError>>> {
        let this = self.inner.clone();
        Box::pin(async move {
            {
                let tr = this.transition.lock().unwrap();
                if !this.is_root
                    && (tr.terminal_task.is_some() || !this.alive.load(Ordering::SeqCst))
                {
                    return Err(Arc::new(CordisError::InactiveEffect));
                }
            }
            let factory = this
                .factory
                .as_ref()
                .and_then(|f| (f.config_type_id() == TypeId::of::<C>()).then(|| f.clone()));
            let Some(factory) = factory else {
                let reason = if this.factory.is_some() {
                    "config type mismatch"
                } else {
                    "fiber has no factory (static plugin cannot update)"
                };
                return Err(Arc::new(CordisError::Validation {
                    issues: vec![reason.into()],
                }));
            };
            // dry-run(D32b):injects 稳定性 → validate_config → build →
            // 实例 validate,产物丢弃(build 纯构造契约)。用户回调 panic
            // 转错误不杀调用方。injects 校验:新 config 派生的声明必须与
            // spawn 时注册的集合相等(评审:registry 只在 spawn 注册一次,
            // 漂移会让 notify/驱逐静默失效——fail fast 优于静默)。
            let boxed: Arc<dyn Any + Send + Sync> = Arc::new(new_config);
            let spawn_injects: HashSet<TypeKey> =
                this.spawn_injects.lock().unwrap().iter().cloned().collect();
            let dry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let derived: HashSet<TypeKey> =
                    factory.injects_erased(boxed.as_ref()).into_iter().collect();
                if derived != spawn_injects {
                    return Err(CordisError::Validation {
                        issues: vec![
                            "injects(config) must be stable across configs (spawn-time keys \
                             differ from new config)"
                                .into(),
                        ],
                    });
                }
                factory.validate_config_erased(boxed.as_ref())?;
                let instance = factory.build_erased(boxed.as_ref())?;
                instance.validate()?;
                Ok::<(), CordisError>(())
            }));
            match dry {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(Arc::new(e)),
                Err(p) => return Err(Arc::new(panic_error(p))),
            }
            *this.config.lock().unwrap() = Some(boxed);
            this.cancel_current(); // 预取消:运行中的 apply 协作退出(同 restart)
            let task = TransitionTask::new();
            this.post_join(task.clone(), Intent::Restart);
            join_task(&task).await
        })
    }

    /// 当前 config 快照(D32,诊断用)。非工厂 fiber 或类型不符返回 None。
    pub fn current_config<C: Send + Sync + 'static>(&self) -> Option<Arc<C>> {
        let boxed = self.inner.config.lock().unwrap().clone()?;
        boxed.downcast::<C>().ok()
    }
}

pub(crate) async fn join_task(task: &Arc<TransitionTask>) -> Result<(), Arc<CordisError>> {
    let mut rx = task.done_tx.subscribe();
    loop {
        match &*rx.borrow() {
            TaskDone::Done(err) => return err.clone().map_or(Ok(()), Err),
            TaskDone::Running => {}
        }
        if rx.changed().await.is_err() {
            // 发送端随 fiber 析构:不得伪装成功(评审 #8)
            return Err(Arc::new(CordisError::PluginFailed(
                "fiber dropped before transition completed".into(),
            )));
        }
    }
}

/// settle(`IntoFuture`)= Settle 栅栏 + join:"稳定了吗"由 mailbox 的 FIFO
/// 顺序直接回答——Settle 排到时,此前入队的意图(初始装载、再装载通知、
/// restart)都已处理完;错误只认 Failed(dispose 聚合错误经 dispose() 返回)。
fn settle(this: &FiberInner) -> BoxFuture<'static, Result<(), Arc<CordisError>>> {
    let task = TransitionTask::new();
    this.post_join(task.clone(), Intent::Settle);
    Box::pin(async move { join_task(&task).await })
}

impl std::future::IntoFuture for FiberView {
    type Output = Result<(), Arc<CordisError>>;
    type IntoFuture = BoxFuture<'static, Result<(), Arc<CordisError>>>;

    fn into_future(self) -> Self::IntoFuture {
        settle(&self.inner)
    }
}

impl std::future::IntoFuture for &FiberView {
    type Output = Result<(), Arc<CordisError>>;
    type IntoFuture = BoxFuture<'static, Result<(), Arc<CordisError>>>;

    fn into_future(self) -> Self::IntoFuture {
        settle(&self.inner)
    }
}
