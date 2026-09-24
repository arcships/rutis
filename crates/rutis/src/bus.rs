use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

use crate::ctx::Ctx;
use crate::error::{join_panic_error, panic_error, CordisError};
use crate::event::{
    CatchUnwind, DynEvent, ErasedValue, Event, EventOptions, Listener, ListenerAdapter, Terminal,
    TerminalAdapter, WaterfallAdapter, WaterfallListener,
};
use crate::fiber::FiberInner;
use crate::key::{InstanceId, TypeKey};
use crate::{BoxFuture, Disposer, Effect};

/// waterfall 链上的擦除续延:调用下一个监听器,最终落到终态续延。
pub(crate) struct ErasedNext<'a> {
    chain: &'a [Arc<dyn ErasedWaterfallCall>],
    index: usize,
    ctx: &'a Ctx,
    event: &'a DynEvent,
    terminal: &'a mut (dyn ErasedTerminal + 'a),
}

impl<'a> ErasedNext<'a> {
    pub(crate) fn invoke(self) -> BoxFuture<'a, Result<ErasedValue, CordisError>> {
        if self.index < self.chain.len() {
            let ErasedNext {
                chain,
                index,
                ctx,
                event,
                terminal,
            } = self;
            chain[index].call(
                ctx,
                event,
                ErasedNext {
                    chain,
                    index: index + 1,
                    ctx,
                    event,
                    terminal,
                },
            )
        } else {
            let ErasedNext {
                ctx,
                event,
                terminal,
                ..
            } = self;
            terminal.call(ctx, event)
        }
    }
}

pub(crate) trait ErasedCall: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
    ) -> BoxFuture<'a, Result<Option<ErasedValue>, CordisError>>;
}

pub(crate) trait ErasedWaterfallCall: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
        next: ErasedNext<'a>,
    ) -> BoxFuture<'a, Result<ErasedValue, CordisError>>;
}

pub(crate) trait ErasedTerminal: Send {
    fn call<'a>(
        &'a mut self,
        ctx: &'a Ctx,
        e: &'a DynEvent,
    ) -> BoxFuture<'a, Result<ErasedValue, CordisError>>;
}

/// 注册的监听器条目(泛型合一,简化 S4):`C` 为擦除后的调用句柄。
struct Hook<C> {
    call: C,
    once: bool,
    owner: Weak<FiberInner>,
}

/// An accepted instance dispatch owns every fiber whose shutdown must wait
/// for the callback snapshot. Dropping the owner releases all counts.
struct EventFlight(Vec<Arc<FiberInner>>);

impl EventFlight {
    fn new(ctx: &Ctx, id: InstanceId, hooks: &[Arc<Hook<Arc<dyn ErasedCall>>>]) -> Self {
        let mut owners = Vec::new();
        if let Some(owner) = ctx.instance_owner(id) {
            owners.push(owner);
        }
        if let Some(emitter) = ctx.weak_fiber().upgrade() {
            owners.push(emitter);
        }
        for hook in hooks {
            if let Some(owner) = hook.owner.upgrade() {
                owners.push(owner);
            }
        }
        owners.sort_by_key(|fiber| fiber.id);
        owners.dedup_by_key(|fiber| fiber.id);
        for fiber in &owners {
            fiber.begin_event();
        }
        Self(owners)
    }
}

impl Drop for EventFlight {
    fn drop(&mut self) {
        for fiber in &self.0 {
            fiber.finish_event();
        }
    }
}

struct TailCleanup {
    bus: EventBus,
    key: TypeKey,
    generation: u64,
}

impl Drop for TailCleanup {
    fn drop(&mut self) {
        let mut inner = self.bus.inner.lock().unwrap();
        if matches!(inner.dispatch_tail.get(&self.key), Some((cur, _)) if *cur == self.generation) {
            inner.dispatch_tail.remove(&self.key);
            shrink_if_sparse(&mut inner.dispatch_tail);
        }
    }
}

