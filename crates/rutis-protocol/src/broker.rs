//! Broker-owned authorization ledger. No business handler runs under this
//! ledger's lock. Delivery pins and execution pins are accounted separately.
use crate::error::{ErrorCode, ProtocolError, Result};
use crate::identity::{Activation, Delivery, InterfaceView, ObjectIdentity, Scope, Sequence};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    Offered,
    Accepted,
    Released,
    Revoked,
}
impl DeliveryState {
    fn terminal(self) -> bool {
        matches!(self, Self::Released | Self::Revoked)
    }
}

struct Grant {
    delivery: Delivery,
    methods: BTreeSet<String>,
    state: DeliveryState,
}

#[derive(Default)]
struct Recipient {
    allocated: u64,
    retired: u64,
    grants: BTreeMap<u64, Grant>,
}

#[derive(Default)]
struct ObjectEntry {
    views: BTreeMap<InterfaceView, BTreeSet<String>>,
    delivery_pins: usize,
    execution_pins: usize,
}

/// Connection identity is authenticated by the transport/supervisor. These
/// methods take that identity explicitly; a frame cannot self-select its sender.
#[derive(Default)]
pub struct Broker {
    activations: BTreeSet<Activation>,
    scopes: BTreeMap<Scope, Option<Scope>>,
    recipients: BTreeMap<(String, Sequence), Recipient>,
    objects: BTreeMap<ObjectIdentity, ObjectEntry>,
    calls: BTreeMap<(Activation, Sequence), ObjectIdentity>,
    next_objects: BTreeMap<Activation, u64>,
    latest_epochs: BTreeMap<String, Sequence>,
    closed_epochs: BTreeMap<String, Sequence>,
    latest_activations: BTreeMap<(String, Sequence), Sequence>,
    latest_scopes: BTreeMap<Activation, Sequence>,
    latest_calls: BTreeMap<Activation, Sequence>,
}

fn error(code: ErrorCode, message: &str) -> ProtocolError {
    ProtocolError::new(code, "authorization", message)
}

impl Broker {
    pub fn start_activation(&mut self, activation: Activation) -> Result<()> {
        if self
            .closed_epochs
            .get(&activation.runtime)
            .is_some_and(|closed| activation.epoch <= *closed)
        {
            return Err(error(ErrorCode::StaleObject, "closed runtime epoch"));
        }
        let key = (activation.runtime.clone(), activation.epoch);
        if self
            .latest_activations
            .get(&key)
            .is_some_and(|last| activation.activation <= *last)
        {
            return Err(error(
                ErrorCode::StaleObject,
                "activation id cannot be reused",
            ));
        }
        if let Some(last) = self.latest_epochs.get(&activation.runtime) {
            if activation.epoch < *last {
                return Err(error(ErrorCode::StaleObject, "old runtime epoch"));
            }
            if activation.epoch > *last
                && (self
                    .activations
                    .iter()
                    .any(|a| a.runtime == activation.runtime)
                    || self.calls.iter().any(|((caller, _), object)| {
                        caller.runtime == activation.runtime
                            || object.owner.runtime == activation.runtime
                    }))
            {
                return Err(error(
                    ErrorCode::Unavailable,
                    "old runtime still has active members or executing calls",
                ));
            }
        }
        if !self.activations.insert(activation.clone()) {
            return Err(error(
                ErrorCode::InvalidParams,
                "activation already registered",
            ));
        }
        self.latest_epochs
            .insert(activation.runtime.clone(), activation.epoch);
        self.latest_activations.insert(key, activation.activation);
        self.recipients
            .entry((activation.runtime, activation.epoch))
            .or_default();
        Ok(())
    }

