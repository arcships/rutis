//! What a waiting row waits for, and why each is not there
//! ([`Loader::waits`]): what rutis-host's `waiting for …` lines say.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use rutis::host::diagnostics::{DependencyStatus, PluginDiagnostics};
use rutis::host::{CtxHost, FiberState, PluginId, TypeKey};

use super::desired::{Desired, Row};
use super::{CycleLink, EntryInfo, EntryStatus, Inner, Loader, Slot, State};
use crate::resolver::Resolved;

/// One service a waiting copy waits for ([`Loader::waits`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Wait {
    /// The service by name; a key that has none (a Rust type), described.
    pub service: String,
    pub reason: WaitReason,
}

/// Why a service a copy waits for is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WaitReason {
    /// It is the session of the copy's own runtime, `service` the id of
    /// its row (`py`, a connection row): the runtime has not released its
    /// rows (it is starting, down, or not connected).
    OwnRuntime,
    /// No row declares it. `like` is a service name close to it that a
    /// row declares or something provides (a misspelt name).
    NoProvider { like: Option<String> },
    /// No enabled row declares it, but rows that are disabled (themselves
    /// or by a group) were never resolved, so what they provide is not
    /// known: one of them may (`Loader::check` resolves them). `like` as
    /// for [`WaitReason::NoProvider`].
    NoEnabledProvider { like: Option<String> },
    /// rutis knows why, in a sentence (a Rust service of that name whose
    /// type is not marked `#[rutis::service]`).
    Explained(String),
    /// The row that provides it is disabled.
    ProviderDisabled { row: String },
    /// The row that provides it cannot run: it failed, or does not resolve.
    ProviderFailed { row: String, error: String },
    /// The row that provides it does not run: a group around it is not
    /// running, or it stopped in its instance.
    ProviderInactive { row: String },
    /// The row that provides it waits for its own runtime, `runtime`.
    ProviderRuntime { row: String, runtime: String },
    /// The row that provides it waits for `service`, which the waiting
    /// copy's row provides: they inject each other.
    Cycle { row: String, service: String },
    /// Three or more rows wait for each other's services in a cycle,
    /// starting from the waiting row and returning to it: none of them
    /// starts. Two-row cycles remain [`WaitReason::Cycle`].
    CycleChain { links: Vec<CycleLink> },
    /// Waiting directly on an undeclared service, or through a chain of
    /// waiting providers (`chain`) on a missing service `missing`, while
    /// one or more unstarted rows in the same scope (`candidates`) may
    /// provide services only once they run (`announces: true`, or
    /// declarations unavailable).
    UnknownProvider {
        chain: Vec<CycleLink>,
        missing: String,
        candidates: Vec<String>,
    },
    /// The row that provides it waits too, for `services`.
    ProviderWaiting { row: String, services: Vec<String> },
    /// The row that provides it is starting.
    ProviderStarting { row: String },
    /// The row that provides it runs, but has not provided it (or not
    /// where this copy looks).
    ProviderRunning { row: String },
    /// Anything else, as rutis says it (being removed, out of scope).
    Other(String),
}

impl Wait {
    pub fn new(service: impl Into<String>, reason: WaitReason) -> Self {
        Self {
            service: service.into(),
            reason,
        }
    }
}

/// A row as [`Loader::waits`] sees it: its status, its fiber, what it
/// declares it provides, whether it may announce undeclared services at
/// runtime, and its effective per-service `isolate` labels.
struct Fact {
    status: EntryStatus,
    plugin: Option<PluginId>,
    provides: Vec<String>,
    /// May provide services beyond `provides` once it runs.
    announces: bool,
    /// Wanted to run, but its declarations are not known yet (unresolved,
    /// `declares: None`, or declaration callback failed).
    unknown_decl: bool,
    /// Disabled (itself or by a group), and what it provides not known: it
    /// was never resolved.
    unknown_off: bool,
    /// Effective `(service, label)` isolates from this row and its
    /// enclosing groups (innermost label wins).
    isolates: Vec<(String, String)>,
}

impl Fact {
    fn isolate_of(&self, service: &str) -> Option<&str> {
        self.isolates
            .iter()
            .find(|(name, _)| name == service)
            .map(|(_, label)| label.as_str())
    }

    fn is_pending(&self) -> bool {
        matches!(
            &self.status,
            EntryStatus::Running(snapshot) if snapshot.state == FiberState::Pending
        )
    }

    fn is_live(&self) -> bool {
        matches!(
            &self.status,
            EntryStatus::Running(snapshot)
                if matches!(
                    snapshot.state,
                    FiberState::Active | FiberState::Loading | FiberState::Pending
                )
        )
    }

    fn is_unstarted_unknown(&self) -> bool {
        (self.announces || self.unknown_decl)
            && matches!(
                &self.status,
                EntryStatus::Unresolved(_)
                    | EntryStatus::Running(rutis::host::Snapshot {
                        state: FiberState::Pending | FiberState::Loading,
                        ..
                    })
            )
    }
}

