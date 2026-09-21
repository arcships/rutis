//! D33 动态事件键契约测试(design-config-hot-update-and-dynamic-events-2026-09-21 §2.6)。
//!
//! 覆盖:keyed 通道隔离 / 动态名 / D31 尾链保序 / 四语义 keyed 变体 /
//! once·prepend / fiber 卸载清理 / parity 补拍(cordis events.spec 字符串名内核)。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rutis::{
    BoxFuture, CordisError, Ctx, Effect, Event, FiberState, Listener, Next, Plugin, Terminal,
    TypeKey, WaterfallListener,
};

// ── 测试事件与监听器(命名 struct,同 contract.rs 模式) ──────────────

#[derive(Debug, Clone)]
struct Ping {
    value: u32,
}

impl Event for Ping {
    const NAME: &'static str = "test::Ping";
    type Value = u32;
}

#[derive(Debug, Clone)]
struct Other;

impl Event for Other {
    const NAME: &'static str = "test::Other";
    type Value = u32;
}

type Hits = Arc<AtomicUsize>;
type Log = Arc<Mutex<Vec<u32>>>;

fn hits() -> Hits {
    Arc::new(AtomicUsize::new(0))
}

/// 计数并可选记录事件值。
struct Counting {
    hits: Hits,
    log: Option<Log>,
}

impl Listener<Ping> for Counting {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a Ping,
    ) -> BoxFuture<'a, Result<Option<u32>, CordisError>> {
        let hits = self.hits.clone();
        let v = e.value;
        let log = self.log.clone();
        Box::pin(async move {
            hits.fetch_add(1, Ordering::SeqCst);
            if let Some(log) = log {
                log.lock().unwrap().push(v);
            }
            Ok(None)
        })
    }
}

/// serial 短路值。
struct Bail(u32);

impl Listener<Ping> for Bail {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a Ping,
    ) -> BoxFuture<'a, Result<Option<u32>, CordisError>> {
        Box::pin(async move { Ok(Some(self.0)) })
    }
}

/// 必错监听器。
struct Failing;

impl Listener<Ping> for Failing {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a Ping,
    ) -> BoxFuture<'a, Result<Option<u32>, CordisError>> {
        Box::pin(async move { Err(CordisError::PluginFailed("boom".into())) })
    }
}

/// waterfall veto:不调 next,改写返回值。
struct Veto(u32);

impl WaterfallListener<Ping> for Veto {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _e: &'a Ping,
        _next: Next<'a, Ping>,
    ) -> BoxFuture<'a, Result<u32, CordisError>> {
        Box::pin(async move { Ok(self.0) })
    }
}

/// waterfall 中间件:值加事件值后放行。
struct AddEventValue;

impl WaterfallListener<Ping> for AddEventValue {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        e: &'a Ping,
        next: Next<'a, Ping>,
    ) -> BoxFuture<'a, Result<u32, CordisError>> {
        Box::pin(async move {
            let v = next.call().await?;
            Ok(v + e.value)
        })
    }
}

/// waterfall 终态:返回固定值,计数到达次数。
struct TerminalCount {
    value: u32,
    reached: Hits,
}

impl Terminal<Ping> for TerminalCount {
    fn call<'a>(&'a self, _ctx: &'a Ctx, _e: &'a Ping) -> BoxFuture<'a, Result<u32, CordisError>> {
        let reached = self.reached.clone();
        let v = self.value;
        Box::pin(async move {
            reached.fetch_add(1, Ordering::SeqCst);
            Ok(v)
        })
    }
}

async fn soon(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await
}

// ── 1. keyed 通道隔离 + 静态/动态同名互通 ──────────────────────────

#[tokio::test]
async fn keyed_channels_are_isolated_by_name() {
    let ctx = Ctx::root().unwrap();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let h_b = hits();

    ctx.events()
        .on_keyed::<Ping>(
            &ctx,
            "chan/a",
            Counting {
                hits: hits(),
                log: Some(log.clone()),
            },
        )
        .unwrap();
    ctx.events()
        .on_keyed::<Ping>(
            &ctx,
            "chan/b",
            Counting {
                hits: h_b.clone(),
                log: None,
            },
        )
        .unwrap();

    // 只发 a:b 通道不收
    ctx.events()
        .emit_keyed(&ctx, "chan/a", Arc::new(Ping { value: 7 }));
    soon(30).await;
    assert_eq!(log.lock().unwrap().as_slice(), [7]);
    assert_eq!(h_b.load(Ordering::SeqCst), 0);

    // 发 b:a 通道不收
    ctx.events()
        .emit_keyed(&ctx, "chan/b", Arc::new(Ping { value: 9 }));
    soon(30).await;
    assert_eq!(log.lock().unwrap().as_slice(), [7]);
    assert_eq!(h_b.load(Ordering::SeqCst), 1);

    // 静态/动态同名互通:TypeKey::keyed 与 keyed_dynamic 按内容等值
    let k_static = TypeKey::keyed::<Ping>("chan/a");
    assert_eq!(k_static, TypeKey::keyed_dynamic::<Ping>("chan/a"));
    assert_ne!(k_static, TypeKey::keyed_dynamic::<Ping>("chan/x"));
    // 同名不同类型不等(类型烙在键里)
    assert_ne!(
        TypeKey::keyed_dynamic::<Ping>("chan/a"),
        TypeKey::keyed_dynamic::<Other>("chan/a")
    );
}

