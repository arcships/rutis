use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::bus::EventBus;
use crate::effect::{Disposer, Effect, EffectRecord};
use crate::error::{default_sink, CordisError, ErrorSink};
use crate::fiber::{
    join_task, spawn_fiber, DisposeWaitError, FiberInner, FiberState, FiberView, Intent,
    TransitionTask,
};
use crate::key::{ScopeId, TypeKey};
use crate::registry::{Binding, CheckFn, Registry, StoredValue};
use crate::Plugin;
use crate::PluginFactory;

pub(crate) struct Shared {
    pub handle: Handle,
    pub bus: EventBus,
    pub registry: Registry,
    pub error_sink: ErrorSink,
    pub next_plugin_id: AtomicU64,
    pub closing: AtomicBool,
    pub shutdown_task: Mutex<Option<Arc<TransitionTask>>>,
}

pub(crate) struct CtxInner {
    pub(crate) shared: Arc<Shared>,
    pub(crate) parent: Option<Ctx>,
    pub(crate) fiber: Weak<FiberInner>,
    pub(crate) isolate: Option<(TypeKey, ScopeId)>,
}

/// 上下文 = `Arc<CtxInner>`(所有权模型:Clone 廉价;isolate/plugin 返回共享内核的新 Ctx)。
#[derive(Clone)]
pub struct Ctx(Arc<CtxInner>);

impl Ctx {
    pub(crate) fn new_child(
        shared: Arc<Shared>,
        parent: &Ctx,
        fiber: Weak<FiberInner>,
        isolate: Option<(TypeKey, ScopeId)>,
    ) -> Self {
        Self(Arc::new(CtxInner {
            shared,
            parent: Some(parent.clone()),
            fiber,
            isolate,
        }))
    }

    pub(crate) fn new_root(shared: Arc<Shared>, fiber: Weak<FiberInner>) -> Self {
        Self(Arc::new(CtxInner {
            shared,
            parent: None,
            fiber,
            isolate: None,
        }))
    }

    pub(crate) fn weak_fiber(&self) -> Weak<FiberInner> {
        self.0.fiber.clone()
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.0.shared
    }

    pub fn handle(&self) -> &Handle {
        &self.0.shared.handle
    }

    /// 错误路由:插件运行期(apply 之外)的异步错误经此上报,不崩 root。
    /// 供外部插件在 turn 边界兜底(如 session 落盘失败),与框架内部
    /// 路由同一 sink——可观测,不静默。
    pub fn error_sink(&self) -> ErrorSink {
        self.0.shared.error_sink.clone()
    }

    /// 自动路径:`Handle::try_current()` 失败返回明确错误,绝不隐式建 runtime(D8)。
    pub fn root() -> Result<Ctx, CordisError> {
        let handle = Handle::try_current().map_err(|_| {
            CordisError::PluginFailed(
                "no tokio runtime in scope; construct inside #[tokio::test]/runtime, or use Ctx::root_with(handle)".into(),
            )
        })?;
        Ok(Self::root_with_sink(handle, default_sink()))
    }

    /// 注入构造(优先路径,D8)。
    pub fn root_with(handle: Handle) -> Ctx {
        Self::root_with_sink(handle, default_sink())
    }

    /// 注入构造 + 自定义 ErrorSink。
    pub fn root_with_sink(handle: Handle, sink: ErrorSink) -> Ctx {
        let shared = Arc::new(Shared {
            handle,
            bus: EventBus::new(),
            registry: Registry::new(),
            error_sink: sink,
            next_plugin_id: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            shutdown_task: Mutex::new(None),
        });
        let root_fiber = spawn_fiber(&shared, None, None, true);
        root_fiber.ctx.clone()
    }

    /// root fiber 句柄(root dispose 清子树 / root restart,§五 root_restart)。
    /// 最终 shutdown 并释放所有 `FiberView` 后返回 None。
    pub fn root_view(&self) -> Option<FiberView> {
        let mut current = self.clone();
        while let Some(parent) = current.0.parent.clone() {
            current = parent;
        }
        current.0.fiber.upgrade().map(FiberView::from_inner)
    }