    pub fn open_scope(&mut self, scope: Scope, parent: Option<Scope>) -> Result<()> {
        if !self.activations.contains(&scope.activation) {
            return Err(error(
                ErrorCode::Unavailable,
                "activation is not registered",
            ));
        }
        if self.scopes.contains_key(&scope) {
            return Err(error(ErrorCode::InvalidParams, "scope already registered"));
        }
        if let Some(parent) = &parent {
            if parent.activation != scope.activation || !self.scopes.contains_key(parent) {
                return Err(error(ErrorCode::ScopeClosed, "parent scope is unavailable"));
            }
        }
        if self
            .latest_scopes
            .get(&scope.activation)
            .is_some_and(|last| scope.scope <= *last)
        {
            return Err(error(ErrorCode::StaleObject, "scope id cannot be reused"));
        }
        self.latest_scopes
            .insert(scope.activation.clone(), scope.scope);
        self.scopes.insert(scope, parent);
        Ok(())
    }

    pub fn register_object(
        &mut self,
        owner: &Activation,
        views: BTreeMap<InterfaceView, BTreeSet<String>>,
    ) -> Result<ObjectIdentity> {
        if !self.activations.contains(owner) {
            return Err(error(ErrorCode::Unavailable, "object owner unavailable"));
        }
        if views.is_empty() {
            return Err(error(
                ErrorCode::InterfaceMismatch,
                "object must expose a declared interface view",
            ));
        }
        let next = self.next_objects.entry(owner.clone()).or_default();
        *next = next
            .checked_add(1)
            .ok_or_else(|| error(ErrorCode::Unavailable, "object sequence exhausted"))?;
        let identity = ObjectIdentity {
            owner: owner.clone(),
            object: Sequence(*next),
        };
        self.objects.insert(
            identity.clone(),
            ObjectEntry {
                views,
                ..Default::default()
            },
        );
        Ok(identity)
    }