// ── 2. 运行时构造的动态名 ─────────────────────────────────────────

#[tokio::test]
async fn runtime_constructed_name_dispatches() {
    let ctx = Ctx::root().unwrap();
    let h = hits();
    // 名字运行时才知道(如桥转发宿主事件)
    let name = format!("host/session-{}", 42);
    ctx.events()
        .on_keyed::<Ping>(
            &ctx,
            name.clone(),
            Counting {
                hits: h.clone(),
                log: None,
            },
        )
        .unwrap();
    ctx.events()
        .emit_keyed(&ctx, name, Arc::new(Ping { value: 1 }));
    soon(30).await;
    assert_eq!(h.load(Ordering::SeqCst), 1);
}

// ── 3. D31 尾链:同名保序 ─────────────────────────────────────────

#[tokio::test]
async fn keyed_emit_preserves_emission_order_per_name() {
    let ctx = Ctx::root().unwrap();
    let log: Log = Arc::new(Mutex::new(Vec::new()));

    ctx.events()
        .on_keyed::<Ping>(
            &ctx,
            "ordered",
            Counting {
                hits: hits(),
                log: Some(log.clone()),
            },
        )
        .unwrap();

    // 并发背靠背 emit 同名:到达序 = 发射序(D31)
    let ctx2 = ctx.clone();
    tokio::spawn(async move {
        for i in 0..20u32 {
            ctx2.events()
                .emit_keyed(&ctx2, "ordered", Arc::new(Ping { value: i }));
        }
    })
    .await
    .unwrap();
    soon(100).await;
    let got = log.lock().unwrap().clone();
    assert_eq!(got, (0..20).collect::<Vec<u32>>());
}

// ── 4. 四语义 keyed 变体 ─────────────────────────────────────────

#[tokio::test]
async fn keyed_serial_short_circuits() {
    let ctx = Ctx::root().unwrap();
    ctx.events()
        .on_keyed::<Ping>(&ctx, "serial", Bail(11))
        .unwrap();
    ctx.events()
        .on_keyed::<Ping>(&ctx, "serial", Bail(22))
        .unwrap();

    let out = ctx
        .events()
        .serial_keyed(&ctx, "serial", &Ping { value: 0 })
        .await
        .unwrap();
    assert_eq!(out, Some(11)); // 注册序第一个短路

    // 空通道:None
    let out = ctx
        .events()
        .serial_keyed(&ctx, "empty", &Ping { value: 0 })
        .await
        .unwrap();
    assert_eq!(out, None);
}

#[tokio::test]
async fn keyed_waterfall_veto_and_chain() {
    let ctx = Ctx::root().unwrap();

    // veto:不调 next,终态不可达
    ctx.events()
        .on_waterfall_keyed::<Ping>(&ctx, "wf", Veto(99))
        .unwrap();
    let terminal = TerminalCount {
        value: 1,
        reached: hits(),
    };
    let reached = terminal.reached.clone();
    let out = ctx
        .events()
        .waterfall_keyed(&ctx, "wf", &Ping { value: 1 }, terminal)
        .await
        .unwrap();
    assert_eq!(out, 99);
    assert_eq!(reached.load(Ordering::SeqCst), 0); // veto 生效,终态未达

    // 正常链:中间件 + 终态
    ctx.events()
        .on_waterfall_keyed::<Ping>(&ctx, "wf2", AddEventValue)
        .unwrap();
    let out = ctx
        .events()
        .waterfall_keyed(
            &ctx,
            "wf2",
            &Ping { value: 10 },
            TerminalCount {
                value: 5,
                reached: hits(),
            },
        )
        .await
        .unwrap();
    assert_eq!(out, 15); // 5 + 10
}

#[tokio::test]
async fn keyed_parallel_runs_all_and_aggregates() {
    let ctx = Ctx::root().unwrap();
    let h = hits();
    ctx.events()
        .on_keyed::<Ping>(
            &ctx,
            "par",
            Counting {
                hits: h.clone(),
                log: None,
            },
        )
        .unwrap();
    ctx.events().on_keyed::<Ping>(&ctx, "par", Failing).unwrap();

    let out = ctx
        .events()
        .parallel_keyed(&ctx, "par", Arc::new(Ping { value: 1 }))
        .await;
    assert!(out.is_err()); // 聚合错误上抛
    assert_eq!(h.load(Ordering::SeqCst), 1); // 全部执行
}

// ── 5. once_keyed 恰好一次 + prepend 顺序 ─────────────────────────