fn insert_hook<C>(list: &mut Vec<Arc<Hook<C>>>, hook: Arc<Hook<C>>, prepend: bool) {
    if prepend {
        list.insert(0, hook);
    } else {
        list.push(hook);
    }
}

/// 稀疏即收缩(0.2.5):同 registry `shrink_if_sparse`。
fn shrink_if_sparse<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, V>) {
    if map.capacity() > 64 && map.len() * 4 < map.capacity() {
        map.shrink_to_fit();
    }
}
fn retain_hook<C>(list: &mut Vec<Arc<Hook<C>>>, hook: &Arc<Hook<C>>) {
    list.retain(|h| !Arc::ptr_eq(h, hook));
}

fn remove_call_hook(bus: &EventBus, key: &TypeKey, hook: &Arc<Hook<Arc<dyn ErasedCall>>>) {
    let mut inner = bus.inner.lock().unwrap();
    let stale = match inner.hooks.get_mut(key) {
        Some(list) => {
            retain_hook(list, hook);
            list.is_empty()
        }
        None => false,
    };
    if stale {
        inner.hooks.remove(key);
        shrink_if_sparse(&mut inner.hooks);
    }
}

/// 快照(保位)并从注册表取出 once 条目:恰好一次由调用方持有的总线锁
/// 互斥直接保证——锁外无需任何第二套同步(简化)。
fn claim_once<C>(list: &mut Vec<Arc<Hook<C>>>) -> Vec<Arc<Hook<C>>> {
    let snapshot = list.clone();
    list.retain(|h| !h.once);
    snapshot
}

#[derive(Default)]
struct BusInner {
    /// 注册面键 = TypeKey(D33:限定名通道;非 keyed 注册 qualifier 为 None)。
    hooks: HashMap<TypeKey, Vec<Arc<Hook<Arc<dyn ErasedCall>>>>>,
    wf_hooks: HashMap<TypeKey, Vec<Arc<Hook<Arc<dyn ErasedWaterfallCall>>>>>,
    /// 同事件键的派发尾链(D31):每次 emit 的派发任务 await 上一个,
    /// 保证同键多次 emit 按发射序执行(修 spawn 调度乱序)。
    /// 值 = (代次, 任务):任务完成后自摘;代次防旧任务误删新尾链
    /// (0.2.1:keyed 通道随实例 churn,空尾链条目不残留)。
    dispatch_tail: HashMap<TypeKey, (u64, tokio::task::JoinHandle<()>)>,
}