    /// This is only used for an owner-originated result/event or service
    /// installation. A recipient cannot grant somebody else's object.
    pub fn offer(
        &mut self,
        owner: &Activation,
        object: &ObjectIdentity,
        recipient: &Scope,
        view: &InterfaceView,
    ) -> Result<Delivery> {
        if owner != &object.owner {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                "third-party delegation is not implemented",
            ));
        }
        if !self.scopes.contains_key(recipient) {
            return Err(error(ErrorCode::ScopeClosed, "recipient scope closed"));
        }
        if !self.activations.contains(owner) {
            return Err(error(ErrorCode::StaleObject, "owner activation closed"));
        }
        let entry = self
            .objects
            .get_mut(object)
            .ok_or_else(|| error(ErrorCode::StaleObject, "unknown object"))?;
        let methods = entry
            .views
            .get(view)
            .ok_or_else(|| error(ErrorCode::InterfaceMismatch, "interface view not exported"))?
            .clone();
        let target = self
            .recipients
            .get_mut(&(
                recipient.activation.runtime.clone(),
                recipient.activation.epoch,
            ))
            .unwrap();
        let sequence = target
            .allocated
            .checked_add(1)
            .ok_or_else(|| error(ErrorCode::Unavailable, "delivery sequence exhausted"))?;
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|_| error(ErrorCode::Unavailable, "grant entropy unavailable"))?;
        let token: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let delivery = Delivery {
            id: Sequence(sequence),
            token,
            object: object.clone(),
            recipient: recipient.clone(),
            view: view.clone(),
        };
        target.allocated = sequence;
        target.grants.insert(
            sequence,
            Grant {
                delivery: delivery.clone(),
                methods,
                state: DeliveryState::Offered,
            },
        );
        entry.delivery_pins += 1;
        Ok(delivery)
    }

    fn grant_mut(
        &mut self,
        caller: &Activation,
        id: Sequence,
        token: &str,
    ) -> Result<Option<&mut Grant>> {
        if self
            .closed_epochs
            .get(&caller.runtime)
            .is_some_and(|closed| caller.epoch <= *closed)
        {
            return Ok(None);
        }
        let active = self.activations.contains(caller);
        let target = self
            .recipients
            .get_mut(&(caller.runtime.clone(), caller.epoch))
            .ok_or_else(|| error(ErrorCode::StaleObject, "recipient epoch closed"))?;
        if id.0 <= target.retired {
            return Ok(None);
        }
        let grant = target
            .grants
            .get_mut(&id.0)
            .ok_or_else(|| error(ErrorCode::CapabilityDenied, "unknown delivery"))?;
        if grant.delivery.recipient.activation != *caller || grant.delivery.token != token {
            return Err(error(
                ErrorCode::CapabilityDenied,
                "delivery belongs to another caller",
            ));
        }
        if !active && !grant.state.terminal() {
            return Err(error(ErrorCode::ScopeClosed, "caller activation closed"));
        }
        Ok(Some(grant))
    }

    pub fn accept(&mut self, caller: &Activation, id: Sequence, token: &str) -> Result<()> {
        if let Some(grant) = self.grant_mut(caller, id, token)? {
            if grant.state == DeliveryState::Offered {
                grant.state = DeliveryState::Accepted;
            }
        }
        Ok(())
    }

    pub fn release(&mut self, caller: &Activation, id: Sequence, token: &str) -> Result<()> {
        let object = match self.grant_mut(caller, id, token)? {
            Some(grant) if !grant.state.terminal() => {
                grant.state = DeliveryState::Released;
                Some(grant.delivery.object.clone())
            }
            _ => None,
        };
        if let Some(object) = object {
            self.objects.get_mut(&object).unwrap().delivery_pins -= 1;
        }
        Ok(())
    }

    /// Receiver confirms it processed the delivery envelopes and final states
    /// through this prefix. The broker independently requires a terminal prefix.
    /// No timeout or clock is accepted as evidence for retirement.
    pub fn retire(
        &mut self,
        caller: &Activation,
        delivered_through: Sequence,
        terminal_through: Sequence,
    ) -> Result<()> {
        if !self.activations.contains(caller) {
            return Err(error(ErrorCode::ScopeClosed, "caller activation closed"));
        }
        let target = self
            .recipients
            .get_mut(&(caller.runtime.clone(), caller.epoch))
            .unwrap();
        let through = terminal_through.0;
        if through < target.retired
            || through > delivered_through.0
            || delivered_through.0 > target.allocated
            || target
                .grants
                .range(..=through)
                .any(|(_, grant)| !grant.state.terminal())
        {
            return Err(error(
                ErrorCode::InvalidParams,
                "unconfirmed or nonterminal delivery prefix",
            ));
        }
        target.grants.retain(|id, _| *id > through);
        target.retired = through;
        Ok(())
    }

    /// Owner-returned references still obtain execution pins and use the same
    /// call record as ordinary remote calls. No raw-owner dispatch shortcut.
    pub fn begin_call(
        &mut self,
        caller: &Scope,
        call: Sequence,
        delivery: &Delivery,
        method: &str,
        target_owner: &Activation,
    ) -> Result<ObjectIdentity> {
        if !self.scopes.contains_key(caller) {
            return Err(error(ErrorCode::ScopeClosed, "call scope closed"));
        }
        if &delivery.recipient != caller {
            return Err(error(
                ErrorCode::CapabilityDenied,
                "delivery scope mismatch",
            ));
        }
        if target_owner != &delivery.object.owner {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                "third-party delegation is not implemented",
            ));
        }
        if self.calls.contains_key(&(caller.activation.clone(), call)) {
            return Err(error(ErrorCode::InvalidParams, "call id already executing"));
        }
        if self
            .latest_calls
            .get(&caller.activation)
            .is_some_and(|last| call <= *last)
        {
            return Err(error(ErrorCode::StaleObject, "call id cannot be reused"));
        }
        let Some(grant) = self.grant_mut(&caller.activation, delivery.id, &delivery.token)? else {
            return Err(error(ErrorCode::StaleObject, "delivery retired"));
        };
        if grant.state != DeliveryState::Accepted || grant.delivery != *delivery {
            return Err(error(ErrorCode::StaleObject, "grant not active"));
        }
        if !grant.methods.contains(method) {
            return Err(error(
                ErrorCode::CapabilityDenied,
                "method outside interface view",
            ));
        }
        let object = grant.delivery.object.clone();
        if !self.activations.contains(&object.owner) {
            return Err(error(ErrorCode::StaleObject, "owner closed"));
        }
        self.objects.get_mut(&object).unwrap().execution_pins += 1;
        self.calls
            .insert((caller.activation.clone(), call), object.clone());
        self.latest_calls.insert(caller.activation.clone(), call);
        Ok(object)
    }

    /// Cancellation of a waiter never calls this. Only the owner-side finished
    /// acknowledgement (including registered children) releases execution pins.
    pub fn finish_call(
        &mut self,
        owner: &Activation,
        caller: &Activation,
        call: Sequence,
    ) -> Result<()> {
        let key = (caller.clone(), call);
        if let Some(object) = self.calls.get(&key) {
            if &object.owner != owner {
                return Err(error(
                    ErrorCode::CapabilityDenied,
                    "finished from wrong owner",
                ));
            }
            let object = self.calls.remove(&key).unwrap();
            self.objects.get_mut(&object).unwrap().execution_pins -= 1;
        }
        Ok(())
    }

    pub fn close_scope(&mut self, scope: &Scope) {
        let mut closed = BTreeSet::from([scope.clone()]);
        loop {
            let count = closed.len();
            for (child, parent) in &self.scopes {
                if parent.as_ref().is_some_and(|p| closed.contains(p)) {
                    closed.insert(child.clone());
                }
            }
            if count == closed.len() {
                break;
            }
        }
        self.scopes.retain(|s, _| !closed.contains(s));
        for recipient in self.recipients.values_mut() {
            for grant in recipient.grants.values_mut() {
                if closed.contains(&grant.delivery.recipient) && !grant.state.terminal() {
                    grant.state = DeliveryState::Revoked;
                    self.objects
                        .get_mut(&grant.delivery.object)
                        .unwrap()
                        .delivery_pins -= 1;
                }
            }
        }
    }

    pub fn close_activation(&mut self, activation: &Activation) {
        let scopes: Vec<_> = self
            .scopes
            .keys()
            .filter(|s| &s.activation == activation)
            .cloned()
            .collect();
        for scope in scopes {
            self.close_scope(&scope);
        }
        self.activations.remove(activation);
        for recipient in self.recipients.values_mut() {
            for grant in recipient.grants.values_mut() {
                if &grant.delivery.object.owner == activation && !grant.state.terminal() {
                    grant.state = DeliveryState::Revoked;
                    self.objects
                        .get_mut(&grant.delivery.object)
                        .unwrap()
                        .delivery_pins -= 1;
                }
            }
        }
    }

    /// Closing a connection closes its epoch immediately. Execution pins stay
    /// until tasks finish or the supervisor confirms the owner process is reaped.
    pub fn close_epoch(&mut self, runtime: &str, epoch: Sequence) {
        let activations: Vec<_> = self
            .activations
            .iter()
            .filter(|a| a.runtime == runtime && a.epoch == epoch)
            .cloned()
            .collect();
        for activation in activations {
            self.close_activation(&activation);
        }
        self.closed_epochs
            .entry(runtime.into())
            .and_modify(|old| *old = (*old).max(epoch))
            .or_insert(epoch);
        self.recipients.remove(&(runtime.into(), epoch));
    }

    pub fn pins(&self, object: &ObjectIdentity) -> (usize, usize) {
        self.objects
            .get(object)
            .map(|o| (o.delivery_pins, o.execution_pins))
            .unwrap_or_default()
    }
    pub fn state(&self, recipient: &Activation, id: Sequence) -> Option<DeliveryState> {
        self.recipients
            .get(&(recipient.runtime.clone(), recipient.epoch))
            .and_then(|r| r.grants.get(&id.0))
            .map(|g| g.state)
    }
}
