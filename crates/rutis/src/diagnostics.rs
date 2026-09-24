//! Read-only snapshots of fiber ownership and service resolution.
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};

use crate::{CordisError, FiberState, InstanceId, PluginId, ServiceReadFailure, TypeKey};

/// A read-only, best-effort view of live fibers and service bindings.
///
/// Fields are collected under separate locks, so concurrent lifecycle changes
/// can make entries reflect different moments. A retained snapshot does not
/// update itself; call [`crate::Ctx::diagnostics`] again for a fresh view.
#[derive(Debug, Clone)]
pub struct RuntimeDiagnostics {
    pub shutting_down: bool,
    pub plugins: Vec<PluginDiagnostics>,
    pub bindings: Vec<BindingDiagnostics>,
}

#[derive(Debug, Clone)]
pub struct PluginDiagnostics {
    pub id: PluginId,
    pub instance: InstanceId,
    pub parent: Option<PluginId>,
    pub name: String,
    pub state: FiberState,
    pub generation: u64,
    pub error: Option<Arc<CordisError>>,
    pub injects: Vec<DependencyDiagnostics>,
    pub resolved_dependencies: Vec<ResolvedDependency>,
    pub accesses: Vec<ServiceAccess>,
}

#[derive(Debug, Clone)]
pub struct DependencyDiagnostics {
    pub key: TypeKey,
    pub scope: Option<String>,
    pub status: DependencyStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyStatus {
    OutOfScope,
    Missing,
    Removing,
    ProviderInactive(FiberState),
    CheckPending,
    CheckRejected,
    CheckPanicked,
    Ready,
}

impl std::fmt::Display for DependencyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfScope => f.write_str("instance out of scope"),
            Self::Missing => f.write_str("service missing"),
            Self::Removing => f.write_str("service being removed"),
            Self::ProviderInactive(state) => write!(f, "provider inactive ({state:?})"),
            Self::CheckPending => f.write_str("check pending"),
            Self::CheckRejected => f.write_str("check rejected"),
            Self::CheckPanicked => f.write_str("check panicked"),
            Self::Ready => f.write_str("ready"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub key: TypeKey,
    pub scope: Option<String>,
    pub provider: PluginId,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccess {
    pub key: TypeKey,
    pub scope: Option<String>,
    pub provider: Option<PluginId>,
    pub generation: Option<u64>,
    pub declared: bool,
    pub external: bool,
    pub out_of_scope: bool,
    /// Whether this access used `require` / `require_as`.
    pub strict: bool,
    /// Rejection reason for a strict read; absent on success and optional reads.
    pub failure: Option<ServiceReadFailure>,
}

#[derive(Debug, Clone)]
pub struct BindingDiagnostics {
    pub key: TypeKey,
    pub scope: Option<String>,
    pub provider: PluginId,
    pub generation: u64,
    pub removing: bool,
}

/// Identity of one service generation. No service value is retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingIdentity {
    pub provider: PluginId,
    pub instance: InstanceId,
    pub generation: u64,
    pub key: TypeKey,
    pub scope: Option<String>,
}

/// A structural lifecycle change, or one rejected strict read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticChangeKind {
    PluginRegistered {
        plugin: PluginId,
        instance: InstanceId,
        parent: Option<PluginId>,
        name: String,
        state: FiberState,
        generation: u64,
    },
    PluginTerminated {
        plugin: PluginId,
        instance: InstanceId,
        parent: Option<PluginId>,
    },
    StateChanged {
        plugin: PluginId,
        instance: InstanceId,
        generation: u64,
        from: FiberState,
        to: FiberState,
    },
    BindingRegistered(BindingIdentity),
    BindingRemoving(BindingIdentity),
    BindingRemoved(BindingIdentity),
    StrictReadDenied {
        plugin: PluginId,
        instance: InstanceId,
        key: TypeKey,
        scope: Option<String>,
        reason: ServiceReadFailure,
    },
}

/// Root-wide enqueue order. Events from distinct fibers do not claim causal order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticChange {
    pub seq: u64,
    pub kind: DiagnosticChangeKind,
}

/// Subscribe first, then scan a best-effort snapshot. The snapshot may overlap
/// changes after `cursor`; consumers should replay by identity and sequence.
/// This feed does not cover every mutable snapshot field, such as cached checks
/// and access history. `Lagged` from `recv()` requires a fresh subscription.
pub struct DiagnosticSubscription {
    pub initial: RuntimeDiagnostics,
    pub cursor: u64,
    pub changes: broadcast::Receiver<DiagnosticChange>,
}

struct HubState {
    seq: u64,
    live_children: usize,
    sender: Option<broadcast::Sender<DiagnosticChange>>,
}

/// A bounded, read-only broadcast. Sending never calls subscriber code.
pub(crate) struct DiagnosticHub {
    state: Mutex<HubState>,
    live_tx: watch::Sender<usize>,
}

impl DiagnosticHub {
    pub(crate) fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        let (live_tx, _) = watch::channel(0);
        Self {
            state: Mutex::new(HubState {
                seq: 0,
                live_children: 0,
                sender: Some(sender),
            }),
            live_tx,
        }
    }

    pub(crate) fn subscribe(&self) -> Option<(u64, broadcast::Receiver<DiagnosticChange>)> {
        let state = self.state.lock().unwrap();
        state
            .sender
            .as_ref()
            .map(|sender| (state.seq, sender.subscribe()))
    }

    pub(crate) fn publish(&self, kind: DiagnosticChangeKind) {
        let mut state = self.state.lock().unwrap();
        let Some(sender) = state.sender.clone() else {
            return;
        };
        match &kind {
            DiagnosticChangeKind::PluginRegistered { .. } => {
                state.live_children += 1;
                self.live_tx.send_replace(state.live_children);
            }
            DiagnosticChangeKind::PluginTerminated {
                parent: Some(_), ..
            } => {
                state.live_children -= 1;
                self.live_tx.send_replace(state.live_children);
            }
            _ => {}
        }
        state.seq = state
            .seq
            .checked_add(1)
            .expect("diagnostic sequence exhausted");
        let _ = sender.send(DiagnosticChange {
            seq: state.seq,
            kind,
        });
    }

    pub(crate) fn close(&self) {
        self.state.lock().unwrap().sender.take();
    }

    pub(crate) async fn wait_children(&self) {
        let mut live = self.live_tx.subscribe();
        while *live.borrow_and_update() != 0 {
            if live.changed().await.is_err() {
                break;
            }
        }
    }
}