/// 类型化事件总线(D3:回调注册表;D16:四分发,无同步 bail)。
///
/// 监听器经 `Ctx` 注册,自动归该 fiber 所有(D28)。
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusInner::default())),
        }
    }

    #[cfg(test)]
    pub(crate) fn table_counts(&self) -> (usize, usize, usize) {
        let inner = self.inner.lock().unwrap();
        (
            inner.hooks.len(),
            inner.wf_hooks.len(),
            inner.dispatch_tail.len(),
        )
    }

    /// 注册监听器(默认追加在后)。
    pub fn on<E: Event>(&self, ctx: &Ctx, l: impl Listener<E>) -> Result<Disposer, CordisError> {
        self.add_hook(TypeKey::of::<E>(), ctx, l, EventOptions::default(), false)
    }

    /// Register a listener visible only to the owning instance subtree.
    pub fn on_instance<E: Event>(
        &self,
        ctx: &Ctx,
        id: InstanceId,
        listener: impl Listener<E>,
    ) -> Result<Disposer, CordisError> {
        self.ensure_instance_ctx(ctx, id)?;
        self.add_hook(
            TypeKey::instance::<E>(id),
            ctx,
            listener,
            EventOptions::default(),
            false,
        )
    }

    fn ensure_instance_ctx(&self, ctx: &Ctx, id: InstanceId) -> Result<(), CordisError> {
        if !Arc::ptr_eq(&self.inner, &ctx.events().inner) {
            return Err(CordisError::InstanceOutOfScope { instance: id });
        }
        ctx.check_instance(&TypeKey::instance::<()>(id))
    }

    /// 注册监听器(带选项)。
    pub fn on_opt<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl Listener<E>,
        opts: EventOptions,
    ) -> Result<Disposer, CordisError> {
        self.add_hook(TypeKey::of::<E>(), ctx, l, opts, false)
    }

    /// 注册一次性监听器:至多调用一次。
    pub fn once<E: Event>(&self, ctx: &Ctx, l: impl Listener<E>) -> Result<Disposer, CordisError> {
        self.add_hook(TypeKey::of::<E>(), ctx, l, EventOptions::default(), true)
    }

    /// 注册带动态限定名的监听器(D33):同事件类型多通道互不串扰。
    /// name 与 `emit_keyed` 按字符串内容匹配。
    pub fn on_keyed<E: Event>(
        &self,
        ctx: &Ctx,
        name: impl Into<std::sync::Arc<str>>,
        l: impl Listener<E>,
    ) -> Result<Disposer, CordisError> {
        self.add_hook(
            TypeKey::keyed_dynamic::<E>(name),
            ctx,
            l,
            EventOptions::default(),
            false,
        )
    }

    /// 注册带动态限定名的监听器(带选项)。
    pub fn on_keyed_opt<E: Event>(
        &self,
        ctx: &Ctx,
        name: impl Into<std::sync::Arc<str>>,
        l: impl Listener<E>,
        opts: EventOptions,
    ) -> Result<Disposer, CordisError> {
        self.add_hook(TypeKey::keyed_dynamic::<E>(name), ctx, l, opts, false)
    }

    /// 注册带动态限定名的一次性监听器。
    pub fn once_keyed<E: Event>(
        &self,
        ctx: &Ctx,
        name: impl Into<std::sync::Arc<str>>,
        l: impl Listener<E>,
    ) -> Result<Disposer, CordisError> {
        self.add_hook(
            TypeKey::keyed_dynamic::<E>(name),
            ctx,
            l,
            EventOptions::default(),
            true,
        )
    }

    /// 注册 waterfall 监听器(D17:独立注册面)。
    pub fn on_waterfall<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl WaterfallListener<E>,
    ) -> Result<Disposer, CordisError> {
        self.add_wf_hook(TypeKey::of::<E>(), ctx, l, EventOptions::default(), false)
    }

    /// 注册 waterfall 监听器(带选项)。
    pub fn on_waterfall_opt<E: Event>(
        &self,
        ctx: &Ctx,
        l: impl WaterfallListener<E>,
        opts: EventOptions,
    ) -> Result<Disposer, CordisError> {
        self.add_wf_hook(TypeKey::of::<E>(), ctx, l, opts, false)
    }

    /// 注册带动态限定名的 waterfall 监听器(D33)。
    pub fn on_waterfall_keyed<E: Event>(
        &self,
        ctx: &Ctx,
        name: impl Into<std::sync::Arc<str>>,
        l: impl WaterfallListener<E>,
    ) -> Result<Disposer, CordisError> {
        self.add_wf_hook(
            TypeKey::keyed_dynamic::<E>(name),
            ctx,
            l,
            EventOptions::default(),
            false,
        )
    }

    fn add_hook<E: Event>(
        &self,
        key: TypeKey,
        ctx: &Ctx,
        l: impl Listener<E>,
        opts: EventOptions,
        once: bool,
    ) -> Result<Disposer, CordisError> {
        let hook: Arc<Hook<Arc<dyn ErasedCall>>> = Arc::new(Hook {
            call: Arc::new(ListenerAdapter(l, std::marker::PhantomData)),
            once,
            owner: ctx.weak_fiber(),
        });
        let bus = self.clone();
        let access_ctx = ctx.clone();
        ctx.register_internal_effect(move |_, _, _| {
            access_ctx.check_instance(&key)?;
            {
                let mut inner = bus.inner.lock().unwrap();
                let list = inner.hooks.entry(key.clone()).or_default();
                insert_hook(list, hook.clone(), opts.prepend);
            }
            if key.instance_id().is_some() {
                let owner = hook.owner.clone();
                let shared = access_ctx.shared().clone();
                Ok(Effect::AsyncDisposer(Box::new(move || {
                    {
                        // Serialize removal with an instance dispatch's
                        // snapshot and flight registration. Otherwise the
                        // drain may finish before a snapshotted callback starts.
                        let _admission = shared.admission.lock().unwrap();
                        remove_call_hook(&bus, &key, &hook);
                    }
                    Box::pin(async move {
                        if let Some(owner) = owner.upgrade() {
                            owner.wait_events().await;
                        }
                        Ok(())
                    })
                })))
            } else {
                Ok(Effect::Disposer(Box::new(move || {
                    remove_call_hook(&bus, &key, &hook);
                    Ok(())
                })))
            }
        })
    }

    fn add_wf_hook<E: Event>(
        &self,
        key: TypeKey,
        ctx: &Ctx,
        l: impl WaterfallListener<E>,
        opts: EventOptions,
        once: bool,
    ) -> Result<Disposer, CordisError> {
        let hook: Arc<Hook<Arc<dyn ErasedWaterfallCall>>> = Arc::new(Hook {
            call: Arc::new(WaterfallAdapter(l, std::marker::PhantomData)),
            once,
            owner: ctx.weak_fiber(),
        });
        let bus = self.clone();
        ctx.register_internal_effect(move |_, _, _| {
            {
                let mut inner = bus.inner.lock().unwrap();
                let list = inner.wf_hooks.entry(key.clone()).or_default();
                insert_hook(list, hook.clone(), opts.prepend);
            }
            Ok(Effect::Disposer(Box::new(move || {
                let mut inner = bus.inner.lock().unwrap();
                let stale = match inner.wf_hooks.get_mut(&key) {
                    Some(list) => {
                        retain_hook(list, &hook);
                        list.is_empty()
                    }
                    None => false,
                };
                if stale {
                    inner.wf_hooks.remove(&key);
                    shrink_if_sparse(&mut inner.wf_hooks);
                }
                Ok(())
            })))
        })
    }

    /// 快照监听器并取出 once 条目(简化:恰好一次由总线锁的互斥直接保证,
    /// 无需第二套原子认领)。**快照保持注册序**(§四:顺序控制影响
    /// serial/waterfall 结果);once 从注册表删除后,Disposer/卸载的
    /// 移除自然变 no-op。
    fn take_hooks(&self, key: &TypeKey) -> Vec<Arc<Hook<Arc<dyn ErasedCall>>>> {
        let mut inner = self.inner.lock().unwrap();
        let mut snapshot = match inner.hooks.get_mut(key) {
            None => return Vec::new(),
            Some(list) => claim_once(list),
        };
        if key.instance_id().is_some() {
            snapshot.retain(|hook| {
                hook.owner.upgrade().is_some_and(|owner| {
                    owner.alive.load(Ordering::SeqCst) && !owner.closing.load(Ordering::SeqCst)
                })
            });
        }
        if inner.hooks.get(key).is_some_and(|l| l.is_empty()) {
            inner.hooks.remove(key);
            shrink_if_sparse(&mut inner.hooks);
        }
        snapshot
    }

    fn take_instance_hooks(
        &self,
        ctx: &Ctx,
        id: InstanceId,
        key: &TypeKey,
    ) -> Result<(Vec<Arc<Hook<Arc<dyn ErasedCall>>>>, EventFlight), CordisError> {
        let _admission = ctx.shared().admission.lock().unwrap();
        self.ensure_instance_ctx(ctx, id)?;
        ctx.registration_open()?;
        let hooks = self.take_hooks(key);
        let flight = EventFlight::new(ctx, id, &hooks);
        Ok((hooks, flight))
    }

    fn take_wf_hooks(&self, key: &TypeKey) -> Vec<Arc<dyn ErasedWaterfallCall>> {
        let mut inner = self.inner.lock().unwrap();
        let snapshot = match inner.wf_hooks.get_mut(key) {
            None => return Vec::new(),
            Some(list) => claim_once(list),
        };
        if inner.wf_hooks.get(key).is_some_and(|l| l.is_empty()) {
            inner.wf_hooks.remove(key);
            shrink_if_sparse(&mut inner.wf_hooks);
        }
        snapshot.into_iter().map(|h| h.call.clone()).collect()
    }

    /// emit:触发即忘(D16/D30)。**同事件键按发射序串行派发**(D31):
    /// 单次持锁内"取上一派发任务句柄 → spawn 新任务 → 存为尾"(原子,
    /// 防 remove/insert 两段锁在并发同键 emit 下分叉链);任务内先
    /// await 上一个,再按注册序逐个 await 监听器。监听器 panic 经
    /// CatchUnwind 捕获路由 ErrorSink,`prev.await` 正常返回,链不断;
    /// 监听器内重入 emit 同键事件仅排到链尾,不死锁。跨事件键不保证
    /// 顺序(已知边界,见 D31)。spawn 在临界区内只入队不同步执行,
    /// std Mutex 无重入,故 `take_hooks` 的锁必须已释放。
    pub fn emit<E: Event>(&self, ctx: &Ctx, e: Arc<E>) {
        let _ = self.emit_keyed_inner(TypeKey::of::<E>(), ctx, e);
    }

    /// emit 的 keyed 通道(D33):同类型不同名互不串扰,同名共享尾链。
    pub fn emit_keyed<E: Event>(&self, ctx: &Ctx, name: impl Into<std::sync::Arc<str>>, e: Arc<E>) {
        let _ = self.emit_keyed_inner(TypeKey::keyed_dynamic::<E>(name), ctx, e);
    }

    /// Queue an event for one instance. A successful return means the event
    /// has been accepted; callback failures still go to the error sink.
    pub fn emit_instance<E: Event>(
        &self,
        ctx: &Ctx,
        id: InstanceId,
        e: Arc<E>,
    ) -> Result<(), CordisError> {
        self.emit_keyed_inner(TypeKey::instance::<E>(id), ctx, e)
    }

    fn emit_keyed_inner<E: Event>(
        &self,
        key: TypeKey,
        ctx: &Ctx,
        e: Arc<E>,
    ) -> Result<(), CordisError> {
        let _admission = key
            .instance_id()
            .map(|_| ctx.shared().admission.lock().unwrap());
        if let Some(id) = key.instance_id() {
            self.ensure_instance_ctx(ctx, id)?;
            ctx.registration_open()?;
        }
        let hooks = self.take_hooks(&key);
        if hooks.is_empty() {
            return Ok(()); // 不进链:无监听器不产生派发任务
        }
        let flight = key
            .instance_id()
            .map(|id| EventFlight::new(ctx, id, &hooks));
        let ctx2 = ctx.clone();
        let sink = ctx.error_sink();
        let handle = ctx.handle().clone();
        let bus = self.clone();
        let tail_key = key.clone();
        let mut inner = self.inner.lock().unwrap();
        let (gen, prev) = match inner.dispatch_tail.remove(&key) {
            Some((gen, tail)) => (gen + 1, Some(tail)),
            None => (0, None),
        };
        let tail = handle.spawn(async move {
            let _flight = flight;
            let _tail_cleanup = TailCleanup {
                bus,
                key: tail_key,
                generation: gen,
            };
            // 等同键上一次派发完成(链式保序)
            if let Some(prev) = prev {
                let _ = prev.await;
            }
            // 按注册序逐个 await(不并发 spawn,否则退回乱序)
            for hook in hooks {
                let out =
                    CatchUnwind::new(async { hook.call.call(&ctx2, &*e as &DynEvent).await }).await;
                match out {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => sink(Arc::new(err)),
                    Err(p) => sink(Arc::new(panic_error(p))),
                }
            }
        });
        inner.dispatch_tail.insert(key, (gen, tail));
        Ok(())
    }

    /// parallel:并发全等,聚合全部错误(JoinSet,D16)。
    pub async fn parallel<E: Event>(&self, ctx: &Ctx, e: Arc<E>) -> Result<(), CordisError> {
        self.parallel_keyed_inner(TypeKey::of::<E>(), ctx, e).await
    }

    /// parallel 的 keyed 通道(D33)。
    pub async fn parallel_keyed<E: Event>(
        &self,
        ctx: &Ctx,
        name: impl Into<std::sync::Arc<str>>,
        e: Arc<E>,
    ) -> Result<(), CordisError> {
        self.parallel_keyed_inner(TypeKey::keyed_dynamic::<E>(name), ctx, e)
            .await
    }

    /// Run instance listeners concurrently. The accepted dispatch continues
    /// to completion if the caller drops its waiting future.
    pub async fn parallel_instance<E: Event>(
        &self,
        ctx: &Ctx,
        id: InstanceId,
        e: Arc<E>,
    ) -> Result<(), CordisError> {
        let key = TypeKey::instance::<E>(id);
        let (hooks, flight) = self.take_instance_hooks(ctx, id, &key)?;
        let ctx2 = ctx.clone();
        let runner = ctx.handle().spawn(async move {
            let _flight = flight;
            let mut set = tokio::task::JoinSet::new();
            for hook in hooks {
                let ctx3 = ctx2.clone();
                let e2 = e.clone();
                set.spawn(async move { hook.call.call(&ctx3, &*e2 as &DynEvent).await });
            }
            let mut errors = Vec::new();
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => errors.push(err),
                    Err(join_err) => errors.push(join_panic_error(join_err)),
                }
            }
            crate::error::aggregate_errors(errors).map_or(Ok(()), Err)
        });
        runner
            .await
            .unwrap_or_else(|err| Err(join_panic_error(err)))
    }

    async fn parallel_keyed_inner<E: Event>(
        &self,
        key: TypeKey,
        ctx: &Ctx,
        e: Arc<E>,
    ) -> Result<(), CordisError> {
        let hooks = self.take_hooks(&key);
        if hooks.is_empty() {
            return Ok(());
        }
        let mut set = tokio::task::JoinSet::new();
        for hook in hooks {
            let ctx2 = ctx.clone();
            let e2 = e.clone();
            set.spawn_on(
                async move { hook.call.call(&ctx2, &*e2 as &DynEvent).await },
                ctx.handle(),
            );
        }
        let mut errors: Vec<CordisError> = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => errors.push(err),
                // 取消不是 panic:into_panic 会二次 panic(评审 P2)
                Err(join_err) => errors.push(join_panic_error(join_err)),
            }
        }
        match crate::error::aggregate_errors(errors) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// serial:顺序调用至首个短路值 `Ok(Some(v))`(TS serial 语义:
    /// 上一个监听器完成才调用下一个,按注册序短路)。
    /// 内联顺序 await,不 spawn(载荷可借用,`&E` 对齐 §二 草案);
    /// panic 经 CatchUnwind 边界转 `PluginFailed`(D30 精神)。
    pub async fn serial<E: Event>(
        &self,
        ctx: &Ctx,
        e: &E,
    ) -> Result<Option<E::Value>, CordisError> {
        self.serial_keyed_inner(TypeKey::of::<E>(), ctx, e).await
    }

    /// serial 的 keyed 通道(D33)。
    pub async fn serial_keyed<E: Event>(
        &self,
        ctx: &Ctx,
        name: impl Into<std::sync::Arc<str>>,
        e: &E,
    ) -> Result<Option<E::Value>, CordisError> {
        self.serial_keyed_inner(TypeKey::keyed_dynamic::<E>(name), ctx, e)
            .await
    }

    /// Call one instance's listeners in registration order until one bails.
    pub async fn serial_instance<E: Event>(
        &self,
        ctx: &Ctx,
        id: InstanceId,
        e: &E,
    ) -> Result<Option<E::Value>, CordisError> {
        let key = TypeKey::instance::<E>(id);
        let (hooks, _flight) = self.take_instance_hooks(ctx, id, &key)?;
        for hook in hooks {
            let outcome = CatchUnwind::new(hook.call.call(ctx, e as &DynEvent)).await;
            match outcome {
                Ok(Ok(Some(boxed))) => {
                    return boxed
                        .downcast::<E::Value>()
                        .map(|value| Some(*value))
                        .map_err(|_| {
                            CordisError::PluginFailed("serial value type mismatch".into())
                        })
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => return Err(error),
                Err(panic) => return Err(panic_error(panic)),
            }
        }
        Ok(None)
    }

    async fn serial_keyed_inner<E: Event>(
        &self,
        key: TypeKey,
        ctx: &Ctx,
        e: &E,
    ) -> Result<Option<E::Value>, CordisError> {
        for hook in self.take_hooks(&key) {
            let outcome = CatchUnwind::new(hook.call.call(ctx, e as &DynEvent)).await;
            match outcome {
                Ok(Ok(Some(boxed))) => {
                    return match boxed.downcast::<E::Value>() {
                        Ok(v) => Ok(Some(*v)),
                        Err(_) => Err(CordisError::PluginFailed(
                            "serial value type mismatch".into(),
                        )),
                    };
                }
                Ok(Ok(None)) => continue,
                Ok(Err(err)) => return Err(err),
                Err(p) => return Err(panic_error(p)),
            }
        }
        Ok(None)
    }

    /// waterfall:中间件续延(D17)。`terminal` 为调用方兜底续延;
    /// 监听器不调用 `next` 即 veto。内联 CPS 递归(见 §八:panic 向分发者传播)。
    pub fn waterfall<'a, E: Event, T: Terminal<E> + 'a>(
        &self,
        ctx: &'a Ctx,
        e: &'a E,
        terminal: T,
    ) -> BoxFuture<'a, Result<E::Value, CordisError>> {
        self.waterfall_keyed_inner(TypeKey::of::<E>(), ctx, e, terminal)
    }

    /// waterfall 的 keyed 通道(D33)。
    pub fn waterfall_keyed<'a, E: Event, T: Terminal<E> + 'a>(
        &self,
        ctx: &'a Ctx,
        name: impl Into<std::sync::Arc<str>>,
        e: &'a E,
        terminal: T,
    ) -> BoxFuture<'a, Result<E::Value, CordisError>> {
        self.waterfall_keyed_inner(TypeKey::keyed_dynamic::<E>(name), ctx, e, terminal)
    }

    fn waterfall_keyed_inner<'a, E: Event, T: Terminal<E> + 'a>(
        &self,
        key: TypeKey,
        ctx: &'a Ctx,
        e: &'a E,
        terminal: T,
    ) -> BoxFuture<'a, Result<E::Value, CordisError>> {
        let bus = self.clone();
        Box::pin(async move {
            let chain = bus.take_wf_hooks(&key);
            let mut terminal: Box<dyn ErasedTerminal + 'a> =
                Box::new(TerminalAdapter(terminal, std::marker::PhantomData));
            let next = ErasedNext {
                chain: &chain,
                index: 0,
                ctx,
                event: e as &DynEvent,
                terminal: terminal.as_mut(),
            };
            let boxed = next.invoke().await?;
            match boxed.downcast::<E::Value>() {
                Ok(v) => Ok(*v),
                Err(_) => Err(CordisError::PluginFailed(
                    "waterfall value type mismatch".into(),
                )),
            }
        })
    }
}

#[cfg(test)]
mod transient_tests;