/// One non-ready dependency of a Pending row, projected out of kernel
/// diagnostics into plain names so [`WaitGraph`] operates purely on snapshot
/// data.
#[derive(Debug, Clone)]
enum MissingDep {
    /// The row's own runtime session (`service` is its name, `runtime` the
    /// key name shown in [`WaitReason::ProviderRuntime`]).
    OwnRuntime { service: String, runtime: String },
    /// No binding exists for `service`.
    Missing(String),
    /// An inactive binding exists for `service`, owned by `provider` row
    /// when known; `fallback` is the kernel's `DependencyStatus` string.
    InactiveBinding {
        service: String,
        provider: Option<String>,
        fallback: String,
    },
    /// Any other non-Ready kernel status.
    Other { service: String, text: String },
}

impl MissingDep {
    fn service(&self) -> &str {
        match self {
            MissingDep::OwnRuntime { service, .. }
            | MissingDep::Missing(service)
            | MissingDep::InactiveBinding { service, .. }
            | MissingDep::Other { service, .. } => service,
        }
    }
}

impl Loader {
    /// What the copy `entry` (one of [`Loader::entries`]) waits for while
    /// it is `Pending`, and why each is missing: no row declares it (with
    /// a declared name close to it), or the row that does is disabled,
    /// failed, waits for its own runtime, waits for a service of this row
    /// (they inject each other, in a 2-row or 3+-row cycle), waits on a
    /// chain hitting an unannounced/unknown provider, and so on. Empty when
    /// it waits for nothing, or its fiber is gone. Read from a diagnostics
    /// snapshot: best effort. For several copies, [`Loader::waits_of`]
    /// reads it once.
    pub fn waits(&self, entry: &EntryInfo) -> Vec<Wait> {
        self.waits_of(&[entry]).pop().unwrap_or_default()
    }

