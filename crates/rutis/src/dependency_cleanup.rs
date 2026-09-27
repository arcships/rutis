//! Optional observation of actual native consumer generation cleanup. Register
//! before publishing services; receivers preserve outcomes after dependency
//! edges and native fibers have disappeared. No observer drives cleanup.
use crate::{error::aggregate_arcs, fiber::FiberInner, registry::Binding, CordisError, PluginId};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

type Outcome = Result<(), Arc<CordisError>>;

/// Cleanup of one native generation. A receipt is created by native draining,
/// never by a supervisor declaring stop.
#[derive(Clone)]
pub struct ConsumerCleanup {
    id: PluginId,
    generation: u64,
    name: Arc<str>,
    outcome: watch::Receiver<Option<Outcome>>,
}
impl ConsumerCleanup {
    pub fn id(&self) -> PluginId {
        self.id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn result(&self) -> Option<Outcome> {
        self.outcome.borrow().clone()
    }
    pub async fn wait(&self) -> Outcome {
        let mut outcome = self.outcome.clone();
        loop {
            if let Some(result) = outcome.borrow().clone() {
                return result;
            }
            outcome.changed().await.map_err(|_| {
                Arc::new(CordisError::PluginFailed(
                    "native consumer cleanup confirmation lost".into(),
                ))
            })?;
        }
    }
}

/// Records external consumers of this provider subtree. Keep it through the
/// provider's actual native shutdown, then join its recorded receipts; an empty
/// snapshot taken before shutdown is not a cleanup or recovery confirmation.
#[derive(Clone)]
pub struct DependencyCleanup(pub(crate) Arc<Observer>);
impl DependencyCleanup {
    /// Wait for actual native cleanup of the observed provider itself. Only
    /// after this completes can a snapshot of its consumer drains be joined.
    pub async fn wait_provider(&self) -> Outcome {
        let mut provider = self.0.provider.subscribe();
        let cleanup = loop {
            if let Some(cleanup) = provider.borrow().clone() {
                break cleanup;
            }
            provider.changed().await.map_err(|_| {
                Arc::new(CordisError::PluginFailed(
                    "native provider cleanup confirmation lost".into(),
                ))
            })?;
        };
        cleanup.wait().await
    }
    pub fn consumers(&self) -> Vec<ConsumerCleanup> {
        self.0.consumers.lock().unwrap().values().cloned().collect()
    }
}
pub(crate) struct Observer {
    root: Weak<FiberInner>,
    root_generation: u64,
    root_token: CancellationToken,
    provider: watch::Sender<Option<ConsumerCleanup>>,
    providers: Mutex<HashSet<(PluginId, u64)>>,
    consumers: Mutex<BTreeMap<(PluginId, u64), ConsumerCleanup>>,
}
impl Observer {
    pub(crate) fn new(
        root: Weak<FiberInner>,
        root_generation: u64,
        root_token: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            root,
            root_generation,
            root_token,
            provider: watch::channel(None).0,
            providers: Mutex::default(),
            consumers: Mutex::default(),
        })
    }
    pub(crate) fn provider(&self, binding: &Binding) {
        if self.root_token.is_cancelled() {
            return;
        }
        if let (Some(root), Some(provider)) = (self.root.upgrade(), binding.provider.upgrade()) {
            if provider.ctx.is_within(&root.ctx) {
                self.providers
                    .lock()
                    .unwrap()
                    .insert((binding.provider_id, binding.provider_gen));
            }
        }
    }
    pub(crate) fn draining(
        &self,
        fiber: &Arc<FiberInner>,
    ) -> Option<watch::Sender<Option<Outcome>>> {
        if self
            .root
            .upgrade()
            .is_some_and(|root| Arc::ptr_eq(&root, fiber))
            && fiber.state_snapshot().generation == self.root_generation
        {
            if self.provider.borrow().is_some() {
                return None;
            }
            let (outcome, receiver) = watch::channel(None);
            self.provider.send_replace(Some(ConsumerCleanup {
                id: fiber.id,
                generation: fiber.state_snapshot().generation,
                name: fiber.name.clone().into(),
                outcome: receiver,
            }));
            return Some(outcome);
        }
        if self
            .root
            .upgrade()
            .is_some_and(|root| fiber.ctx.is_within(&root.ctx))
        {
            return None; // Internal children remain owned by native subtree cleanup.
        }
        let deps = fiber.last_deps.lock().unwrap().clone()?;
        let providers = self.providers.lock().unwrap();
        if !deps
            .iter()
            .any(|(id, generation, _, _)| providers.contains(&(*id, *generation)))
        {
            return None;
        }
        drop(providers);
        let generation = fiber.state_snapshot().generation;
        let mut consumers = self.consumers.lock().unwrap();
        let key = (fiber.id, generation);
        if consumers.contains_key(&key) {
            return None;
        }
        let (outcome, receiver) = watch::channel(None);
        consumers.insert(
            key,
            ConsumerCleanup {
                id: fiber.id,
                generation,
                name: fiber.name.clone().into(),
                outcome: receiver,
            },
        );
        Some(outcome)
    }
}
pub(crate) struct ConsumerDrain(pub(crate) Vec<watch::Sender<Option<Outcome>>>);
impl ConsumerDrain {
    pub(crate) fn finish(mut self, errors: &[Arc<CordisError>]) {
        if self.0.is_empty() {
            return;
        }
        let result = aggregate_arcs(errors.to_vec()).map_or(Ok(()), Err);
        for outcome in self.0.drain(..) {
            outcome.send_replace(Some(result.clone()));
        }
    }
}
impl Drop for ConsumerDrain {
    fn drop(&mut self) {
        if self.0.is_empty() {
            return;
        }
        let error = Arc::new(CordisError::PluginFailed(
            "native consumer cleanup was abandoned".into(),
        ));
        for outcome in &self.0 {
            outcome.send_replace(Some(Err(error.clone())));
        }
    }
}
