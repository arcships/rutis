//! 终态释放(0.2.1)单元验证:瞬态子插件退出后,mount 记录与 root 级
//! provide 的清理记录、provided 记账在长寿 root 上回到基线。

use super::*;
use crate::{BoxFuture, CordisError, Effect, Plugin};
use std::time::Duration;

struct Noop;

impl Plugin for Noop {
    fn name(&self) -> &str {
        "noop"
    }

    fn apply<'a>(&'a self, _ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async { Ok(Effect::Done) })
    }
}

/// mount 记录经子 fiber 退出路径 drain 自摘,root 的 effects 列表
/// 不随插件 churn 累积。
#[tokio::test]
async fn churn_mount_records_release_from_root() {
    let ctx = Ctx::root().expect("runtime in scope");
    let root = ctx.weak_fiber().upgrade().expect("root alive");
    for _ in 0..25 {
        let view = ctx.plugin(Noop);
        view.dispose().await.unwrap();
        drop(view);
    }
    // 释放尾随 TaskDone(dispose 的 join 点),有界等待列表清空
    for _ in 0..500 {
        if root.effects.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        root.effects.lock().unwrap().is_empty(),
        "mount records must not accumulate on a long-lived root"
    );
}

/// root 级 provide 的 evict 清理记录与 provided 记账随 Disposer 释放。
#[tokio::test]
async fn churn_root_provides_release_accounting() {
    let ctx = Ctx::root().expect("runtime in scope");
    let root = ctx.weak_fiber().upgrade().expect("root alive");
    for i in 0..25 {
        let key = TypeKey::keyed_dynamic::<u32>(format!("svc/{i}"));
        let disposer = ctx.provide_as::<u32>(key, Arc::new(i)).unwrap();
        disposer.dispose().await.unwrap();
    }
    assert!(
        root.provided.lock().unwrap().is_empty(),
        "provided accounting must shrink per released binding"
    );
    assert!(
        root.effects.lock().unwrap().is_empty(),
        "evict cleanups must self-remove after drain"
    );
}
