//! 最小示例:provider 插件 + 依赖它的 consumer。
//!
//! 运行:`cargo run -p rutis --example quickstart`
//!
//! 预期输出:
//! ```text
//! [listener] loaded: hello from greeter v1
//! [listener] loaded: hello from greeter v2
//! done: consumer reloaded without touching it
//! ```
//!
//! 第二行是 rutis 的核心能力在起作用:没有人为 consumer 做任何事,
//! provider 换代(旧卸载 + 新提供)驱动它自动驱逐并重载。

use rutis::{BoxFuture, CordisError, Ctx, Effect, FiberState, FiberView, Plugin, TypeKey};

/// 一个服务:类型即键,`Greeting` 类型的注册槽全局唯一。
struct Greeting(String);

/// provider 插件:apply 时把服务放进注册表,卸载时自动摘除。
struct Greeter {
    version: u32,
}

impl Plugin for Greeter {
    fn name(&self) -> &str {
        "greeter"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        let greeting = Greeting(format!("hello from greeter v{}", self.version));
        Box::pin(async move {
            ctx.provide(greeting)?;
            Ok(Effect::Done)
        })
    }
}

/// consumer 插件:声明依赖 `Greeting`——声明了,装载时机就交给框架。
struct Listener {
    deps: Vec<TypeKey>,
}

impl Plugin for Listener {
    fn name(&self) -> &str {
        "listener"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.deps // 依赖未就绪则停在 Pending,就绪后自动装载
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let greeting = ctx.require::<Greeting>()?.0.clone();
            println!("[listener] loaded: {greeting}");
            Ok(Effect::Done)
        })
    }
}

/// 等待 fiber 进入 Active。注意 settle(`(&view).await`)只保证
/// "处理完此前的意图"——Pending 也是稳定态;等"已装载"要看状态。
async fn wait_active(view: &FiberView) {
    let mut rx = view.watch();
    loop {
        if matches!(rx.borrow().state, FiberState::Active) {
            return;
        }
        rx.changed().await.expect("fiber driver alive");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = Ctx::root()?;

    // 1. 先装 consumer:依赖未到,它停在 Pending(依赖门控)。
    let listener = ctx.plugin(Listener {
        deps: vec![TypeKey::of::<Greeting>()],
    });

    // 2. provider 到位 → 门控放行,consumer 自动装载。
    let v1 = ctx.plugin(Greeter { version: 1 });
    wait_active(&listener).await; // apply 已打印第一行

    // 3. 换 provider:旧的卸载、服务摘除,consumer 被驱逐回 Pending;
    //    新 provider 提供同类型服务,consumer 自动重载——打印第二行。
    //    全程没有碰过 consumer。
    v1.dispose().await?;
    let _v2 = ctx.plugin(Greeter { version: 2 });
    wait_active(&listener).await;
    println!("done: consumer reloaded without touching it");
    Ok(())
}
