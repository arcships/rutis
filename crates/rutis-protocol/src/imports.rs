//! A single runtime import ledger serializes receive/release/scope-close. Proxy
//! identity is cached independently of delivery ids; released wrappers stay dead.
use crate::error::{ErrorCode, ProtocolError, Result};
use crate::identity::{Delivery, InterfaceView, ObjectIdentity, Scope, Sequence};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportControl {
    Accept { id: Sequence, token: String },
    Release { id: Sequence, token: String },
}
impl std::fmt::Debug for ImportControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Accept { .. } => "Accept(<redacted>)",
            Self::Release { .. } => "Release(<redacted>)",
        })
    }
}

struct Seen {
    delivery: Delivery,
    terminal: bool,
}
struct Wrapper {
    object: ObjectIdentity,
    view: InterfaceView,
    scope: Scope,
    tokens: BTreeSet<Sequence>,
    active: bool,
}
type CacheKey = (Scope, ObjectIdentity, InterfaceView);

#[derive(Default)]
struct State {
    scopes: BTreeMap<Scope, Option<Scope>>,
    cache: BTreeMap<CacheKey, Weak<Mutex<Wrapper>>>,
    seen: BTreeMap<Sequence, Seen>,
    retired: u64,
    controls: Vec<ImportControl>,
    latest_scopes: BTreeMap<crate::identity::Activation, Sequence>,
}

#[derive(Clone, Default)]
pub struct Imports(Arc<Mutex<State>>);

/// Cloning is an alias, not a new scope or ownership token. release() closes
/// this wrapper for every alias. Independent users must use independent scopes.
#[derive(Clone)]
pub struct ObjectProxy {
    wrapper: Arc<Mutex<Wrapper>>,
    imports: Weak<Mutex<State>>,
}

fn error(code: ErrorCode, message: &str) -> ProtocolError {
    ProtocolError::new(code, "import", message)
}

impl Imports {
    pub fn open_scope(&self, scope: Scope, parent: Option<Scope>) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.scopes.contains_key(&scope) {
            return Err(error(ErrorCode::InvalidParams, "scope already open"));
        }
        if parent
            .as_ref()
            .is_some_and(|p| p.activation != scope.activation || !state.scopes.contains_key(p))
        {
            return Err(error(ErrorCode::ScopeClosed, "parent scope closed"));
        }
        if state
            .latest_scopes
            .get(&scope.activation)
            .is_some_and(|last| scope.scope <= *last)
        {
            return Err(error(ErrorCode::StaleObject, "scope id cannot be reused"));
        }
        state
            .latest_scopes
            .insert(scope.activation.clone(), scope.scope);
        state.scopes.insert(scope, parent);
        Ok(())
    }

    /// Only the authenticated broker connection supplies Delivery records.
    pub fn receive(&self, delivery: Delivery) -> Result<ObjectProxy> {
        let mut state = self.0.lock().unwrap();
        if delivery.id.0 <= state.retired {
            return Err(error(
                ErrorCode::StaleObject,
                "delivery is below the confirmed retirement watermark",
            ));
        }
        if !state.scopes.contains_key(&delivery.recipient) {
            return Err(error(ErrorCode::ScopeClosed, "delivery scope closed"));
        }
        if let Some(seen) = state.seen.get(&delivery.id) {
            if seen.delivery != delivery {
                return Err(error(
                    ErrorCode::CapabilityDenied,
                    "delivery id changed identity",
                ));
            }
            if seen.terminal {
                return Err(error(
                    ErrorCode::StaleObject,
                    "released delivery cannot revive a wrapper",
                ));
            }
        }
        let key = (
            delivery.recipient.clone(),
            delivery.object.clone(),
            delivery.view.clone(),
        );
        let wrapper = state
            .cache
            .get(&key)
            .and_then(Weak::upgrade)
            .filter(|w| w.lock().unwrap().active)
            .unwrap_or_else(|| {
                Arc::new(Mutex::new(Wrapper {
                    object: delivery.object.clone(),
                    view: delivery.view.clone(),
                    scope: delivery.recipient.clone(),
                    tokens: BTreeSet::new(),
                    active: true,
                }))
            });
        wrapper.lock().unwrap().tokens.insert(delivery.id);
        state.cache.insert(key, Arc::downgrade(&wrapper));
        state.controls.push(ImportControl::Accept {
            id: delivery.id,
            token: delivery.token.clone(),
        });
        state.seen.entry(delivery.id).or_insert(Seen {
            delivery,
            terminal: false,
        });
        Ok(ObjectProxy {
            wrapper,
            imports: Arc::downgrade(&self.0),
        })
    }

    pub fn close_scope(&self, scope: &Scope) {
        let mut state = self.0.lock().unwrap();
        let mut closed = BTreeSet::from([scope.clone()]);
        loop {
            let count = closed.len();
            for (child, parent) in &state.scopes {
                if parent.as_ref().is_some_and(|p| closed.contains(p)) {
                    closed.insert(child.clone());
                }
            }
            if count == closed.len() {
                break;
            }
        }
        state.scopes.retain(|s, _| !closed.contains(s));
        let wrappers: Vec<_> = state
            .cache
            .iter()
            .filter(|((scope, _, _), _)| closed.contains(scope))
            .filter_map(|(_, w)| w.upgrade())
            .collect();
        for wrapper in wrappers {
            release_wrapper(&mut state, &wrapper);
        }
        let pending: Vec<_> = state
            .seen
            .iter()
            .filter(|(_, seen)| !seen.terminal && closed.contains(&seen.delivery.recipient))
            .map(|(id, _)| *id)
            .collect();
        for id in pending {
            release_token(&mut state, id);
        }
        state
            .cache
            .retain(|(scope, _, _), _| !closed.contains(scope));
    }

    /// Receiver-side evidence for a terminal contiguous prefix. Out-of-order
    /// receipt is allowed, but a gap or live delivery prevents retirement.
    pub fn acknowledge_retirement(&self, through: Sequence) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if through.0 == state.retired {
            return Ok(());
        }
        if through.0 < state.retired {
            return Err(error(
                ErrorCode::InvalidParams,
                "watermark cannot move backwards",
            ));
        }
        let entries: Vec<_> = state
            .seen
            .range(Sequence(state.retired + 1)..=through)
            .collect();
        if entries.len() as u64 != through.0 - state.retired
            || entries.iter().any(|(_, seen)| !seen.terminal)
        {
            return Err(error(
                ErrorCode::InvalidParams,
                "delivery prefix has a gap or live token",
            ));
        }
        state.seen.retain(|id, _| id.0 > through.0);
        state.retired = through.0;
        Ok(())
    }

    pub fn take_controls(&self) -> Vec<ImportControl> {
        std::mem::take(&mut self.0.lock().unwrap().controls)
    }
}

