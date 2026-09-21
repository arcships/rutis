//! 宿主事件链路(M3,design-config-hot-update-and-dynamic-events §2.4):
//! `evt/emit` 通知帧 → rutis 内核 keyed 事件的翻译缝。
//!
//! 订阅方:`ctx.events().on_keyed::<HostEvent>(ctx, "session/event", listener)`;
//! 四分发语义 / fiber 生命周期清理 / once / prepend 全部由内核 keyed 面
//! 免费提供。事件名 = `params.event`(字符串内容匹配,静态/动态互通)。

use std::sync::Arc;

use rutis::Ctx;
use serde_json::Value;

use crate::rpc::{EventOrigin, NotifyHook};

/// 宿主透传事件:`evt/emit` 的翻译产物(D33b:定义在本 crate,内核零 serde)。
///
/// `name` 与注册键的 qualifier 同值——监听器只拿到 `&HostEvent`,看不到键
/// 里的 qualifier,因此 name 存于值内供取用;两份数据由 `forward_host_events`
/// 保证一致。`origin` 是帧的三预留字段(scopeId/sessionId/turnId)透传,
/// 用于区分同事件名下不同会话/回合的来源。`payload` 是宿主事件的原始
/// params。serial/waterfall 的短路值类型为 `serde_json::Value`(宿主侧可短路)。
#[derive(Debug, Clone)]
pub struct HostEvent {
    pub name: String,
    pub payload: Value,
    pub origin: EventOrigin,
}

impl rutis::Event for HostEvent {
    const NAME: &'static str = "host/*";
    type Value = Value;
}

/// 组装 `evt/emit` → `HostEvent` 的入站通知钩子。
///
/// - `evt/emit`(`params.event` 非空字符串):翻译为
///   `emit_keyed::<HostEvent>(name, HostEvent)` 转发进内核事件总线。
///   恶形(无 event / 非字符串)丢弃并 `eprintln!` 一行——通知帧无回执
///   通道,错误无处上报,但不可静默消失(可观测性)。
/// - 所有帧:原样交给 `observe`(可选观察者,如 stderr 摘要日志)。
///   **转发先于 observe** 且互不隔离:observe panic/阻塞不吞事件
///   (emit_keyed 只入队尾链任务,不等待派发完成)。
pub fn forward_host_events(ctx: &Ctx, observe: Option<NotifyHook>) -> NotifyHook {
    let ctx = ctx.clone();
    Arc::new(move |method, params, origin| {
        let ctx = ctx.clone();
        let observe = observe.clone();
        Box::pin(async move {
            if method == "evt/emit" {
                if let Some(name) = params.get("event").and_then(Value::as_str) {
                    let payload = params.get("params").cloned().unwrap_or(Value::Null);
                    // 先转发后观察:转发是主线,observe 是诊断辅助。
                    ctx.events().emit_keyed::<HostEvent>(
                        &ctx,
                        name.to_string(),
                        Arc::new(HostEvent {
                            name: name.to_string(),
                            payload,
                            origin: origin.clone(),
                        }),
                    );
                } else {
                    eprintln!(
                        "[rutis-cordis] malformed evt/emit dropped (no string event field): {params}"
                    );
                }
            }
            if let Some(observe) = observe {
                observe(method, params, origin).await;
            }
        })
    })
}