#[tokio::test]
async fn keyed_once_fires_exactly_once() {
    let ctx = Ctx::root().unwrap();
    let h = hits();
    ctx.events()
        .once_keyed::<Ping>(
            &ctx,
            "once",
            Counting {
                hits: h.clone(),
                log: None,
            },
        )
        .unwrap();
    for _ in 0..3 {
        ctx.events()
            .emit_keyed(&ctx, "once", Arc::new(Ping { value: 1 }));
    }
    soon(50).await;
    assert_eq!(h.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn keyed_prepend_runs_first() {
    let ctx = Ctx::root().unwrap();
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    struct Named(&'static str, Arc<Mutex<Vec<&'static str>>>);
    impl Listener<Ping> for Named {
        fn call<'a>(
            &'a self,
            _ctx: &'a Ctx,
            _e: &'a Ping,
        ) -> BoxFuture<'a, Result<Option<u32>, CordisError>> {
            let order = self.1.clone();
            let name = self.0;
            Box::pin(async move {
                order.lock().unwrap().push(name);
                Ok(None)
            })
        }
    }

    // 先注册 base,再 prepend front
    ctx.events()
        .on_keyed::<Ping>(&ctx, "prep", Named("base", order.clone()))
        .unwrap();
    ctx.events()
        .on_keyed_opt::<Ping>(
            &ctx,
            "prep",
            Named("front", order.clone()),
            rutis::EventOptions { prepend: true },
        )
        .unwrap();

    ctx.events()
        .emit_keyed(&ctx, "prep", Arc::new(Ping { value: 1 }));
    soon(30).await;
    assert_eq!(order.lock().unwrap().as_slice(), ["front", "base"]);
}

// ── 6. 同名不同类型不串扰(类型烙在键里) ───────────────────────────

#[tokio::test]
async fn same_name_different_event_types_do_not_cross() {
    let ctx = Ctx::root().unwrap();
    let h_ping = hits();
    ctx.events()
        .on_keyed::<Ping>(
            &ctx,
            "shared-name",
            Counting {
                hits: h_ping.clone(),
                log: None,
            },
        )
        .unwrap();
    // Other 类型同名 emit:Ping 通道不收
    ctx.events()
        .emit_keyed(&ctx, "shared-name", Arc::new(Other));
    soon(30).await;
    assert_eq!(h_ping.load(Ordering::SeqCst), 0);
}

// ── 7. 监听器随注册方 fiber 卸载自动摘除(D28 keyed 路径) ──────────

#[tokio::test]
async fn keyed_listener_removed_with_owner_fiber() {
    let ctx = Ctx::root().unwrap();
    let h = hits();

    struct Owner {
        h: Hits,
    }
    impl Plugin for Owner {
        fn name(&self) -> &str {
            "owner"
        }
        fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
            let h = self.h.clone();
            Box::pin(async move {
                ctx.events()
                    .on_keyed::<Ping>(ctx, "owned", Counting { hits: h, log: None })?;
                Ok(Effect::Done)
            })
        }
    }

    let owner = ctx.plugin(Owner { h: h.clone() });
    owner.clone().await.expect("owner loads");

    ctx.events()
        .emit_keyed(&ctx, "owned", Arc::new(Ping { value: 1 }));
    soon(30).await;
    assert_eq!(h.load(Ordering::SeqCst), 1);

    owner.dispose().await.unwrap();
    assert_eq!(owner.state().state, FiberState::Disposed);

    ctx.events()
        .emit_keyed(&ctx, "owned", Arc::new(Ping { value: 2 }));
    soon(30).await;
    assert_eq!(h.load(Ordering::SeqCst), 1); // 卸载后不再收
}

// ── 8. parity 补拍:cordis events.spec 字符串事件名内核 ────────────
// cordis 原用例(events.spec.ts `ctx.on()` / `ctx.once()` / `ctx.waterfall()`)
// 原判"部分对拍(载体:字符串事件名)";keyed 落地后字符串名内核可全拍:
// 名字 → keyed 通道,语义(注册→触发、once 恰好一次、waterfall next 链)不变。

#[tokio::test]
async fn parity_string_event_name_on_once_waterfall() {
    let ctx = Ctx::root().unwrap();
    let h_on = hits();
    let h_once = hits();

    ctx.events()
        .on_keyed::<Ping>(
            &ctx,
            "evt/foo",
            Counting {
                hits: h_on.clone(),
                log: None,
            },
        )
        .unwrap();
    ctx.events()
        .once_keyed::<Ping>(
            &ctx,
            "evt/bar",
            Counting {
                hits: h_once.clone(),
                log: None,
            },
        )
        .unwrap();

    ctx.events()
        .emit_keyed(&ctx, "evt/foo", Arc::new(Ping { value: 1 }));
    ctx.events()
        .emit_keyed(&ctx, "evt/bar", Arc::new(Ping { value: 1 }));
    ctx.events()
        .emit_keyed(&ctx, "evt/bar", Arc::new(Ping { value: 1 }));
    soon(50).await;
    assert_eq!(h_on.load(Ordering::SeqCst), 1);
    assert_eq!(h_once.load(Ordering::SeqCst), 1);

    // waterfall:无中间件直落终态(空链 = 注册序语义的最简内核)
    let out = ctx
        .events()
        .waterfall_keyed(
            &ctx,
            "evt/wf",
            &Ping { value: 3 },
            TerminalCount {
                value: 0,
                reached: hits(),
            },
        )
        .await
        .unwrap();
    assert_eq!(out, 0);
}
