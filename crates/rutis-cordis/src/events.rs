//! 宿主事件链路(M3,design-config-hot-update-and-dynamic-events §2.4):
//! `evt/emit` 通知帧 → rutis 内核 keyed 事件的翻译缝。
//!
//! 订阅方:`ctx.events().on_keyed::<HostEvent>(ctx, "session/event", listener)`;
//! 四分发语义 / fiber 生命周期清理 / once / prepend 全部由内核 keyed 面
//! 免费提供。事件名 = `params.event`(字符串内容匹配,静态/动态互通)。

use std::sync::Arc;

use rutis::Ctx;
use serde_json::Value;

use crate::rpc::NotifyHook;

/// 宿主透传事件:`evt/emit` 的翻译产物(D33b:定义在本 crate,内核零 serde)。
///
/// `name` 冗余存于载荷(keyed 键里的限定名对监听器不可见,从值内取);
/// `payload` 是宿主事件的原始 params。serial/waterfall 的短路值类型为
/// `serde_json::Value`(宿主侧可短路)。
#[derive(Debug, Clone)]
pub struct HostEvent {
    pub name: String,
    pub payload: Value,
}

impl rutis::Event for HostEvent {
    const NAME: &'static str = "host/*";
    type Value = Value;
}

/// 组装 `evt/emit` → `HostEvent` 的入站通知钩子。
///
/// - `evt/emit`(`params.event` 非空字符串):翻译为
///   `emit_keyed::<HostEvent>(name, HostEvent)` 转发进内核事件总线;
///   恶形(无 event / 非字符串)静默丢弃——通知帧无回执通道,报错无处去。
/// - 其余 Ntf 与所有帧:原样交给 `observe`(可选观察者,如 stderr 摘要日志),
///   `evt/emit` 也会到达 observe(转发与观察并行,互不替代)。
pub fn forward_host_events(ctx: &Ctx, observe: Option<NotifyHook>) -> NotifyHook {
    let ctx = ctx.clone();
    Arc::new(move |method, params| {
        let ctx = ctx.clone();
        let observe = observe.clone();
        Box::pin(async move {
            if let Some(observe) = observe {
                observe(method.clone(), params.clone()).await;
            }
            if method == "evt/emit" {
                if let Some(name) = params.get("event").and_then(Value::as_str) {
                    let payload = params.get("params").cloned().unwrap_or(Value::Null);
                    ctx.events().emit_keyed::<HostEvent>(
                        &ctx,
                        name.to_string(),
                        Arc::new(HostEvent {
                            name: name.to_string(),
                            payload,
                        }),
                    );
                }
            }
        })
    })
}
