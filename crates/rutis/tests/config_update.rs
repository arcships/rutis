//! D32 配置热更新契约测试(design-config-hot-update-and-dynamic-events-2026-09-21 §1.7)。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rutis::{
    BoxFuture, CordisError, Ctx, Effect, FiberState, FiberView, Plugin, PluginFactory, TypeKey,
};

// ── 测试助手 ─────────────────────────────────────────────────────

/// 会说当前 config 的服务(装载后由消费方 get 断言其值)。
#[derive(Debug, Clone, PartialEq)]
struct ConfigSvc {
    label: String,
    n: u32,
}

/// 工厂构造的插件:provide 一个由 config 派生的 ConfigSvc,
/// 记录 apply / 清理次数与顺序。
struct ConfigPlugin {
    svc: ConfigSvc,
    apply_log: Arc<Mutex<Vec<String>>>,
    cleanups: Arc<AtomicUsize>,
    fail_apply: bool,
}

impl Plugin for ConfigPlugin {
    fn name(&self) -> &str {
        "config-plugin"
    }

    fn validate(&self) -> Result<(), CordisError> {
        if self.svc.n == u32::MAX {
            // 实例级校验失败通道(dry-run 第 3 步)
            Err(CordisError::Validation {
                issues: vec!["bad instance".into()],
            })
        } else {
            Ok(())
        }
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        let svc = self.svc.clone();
        let log = self.apply_log.clone();
        let cleanups = self.cleanups.clone();
        let fail = self.fail_apply;
        Box::pin(async move {
            log.lock().unwrap().push(format!("apply:{}", svc.label));
            if fail {
                return Err(CordisError::PluginFailed("apply boom".into()));
            }
            ctx.provide(ConfigSvc { ..svc })?;
            Ok(Effect::Disposer(Box::new(move || {
                cleanups.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })))
        })
    }
}

#[derive(Clone)]
struct TestFactory {
    apply_log: Arc<Mutex<Vec<String>>>,
    cleanups: Arc<AtomicUsize>,
}

