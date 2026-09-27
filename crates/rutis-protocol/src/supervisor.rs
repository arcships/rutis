//! Frozen epoch cleanup, independently joining actual native and OS evidence.
//! This creates an old-epoch quiescence receipt, never an automatic new epoch.
use crate::{
    broker::ExecutionRecord,
    error::{ErrorCode, ProtocolError, Result},
    frame::Peer,
    host::{EpochNativeCleanup, HostGraph},
    process::{EpochReaping, ProcessHandle, ProcessStatus, ReapedEpoch},
    session::RuntimeIdentity,
    snapshot::SnapshotGroup,
};
use rutis::{ConsumerCleanup, CordisError, PluginId};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

fn fail(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Unavailable, "epoch_supervisor", message)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryPhase {
    Running,
    Closing,
    RecoveryBlocked,
    Quarantined,
    Quiescent,
}
#[derive(Clone, Debug)]
pub struct NativeMemberStatus {
    pub instance: String,
    pub identity: RuntimeIdentity,
    pub id: Option<PluginId>,
    pub name: Option<String>,
    pub state: Option<rutis::Snapshot>,
    pub cleanup: Option<std::result::Result<(), Arc<CordisError>>>,
    pub paired: Option<Result<()>>,
    pub error: Option<ProtocolError>,
}
#[derive(Clone)]
pub struct RecoveryStatus {
    pub identity: RuntimeIdentity,
    pub phase: RecoveryPhase,
    pub process: ProcessStatus,
    pub reaped: Option<Result<ReapedEpoch>>,
    pub native: Option<Result<()>>,
    pub members: Vec<NativeMemberStatus>,
    pub consumers: Vec<ConsumerCleanup>,
    pub executions: Vec<ExecutionRecord>,
    pub error: Option<ProtocolError>,
}
/// Constructed only after native cleanup, bound OS reaping, and actual residual
/// executions have finished. It does not itself authorize an unchecked rebind.
#[derive(Clone)]
pub struct QuiescentEpoch {
    group: String,
    graph: Weak<HostGraph>,
    reaped: ReapedEpoch,
    consumers: Vec<(PluginId, u64)>,
}
impl QuiescentEpoch {
    pub fn group(&self) -> &str {
        &self.group
    }
    pub fn identity(&self) -> &RuntimeIdentity {
        self.reaped.identity()
    }
    pub fn reaped(&self) -> &ReapedEpoch {
        &self.reaped
    }
    pub fn consumers(&self) -> &[(PluginId, u64)] {
        &self.consumers
    }
    pub fn is_for(&self, graph: &Arc<HostGraph>) -> bool {
        Weak::ptr_eq(&self.graph, &Arc::downgrade(graph))
    }
}
#[derive(Clone)]
pub struct RetainedSnapshot {
    pub identity: RuntimeIdentity,
    pub path: PathBuf,
    pub error: ProtocolError,
}
static RETAINED: OnceLock<Mutex<Vec<(RetainedSnapshot, SnapshotGroup)>>> = OnceLock::new();
/// Diagnostic only; there is no same-process override that releases unproven code.
pub fn retained_snapshots() -> Vec<RetainedSnapshot> {
    RETAINED
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .iter()
        .map(|(record, _)| record.clone())
        .collect()
}
struct SnapshotLease {
    group: Option<SnapshotGroup>,
    identity: RuntimeIdentity,
    error: ProtocolError,
}
impl Drop for SnapshotLease {
    fn drop(&mut self) {
        if let Some(group) = self.group.take() {
            let record = RetainedSnapshot {
                identity: self.identity.clone(),
                path: group.snapshot_root().into(),
                error: self.error.clone(),
            };
            RETAINED
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .push((record, group));
        }
    }
}
struct Control {
    graph: Arc<HostGraph>,
    group: String,
    identity: RuntimeIdentity,
    process: ProcessHandle,
    epoch: EpochReaping,
    closed: CancellationToken,
    native: OnceLock<EpochNativeCleanup>,
    native_result: Mutex<Option<Result<()>>>,
    os_result: Mutex<Option<Result<ReapedEpoch>>>,
    completion: watch::Receiver<Option<Result<QuiescentEpoch>>>,
    hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl Control {
    fn close(&self) {
        self.native
            .get_or_init(|| self.graph.close_epoch(&self.identity));
        self.closed.cancel();
    }
    fn status(&self) -> RecoveryStatus {
        let reaped = self.os_result.lock().unwrap().clone();
        let native_result = self.native_result.lock().unwrap().clone();
        let completion = self.completion.borrow().clone();
        let consumers = self
            .native
            .get()
            .map(|native| native.consumers())
            .unwrap_or_default();
        let members = self
            .native
            .get()
            .map(|native| native.members())
            .unwrap_or_default()
            .into_iter()
            .map(|member| {
                let (id, name, state, cleanup, error) = match member.mounted {
                    Some(Ok(native)) => (
                        Some(native.id()),
                        Some(native.name().into()),
                        Some(native.state()),
                        native.result(),
                        None,
                    ),
                    Some(Err(error)) => (None, None, None, None, Some(error)),
                    None => (None, None, None, None, None),
                };
                NativeMemberStatus {
                    instance: member.instance,
                    identity: member.identity,
                    id,
                    name,
                    state,
                    cleanup,
                    paired: member.paired,
                    error,
                }
            })
            .collect::<Vec<_>>();
        let phase = if reaped.as_ref().is_some_and(Result::is_err) {
            RecoveryPhase::Quarantined
        } else if completion.as_ref().is_some_and(Result::is_ok) {
            RecoveryPhase::Quiescent
        } else if completion.as_ref().is_some_and(Result::is_err)
            || native_result.as_ref().is_some_and(Result::is_err)
            || consumers
                .iter()
                .any(|consumer| consumer.result().is_some_and(|result| result.is_err()))
            || members.iter().any(|member| {
                member.error.is_some()
                    || member
                        .cleanup
                        .as_ref()
                        .is_some_and(|result| result.is_err())
            })
        {
            RecoveryPhase::RecoveryBlocked
        } else if self.closed.is_cancelled() {
            RecoveryPhase::Closing
        } else {
            RecoveryPhase::Running
        };
        RecoveryStatus {
            identity: self.identity.clone(),
            phase,
            process: self.process.status(),
            reaped,
            native: native_result,
            members,
            consumers,
            executions: self.graph.objects().host().epoch_executions(&self.identity),
            error: completion.and_then(|result| result.err()),
        }
    }
}
struct Lease(Arc<Control>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.process.terminate();
        self.0.close();
    }
}
#[derive(Clone)]
pub struct EpochSupervisor(Arc<Lease>);
/// A waiter is not an owning supervisor lease. Last supervisor Drop closes the
/// epoch even if these observation handles remain alive.
#[derive(Clone)]
pub struct EpochCleanup(Arc<Control>);
pub enum RecoveryWait {
    Confirmed(QuiescentEpoch),
    RecoveryBlocked(RecoveryStatus),
    Quarantined(RecoveryStatus),
}
impl EpochSupervisor {
    /// Trusted Host operation using this launch's actual private peer or Host
    /// transport wrapper. No protocol frame can nominate a process or snapshot.
    pub fn attach(
        graph: Arc<HostGraph>,
        group: &str,
        frozen: SnapshotGroup,
        process: ProcessHandle,
        identity: RuntimeIdentity,
        peer: &Peer,
    ) -> Result<Self> {
        let prepared = graph
            .objects()
            .plan()
            .groups()
            .get(group)
            .ok_or_else(|| fail("unknown frozen supervisor group"))?;
        if !process.uses_snapshot(&frozen)
            || frozen.code_sha256() != prepared.code_sha256()
            || frozen.environment_sha256() != prepared.environment_sha256()
            || frozen
                .members()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                != prepared.members().iter().cloned().collect()
        {
            return Err(fail(
                "supervisor snapshot differs from actual frozen launch",
            ));
        }
        let snapshot = SnapshotLease {
            group: Some(frozen),
            identity: identity.clone(),
            error: fail("epoch cleanup unconfirmed"),
        };
        graph.supervise_epoch(&identity, group)?;
        let epoch = process.attach_epoch(&graph.objects().host(), identity.clone(), peer)?;
        let (completed, completion) = watch::channel(None);
        let control = Arc::new(Control {
            graph,
            group: group.into(),
            identity,
            process,
            epoch,
            closed: CancellationToken::new(),
            native: OnceLock::new(),
            native_result: Mutex::new(None),
            os_result: Mutex::new(None),
            completion,
            hook: Mutex::new(None),
        });
        let weak = Arc::downgrade(&control);
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(control) = weak.upgrade() {
                control.close();
            }
        });
        *control.hook.lock().unwrap() = Some(hook.clone());
        peer.on_close(&hook);
        let worker = control.clone();
        tokio::spawn(async move {
            let mut snapshot = snapshot;
            worker.closed.cancelled().await;
            let native = worker.native.get().unwrap().clone();
            let native_wait = async {
                let result = native.wait().await;
                *worker.native_result.lock().unwrap() = Some(result.clone());
                result
            };
            let os_wait = async {
                let result = worker.epoch.wait().await;
                *worker.os_result.lock().unwrap() = Some(result.clone());
                result
            };
            let (native, reaped) = tokio::join!(native_wait, os_wait);
            let result = match (native, reaped) {
                (_, Err(error)) | (Err(error), _) => Err(error),
                (Ok(()), Ok(reaped)) => {
                    worker
                        .graph
                        .objects()
                        .host()
                        .wait_epoch_idle(&worker.identity)
                        .await;
                    // Admission remains sealed; old executions cannot be added
                    // again between this check and receipt publication.
                    snapshot.group.take();
                    Ok(QuiescentEpoch {
                        group: worker.group.clone(),
                        graph: Arc::downgrade(&worker.graph),
                        reaped,
                        consumers: worker
                            .native
                            .get()
                            .unwrap()
                            .consumers()
                            .iter()
                            .map(|consumer| (consumer.id(), consumer.generation()))
                            .collect(),
                    })
                }
            };
            if let Err(error) = &result {
                snapshot.error = error.clone();
            }
            drop(snapshot);
            completed.send_replace(Some(result));
        });
        Ok(Self(Arc::new(Lease(control))))
    }
    pub fn terminate(&self) -> EpochCleanup {
        self.0 .0.process.terminate();
        self.0 .0.close();
        self.cleanup()
    }
    pub fn cleanup(&self) -> EpochCleanup {
        EpochCleanup(self.0 .0.clone())
    }
    pub fn reaping(&self) -> EpochReaping {
        self.0 .0.epoch.clone()
    }
    pub fn status(&self) -> RecoveryStatus {
        self.0 .0.status()
    }
}
impl EpochCleanup {
    pub fn status(&self) -> RecoveryStatus {
        self.0.status()
    }
    pub async fn wait(&self) -> Result<QuiescentEpoch> {
        let mut completion = self.0.completion.clone();
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            completion
                .changed()
                .await
                .map_err(|_| fail("epoch supervision task lost"))?;
        }
    }
    /// A deadline ends only this management wait. It never cancels cleanup,
    /// releases a snapshot, or starts a replacement epoch on late completion.
    pub async fn wait_until(&self, deadline: tokio::time::Instant) -> RecoveryWait {
        if let Ok(Ok(receipt)) = tokio::time::timeout_at(deadline, self.wait()).await {
            return RecoveryWait::Confirmed(receipt);
        }
        let mut status = self.status();
        if status.phase == RecoveryPhase::Quarantined {
            RecoveryWait::Quarantined(status)
        } else {
            status.phase = RecoveryPhase::RecoveryBlocked;
            RecoveryWait::RecoveryBlocked(status)
        }
    }
}
