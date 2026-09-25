//! 终态释放(0.2.1)单元验证:keyed 通道摘除监听后删除空条目,
//! 派发尾链任务完成后自摘——随实例 churn 的通道与尾链不残留。

use super::*;
use crate::ctx::Ctx;
use crate::error::CordisError;
use crate::event::{Event, Listener, Next};
use crate::BoxFuture;
use std::time::Duration;

struct Ping;

impl Event for Ping {
    const NAME: &'static str = "test::TransientPing";
    type Value = ();
}

struct Nop;

impl Listener<Ping> for Nop {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a Ping,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        Box::pin(async { Ok(None) })
    }
}

struct NopWaterfall;

impl crate::event::WaterfallListener<Ping> for NopWaterfall {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a Ping,
        next: Next<'a, Ping>,
    ) -> BoxFuture<'a, Result<(), CordisError>> {
        Box::pin(async move { next.call().await })
    }
}

#[tokio::test]
async fn keyed_channels_and_dispatch_tails_prune() {
    let ctx = Ctx::root().expect("runtime in scope");
    let bus = ctx.events().clone();
    for i in 0..25 {
        let name = format!("ch/{i}");
        let listener = bus.on_keyed::<Ping>(&ctx, name.clone(), Nop).unwrap();
        let wf = bus
            .on_waterfall_keyed::<Ping>(&ctx, name.clone(), NopWaterfall)
            .unwrap();
        bus.emit_keyed::<Ping>(&ctx, name.clone(), Arc::new(Ping));
        listener.dispose().await.unwrap();
        wf.dispose().await.unwrap();
    }
    // 最后一次派发任务完成后自摘尾链;监听条目已在 dispose 内删除
    for _ in 0..500 {
        let empty = {
            let inner = bus.inner.lock().unwrap();
            inner.hooks.is_empty() && inner.wf_hooks.is_empty() && inner.dispatch_tail.is_empty()
        };
        if empty {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    {
        let inner = bus.inner.lock().unwrap();
        assert!(
            inner.hooks.is_empty() && inner.wf_hooks.is_empty(),
            "keyed channels must drop empty listener lists"
        );
        assert!(
            inner.dispatch_tail.is_empty(),
            "dispatch tails must self-remove on completion"
        );
    }
    // 摘除后再次注册/派发同键通道仍工作(条目按需重建)
    let listener = bus.on_keyed::<Ping>(&ctx, "ch/0", Nop).unwrap();
    bus.emit_keyed::<Ping>(&ctx, "ch/0", Arc::new(Ping));
    listener.dispose().await.unwrap();
}

#[tokio::test]
async fn dispatch_observers_prune_after_repeated_registration() {
    let ctx = Ctx::root().unwrap();
    let bus = ctx.events().clone();
    for _ in 0..1000 {
        let observer = bus.observe_dispatch(&ctx, |_| {}).unwrap();
        assert_eq!(bus.observer_count(), 1);
        observer.dispose().await.unwrap();
        assert_eq!(bus.observer_count(), 0);
    }
}
