//! M3 端到端:`evt/emit` 通知帧 → `forward_host_events` → 内核 keyed 事件
//! `HostEvent` 的订阅方收到(design-config-hot-update-and-dynamic-events §2.6-9)。
//!
//! 走 MemoryWire(进程内桥),宿主侧用真帧序列驱动;形状与 rutis_dsh runner
//! 的组装一致(forward + stderr 观察者并行)。

use std::sync::{Arc, Mutex};

use rutis::Listener;
use rutis_cordis::{Bridge, BridgeConfig, ExpectedHost, Frame, InboundHooks, MemoryWire, Wire};
use serde_json::{json, Value};

/// 记录 (name, payload) 的订阅监听器。
struct Recorder {
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Listener<rutis_cordis::HostEvent> for Recorder {
    fn call<'a>(
        &'a self,
        _ctx: &'a rutis::Ctx,
        e: &'a rutis_cordis::HostEvent,
    ) -> rutis::BoxFuture<'a, Result<Option<Value>, rutis::CordisError>> {
        let seen = self.seen.clone();
        let name = e.name.clone();
        let payload = e.payload.clone();
        Box::pin(async move {
            seen.lock().unwrap().push((name, payload));
            Ok(None)
        })
    }
}

async fn hello_from_host(wire: &MemoryWire) {
    wire.send(Frame::Req {
        id: 1,
        method: "hello".into(),
        params: json!({
            "protocol": 1,
            "base": "cordis",
            "baseSemver": "4.0.1",
            "stack": ["node"],
            "caps": { "services": ["llm"], "wfKinds": [], "scopes": [] },
        }),
        scope_id: None,
        session_id: None,
        turn_id: None,
    })
    .await
    .expect("send hello");
    // 桥的回包(不细验,m1.rs 已锁)
    wire.recv().await.expect("bridge alive for hello res");
}

#[tokio::test]
async fn host_evt_emit_reaches_keyed_subscriber() {
    let ctx = rutis::Ctx::root().unwrap();
    let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));

    // 订阅两个名字的宿主事件(运行时字符串)
    ctx.events()
        .on_keyed::<rutis_cordis::HostEvent>(&ctx, "session/event", Recorder { seen: seen.clone() })
        .unwrap();
    let other_seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    ctx.events()
        .on_keyed::<rutis_cordis::HostEvent>(
            &ctx,
            "agent/turn",
            Recorder {
                seen: other_seen.clone(),
            },
        )
        .unwrap();

    // 桥组装:forward_host_events(ctx, observe)——观察者并行收全部帧
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let obs = observed.clone();
    let hooks = InboundHooks {
        on_request: None,
        on_notify: Some(rutis_cordis::forward_host_events(
            &ctx,
            Some(Arc::new(move |method, params| {
                let obs = obs.clone();
                Box::pin(async move {
                    if method == "evt/emit" {
                        obs.lock()
                            .unwrap()
                            .push(params["event"].as_str().unwrap_or("?").into());
                    }
                })
            })),
        )),
    };

    let (bridge_wire, host_wire) = MemoryWire::pair(64);
    let mut bridge = Bridge::start(
        Box::new(bridge_wire),
        BridgeConfig::default(),
        hooks,
        ExpectedHost::protocol(1),
        json!({ "services": ["llm"], "wfKinds": [], "scopes": [] }),
    );
    hello_from_host(&host_wire).await;
    bridge.ready().await.expect("handshake");

    // 宿主发 evt/emit:session/event
    host_wire
        .send(Frame::Ntf {
            method: "evt/emit".into(),
            params: json!({
                "event": "session/event",
                "params": { "kind": "message", "text": "hi" },
            }),
            scope_id: None,
            session_id: Some("s-1".into()),
            turn_id: None,
        })
        .await
        .expect("send evt");
    // 另一个名字 + 恶形(无 event 字段,应静默丢弃)
    host_wire
        .send(Frame::Ntf {
            method: "evt/emit".into(),
            params: json!({
                "event": "agent/turn",
                "params": { "n": 7 },
            }),
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send evt 2");
    host_wire
        .send(Frame::Ntf {
            method: "evt/emit".into(),
            params: json!({ "params": {} }), // 恶形:无 event
            scope_id: None,
            session_id: None,
            turn_id: None,
        })
        .await
        .expect("send malformed evt");

    // 等 emit 尾链派发完成
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    // 订阅方:按名字精确命中,载荷保真
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [(
            "session/event".to_string(),
            json!({ "kind": "message", "text": "hi" })
        )],
        "session/event subscriber got exactly its event"
    );
    assert_eq!(
        other_seen.lock().unwrap().as_slice(),
        [("agent/turn".to_string(), json!({ "n": 7 }))],
        "agent/turn subscriber got exactly its event"
    );
    // 观察者:全部 evt/emit 都到达(恶形也到达观察者,但不进事件总线 → "?")
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        ["session/event", "agent/turn", "?"]
    );
}