fn release_token(state: &mut State, id: Sequence) {
    if let Some(seen) = state.seen.get_mut(&id) {
        if !seen.terminal {
            seen.terminal = true;
            state.controls.push(ImportControl::Release {
                id,
                token: seen.delivery.token.clone(),
            });
        }
    }
}

fn release_wrapper(state: &mut State, wrapper: &Arc<Mutex<Wrapper>>) {
    let mut wrapper = wrapper.lock().unwrap();
    if !wrapper.active {
        return;
    }
    wrapper.active = false;
    for id in std::mem::take(&mut wrapper.tokens) {
        release_token(state, id);
    }
}

impl ObjectProxy {
    pub fn same_wrapper(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.wrapper, &other.wrapper)
    }
    pub fn same_object(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
    pub fn identity(&self) -> ObjectIdentity {
        self.wrapper.lock().unwrap().object.clone()
    }
    pub fn view(&self) -> InterfaceView {
        self.wrapper.lock().unwrap().view.clone()
    }
    pub fn scope(&self) -> Scope {
        self.wrapper.lock().unwrap().scope.clone()
    }

    pub fn delivery(&self) -> Result<Delivery> {
        let imports = self
            .imports
            .upgrade()
            .ok_or_else(|| error(ErrorCode::ScopeClosed, "runtime import ledger dropped"))?;
        let state = imports.lock().unwrap();
        let wrapper = self.wrapper.lock().unwrap();
        if !wrapper.active || !state.scopes.contains_key(&wrapper.scope) {
            return Err(error(ErrorCode::ScopeClosed, "proxy wrapper released"));
        }
        let id = wrapper
            .tokens
            .first()
            .ok_or_else(|| error(ErrorCode::StaleObject, "proxy has no delivery token"))?;
        Ok(state.seen.get(id).unwrap().delivery.clone())
    }

    pub fn release(&self) {
        if let Some(imports) = self.imports.upgrade() {
            release_wrapper(&mut imports.lock().unwrap(), &self.wrapper);
        } else {
            self.wrapper.lock().unwrap().active = false;
        }
    }
}
