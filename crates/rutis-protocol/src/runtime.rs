//! A service runner acquires its immutable epoch identity from successful
//! private hello, before construction. No launch argument fabricates a member.
use crate::{
    error::{ErrorCode, ProtocolError, Result},
    frame::{Handler, Peer, WeakPeer},
    lifecycle::{Hello, Runner},
    services::Bundles,
    session::{RuntimeIdentity, RuntimeObjects},
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    objects: Option<Arc<RuntimeObjects>>,
    peer: Option<WeakPeer>,
}
pub struct ObjectSession {
    bundles: Bundles,
    state: Mutex<State>,
}
impl ObjectSession {
    pub fn new(bundles: Bundles) -> Arc<Self> {
        Arc::new(Self {
            bundles,
            state: Mutex::default(),
        })
    }
    pub(crate) fn admit(&self, hello: &Hello) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.objects.is_some() {
            return Err(ProtocolError::new(
                ErrorCode::Unavailable,
                "hello",
                "object session cannot rebind",
            ));
        }
        let objects = RuntimeObjects::new(
            RuntimeIdentity {
                runtime: hello.identity.runtime.clone(),
                epoch: hello.identity.epoch,
            },
            self.bundles.clone(),
        );
        if let Some(peer) = state.peer.as_ref().and_then(WeakPeer::upgrade) {
            objects.attach(&peer)?;
        }
        state.objects = Some(objects);
        Ok(())
    }
    pub fn objects(&self) -> Result<Arc<RuntimeObjects>> {
        self.state.lock().unwrap().objects.clone().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::Unavailable,
                "object_session",
                "private hello has not completed",
            )
        })
    }
    pub fn attach(&self, peer: &Peer) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.peer.is_some() {
            return Err(ProtocolError::new(
                ErrorCode::Unavailable,
                "object_session",
                "private peer cannot rebind",
            ));
        }
        if let Some(objects) = &state.objects {
            objects.attach(peer)?;
        }
        state.peer = Some(peer.downgrade());
        Ok(())
    }
    pub fn handler(self: &Arc<Self>, runner: Arc<Runner>) -> Handler {
        let session = self.clone();
        Arc::new(move |method, value| match session.objects() {
            Ok(objects) => objects.lifecycle_handler(runner.clone())(method, value),
            Err(error) if method.starts_with("object/") => Box::pin(async move { Err(error) }),
            Err(_) => runner.handle(&method, value),
        })
    }
}