    /// 最终关闭 root。重复或并发调用共享同一完成结果；现有 `dispose`
    /// 仍可 `restart`。关闭会拒绝新注册并等待当前装载、子树与清理完成。
    /// 不协作的代码可能使等待无限延长，可用 `shutdown_with_timeout` 限制等待。
    pub fn shutdown(&self) -> crate::BoxFuture<'static, Result<(), Arc<CordisError>>> {
        let task = {
            let mut slot = self.0.shared.shutdown_task.lock().unwrap();
            if let Some(task) = slot.as_ref() {
                task.clone()
            } else {
                let task = TransitionTask::new();
                *slot = Some(task.clone());
                self.0.shared.closing.store(true, Ordering::SeqCst);
                if let Some(root) = self.root_view() {
                    root.inner.cancel_current();
                    root.inner.post(Intent::Shutdown(task.clone()));
                } else {
                    task.complete(Some(Arc::new(CordisError::Closed)));
                }
                task
            }
        };
        Box::pin(async move { join_task(&task).await })
    }

    /// 限制等待最终关闭的时间。超时后关闭仍在后台进行，重复调用
    /// `shutdown()` 可继续 join 同一结果。
    pub fn shutdown_with_timeout(
        &self,
        limit: Duration,
    ) -> crate::BoxFuture<'static, Result<(), DisposeWaitError>> {
        let root = self.root_view();
        let pending = self.shutdown();
        Box::pin(async move {
            let started = Instant::now();
            match tokio::time::timeout(limit, pending).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(DisposeWaitError::Failed(error)),
                Err(_) => {
                    let snapshot = root.as_ref().map(FiberView::state);
                    Err(DisposeWaitError::TimedOut {
                        plugin_id: root.as_ref().map_or(crate::PluginId(1), |v| v.id),
                        generation: snapshot.as_ref().map_or(0, |s| s.generation),
                        state: snapshot.map_or(FiberState::Disposed, |s| s.state),
                        elapsed: started.elapsed(),
                    })
                }
            }
        })
    }

    /// 事件总线(全局唯一;事件分发不跨 isolate 过滤,D29)。
    pub fn events(&self) -> &EventBus {
        &self.0.shared.bus
    }

    /// scope 解析:沿 Ctx 父链回溯,取该键最近的 isolate 覆盖(§四:保留父链查找)。
    pub(crate) fn scope_for(&self, key: &TypeKey) -> Option<ScopeId> {
        let mut current = Some(self.clone());
        while let Some(ctx) = current {
            if let Some((k, scope)) = &ctx.0.isolate {
                if k == key {
                    return Some(scope.clone());
                }
            }
            current = ctx.0.parent.clone();
        }
        None
    }

    /// isolate 作用域(支柱 3):按 ServiceKey 隔离,同 label 合并(TS 语义,D21);
    /// 返回的 Ctx 保留原 fiber 所有权(D28)。
    pub fn isolate(&self, key: impl Into<TypeKey>, label: &str) -> Ctx {
        Ctx::new_child(
            self.0.shared.clone(),
            self,
            self.0.fiber.clone(),
            Some((key.into(), Arc::from(label))),
        )
    }

    /// 类型键读取(显式定位器,D13):沿父链解析作用域;
    /// provider 非 Active 时不可见,但其子树内自访问除外(清理期自访问,§四)。
    /// 访问方自身失活(Unloading/Disposed)时同样不可见——TS inactive context
    /// 语义(reflect.spec 'service inject leak' 的语言无关内核;provider 子树
    /// 内自访问豁免,与清理期自访问同一条规则)。
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.get_as::<T>(TypeKey::of::<T>())
    }

    /// 带显式 key 的读取(多实例,shaku Keyed 模式)。
    pub fn get_as<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: impl Into<TypeKey>,
    ) -> Option<Arc<T>> {
        let key = key.into();
        let scope = self.scope_for(&key);
        let binding = self.0.shared.registry.lookup(&key, scope.as_ref())?;
        let provider = binding.provider.upgrade()?;
        let self_access = self.in_subtree_of(&provider);
        if !self_access {
            match self.0.fiber.upgrade() {
                None => return None,
                Some(accessor)
                    if matches!(
                        accessor.state(),
                        FiberState::Unloading | FiberState::Disposed
                    ) =>
                {
                    return None;
                }
                _ => {}
            }
            let visible = provider.state() == FiberState::Active
                && !binding.removing.load(std::sync::atomic::Ordering::SeqCst);
            if !visible {
                return None;
            }
        }
        binding.value.downcast::<T>()
    }

    fn in_subtree_of(&self, other: &Arc<FiberInner>) -> bool {
        let mut current = self.0.fiber.upgrade();
        while let Some(fiber) = current {
            if Arc::ptr_eq(&fiber, other) {
                return true;
            }
            current = fiber.parent_fiber.as_ref().and_then(|w| w.upgrade());
        }
        false
    }

    /// 值语义注册便捷入口(D13)。
    pub fn provide<T: Send + Sync + 'static>(&self, value: T) -> Result<Disposer, CordisError> {
        self.provide_as::<T>(TypeKey::of::<T>(), Arc::new(value))
    }

    /// trait 对象 / 共享实例注册入口。
    pub fn provide_as<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: impl Into<TypeKey>,
        value: Arc<T>,
    ) -> Result<Disposer, CordisError> {
        self.provide_inner(key.into(), value, None)
    }

    /// 带 `check()` 谓词的注册(§四:check 门控保留)。
    pub fn provide_as_with_check<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: impl Into<TypeKey>,
        value: Arc<T>,
        check: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Disposer, CordisError> {
        self.provide_inner(key.into(), value, Some(Arc::new(check)))
    }

    fn provide_inner<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: TypeKey,
        value: Arc<T>,
        check: Option<CheckFn>,
    ) -> Result<Disposer, CordisError> {
        if !key.has_type::<T>() {
            return Err(CordisError::Validation {
                issues: vec![format!(
                    "service key {} expects value of type {}, got {}",
                    key.describe(),
                    key.type_name(),
                    std::any::type_name::<T>()
                )],
            });
        }
        if self.0.shared.closing.load(Ordering::SeqCst) {
            return Err(CordisError::Closed);
        }
        let fiber = self.0.fiber.upgrade().ok_or(CordisError::InactiveEffect)?;
        {
            // 重入报错检查先于重复注册检查(TS assertActive 语义,fiber.ts:434-436)
            let tr = fiber.transition.lock().unwrap();
            if matches!(tr.state, FiberState::Unloading | FiberState::Disposed) {
                return Err(CordisError::InactiveEffect);
            }
        }
        let scope = self.scope_for(&key);
        let provider_gen = {
            let tr = fiber.transition.lock().unwrap();
            tr.generation
        };
        // 同步原子插入为主操作(评审 #9):重复注册直接把错误返给调用方,
        // 不再丢进 error sink 后假报 Ok
        let stored = self.0.shared.registry.insert_binding(
            key.clone(),
            scope.clone(),
            Binding {
                value: StoredValue::new(value),
                provider: Arc::downgrade(&fiber),
                provider_id: fiber.id,
                provider_gen,
                check,
                removing: std::sync::atomic::AtomicBool::new(false),
            },
        )?;
        fiber
            .provided
            .lock()
            .unwrap()
            .push((key.clone(), scope.clone()));
        if fiber.state() == FiberState::Active {
            self.0.shared.registry.notify_key_changed(&key);
        }

        // 清理 effect 只负责删除:驱逐该三元组精确匹配的消费者并最终摘除
        let shared = self.0.shared.clone();
        let pid = fiber.id;
        let provider = Arc::downgrade(&fiber);
        let evict_scope = scope.clone();
        let evict_key = key.clone();
        match self.effect(move || {
            Effect::AsyncDisposer(Box::new(move || {
                let shared = shared.clone();
                let provider = provider.clone();
                let scope = evict_scope.clone();
                Box::pin(async move {
                    evict_and_finalize(&shared, provider, pid, provider_gen, evict_key, scope).await
                })
            }))
        }) {
            Ok(disposer) => Ok(disposer),
            Err(e) => {
                // 极窄竞态兜底:插入后 fiber 进入卸载——回滚自己的那份插入
                self.0
                    .shared
                    .registry
                    .finalize_binding_if(key.clone(), scope, &stored);
                Err(e)
            }
        }
    }

    /// 注册清理效应(D23):`f` 立即执行,返回的清理在卸载时 LIFO 执行。
    /// fiber 已 Disposed/Unloading 时返回 `InactiveEffect`(§四:重入报错)。
    pub fn effect(&self, f: impl FnOnce() -> Effect) -> Result<Disposer, CordisError> {
        let record = self.register_effect(f)?;
        let handle = self.handle().clone();
        Ok(Disposer::new(Box::new(move || {
            let record = record.clone();
            let handle = handle.clone();
            Box::pin(async move { record.drain(&handle).await })
        })))
    }

    /// `effect` 的内部形态:返回记录本体,供 mount 登记等持有引用。
    /// Disposer 语义不变:drop 不触发清理,fiber 卸载仍兜底(D28)。
    pub(crate) fn register_effect(
        &self,
        f: impl FnOnce() -> Effect,
    ) -> Result<Arc<EffectRecord>, CordisError> {
        if self.0.shared.closing.load(Ordering::SeqCst) {
            return Err(CordisError::Closed);
        }
        let fiber = self.0.fiber.upgrade().ok_or(CordisError::InactiveEffect)?;
        // factory 锁外执行;但"状态检查 + effects 入队"必须在同一临界区
        //(transition → effects 嵌套,锁序无反向):否则检查通过后驱动恰好
        // 卸载取走 effects,新记录漏掉本轮清理而泄漏(评审 P1)
        let record = EffectRecord::new(f(), self.0.fiber.clone());
        let handle = self.handle().clone();
        {
            let tr = fiber.transition.lock().unwrap();
            if self.0.shared.closing.load(Ordering::SeqCst)
                || matches!(tr.state, FiberState::Unloading | FiberState::Disposed)
            {
                drop(tr);
                // 生命周期已越过登记点:f() 可能已有副作用(如插入了监听器),
                // 立即排干该记录的清理并返失败
                let sink = self.error_sink();
                let drain_handle = handle.clone();
                handle.spawn(async move {
                    if let Err(e) = record.drain(&drain_handle).await {
                        sink(e);
                    }
                });
                return Err(if self.0.shared.closing.load(Ordering::SeqCst) {
                    CordisError::Closed
                } else {
                    CordisError::InactiveEffect
                });
            }
            fiber.effects.lock().unwrap().push(record.clone());
        }
        Ok(record)
    }

    /// 装载插件(支柱 1)。返回 FiberView;级联卸载:child dispose 注册为
    /// parent fiber 的 effect(D28:child plugin 自动归 parent fiber 所有)。
    pub fn plugin(&self, p: impl Plugin) -> FiberView {
        let fiber = spawn_fiber(&self.0.shared, Some(self), Some(Arc::new(p)), false);
        self.mount_fiber(fiber)
    }

    /// 工厂模式装载(D32:配置热更新):依赖门控声明从 config 派生,
    /// 每代装载用当前 config 构造实例;返回的 FiberView 可 `update(config)`。
    pub fn plugin_with<C: Send + Sync + 'static>(
        &self,
        factory: impl PluginFactory<C>,
        config: C,
    ) -> FiberView {
        let fiber =
            crate::fiber::spawn_factory_fiber(&self.0.shared, Some(self), factory, config, false);
        self.mount_fiber(fiber)
    }

    /// 工厂模式装载的闭包便捷形态(D32):零依赖声明的单方法工厂。
    /// 需要声明 `injects`/`validate_config` 时实现 [`PluginFactory`]。
    pub fn plugin_from<C: Send + Sync + 'static>(
        &self,
        build: impl Fn(&C) -> Result<Box<dyn Plugin>, CordisError> + Send + Sync + 'static,
        config: C,
    ) -> FiberView {
        struct ClosureFactory<F> {
            build: F,
        }
        impl<C, F> PluginFactory<C> for ClosureFactory<F>
        where
            C: Send + Sync + 'static,
            F: Fn(&C) -> Result<Box<dyn Plugin>, CordisError> + Send + Sync + 'static,
        {
            fn build(&self, config: &C) -> Result<Box<dyn Plugin>, CordisError> {
                (self.build)(config)
            }
        }
        self.plugin_with(ClosureFactory { build }, config)
    }

    /// 装配收尾:级联卸载 effect + 初始装载意图(评审 #10:parent 失活时
    /// 处置子 fiber,不再触发装载)。
    fn mount_fiber(&self, fiber: std::sync::Arc<crate::fiber::FiberInner>) -> FiberView {
        let view = FiberView::from_inner(fiber.clone());
        if !fiber.alive.load(Ordering::SeqCst) {
            return view;
        }
        let child = view.clone();
        let sink = self.error_sink();
        let registered = self.register_effect(move || {
            Effect::AsyncDisposer(Box::new(move || {
                let child = child.clone();
                let sink = sink.clone();
                Box::pin(async move {
                    // 级联 dispose:parent 卸载时子尚未处置,错误仅经 sink 可见;
                    // 子已 Disposed(调用方已从 dispose() 收到同一错误)则不再
                    // 重复上报——0.2.1 子终态退出也会 drain 本记录
                    let delivered = matches!(child.state().state, FiberState::Disposed);
                    if let Err(e) = child.dispose().await {
                        if !delivered {
                            sink(e);
                        }
                    }
                    Ok(())
                })
            }))
        });
        match registered {
            Ok(record) => {
                // 子终态退出时经此引用 drain,记录从本 fiber 的 effects
                // 列表自摘(0.2.1:长寿 parent 下瞬态子插件不残留 mount 记录)
                *fiber.mount.lock().unwrap() = Some(record);
                fiber.post(Intent::RefreshDeps);
            }
            Err(_) => {
                // parent 已失活:处置子 fiber,且不再触发装载(评审 #10:
                // 避免 Dispose 之后入队的重载意图无人处理)
                if self.0.shared.closing.load(Ordering::SeqCst) {
                    fiber.post(Intent::Shutdown(TransitionTask::new()));
                } else {
                    fiber.post(Intent::Dispose);
                }
            }
        }
        view
    }

    /// 当前 fiber 代的取消 token(D27:每代独立 token,卸载第②步取消)。
    pub fn cancellation_token(&self) -> CancellationToken {
        match self.0.fiber.upgrade() {
            Some(fiber) => fiber.current_token(),
            // fiber 已析构 ≡ 代已结束:返回预取消 token,cancelled() 不永等(评审 P2)
            None => {
                let token = CancellationToken::new();
                token.cancel();
                token
            }
        }
    }

    /// 等待当前 fiber 代被取消(协作取消;不观察则 dispose 无限等待,D27 限制)。
    pub fn cancelled(&self) -> impl Future<Output = ()> + Send + 'static {
        let token = self.cancellation_token();
        async move { token.cancelled().await }
    }

    /// 触发依赖重查(check() 谓词结果变更等场景)。
    pub fn refresh(&self) {
        self.0.shared.registry.refresh_all();
    }
}