impl TestFactory {
    fn new() -> Self {
        Self {
            apply_log: Arc::new(Mutex::new(Vec::new())),
            cleanups: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// 测试 config:n≥1000 时走指定失败通道:
/// 1000 = validate_config 失败,1001 = build 失败,u32::MAX = 实例 validate 失败。
#[derive(Debug, Clone, PartialEq)]
struct TestConfig {
    label: String,
    n: u32,
}

impl PluginFactory<TestConfig> for TestFactory {
    fn name(&self) -> &str {
        "test-factory"
    }

    fn validate_config(&self, config: &TestConfig) -> Result<(), CordisError> {
        if config.n == 1000 {
            Err(CordisError::Validation {
                issues: vec!["config invalid".into()],
            })
        } else {
            Ok(())
        }
    }

    fn build(&self, config: &TestConfig) -> Result<Box<dyn Plugin>, CordisError> {
        if config.n == 1001 {
            return Err(CordisError::PluginFailed("build boom".into()));
        }
        Ok(Box::new(ConfigPlugin {
            svc: ConfigSvc {
                label: config.label.clone(),
                n: config.n,
            },
            apply_log: self.apply_log.clone(),
            cleanups: self.cleanups.clone(),
            fail_apply: false,
        }))
    }
}

fn cfg(label: &str, n: u32) -> TestConfig {
    TestConfig {
        label: label.into(),
        n,
    }
}

async fn settle(view: &FiberView) {
    view.clone().await.expect("settle");
}

// ── 1. Active 态 update:新 config 生效 + 恰好一次清理 ──────────────

#[tokio::test]
async fn update_active_applies_new_config_and_cleans_once() {
    let ctx = Ctx::root().unwrap();
    let factory = TestFactory::new();
    let view = ctx.plugin_with(factory.clone(), cfg("v1", 1));
    settle(&view).await;
    assert_eq!(ctx.get::<ConfigSvc>().unwrap().label.clone(), "v1");
    let gen_before = view.state().generation;

    view.update(cfg("v2", 2)).await.expect("update");
    assert_eq!(view.state().state, FiberState::Active);
    assert_eq!(view.state().generation, gen_before + 1);
    assert_eq!(ctx.get::<ConfigSvc>().unwrap().label.clone(), "v2");
    assert_eq!(ctx.get::<ConfigSvc>().unwrap().n, 2);
    // 旧代清理恰好一次,两代 apply 各一次
    assert_eq!(factory.cleanups.load(Ordering::SeqCst), 1);
    assert_eq!(
        factory.apply_log.lock().unwrap().as_slice(),
        ["apply:v1", "apply:v2"]
    );
}

// ── 2. dry-run 三种失败:Err 返回,现状不动 ─────────────────────────

#[tokio::test]
async fn update_dry_run_failure_leaves_state_untouched() {
    for (n, what) in [
        (1000u32, "validate_config"),
        (1001, "build"),
        (u32::MAX, "instance"),
    ] {
        let ctx = Ctx::root().unwrap();
        let factory = TestFactory::new();
        let view = ctx.plugin_with(factory.clone(), cfg("v1", 1));
        settle(&view).await;

        let err = view.update(cfg("bad", n)).await.expect_err(what);
        assert!(
            matches!(
                *err,
                CordisError::Validation { .. } | CordisError::PluginFailed(_)
            ),
            "{what}: unexpected {err:?}"
        );
        // 现状不动:状态/服务/清理计数不变
        assert_eq!(view.state().state, FiberState::Active);
        assert_eq!(ctx.get::<ConfigSvc>().unwrap().label.clone(), "v1");
        assert_eq!(factory.cleanups.load(Ordering::SeqCst), 0);
        assert_eq!(factory.apply_log.lock().unwrap().len(), 1);
    }
}

// ── 3. Pending 态 update:依赖到位后用新 config 装载 ────────────────

#[tokio::test]
async fn update_pending_uses_new_config_when_gate_opens() {
    #[derive(Debug)]
    struct GateDep;
    let factory = TestFactory::new();
    let _f = factory.clone();

    let ctx = Ctx::root().unwrap();
    // 工厂模式 + injects 从 config 派生
    struct GatedFactory(TestFactory);
    impl PluginFactory<TestConfig> for GatedFactory {
        fn name(&self) -> &str {
            "gated-factory"
        }
        fn injects(&self, _config: &TestConfig) -> Vec<TypeKey> {
            vec![TypeKey::of::<GateDep>()]
        }
        fn build(&self, config: &TestConfig) -> Result<Box<dyn Plugin>, CordisError> {
            self.0.build(config)
        }
    }

    let view = ctx.plugin_with(GatedFactory(factory), cfg("v1", 1));
    // 依赖未到:Pending
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert_eq!(view.state().state, FiberState::Pending);

    // Pending 态热更新(不该触发装载)
    let upd = view.update(cfg("v2", 2));
    let v = view.clone();
    tokio::spawn(async move {
        let _ = upd.await;
        let _ = v;
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(view.state().state, FiberState::Pending);

    // 门开:装载用的是新 config
    ctx.provide(GateDep).unwrap();
    settle(&view).await;
    assert_eq!(ctx.get::<ConfigSvc>().unwrap().label.clone(), "v2");
    // v1 从未装载
    assert_eq!(_f.apply_log.lock().unwrap().as_slice(), ["apply:v2"]);
}

// ── 4. Failed 态 update:修复配置热修复 ────────────────────────────

#[tokio::test]
async fn update_failed_recovers_with_new_config() {
    // 工厂按 config 分支:n=0 造一个 apply 必失败的插件,否则正常。
    // 同一 fiber 的 factory 固定,config 才是热修复的变量。
    struct BranchFactory;
    impl PluginFactory<TestConfig> for BranchFactory {
        fn build(&self, config: &TestConfig) -> Result<Box<dyn Plugin>, CordisError> {
            if config.n == 0 {
                Ok(Box::new(BoomPlugin))
            } else {
                Ok(Box::new(ConfigPlugin {
                    svc: ConfigSvc {
                        label: config.label.clone(),
                        n: config.n,
                    },
                    apply_log: Arc::new(Mutex::new(Vec::new())),
                    cleanups: Arc::new(AtomicUsize::new(0)),
                    fail_apply: false,
                }))
            }
        }
    }
    struct BoomPlugin;
    impl Plugin for BoomPlugin {
        fn name(&self) -> &str {
            "boom"
        }
        fn apply<'a>(&'a self, _ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
            Box::pin(async move { Err(CordisError::PluginFailed("boom".into())) })
        }
    }

    let ctx = Ctx::root().unwrap();
    let view = ctx.plugin_with(BranchFactory, cfg("bad", 0));
    let err = view.clone().await.expect_err("load fails");
    assert!(matches!(*err, CordisError::PluginFailed(_)));
    assert_eq!(view.state().state, FiberState::Failed);

    // 热修复:换 config 走正常分支
    view.update(cfg("fixed", 5)).await.expect("recover");
    assert_eq!(view.state().state, FiberState::Active);
    assert_eq!(ctx.get::<ConfigSvc>().unwrap().label.clone(), "fixed");
}

// ── 5. update 后消费者重载 ────────────────────────────────────────

#[tokio::test]
async fn update_evicts_consumers_who_reload() {
    // provider 工厂插件:提供 ConfigSvc
    let ctx = Ctx::root().unwrap();
    let view = ctx.plugin_with(TestFactory::new(), cfg("p1", 1));
    settle(&view).await;

    // 消费者:注入 ConfigSvc,每次 apply 记录读到的服务标签(提供方代际决定)
    let loads = Arc::new(AtomicUsize::new(0));
    struct Consumer {
        loads: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<String>>>,
    }
    impl Plugin for Consumer {
        fn name(&self) -> &str {
            "consumer"
        }
        fn injects(&self) -> &[TypeKey] {
            Box::leak(Box::new([TypeKey::of::<ConfigSvc>()]))
        }
        fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
            let loads = self.loads.clone();
            let seen = self.seen.clone();
            Box::pin(async move {
                seen.lock()
                    .unwrap()
                    .push(ctx.get::<ConfigSvc>().unwrap().label.clone());
                loads.fetch_add(1, Ordering::SeqCst);
                Ok(Effect::Done)
            })
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let consumer = ctx.plugin(Consumer {
        loads: loads.clone(),
        seen: seen.clone(),
    });
    consumer.clone().await.expect("consumer loads");
    assert_eq!(seen.lock().unwrap().as_slice(), ["p1"]);

    // provider 热更新:消费者被驱逐并自动重载,读到新值
    view.update(cfg("p2", 2)).await.expect("update");
    assert_eq!(loads.load(Ordering::SeqCst), 2);
    assert_eq!(seen.lock().unwrap().as_slice(), ["p1", "p2"]);
}

// ── 6. 并发 update × dispose / update × update ─────────────────────

#[tokio::test]
async fn concurrent_update_and_dispose_settle_correctly() {
    let ctx = Ctx::root().unwrap();
    let view = ctx.plugin_with(TestFactory::new(), cfg("v1", 1));
    settle(&view).await;

    // dispose 先登记:update 拒绝
    let dispose = view.dispose();
    let upd = view.update(cfg("v2", 2)).await;
    assert!(matches!(upd, Err(e) if matches!(*e, CordisError::InactiveEffect)));
    dispose.await.expect("dispose");
    assert_eq!(view.state().state, FiberState::Disposed);

    // 并发双 update:mailbox FIFO,后者覆盖,两次 join 均落定
    let ctx2 = Ctx::root().unwrap();
    let view2 = ctx2.plugin_with(TestFactory::new(), cfg("v1", 1));
    settle(&view2).await;
    let a = view2.update(cfg("v2", 2));
    let b = view2.update(cfg("v3", 3));
    let (a, b) = tokio::join!(a, b);
    a.expect("first update");
    b.expect("second update");
    assert_eq!(ctx2.get::<ConfigSvc>().unwrap().label, "v3");
}

// ── 7. 工厂 injects 门控 ───────────────────────────────────────────

#[tokio::test]
async fn factory_injects_gate_plugin_until_ready() {
    #[derive(Debug)]
    struct Need;
    struct F;
    impl PluginFactory<TestConfig> for F {
        fn injects(&self, _config: &TestConfig) -> Vec<TypeKey> {
            vec![TypeKey::of::<Need>()]
        }
        fn build(&self, config: &TestConfig) -> Result<Box<dyn Plugin>, CordisError> {
            Ok(Box::new(ConfigPlugin {
                svc: ConfigSvc {
                    label: config.label.clone(),
                    n: config.n,
                },
                apply_log: Arc::new(Mutex::new(Vec::new())),
                cleanups: Arc::new(AtomicUsize::new(0)),
                fail_apply: false,
            }))
        }
    }
    let ctx = Ctx::root().unwrap();
    let view = ctx.plugin_with(F, cfg("v1", 1));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert_eq!(view.state().state, FiberState::Pending);
    assert!(ctx.get::<ConfigSvc>().is_none());

    ctx.provide(Need).unwrap();
    settle(&view).await;
    assert_eq!(ctx.get::<ConfigSvc>().unwrap().label.clone(), "v1");
}

// ── 8. 静态 fiber / 类型不匹配的 update 报错 ───────────────────────

#[tokio::test]
async fn update_rejects_static_fiber_and_type_mismatch() {
    struct StaticPlugin;
    impl Plugin for StaticPlugin {
        fn name(&self) -> &str {
            "static"
        }
        fn apply<'a>(&'a self, _ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
            Box::pin(async move { Ok(Effect::Done) })
        }
    }
    let ctx = Ctx::root().unwrap();
    let static_view = ctx.plugin(StaticPlugin);
    static_view.clone().await.expect("loads");
    // 静态 fiber:无工厂
    let err = static_view.update(cfg("x", 1)).await.err().unwrap();
    assert!(matches!(*err, CordisError::Validation { .. }));

    // 工厂 fiber + 错误 config 类型
    let fview = ctx.plugin_with(TestFactory::new(), cfg("v1", 1));
    settle(&fview).await;
    #[derive(Debug, Clone, PartialEq)]
    struct OtherConfig;
    let err = fview.update(OtherConfig).await.err().unwrap();
    assert!(matches!(*err, CordisError::Validation { .. }));
    // 现状不动
    assert_eq!(fview.state().state, FiberState::Active);
    assert_eq!(ctx.get::<ConfigSvc>().unwrap().label.clone(), "v1");
}

// ── 9. 依赖驱动重载用当前 config 重造实例 ──────────────────────────

#[tokio::test]
async fn dependency_reload_rebuilds_with_current_config() {
    // provider(普通 provide)+ 工厂消费者:apply 里读 config 决定服务值
    #[derive(Debug)]
    struct Dep;
    struct RecordFactory {
        seen: Arc<Mutex<Vec<u32>>>,
    }
    struct RecordingPlugin {
        n: u32,
        seen: Arc<Mutex<Vec<u32>>>,
    }
    impl Plugin for RecordingPlugin {
        fn name(&self) -> &str {
            "recording"
        }
        fn injects(&self) -> &[TypeKey] {
            Box::leak(Box::new([TypeKey::of::<Dep>()]))
        }
        fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
            let n = self.n;
            let seen = self.seen.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(n);
                ctx.provide(ConfigSvc {
                    label: format!("gen{n}"),
                    n,
                })?;
                Ok(Effect::Done)
            })
        }
    }
    impl PluginFactory<TestConfig> for RecordFactory {
        fn injects(&self, _config: &TestConfig) -> Vec<TypeKey> {
            vec![TypeKey::of::<Dep>()]
        }
        fn build(&self, config: &TestConfig) -> Result<Box<dyn Plugin>, CordisError> {
            Ok(Box::new(RecordingPlugin {
                n: config.n,
                seen: self.seen.clone(),
            }))
        }
    }

    let ctx = Ctx::root().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let view = ctx.plugin_with(RecordFactory { seen: seen.clone() }, cfg("v1", 1));
    // 依赖未到:Pending
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert_eq!(seen.lock().unwrap().len(), 0);

    let d1 = ctx.provide(Dep).unwrap();
    settle(&view).await;
    assert_eq!(seen.lock().unwrap().as_slice(), [1]);

    // 依赖摘除 → 驱逐回 Pending;重新提供 → 重载仍用当前 config(n=1)
    d1.dispose().await.expect("evict");
    settle(&view).await;
    assert_eq!(view.state().state, FiberState::Pending);
    let d2 = ctx.provide(Dep).unwrap();
    settle(&view).await;
    assert_eq!(seen.lock().unwrap().as_slice(), [1, 1]);
    drop(d2);

    // update 后的依赖驱动重载 → 用新 config
    view.update(cfg("v2", 2)).await.expect("update");
    assert_eq!(seen.lock().unwrap().as_slice(), [1, 1, 2]);
}

// ── 10. current_config 快照 ───────────────────────────────────────

#[tokio::test]
async fn current_config_returns_snapshot() {
    let ctx = Ctx::root().unwrap();
    let view = ctx.plugin_with(TestFactory::new(), cfg("v1", 1));
    settle(&view).await;
    assert_eq!(view.current_config::<TestConfig>().unwrap().label, "v1");
    view.update(cfg("v2", 2)).await.expect("update");
    assert_eq!(view.current_config::<TestConfig>().unwrap().label, "v2");
    // 类型不符 → None
    assert!(view.current_config::<String>().is_none());
}
