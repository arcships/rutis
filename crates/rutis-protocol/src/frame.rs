//! Private stream framing and an independent request pump. A writer task owns
//! each complete write, so dropping a requester cannot leave half a frame.
use crate::error::{ErrorCode, Execution, ProtocolError, Result};
use crate::identity::Sequence;
use crate::json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Request {
        id: Sequence,
        method: String,
        params: Value,
    },
    Response {
        id: Sequence,
        result: Value,
    },
    Error {
        id: Sequence,
        error: ProtocolError,
    },
}
fn failure(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, "frame", message)
}
pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Value>> {
    let mut header = [0u8; 4];
    if reader
        .read(&mut header[..1])
        .await
        .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))?
        == 0
    {
        return Ok(None);
    }
    reader
        .read_exact(&mut header[1..])
        .await
        .map_err(|_| failure(ErrorCode::InvalidParams, "truncated frame header"))?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > json::MAX_JSON_BYTES {
        return Err(failure(ErrorCode::InvalidParams, "invalid frame length"));
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|_| failure(ErrorCode::InvalidParams, "truncated frame body"))?;
    json::decode(&bytes).map(Some)
}
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    let bytes =
        serde_json::to_vec(value).map_err(|e| failure(ErrorCode::InvalidParams, e.to_string()))?;
    json::decode(&bytes)?;
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend(bytes);
    Ok(frame)
}
pub async fn write<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    writer
        .write_all(&encode(value)?)
        .await
        .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))
}
pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'static>>;
pub type Handler = Arc<dyn Fn(String, Value) -> HandlerFuture + Send + Sync>;
struct State {
    next: u64,
    latest_request: Sequence,
    pending: BTreeMap<Sequence, oneshot::Sender<Result<Value>>>,
    closed: Option<ProtocolError>,
}
struct Inner {
    state: Mutex<State>,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    closing: tokio_util::sync::CancellationToken,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.closing.cancel();
    }
}
#[derive(Clone)]
pub struct Peer(Arc<Inner>);
#[derive(Clone)]
pub struct WeakPeer(Weak<Inner>);
impl WeakPeer {
    pub fn upgrade(&self) -> Option<Peer> {
        self.0.upgrade().map(Peer)
    }
}
impl Peer {
    pub fn start<S>(stream: S, handler: Handler) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (outgoing, mut writes) = mpsc::unbounded_channel::<Vec<u8>>();
        let peer = Self(Arc::new(Inner {
            state: Mutex::new(State {
                next: 0,
                latest_request: Sequence(0),
                pending: BTreeMap::new(),
                closed: None,
            }),
            outgoing,
            closing: tokio_util::sync::CancellationToken::new(),
        }));
        let write_peer = peer.downgrade();
        let closing = peer.0.closing.clone();
        tokio::spawn(async move {
            loop {
                let bytes = tokio::select! { _ = closing.cancelled() => break, bytes = writes.recv() => match bytes { Some(bytes) => bytes, None => break } };
                let result = tokio::select! { _ = closing.cancelled() => break, result = writer.write_all(&bytes) => result };
                if let Err(error) = result {
                    if let Some(peer) = write_peer.upgrade() {
                        peer.close(failure(ErrorCode::Unavailable, error.to_string()));
                    }
                    break;
                }
            }
        });
        let read_peer = peer.downgrade();
        let closing = peer.0.closing.clone();
        tokio::spawn(async move {
            let result = async {
                loop {
                    let value = tokio::select! { _ = closing.cancelled() => break, value = read(&mut reader) => match value? { Some(value) => value, None => break } };
                    let Some(read_peer) = read_peer.upgrade() else { break; };
                    let message: Message = serde_json::from_value(value).map_err(|e| failure(ErrorCode::InvalidParams, e.to_string()))?;
                    match message {
                        Message::Response { id, result } => read_peer.response(id, Ok(result))?,
                        Message::Error { id, error } => read_peer.response(id, Err(error))?,
                        Message::Request { id, method, params } => {
                            {
                                let mut state = read_peer.0.state.lock().unwrap();
                                if id <= state.latest_request { return Err(failure(ErrorCode::InvalidParams, "request id cannot be reused or reordered")); }
                                state.latest_request = id;
                            }
                            // Trusted runtime handlers synchronously admit
                            // control metadata and return the business future.
                            let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(method, params))).map_err(|_| failure(ErrorCode::Business, "request admission panicked"))?;
                            let response_peer = read_peer.clone();
                            tokio::spawn(async move {
                                let result = tokio::spawn(future).await.unwrap_or_else(|e| {
                                    let mut error = failure(ErrorCode::Business, e.to_string()); error.execution = Execution::Unknown; Err(error)
                                });
                                let message = match result { Ok(result) => Message::Response { id, result }, Err(error) => Message::Error { id, error } };
                                if let Err(error) = response_peer.send(message) { response_peer.close(error); }
                            });
                        }
                    }
                }
                Ok::<_, ProtocolError>(())
            }.await;
            if let Some(peer) = read_peer.upgrade() {
                peer.close(result.err().unwrap_or_else(|| {
                    failure(ErrorCode::Unavailable, "private stream disconnected")
                }));
            }
        });
        peer
    }
    fn send(&self, message: Message) -> Result<()> {
        let bytes = encode(
            &serde_json::to_value(message)
                .map_err(|e| failure(ErrorCode::InvalidParams, e.to_string()))?,
        )?;
        if let Some(error) = &self.0.state.lock().unwrap().closed {
            return Err(error.clone());
        }
        self.0
            .outgoing
            .send(bytes)
            .map_err(|_| failure(ErrorCode::Unavailable, "writer stopped"))
    }
    fn response(&self, id: Sequence, response: Result<Value>) -> Result<()> {
        let send = self
            .0
            .state
            .lock()
            .unwrap()
            .pending
            .remove(&id)
            .ok_or_else(|| {
                failure(
                    ErrorCode::InvalidParams,
                    "response does not identify a pending request",
                )
            })?;
        // Resource-bearing callers must keep their execution transaction alive
        // and reject unconsumed handoffs; this layer only carries JSON.
        let _ = send.send(response);
        Ok(())
    }
    pub fn start_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value>>> {
        let (send, receive) = oneshot::channel();
        {
            let mut state = self.0.state.lock().unwrap();
            if let Some(error) = &state.closed {
                return Err(error.clone());
            }
            state.next = state
                .next
                .checked_add(1)
                .ok_or_else(|| failure(ErrorCode::Unavailable, "request sequence exhausted"))?;
            let id = Sequence(state.next);
            state.pending.insert(id, send);
            // Queue under the allocation lock. Concurrent callers cannot put
            // id 2 on the stream ahead of id 1.
            let result = serde_json::to_value(Message::Request {
                id,
                method: method.into(),
                params,
            })
            .map_err(|e| failure(ErrorCode::InvalidParams, e.to_string()))
            .and_then(|v| encode(&v))
            .and_then(|bytes| {
                self.0
                    .outgoing
                    .send(bytes)
                    .map_err(|_| failure(ErrorCode::Unavailable, "writer stopped"))
            });
            if let Err(error) = result {
                state.pending.remove(&id);
                return Err(error);
            }
        }
        Ok(receive)
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.start_request(method, params)?
            .await
            .map_err(|_| failure(ErrorCode::Unavailable, "request pump stopped"))?
    }
    pub fn is_closed(&self) -> bool {
        self.0.state.lock().unwrap().closed.is_some()
    }
    pub fn downgrade(&self) -> WeakPeer {
        WeakPeer(Arc::downgrade(&self.0))
    }
    pub fn close(&self, error: ProtocolError) {
        let pending = {
            let mut state = self.0.state.lock().unwrap();
            if state.closed.is_some() {
                return;
            }
            state.closed = Some(error.clone());
            std::mem::take(&mut state.pending)
        };
        self.0.closing.cancel();
        for send in pending.into_values() {
            let mut error = error.clone();
            error.execution = Execution::Unknown;
            let _ = send.send(Err(error));
        }
    }
}