/// 服务摘除(D14):①捕获本次绑定并标记摘除(严格解析立即失败,绑定保留供
/// 清理期自访问)→ ②预取消 + 可 join 的依赖重查(驱动已退出的消费者任务即刻
/// 完成,不 join 永等,评审 #3)→ ③并发排干后按 Arc 身份最终摘除——
/// 摘除窗口内被新 provide 替换过的槽位不动(TS dispose 同步释放槽位语义,
/// 对拍 fiber.spec inertia lock 2)→ ④摘除 provider 的 `provided` 记账
/// (0.2.1:长寿 root 上反复 provide/dispose 不累积)。
async fn evict_and_finalize(
    shared: &Arc<Shared>,
    provider: Weak<FiberInner>,
    pid: crate::PluginId,
    provider_gen: u64,
    key: TypeKey,
    scope: Option<crate::key::ScopeId>,
) -> Result<(), CordisError> {
    let old = shared
        .registry
        .lookup(&key, scope.as_ref())
        .filter(|b| b.provider_id == pid && b.provider_gen == provider_gen);
    if let Some(binding) = &old {
        binding
            .removing
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let consumers: Vec<Arc<FiberInner>> = shared
        .registry
        .consumers_of(&key, (pid, provider_gen, key.clone(), scope.clone()));
    let mut tasks = Vec::new();
    for fiber in &consumers {
        fiber.cancel_current();
        let task = TransitionTask::new();
        // post_join 返回 false 时任务已在 post 内即刻完成(评审 #3)
        fiber.post_join(task.clone(), Intent::RefreshDepsJoin);
        tasks.push(task);
    }
    // 其它注入该键的 fiber(Pending 者)也重查(不取消:可能是等值合并)
    shared.registry.notify_key_changed(&key);
    for task in tasks {
        let _ = join_task(&task).await;
    }
    // 清理期自访问结束,最终摘除(仅当槽位未被替换)
    if let Some(binding) = old {
        shared
            .registry
            .finalize_binding_if(key.clone(), scope.clone(), &binding);
    }
    // ④摘除 provider 的 provided 记账:仅移除本键本作用域的一条(同键
    // 新 provide 的条目保留;fiber 卸载整表清空后此处自然 no-op)
    if let Some(fiber) = provider.upgrade() {
        let mut provided = fiber.provided.lock().unwrap();
        if let Some(pos) = provided.iter().position(|(k, s)| *k == key && *s == scope) {
            provided.swap_remove(pos);
        }
        // 稀疏即收缩(0.2.5):长寿 root 的瞬态记账容量不随历史 provide 滞留
        //(常驻键使表永不为空,按浪费率收缩)。
        if provided.capacity() > 64 && provided.len() * 4 < provided.capacity() {
            provided.shrink_to_fit();
        }
    }
    Ok(())
}