    /// [`Loader::waits`] of each of `entries`, in order, from one snapshot
    /// of the rows and one of the diagnostics (a host following every
    /// waiting row reads them once per look).
    pub fn waits_of(&self, entries: &[&EntryInfo]) -> Vec<Vec<Wait>> {
        if entries.iter().all(|entry| entry.plugin.is_none()) {
            return vec![Vec::new(); entries.len()];
        }
        // Each entry's instance (`None` outside instances), and the rows as
        // each of those sees them.
        let (root, scopes, rows, records) = {
            let state = self.inner.state.lock().unwrap();
            let Some(root) = state.last_root.clone() else {
                return vec![Vec::new(); entries.len()];
            };
            let scopes: Vec<Option<u64>> = entries
                .iter()
                .map(|entry| {
                    entry.instance.as_ref().and_then(|instance| {
                        state
                            .instances
                            .iter()
                            .find(|(_, record)| record.plugin == instance.plugin)
                            .map(|(number, _)| *number)
                    })
                })
                .collect();
            let mut rows: HashMap<Option<u64>, Vec<ReadRow>> = HashMap::new();
            for scope in &scopes {
                rows.entry(*scope)
                    .or_insert_with(|| read_rows(&state, *scope));
            }
            let records: HashMap<PluginId, String> = state
                .running
                .iter()
                .map(|(slot, running)| (running.view.id, slot.row.clone()))
                .collect();
            (root, scopes, rows, records)
        };
        let facts: HashMap<Option<u64>, Vec<(String, Fact)>> = rows
            .into_iter()
            .map(|(scope, rows)| (scope, facts(rows)))
            .collect();
        let diagnostics = root.diagnostics();
        let fibers: HashMap<PluginId, &PluginDiagnostics> = diagnostics
            .plugins
            .iter()
            .map(|fiber| (fiber.id, fiber))
            .collect();
        let deps_of = |plugin: PluginId| -> Vec<MissingDep> {
            fibers
                .get(&plugin)
                .map(|fiber| {
                    fiber
                        .injects
                        .iter()
                        .filter(|inject| inject.status != DependencyStatus::Ready)
                        .map(|inject| {
                            let service = self
                                .service_name(&inject.key)
                                .unwrap_or_else(|| inject.key.describe());
                            if own_runtime(&inject.key) {
                                return MissingDep::OwnRuntime {
                                    service,
                                    runtime: inject.key.name().unwrap_or_default().to_owned(),
                                };
                            }
                            match inject.status {
                                DependencyStatus::Missing => MissingDep::Missing(service),
                                DependencyStatus::ProviderInactive(_) => {
                                    let provider = diagnostics
                                        .bindings
                                        .iter()
                                        .find(|binding| binding.key == inject.key)
                                        .and_then(|binding| {
                                            row_of(binding.provider, &records, &fibers)
                                        });
                                    MissingDep::InactiveBinding {
                                        service,
                                        provider,
                                        fallback: inject.status.to_string(),
                                    }
                                }
                                other => MissingDep::Other {
                                    service,
                                    text: other.to_string(),
                                },
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let binding_names: Vec<(Option<String>, String)> = diagnostics
            .bindings
            .iter()
            .filter_map(|binding| {
                let name = self.service_name(&binding.key)?;
                let provider = row_of(binding.provider, &records, &fibers);
                Some((provider, name))
            })
            .collect();
        let catalog_names: Vec<String> = self.inner.catalog.names().collect();
        let deps_by_scope: HashMap<Option<u64>, HashMap<String, Vec<MissingDep>>> = facts
            .iter()
            .map(|(&scope, scope_facts)| {
                let map = scope_facts
                    .iter()
                    .filter_map(|(id, fact)| fact.plugin.map(|p| (id.clone(), deps_of(p))))
                    .collect();
                (scope, map)
            })
            .collect();
        let explain_unexported = |service: &str| unexported(&root, service);

        entries
            .iter()
            .zip(scopes)
            .map(|(entry, scope)| {
                let Some(plugin) = entry.plugin else {
                    return Vec::new();
                };
                let scope_facts = &facts[&scope];
                let scope_deps = &deps_by_scope[&scope];
                let graph = WaitGraph {
                    facts: scope_facts,
                    deps: scope_deps,
                    binding_names: &binding_names,
                    catalog_names: &catalog_names,
                    unexported: &explain_unexported,
                };
                let entry_deps = scope_deps
                    .get(&entry.id)
                    .cloned()
                    .unwrap_or_else(|| deps_of(plugin));
                graph.waits_for_row(&entry.id, &entry_deps)
            })
            .collect()
    }

    /// The name a key is known by: the catalog's name for it, or its own.
    fn service_name(&self, key: &TypeKey) -> Option<String> {
        self.inner
            .catalog
            .name_of(key)
            .or_else(|| key.name().map(str::to_owned))
    }
}

/// Pure snapshot view of rows and their missing dependencies in one instance
/// scope, used to diagnose cycles and unknown-provider chains in
/// [`Loader::waits_of`].
struct WaitGraph<'a> {
    facts: &'a [(String, Fact)],
    deps: &'a HashMap<String, Vec<MissingDep>>,
    binding_names: &'a [(Option<String>, String)],
    catalog_names: &'a [String],
    unexported: &'a dyn Fn(&str) -> Option<String>,
}

impl<'a> WaitGraph<'a> {
    fn waits_for_row(&self, row_id: &str, row_deps: &[MissingDep]) -> Vec<Wait> {
        let entry_fact = self.fact_of(row_id);
        row_deps
            .iter()
            .map(|dep| match dep {
                MissingDep::OwnRuntime { service, .. } => Wait {
                    service: service.clone(),
                    reason: WaitReason::OwnRuntime,
                },
                MissingDep::Missing(service) => {
                    let reason = if let Some(why) = (self.unexported)(service) {
                        WaitReason::Explained(why)
                    } else if let Some((row, fact)) =
                        self.declared_provider(row_id, entry_fact, service)
                    {
                        self.provider(row_id, service, row, fact, false)
                    } else {
                        let like = self.closest_for(row_id, service);
                        if like.is_none() {
                            let candidates = self.unstarted_candidates(row_id, entry_fact, service);
                            if !candidates.is_empty() {
                                return Wait {
                                    service: service.clone(),
                                    reason: WaitReason::UnknownProvider {
                                        chain: Vec::new(),
                                        missing: service.clone(),
                                        candidates,
                                    },
                                };
                            }
                        }
                        // Disabled rows never resolved may provide it: not "no row".
                        if self
                            .facts
                            .iter()
                            .any(|(id, fact)| id != row_id && fact.unknown_off)
                        {
                            WaitReason::NoEnabledProvider { like }
                        } else {
                            WaitReason::NoProvider { like }
                        }
                    };
                    Wait {
                        service: service.clone(),
                        reason,
                    }
                }
                MissingDep::InactiveBinding {
                    service,
                    provider,
                    fallback,
                } => {
                    let reason = match provider
                        .as_deref()
                        .and_then(|row| self.fact_of(row).map(|fact| (row, fact)))
                    {
                        Some((row, fact)) if row != row_id => {
                            self.provider(row_id, service, row, fact, true)
                        }
                        _ => WaitReason::Other(fallback.clone()),
                    };
                    Wait {
                        service: service.clone(),
                        reason,
                    }
                }
                MissingDep::Other { service, text } => Wait {
                    service: service.clone(),
                    reason: WaitReason::Other(text.clone()),
                },
            })
            .collect()
    }

    /// Closest known service name provided or declared by something other
    /// than `waiter` itself (so a row is never suggested its own service).
    fn closest_for(&self, waiter: &str, service: &str) -> Option<String> {
        let mut known: Vec<String> = self
            .facts
            .iter()
            .filter(|(id, _)| id != waiter)
            .flat_map(|(_, fact)| fact.provides.iter().cloned())
            .collect();
        known.extend(self.binding_names.iter().filter_map(|(provider, name)| {
            if provider.as_deref() == Some(waiter) {
                return None;
            }
            Some(name.clone())
        }));
        let waiter_provides = self
            .fact_of(waiter)
            .map(|fact| fact.provides.as_slice())
            .unwrap_or_default();
        known.extend(
            self.catalog_names
                .iter()
                .filter(|name| {
                    !waiter_provides.iter().any(|p| p == *name)
                        && !self
                            .binding_names
                            .iter()
                            .any(|(provider, b)| provider.as_deref() == Some(waiter) && b == *name)
                })
                .cloned(),
        );
        known.sort_unstable();
        known.dedup();
        closest(service, &known)
    }

    fn fact_of(&self, id: &str) -> Option<&'a Fact> {
        self.facts
            .iter()
            .find(|(row, _)| row == id)
            .map(|(_, fact)| fact)
    }

    fn deps_of(&self, id: &str) -> &'a [MissingDep] {
        self.deps.get(id).map(Vec::as_slice).unwrap_or_default()
    }

    fn same_isolate(a: Option<&Fact>, b: &Fact, service: &str) -> bool {
        let a_iso = a.and_then(|fact| fact.isolate_of(service));
        a_iso == b.isolate_of(service)
    }

    /// Best matching declared provider of `service` visible to `waiter`
    /// (`!= waiter`, same isolate scope). Prefers `Active`, then `Loading`,
    /// then `Pending`, then non-live rows when several declare it.
    fn declared_provider(
        &self,
        waiter: &str,
        waiter_fact: Option<&Fact>,
        service: &str,
    ) -> Option<(&'a str, &'a Fact)> {
        let rank = |fact: &Fact| match &fact.status {
            EntryStatus::Running(snapshot) => match snapshot.state {
                FiberState::Active => 0u8,
                FiberState::Loading => 1,
                FiberState::Pending => 2,
                _ => 3,
            },
            _ => 3,
        };
        let mut best: Option<(u8, &'a str, &'a Fact)> = None;
        for (id, fact) in self.facts {
            if id == waiter
                || !fact.provides.iter().any(|p| p == service)
                || !Self::same_isolate(waiter_fact, fact, service)
            {
                continue;
            }
            let r = rank(fact);
            if best.is_none_or(|(best_r, _, _)| r < best_r) {
                best = Some((r, id.as_str(), fact));
                if r == 0 {
                    break;
                }
            }
        }
        best.map(|(_, id, fact)| (id, fact))
    }

    /// Whether any other row (`!= waiter`) in `waiter`'s isolate scope
    /// declares `service`.
    fn has_declared_provider(
        &self,
        waiter: &str,
        waiter_fact: Option<&Fact>,
        service: &str,
    ) -> bool {
        self.declared_provider(waiter, waiter_fact, service)
            .is_some()
    }

    /// The sole live (`Active`/`Loading`/`Pending`) row (`!= waiter`) in
    /// `waiter`'s isolate scope that declares `service`; `None` when none or
    /// more than one live row does (with an alternative live provider the
    /// waiter is not guaranteed deadlocked).
    fn sole_live_provider(
        &self,
        waiter: &str,
        waiter_fact: Option<&Fact>,
        service: &str,
    ) -> Option<(&'a str, &'a Fact)> {
        let mut found = None;
        for (id, fact) in self.facts {
            if id == waiter
                || !fact.is_live()
                || !fact.provides.iter().any(|p| p == service)
                || !Self::same_isolate(waiter_fact, fact, service)
            {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some((id.as_str(), fact));
        }
        found
    }

    /// Outgoing missing non-runtime dependencies of `row`, sorted by
    /// service name for deterministic traversal; empty if `row` waits on
    /// its own runtime.
    fn service_waits(&self, row: &str) -> Vec<&'a MissingDep> {
        let deps = self.deps_of(row);
        if deps
            .iter()
            .any(|dep| matches!(dep, MissingDep::OwnRuntime { .. }))
        {
            return Vec::new();
        }
        let mut out: Vec<&'a MissingDep> = deps.iter().collect();
        out.sort_by(|a, b| a.service().cmp(b.service()));
        out
    }

    /// Resolve a single missing dependency of `curr` to a sole live
    /// provider edge `curr --service--> next`, if one exists.
    fn edge_target(
        &self,
        curr: &str,
        curr_fact: &Fact,
        dep: &MissingDep,
    ) -> Option<(&'a str, &'a Fact)> {
        match dep {
            MissingDep::Missing(service) => {
                if (self.unexported)(service).is_some() {
                    return None;
                }
                self.sole_live_provider(curr, Some(curr_fact), service)
            }
            MissingDep::InactiveBinding { provider, .. } => provider
                .as_deref()
                .filter(|row| *row != curr)
                .and_then(|row| {
                    self.facts
                        .iter()
                        .find(|(id, _)| id == row)
                        .map(|(id, fact)| (id.as_str(), fact))
                }),
            MissingDep::OwnRuntime { .. } | MissingDep::Other { .. } => None,
        }
    }

    /// Why the row `row`, which declares `first_service` (or holds its
    /// inactive binding when `from_binding` is true), does not provide it
    /// to `start`.
    fn provider(
        &self,
        start: &str,
        first_service: &str,
        row: &str,
        fact: &Fact,
        from_binding: bool,
    ) -> WaitReason {
        let row_owned = row.to_owned();
        match &fact.status {
            EntryStatus::Disabled => WaitReason::ProviderDisabled { row: row_owned },
            EntryStatus::Unresolved(error) => WaitReason::ProviderFailed {
                row: row_owned,
                error: error.to_string(),
            },
            EntryStatus::Running(snapshot) => match snapshot.state {
                FiberState::Failed => WaitReason::ProviderFailed {
                    row: row_owned,
                    error: snapshot
                        .error
                        .as_ref()
                        .map_or_else(|| "failed".to_owned(), ToString::to_string),
                },
                FiberState::Pending => {
                    let waits = self.deps_of(row);
                    if let Some(runtime) = waits.iter().find_map(|dep| match dep {
                        MissingDep::OwnRuntime { runtime, .. } => Some(runtime),
                        _ => None,
                    }) {
                        return WaitReason::ProviderRuntime {
                            row: row_owned,
                            runtime: runtime.clone(),
                        };
                    }
                    let services: Vec<String> =
                        waits.iter().map(|dep| dep.service().to_owned()).collect();
                    // Only traverse for cycles / unknown-provider chains
                    // when `row` is the sole live provider of `first_service`
                    // (or holds its inactive binding), so an alternative
                    // starting provider never triggers a false cycle.
                    let start_fact = self.fact_of(start);
                    let sole_first = from_binding
                        || self
                            .sole_live_provider(start, start_fact, first_service)
                            .is_some_and(|(id, _)| id == row);
                    if sole_first {
                        if let Some(links) = self.find_cycle(start, first_service, row) {
                            return match links.as_slice() {
                                [_, second] => WaitReason::Cycle {
                                    row: second.row.clone(),
                                    service: second.service.clone(),
                                },
                                _ => WaitReason::CycleChain { links },
                            };
                        }
                        if let Some((chain, missing, candidates)) =
                            self.find_unknown_provider_chain(start, first_service, row)
                        {
                            return WaitReason::UnknownProvider {
                                chain,
                                missing,
                                candidates,
                            };
                        }
                    }
                    WaitReason::ProviderWaiting {
                        row: row_owned,
                        services,
                    }
                }
                FiberState::Loading => WaitReason::ProviderStarting { row: row_owned },
                FiberState::Active => WaitReason::ProviderRunning { row: row_owned },
                _ => WaitReason::ProviderInactive { row: row_owned },
            },
            _ => WaitReason::ProviderInactive { row: row_owned },
        }
    }

    /// Shortest simple cycle starting with `start --first_service--> first_provider`
    /// and returning to `start` along sole-live-provider Pending edges.
    fn find_cycle(
        &self,
        start: &str,
        first_service: &str,
        first_provider: &str,
    ) -> Option<Vec<CycleLink>> {
        let mut parent: HashMap<&str, (&str, String)> = HashMap::new();
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();

        // Find the &'a str slice for `first_provider` in `self.facts` so
        // lifetimes stay uniform with `self.facts`.
        let first_id = self
            .facts
            .iter()
            .find(|(id, _)| id == first_provider)
            .map(|(id, _)| id.as_str())?;
        let start_id = self
            .facts
            .iter()
            .find(|(id, _)| id == start)
            .map(|(id, _)| id.as_str())?;

        parent.insert(first_id, (start_id, first_service.to_owned()));
        visited.insert(first_id);
        queue.push_back(first_id);

        while let Some(curr) = queue.pop_front() {
            let Some(curr_fact) = self.fact_of(curr) else {
                continue;
            };
            for dep in self.service_waits(curr) {
                let Some((next, next_fact)) = self.edge_target(curr, curr_fact, dep) else {
                    continue;
                };
                if next == start_id {
                    let mut links = vec![CycleLink::new(curr, dep.service(), start_id)];
                    let mut cursor = curr;
                    while let Some((prev, svc)) = parent.get(cursor) {
                        links.push(CycleLink::new(*prev, svc.clone(), cursor));
                        if *prev == start_id {
                            break;
                        }
                        cursor = *prev;
                    }
                    links.reverse();
                    return Some(links);
                }
                if next_fact.is_pending() && visited.insert(next) {
                    parent.insert(next, (curr, dep.service().to_owned()));
                    queue.push_back(next);
                }
            }
        }
        None
    }

    /// Shortest Pending wait chain starting with `start --first_service--> first_provider`
    /// that hits a missing undeclared service (with no close spelling match)
    /// while unstarted candidate rows exist in the same scope.
    fn find_unknown_provider_chain(
        &self,
        start: &str,
        first_service: &str,
        first_provider: &str,
    ) -> Option<(Vec<CycleLink>, String, Vec<String>)> {
        let first_id = self
            .facts
            .iter()
            .find(|(id, _)| id == first_provider)
            .map(|(id, _)| id.as_str())?;
        let start_id = self
            .facts
            .iter()
            .find(|(id, _)| id == start)
            .map(|(id, _)| id.as_str())?;

        let mut parent: HashMap<&str, (&str, String)> = HashMap::new();
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();

        parent.insert(first_id, (start_id, first_service.to_owned()));
        visited.insert(start_id);
        visited.insert(first_id);
        queue.push_back(first_id);

        while let Some(curr) = queue.pop_front() {
            let Some(curr_fact) = self.fact_of(curr) else {
                continue;
            };
            for dep in self.service_waits(curr) {
                if let MissingDep::Missing(service) = dep {
                    if (self.unexported)(service).is_none()
                        && !self.has_declared_provider(curr, Some(curr_fact), service)
                    {
                        if self.closest_for(curr, service).is_none() {
                            let candidates =
                                self.unstarted_candidates(curr, Some(curr_fact), service);
                            if !candidates.is_empty() {
                                let mut chain = Vec::new();
                                let mut cursor = curr;
                                while let Some((prev, svc)) = parent.get(cursor) {
                                    chain.push(CycleLink::new(*prev, svc.clone(), cursor));
                                    if *prev == start_id {
                                        break;
                                    }
                                    cursor = *prev;
                                }
                                chain.reverse();
                                return Some((chain, service.clone(), candidates));
                            }
                        }
                        continue;
                    }
                }
                if let Some((next, next_fact)) = self.edge_target(curr, curr_fact, dep) {
                    if next_fact.is_pending() && visited.insert(next) {
                        parent.insert(next, (curr, dep.service().to_owned()));
                        queue.push_back(next);
                    }
                }
            }
        }
        None
    }

    /// Unstarted rows (`Pending`, `Loading`, or `Unresolved`) in the same
    /// isolate scope as `terminal` for `missing` whose provided services
    /// are not fully declared (`announces` or `unknown_decl`), excluding
    /// `terminal` itself. Candidates that can reach `terminal` via Pending
    /// wait edges come first (shortest distance, then tree order), followed
    /// by remaining candidates in tree order.
    fn unstarted_candidates(
        &self,
        terminal: &str,
        terminal_fact: Option<&Fact>,
        missing: &str,
    ) -> Vec<String> {
        let eligible: Vec<(usize, &str)> = self
            .facts
            .iter()
            .enumerate()
            .filter(|(_, (id, fact))| {
                id != terminal
                    && Self::same_isolate(terminal_fact, fact, missing)
                    && fact.is_unstarted_unknown()
            })
            .map(|(index, (id, _))| (index, id.as_str()))
            .collect();
        if eligible.len() <= 1 {
            return eligible.into_iter().map(|(_, id)| id.to_owned()).collect();
        }
        // Build reverse adjacency over Pending sole-live-provider edges so
        // candidates waiting (directly or transitively) on `terminal` rank
        // first as potential dynamic-cycle participants.
        let mut reverse: HashMap<&str, Vec<&str>> = HashMap::new();
        for (id, fact) in self.facts {
            if !fact.is_pending() {
                continue;
            }
            for dep in self.service_waits(id) {
                if let Some((target, _)) = self.edge_target(id, fact, dep) {
                    reverse.entry(target).or_default().push(id.as_str());
                }
            }
        }
        let mut dist: HashMap<&str, usize> = HashMap::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        dist.insert(terminal, 0);
        queue.push_back(terminal);
        while let Some(node) = queue.pop_front() {
            let next_dist = dist[node] + 1;
            for &prev in reverse.get(node).into_iter().flatten() {
                if !dist.contains_key(prev) {
                    dist.insert(prev, next_dist);
                    queue.push_back(prev);
                }
            }
        }
        let mut ranked: Vec<(Option<usize>, usize, &str)> = eligible
            .into_iter()
            .map(|(tree_index, id)| (dist.get(id).copied(), tree_index, id))
            .collect();
        ranked.sort_by(|a, b| match (a.0, b.0) {
            (Some(da), Some(db)) => da.cmp(&db).then(a.1.cmp(&b.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.cmp(&b.1),
        });
        ranked.into_iter().map(|(_, _, id)| id.to_owned()).collect()
    }
}

/// Effective `(service, label)` isolates for `row`, walking up enclosing
/// groups (innermost label wins for each service name).
fn effective_isolates(desired: &Desired, row: &Row) -> Vec<(String, String)> {
    let mut isolates: Vec<(String, String)> = Vec::new();
    let mut current = Some(row);
    while let Some(r) = current {
        for (service, label) in &r.raw_scope.isolate {
            if !isolates.iter().any(|(name, _)| name == service) {
                isolates.push((service.clone(), label.clone()));
            }
        }
        current = r.parent.as_deref().and_then(|id| desired.row(id));
    }
    isolates
}

/// A row as read under the loader's lock: its id, status and fiber, its
/// resolution and raw config, whose declarations are asked after, whether
/// it is enabled (itself and the groups around it), and its effective
/// isolate labels.
struct ReadRow {
    id: String,
    status: EntryStatus,
    plugin: Option<PluginId>,
    resolved: Option<Arc<Resolved>>,
    config: serde_json::Value,
    wanted: bool,
    isolates: Vec<(String, String)>,
}

/// Each plugin row (for a row inside instances, its copy in instance
/// `scope`).
fn read_rows(state: &State, scope: Option<u64>) -> Vec<ReadRow> {
    state
        .desired
        .rows
        .iter()
        .filter(|row| !row.group)
        .map(|row| {
            let copy = state.desired.instanced_group(row).and(scope);
            let info = Inner::info(state, row, copy);
            let running = state.running.get(&Slot {
                row: row.id.clone(),
                scope: copy,
            });
            let resolved = running
                .and_then(|running| running.resolved.clone())
                .or_else(|| {
                    row.name
                        .as_ref()
                        .and_then(|name| state.resolved.get(name))
                        .and_then(|resolved| resolved.as_ref().ok().cloned())
                });
            ReadRow {
                id: row.id.clone(),
                status: info.status,
                plugin: info.plugin,
                resolved,
                config: row.config.clone(),
                wanted: state.desired.wanted(row),
                isolates: effective_isolates(&state.desired, row),
            }
        })
        .collect()
}

/// Each row by id, in tree order, with what it declares it provides: asked
/// of its plugin outside the loader's lock.
fn facts(rows: Vec<ReadRow>) -> Vec<(String, Fact)> {
    rows.into_iter()
        .map(
            |ReadRow {
                 id,
                 status,
                 plugin,
                 resolved,
                 config,
                 wanted,
                 isolates,
             }| {
                let declared = resolved
                    .and_then(|resolved| {
                        resolved.declares.as_ref().map(|declares| declares(&config))
                    })
                    .and_then(Result::ok);
                let (provides, announces, unknown_decl, unknown_off) = match declared {
                    Some(declared) => (declared.provides, declared.announces, false, false),
                    None => (Vec::new(), false, wanted, !wanted),
                };
                let fact = Fact {
                    status,
                    plugin,
                    provides,
                    announces,
                    unknown_decl,
                    unknown_off,
                    isolates,
                };
                (id, fact)
            },
        )
        .collect()
}

/// The row whose fiber is `plugin` or an ancestor of it.
pub(super) fn row_of(
    plugin: PluginId,
    records: &HashMap<PluginId, String>,
    fibers: &HashMap<PluginId, &PluginDiagnostics>,
) -> Option<String> {
    let mut current = Some(plugin);
    while let Some(id) = current {
        if let Some(row) = records.get(&id) {
            return Some(row.clone());
        }
        current = fibers.get(&id).and_then(|fiber| fiber.parent);
    }
    None
}

/// How messages name `key`, when the loader knows it: a row's runtime by
/// the id of its row, a service by its catalog name or the name it crosses
/// between languages under; `None` for other keys (Rust types).
pub(crate) fn key_name(catalog: &crate::ServiceCatalog, key: &TypeKey) -> Option<String> {
    if own_runtime(key) {
        return key.name().map(str::to_owned);
    }
    if let Some(name) = catalog.name_of(key) {
        return Some(name);
    }
    #[cfg(feature = "runtimes")]
    if key.type_id() == std::any::TypeId::of::<dyn rutis_bridge::session::HostDispatch>() {
        return key.name().map(str::to_owned);
    }
    None
}

/// Whether `key` is the session of a row's own runtime
/// ([`SessionRows`](crate::runtime::SessionRows)), named by its row's id.
pub(super) fn own_runtime(key: &TypeKey) -> bool {
    #[cfg(feature = "runtimes")]
    return key.type_id() == std::any::TypeId::of::<crate::runtime::SessionRows>();
    #[cfg(not(feature = "runtimes"))]
    {
        let _ = key;
        false
    }
}

/// Why `name` is missing, when rutis knows more (see
/// `rutis_bridge::exports::unexported`).
fn unexported(root: &rutis::Ctx, name: &str) -> Option<String> {
    #[cfg(feature = "runtimes")]
    return rutis_bridge::exports::unexported(root, name);
    #[cfg(not(feature = "runtimes"))]
    {
        let _ = (root, name);
        None
    }
}

/// The name of `known` closest to `name` (the rule shared with config
/// keys, config schema design §6.1).
pub(crate) use rutis::config::closest;
#[cfg(test)]
use rutis::config::distance;

#[cfg(test)]
#[path = "waits_prop_tests.rs"]
mod prop_tests;

#[cfg(test)]
mod tests {
    use super::{closest, distance};

    #[test]
    fn a_misspelt_name_finds_the_one_it_meant() {
        let known = ["llm", "weather", "calendar"].map(str::to_owned);
        assert_eq!(closest("lmm", &known).as_deref(), Some("llm"));
        assert_eq!(closest("waether", &known).as_deref(), Some("weather"));
        assert_eq!(closest("calender", &known).as_deref(), Some("calendar"));
        assert_eq!(closest("store", &known), None);
        assert_eq!(closest("llm", &known), None);
        // Too short to guess.
        assert_eq!(closest("ll", &known), None);
        assert_eq!(distance("ab", "ba"), 1);
    }

    #[test]
    fn closest_for_excludes_waiter_own_catalog_service_unless_another_provides_it() {
        use std::collections::HashMap;

        use rutis::host::{FiberState, Snapshot};

        use super::{EntryStatus, Fact, WaitGraph};

        let pending = EntryStatus::Running(Snapshot {
            generation: 1,
            state: FiberState::Pending,
            error: None,
        });
        let make_fact = |provides: &[&str], announces: bool| Fact {
            status: pending.clone(),
            plugin: None,
            provides: provides.iter().map(|s| (*s).to_owned()).collect(),
            announces,
            unknown_decl: false,
            unknown_off: false,
            isolates: Vec::new(),
        };
        let no_unexported = |_: &str| None;
        let deps = HashMap::new();
        let catalog_names = vec!["asvc".to_owned(), "llm".to_owned()];

        // Waiter `a` declares `asvc` and no other row/binding provides it:
        // catalog entry `asvc` must NOT be suggested back to `a` for `bsvc`,
        // while unrelated catalog service `llm` IS still suggested for `lmm`.
        let facts_only_self = vec![("a".to_owned(), make_fact(&["asvc"], false))];
        let graph = WaitGraph {
            facts: &facts_only_self,
            deps: &deps,
            binding_names: &[],
            catalog_names: &catalog_names,
            unexported: &no_unexported,
        };
        assert_eq!(graph.closest_for("a", "bsvc"), None);
        assert_eq!(graph.closest_for("a", "lmm").as_deref(), Some("llm"));

        // If another row declares `asvc`, it IS suggested.
        let facts_with_other = vec![
            ("a".to_owned(), make_fact(&["asvc"], false)),
            ("other".to_owned(), make_fact(&["asvc"], false)),
        ];
        let graph_with_other = WaitGraph {
            facts: &facts_with_other,
            deps: &deps,
            binding_names: &[],
            catalog_names: &catalog_names,
            unexported: &no_unexported,
        };
        assert_eq!(
            graph_with_other.closest_for("a", "bsvc").as_deref(),
            Some("asvc")
        );

        // If the host (provider == None) provides a binding for `asvc`, it IS suggested.
        let host_binding = vec![(None, "asvc".to_owned())];
        let graph_with_host = WaitGraph {
            facts: &facts_only_self,
            deps: &deps,
            binding_names: &host_binding,
            catalog_names: &catalog_names,
            unexported: &no_unexported,
        };
        assert_eq!(
            graph_with_host.closest_for("a", "bsvc").as_deref(),
            Some("asvc")
        );

        // If only `a` holds a binding for `asvc` (even without declaring it in `provides`),
        // `asvc` is still not suggested back to `a` via `catalog_names`.
        let facts_undeclared = vec![("a".to_owned(), make_fact(&[], true))];
        let self_binding = vec![(Some("a".to_owned()), "asvc".to_owned())];
        let graph_self_binding = WaitGraph {
            facts: &facts_undeclared,
            deps: &deps,
            binding_names: &self_binding,
            catalog_names: &catalog_names,
            unexported: &no_unexported,
        };
        assert_eq!(graph_self_binding.closest_for("a", "bsvc"), None);
    }

    /// guarantee: core/features.still-waiting
    /// Reports name a row's runtime and a service as rows do, and leave
    /// Rust types to be described.
    #[cfg(feature = "runtimes")]
    #[test]
    fn keys_are_named_as_rows_name_them() {
        use super::key_name;
        let mut catalog = crate::ServiceCatalog::new();
        catalog.register::<u32>("counter");
        let llm = rutis_bridge::session::host_key("llm").erase();
        assert_eq!(key_name(&catalog, &llm).as_deref(), Some("llm"));
        let counter = rutis::host::TypeKey::of::<u32>();
        assert_eq!(key_name(&catalog, &counter).as_deref(), Some("counter"));
        let gpu = crate::runtime::SessionRows::key("gpu").erase();
        assert_eq!(key_name(&catalog, &gpu).as_deref(), Some("gpu"));
        let other = rutis::host::TypeKey::of::<String>();
        assert_eq!(key_name(&catalog, &other), None);
    }
}
