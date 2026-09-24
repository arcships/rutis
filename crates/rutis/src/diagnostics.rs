//! Read-only snapshots of fiber ownership and service resolution.
use std::sync::Arc;

use crate::{CordisError, FiberState, InstanceId, PluginId, ServiceReadFailure, TypeKey};

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
