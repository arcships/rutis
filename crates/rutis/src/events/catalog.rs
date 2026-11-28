//! Optional catalog of expected dispatch modes by exact event name
//! (`docs/design-event-mode-catalog-2026-11-20.md`).

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Mode, RESERVED_PREFIX};
use crate::error::{Error, ErrorKind};

/// Runtime-local Cordis event prefix that never crosses into the bus.
const INTERNAL_PREFIX: &str = "internal/";

/// What the event bus does when a catalogued event is dispatched with a
/// [`Mode`] other than its expected mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventModePolicy {
    /// Report [`ErrorKind::EventModeMismatch`] through the host's
    /// [`ErrorSink`](crate::host::ErrorSink) once per
    /// `(emitter, event, actual, expected)` while the catalog is unchanged,
    /// and proceed with the dispatch unchanged (default).
    #[default]
    Warn,
    /// Reject the dispatch before selecting or calling any listener,
    /// returning [`ErrorKind::EventModeMismatch`].
    Error,
}

/// One entry in an [`EventModeCatalog`]: the expected dispatch [`Mode`] and an
/// optional per-event [`EventModePolicy`] override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventModeEntry {
    /// The dispatch mode expected for this event name.
    pub mode: Mode,
    /// Overrides the catalog's default policy when `Some`.
    pub policy: Option<EventModePolicy>,
}

/// Optional catalog mapping exact event names to their expected dispatch
/// [`Mode`] and mismatch [`EventModePolicy`].
///
/// Events not listed here (including unlisted dynamic event names) are
/// unchecked. Installed on a root context with
/// [`CtxHost::set_event_mode_catalog`](crate::host::CtxHost::set_event_mode_catalog).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventModeCatalog {
    default_policy: EventModePolicy,
    entries: HashMap<Arc<str>, EventModeEntry>,
}

impl EventModeCatalog {
    /// An empty catalog whose default mismatch policy is
    /// [`EventModePolicy::Warn`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the catalog's default policy for entries without a per-event
    /// override.
    pub fn with_policy(mut self, policy: EventModePolicy) -> Self {
        self.default_policy = policy;
        self
    }

    /// Record that exact event `name` is expected to be dispatched with `mode`,
    /// using the catalog's default mismatch policy.
    pub fn expect(mut self, name: impl Into<Arc<str>>, mode: Mode) -> Self {
        self.entries
            .insert(name.into(), EventModeEntry { mode, policy: None });
        self
    }

    /// Record that exact event `name` is expected to be dispatched with `mode`,
    /// overriding the mismatch policy for this event with `policy`.
    pub fn expect_with(
        mut self,
        name: impl Into<Arc<str>>,
        mode: Mode,
        policy: EventModePolicy,
    ) -> Self {
        self.entries.insert(
            name.into(),
            EventModeEntry {
                mode,
                policy: Some(policy),
            },
        );
        self
    }

    /// The catalog's default mismatch policy.
    pub fn policy(&self) -> EventModePolicy {
        self.default_policy
    }

    /// Look up the expected mode and effective policy for exact event `name`.
    pub fn get(&self, name: &str) -> Option<(Mode, EventModePolicy)> {
        self.entries
            .get(name)
            .map(|entry| (entry.mode, entry.policy.unwrap_or(self.default_policy)))
    }

    /// Look up the raw entry for exact event `name`.
    pub fn entry(&self, name: &str) -> Option<EventModeEntry> {
        self.entries.get(name).copied()
    }

    /// Number of catalogued event names.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no event names are catalogued.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all `(name, entry)` pairs in the catalog.
    pub fn iter(&self) -> impl Iterator<Item = (&str, EventModeEntry)> {
        self.entries.iter().map(|(name, entry)| (&**name, *entry))
    }

    /// Validate all event names before installing the catalog.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let mut names: Vec<&str> = self.entries.keys().map(|name| &**name).collect();
        names.sort_unstable();
        let mut issues = Vec::new();
        for name in names {
            if name.is_empty() {
                issues.push("events: event name cannot be empty".to_owned());
            } else if name.starts_with(RESERVED_PREFIX) {
                issues.push(format!(
                    "events.{name}: rutis/* events are owned by the host and cannot be configured here"
                ));
            } else if name.starts_with(INTERNAL_PREFIX) {
                issues.push(format!(
                    "events.{name}: internal/* events stay inside their runtime and cannot be configured here"
                ));
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(ErrorKind::Validation { issues }.into())
        }
    }
}
