//! A statically linked Rust runner binds its inspected catalog to lazy native
//! factories. Registry admission never constructs a factory or a plugin.
use crate::{
    contract::{check_schema_for_prepare, identifier, validate_json},
    error::{ErrorCode, ProtocolError, Result},
    json,
    managed::ManagedActivation,
    prepare::{digest, PluginEntry, PreparedInstance, RuntimeKind},
    runner_image::{FactoryCatalog, RunnerCatalog},
};
use rutis::{Ctx, PluginFactory};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
};

type Mount = dyn Fn(&Ctx, Value) -> Result<ManagedActivation> + Send + Sync;

pub struct StaticFactory {
    name: String,
    catalog: FactoryCatalog,
    config_schema: Value,
    mount: Box<Mount>,
}

impl StaticFactory {
    /// The constructor is retained without invocation. The embedded catalog
    /// is compared against these linked declarations before RuntimeReady.
    pub fn native<C, F>(
        name: impl Into<String>,
        catalog: FactoryCatalog,
        config_schema: &[u8],
        constructor: impl Fn() -> F + Send + Sync + 'static,
    ) -> Result<Self>
    where
        C: DeserializeOwned + Send + Sync + 'static,
        F: PluginFactory<C>,
    {
        let name = name.into();
        if !identifier(&name) {
            return Err(invalid("invalid static factory name"));
        }
        if digest(config_schema) != catalog.config_sha256 {
            return Err(mismatch("linked factory config schema differs"));
        }
        let config_schema = json::decode(config_schema)?;
        check_schema_for_prepare(&config_schema)?;
        Ok(Self {
            name,
            catalog,
            config_schema,
            mount: Box::new(move |ctx, value| {
                let config: C = serde_json::from_value(value)
                    .map_err(|e| invalid(format!("native config decode: {e}")))?;
                catch_unwind(AssertUnwindSafe(|| {
                    let factory = constructor();
                    factory.validate_config(&config).map_err(native_error)?;
                    ManagedActivation::mount_factory(ctx, factory, config).map_err(native_error)
                }))
                .unwrap_or_else(|_| {
                    Err(ProtocolError::new(
                        ErrorCode::Business,
                        "start",
                        "native factory construction or registration panicked",
                    ))
                })
            }),
        })
    }
}

pub struct StaticFactories {
    catalog: RunnerCatalog,
    entries: BTreeMap<String, StaticFactory>,
}

impl StaticFactories {
    /// `catalog_bytes` must be the same embedded bytes returned by
    /// `embed_runner_catalog!`, rather than a mutable runtime file.
    pub fn admit(
        catalog_bytes: &[u8],
        entries: impl IntoIterator<Item = StaticFactory>,
    ) -> Result<Self> {
        let catalog = RunnerCatalog::parse(catalog_bytes)?;
        let mut linked = BTreeMap::new();
        for entry in entries {
            if catalog.factories.get(&entry.name) != Some(&entry.catalog) {
                return Err(mismatch("linked factory contract differs from catalog"));
            }
            if linked.insert(entry.name.clone(), entry).is_some() {
                return Err(invalid("duplicate linked factory"));
            }
        }
        if linked.keys().ne(catalog.factories.keys()) {
            return Err(mismatch("catalog contains an unlinked factory"));
        }
        Ok(Self {
            catalog,
            entries: linked,
        })
    }

    pub fn catalog(&self) -> &RunnerCatalog {
        &self.catalog
    }

    /// Trusted runner lifecycle entry point. A wire handler must select the
    /// instance and configuration from its frozen host plan, not author input.
    pub fn mount_prepared(
        &self,
        parent: &Ctx,
        instance: &PreparedInstance,
    ) -> Result<ManagedActivation> {
        let package = instance.package();
        if package.manifest().runtime.kind != RuntimeKind::RustRutis
            || package.catalog() != Some(&self.catalog)
        {
            return Err(mismatch("prepared package uses another Rust runner"));
        }
        let PluginEntry::Rust { factory } = &package.manifest().plugin else {
            return Err(mismatch("prepared entry is not a native factory"));
        };
        self.mount(parent, factory, instance.config().clone())
    }

    /// Native mounting alone does not publish protocol services. The runner
    /// must still stage exports and wait for the host's activation ACK.
    pub fn mount(&self, parent: &Ctx, factory: &str, config: Value) -> Result<ManagedActivation> {
        let entry = self
            .entries
            .get(factory)
            .ok_or_else(|| mismatch("factory is not in the linked registry"))?;
        validate_json(&entry.config_schema, &config)?;
        (entry.mount)(parent, config)
    }
}

fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, "start", message)
}
fn mismatch(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InterfaceMismatch, "runner_registry", message)
}
fn native_error(error: rutis::CordisError) -> ProtocolError {
    let code = match error {
        rutis::CordisError::Validation { .. } => ErrorCode::InvalidParams,
        rutis::CordisError::Closed
        | rutis::CordisError::InactiveEffect
        | rutis::CordisError::InactiveGeneration { .. }
        | rutis::CordisError::StaleGeneration { .. } => ErrorCode::Unavailable,
        _ => ErrorCode::Business,
    };
    ProtocolError::new(code, "start", error.to_string())
}
