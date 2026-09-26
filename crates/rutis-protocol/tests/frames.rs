use rutis_protocol::{
    error::{ErrorCode, Execution, ProtocolError},
    frame::{self, Peer},
    identity::Sequence,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Deserialize)]
struct Case {
    name: String,
    hex: String,
    valid: bool,
}
#[tokio::test]
async fn shared_frames_reject_bad_lengths_truncation_and_noncanonical_json() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../../protocol/fixtures/frames.json")).unwrap();
    for case in cases {
        let bytes: Vec<u8> = case
            .hex
            .as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let (mut sender, mut receiver) = tokio::io::duplex(1);
        tokio::spawn(async move {
            for byte in bytes {
                if sender.write_all(&[byte]).await.is_err() {
                    return;
                }
            }
            sender.shutdown().await.unwrap();
        });
        let result = frame::read(&mut receiver).await;
        assert_eq!(result.is_ok(), case.valid, "{}", case.name);
        if case.valid {
            assert!(result.unwrap().is_some());
            assert!(frame::read(&mut receiver).await.unwrap().is_none());
        } else {
            assert_eq!(
                result.err().unwrap().code,
                ErrorCode::InvalidParams,
                "{}",
                case.name
            );
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pump_allows_nested_calls_and_concurrent_request_admission() {
    let (a, b) = tokio::io::duplex(32);
    let peer_a = Peer::start(a, Arc::new(|_, params| Box::pin(async move { Ok(params) })));
    let peer_b_slot = Arc::new(Mutex::new(None::<frame::WeakPeer>));
    let slot = peer_b_slot.clone();
    let peer_b = Peer::start(
        b,
        Arc::new(move |_, params| {
            let peer = slot.lock().unwrap().as_ref().unwrap().upgrade().unwrap();
            Box::pin(async move { peer.request("callback", params).await })
        }),
    );
    *peer_b_slot.lock().unwrap() = Some(peer_b.downgrade());
    let mut tasks = Vec::new();
    for n in 0..64 {
        let peer = peer_a.clone();
        tasks.push(tokio::spawn(async move {
            assert_eq!(peer.request("nested", json!(n)).await.unwrap(), n);
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for task in tasks {
            task.await.unwrap();
        }
    })
    .await
    .unwrap();
    peer_a.close(ProtocolError::new(ErrorCode::Unavailable, "test", "closed"));
    peer_b.close(ProtocolError::new(ErrorCode::Unavailable, "test", "closed"));
}
#[tokio::test]
async fn dropping_waiter_during_partial_write_preserves_frame_and_next_request() {
    let (client, mut server) = tokio::io::duplex(1);
    let peer = Peer::start(
        client,
        Arc::new(|_, value| Box::pin(async move { Ok(value) })),
    );
    let abandoned = peer
        .start_request("large", json!("x".repeat(4096)))
        .unwrap();
    drop(abandoned);
    let responding = tokio::spawn(async move {
        let first = frame::read(&mut server).await.unwrap().unwrap();
        assert_eq!(first["params"].as_str().unwrap().len(), 4096);
        frame::write(
            &mut server,
            &json!({"type":"response", "id":first["id"], "result":null}),
        )
        .await
        .unwrap();
        let second = frame::read(&mut server).await.unwrap().unwrap();
        frame::write(
            &mut server,
            &json!({"type":"response", "id":second["id"], "result":"still-open"}),
        )
        .await
        .unwrap();
    });
    assert_eq!(
        peer.request("next", Value::Null).await.unwrap(),
        "still-open"
    );
    responding.await.unwrap();
}
#[tokio::test]
async fn duplicate_requests_close_stream_without_reexecuting_handler() {
    let (client, mut server) = tokio::io::duplex(512);
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = calls.clone();
    let peer = Peer::start(
        client,
        Arc::new(move |_, value| {
            entered.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(value) })
        }),
    );
    frame::write(
        &mut server,
        &json!({"type":"request", "id":Sequence(1), "method":"once", "params":null}),
    )
    .await
    .unwrap();
    assert_eq!(
        frame::read(&mut server).await.unwrap().unwrap()["type"],
        "response"
    );
    frame::write(
        &mut server,
        &json!({"type":"request", "id":Sequence(1), "method":"once", "params":null}),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), frame::read(&mut server))
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        peer.request("closed", Value::Null).await.unwrap_err().code,
        ErrorCode::InvalidParams
    );
}
#[tokio::test]
async fn closing_midframe_releases_transport_and_reports_uncertain_execution() {
    let (client, mut server) = tokio::io::duplex(1);
    let peer = Peer::start(
        client,
        Arc::new(|_, value| Box::pin(async move { Ok(value) })),
    );
    let pending = peer
        .start_request("large", json!("x".repeat(4096)))
        .unwrap();
    server.read_exact(&mut [0u8; 1]).await.unwrap();
    peer.close(ProtocolError::new(
        ErrorCode::Unavailable,
        "test",
        "connection lost",
    ));
    assert_eq!(
        pending.await.unwrap().unwrap_err().execution,
        Execution::Unknown
    );
    // The writer is canceled even when blocked in write_all; it does not
    // retain the fd forever after application handles disappear.
    let mut remainder = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        server.read_to_end(&mut remainder),
    )
    .await
    .unwrap()
    .unwrap();
}
