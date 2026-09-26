use rutis::{BoxFuture, CordisError, Ctx, Effect, FiberState, Plugin, PluginFactory, TypeKey};
use rutis_protocol::{
    contract::{FAMILY, VERSION},
    error::ErrorCode,
    factories::{StaticFactories, StaticFactory},
    prepare::digest,
    runner_image::{FactoryCatalog, RunnerCatalog},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

const SCHEMA: &[u8] = include_bytes!("../../../protocol/fixtures/plugin.config.json");
#[derive(Deserialize)]
struct Config {
    label: String,
}
#[derive(Default)]
struct Probe {
    constructed: AtomicUsize,
    built: AtomicUsize,
    applied: Mutex<Vec<(Ctx, String)>>,
    cleaned: Mutex<Vec<String>>,
}
struct Factory {
    probe: Arc<Probe>,
    injects: Vec<TypeKey>,
    panic_metadata: bool,
}
impl PluginFactory<Config> for Factory {
    fn name(&self) -> &str {
        assert!(!self.panic_metadata, "metadata panic");
        "probe"
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn validate_config(&self, config: &Config) -> Result<(), CordisError> {
        if config.label == "native-invalid" {
            return Err(CordisError::Validation {
                issues: vec!["native validation rejected config".into()],
            });
        }
        Ok(())
    }
    fn build(&self, config: &Config) -> Result<Box<dyn Plugin>, CordisError> {
        self.probe.built.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Instance {
            probe: self.probe.clone(),
            label: config.label.clone(),
        }))
    }
}
struct Instance {
    probe: Arc<Probe>,
    label: String,
}
impl Plugin for Instance {
    fn name(&self) -> &str {
        "probe-instance"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.provide(self.label.clone())?;
            self.probe
                .applied
                .lock()
                .unwrap()
                .push((ctx.clone(), self.label.clone()));
            let probe = self.probe.clone();
            let label = self.label.clone();
            Ok(Effect::Disposer(Box::new(move || {
                probe.cleaned.lock().unwrap().push(label);
                Ok(())
            })))
        })
    }
}
fn metadata() -> FactoryCatalog {
    FactoryCatalog {
        config_sha256: digest(SCHEMA),
        provides: BTreeMap::new(),
        requires: BTreeMap::new(),
    }
}
fn catalog(entries: impl IntoIterator<Item = (String, FactoryCatalog)>) -> Vec<u8> {
    serde_json::to_vec(&RunnerCatalog {
        protocol_family: FAMILY.into(),
        protocol_version: VERSION.into(),
        framework_version: "0.3.0".into(),
        environment_sha256: digest(b"linked-test-environment"),
        capabilities: BTreeSet::new(),
        factories: entries.into_iter().collect(),
    })
    .unwrap()
}
fn entry(name: &str, probe: Arc<Probe>, injects: Vec<TypeKey>) -> StaticFactory {
    StaticFactory::native(name, metadata(), SCHEMA, move || {
        probe.constructed.fetch_add(1, Ordering::Relaxed);
        Factory {
            probe: probe.clone(),
            injects: injects.clone(),
            panic_metadata: false,
        }
    })
    .unwrap()
}
fn registry(probe: Arc<Probe>, injects: Vec<TypeKey>) -> StaticFactories {
    StaticFactories::admit(
        &catalog([("probe".into(), metadata())]),
        [entry("probe", probe, injects)],
    )
    .unwrap()
}

#[tokio::test]
async fn linked_registry_is_lazy_and_mounts_independent_native_instances() {
    let probe = Arc::new(Probe::default());
    let registry = registry(probe.clone(), vec![]);
    assert_eq!(probe.constructed.load(Ordering::Relaxed), 0);
    assert_eq!(probe.built.load(Ordering::Relaxed), 0);
    let root = Ctx::root().unwrap();
    let a = root.isolate(TypeKey::of::<String>(), "a");
    let b = root.isolate(TypeKey::of::<String>(), "b");
    let first = registry.mount(&a, "probe", json!({"label":"a"})).unwrap();
    let second = registry.mount(&b, "probe", json!({"label":"b"})).unwrap();
    tokio::try_join!(first.view(), second.view()).unwrap();
    assert_eq!(probe.constructed.load(Ordering::Relaxed), 2);
    assert_eq!(probe.built.load(Ordering::Relaxed), 2);
    assert_eq!(&*a.get::<String>().unwrap(), "a");
    assert_eq!(&*b.get::<String>().unwrap(), "b");
    assert!(root.get::<String>().is_none());
    let old = probe
        .applied
        .lock()
        .unwrap()
        .iter()
        .find(|(_, label)| label == "a")
        .unwrap()
        .0
        .clone();
    first.stop().await.unwrap();
    assert_eq!(second.view().state().state, FiberState::Active);
    assert_eq!(*probe.cleaned.lock().unwrap(), vec!["a"]);
    assert!(old.provide(1_u16).is_err());
    assert!(first
        .view()
        .update(Config { label: "c".into() })
        .await
        .is_err());
    second.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

#[test]
fn missing_duplicate_extra_or_changed_factories_fail_without_construction() {
    let probe = Arc::new(Probe::default());
    let bytes = catalog([("probe".into(), metadata())]);
    assert_eq!(
        StaticFactories::admit(&bytes, []).err().unwrap().code,
        ErrorCode::InterfaceMismatch
    );
    assert_eq!(
        StaticFactories::admit(
            &bytes,
            [
                entry("probe", probe.clone(), vec![]),
                entry("probe", probe.clone(), vec![])
            ],
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::InvalidParams
    );
    assert_eq!(
        StaticFactories::admit(&bytes, [entry("unknown", probe.clone(), vec![])])
            .err()
            .unwrap()
            .code,
        ErrorCode::InterfaceMismatch
    );
    let mut changed = metadata();
    changed.requires.insert(
        "db".into(),
        rutis_protocol::runner_image::CatalogService {
            interface: "Database".into(),
            version: "1.0.0".into(),
            bundle_sha256: digest(b"bundle"),
        },
    );
    assert_eq!(
        StaticFactories::admit(
            &catalog([("probe".into(), changed)]),
            [entry("probe", probe.clone(), vec![])]
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::InterfaceMismatch
    );
    assert_eq!(probe.constructed.load(Ordering::Relaxed), 0);
    assert_eq!(probe.built.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn schema_and_typed_decode_precede_constructor_and_native_validation_precedes_mount() {
    let probe = Arc::new(Probe::default());
    let registry = registry(probe.clone(), vec![]);
    let root = Ctx::root().unwrap();
    for value in [
        json!({}),
        json!({"label":1}),
        json!({"label":"x","extra":true}),
    ] {
        assert_eq!(
            registry.mount(&root, "probe", value).err().unwrap().code,
            ErrorCode::InvalidParams
        );
    }
    assert_eq!(probe.constructed.load(Ordering::Relaxed), 0);
    assert_eq!(
        registry
            .mount(&root, "missing", json!({"label":"x"}))
            .err()
            .unwrap()
            .code,
        ErrorCode::InterfaceMismatch
    );
    assert_eq!(
        registry
            .mount(&root, "probe", json!({"label":"native-invalid"}))
            .err()
            .unwrap()
            .code,
        ErrorCode::InvalidParams
    );
    assert_eq!(probe.constructed.load(Ordering::Relaxed), 1);
    assert_eq!(probe.built.load(Ordering::Relaxed), 0);
    assert!(root.get::<String>().is_none());

    let typed = StaticFactory::native::<u8, _>("typed", metadata(), SCHEMA, || {
        panic!("typed decode must reject before construction");
        #[allow(unreachable_code)]
        U8Factory
    })
    .unwrap();
    let typed = StaticFactories::admit(&catalog([("typed".into(), metadata())]), [typed]).unwrap();
    assert_eq!(
        typed
            .mount(&root, "typed", json!({"label":"x"}))
            .err()
            .unwrap()
            .code,
        ErrorCode::InvalidParams
    );
    root.shutdown().await.unwrap();
}
struct U8Factory;
impl PluginFactory<u8> for U8Factory {
    fn build(&self, _: &u8) -> Result<Box<dyn Plugin>, CordisError> {
        unreachable!()
    }
}

#[tokio::test]
async fn native_dependencies_gate_build_and_replacement_never_reuses_an_activation() {
    let root = Ctx::root().unwrap();
    let probe = Arc::new(Probe::default());
    let registry = registry(probe.clone(), vec![TypeKey::of::<u8>()]);
    let first = registry
        .mount(&root, "probe", json!({"label":"first"}))
        .unwrap();
    first.view().await.unwrap();
    assert_eq!(first.view().state().state, FiberState::Pending);
    assert_eq!(probe.built.load(Ordering::Relaxed), 0);
    let service = root.provide(1_u8).unwrap();
    first.view().await.unwrap();
    assert_eq!(first.view().state().state, FiberState::Active);
    let old = probe.applied.lock().unwrap()[0].0.clone();
    service.dispose().await.unwrap();
    assert!(!first.gate().is_open());
    root.provide(2_u8).unwrap();
    first.view().await.unwrap();
    assert_eq!(first.view().state().state, FiberState::Disposed);
    assert_eq!(probe.built.load(Ordering::Relaxed), 1);
    assert!(old.effect(|| Effect::Done).is_err());
    first.stop().await.unwrap();
    let second = registry
        .mount(&root, "probe", json!({"label":"second"}))
        .unwrap();
    second.view().await.unwrap();
    assert_eq!(second.view().state().state, FiberState::Active);
    assert_eq!(probe.built.load(Ordering::Relaxed), 2);
    second.stop().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test]
async fn constructor_and_metadata_panics_are_errors_and_leave_no_native_mount() {
    let root = Ctx::root().unwrap();
    let probe = Arc::new(Probe::default());
    let constructors = StaticFactory::native::<Config, Factory>("ctor", metadata(), SCHEMA, || {
        panic!("constructor panic")
    })
    .unwrap();
    let captured = probe.clone();
    let metadata_panic = StaticFactory::native("metadata", metadata(), SCHEMA, move || Factory {
        probe: captured.clone(),
        injects: vec![],
        panic_metadata: true,
    })
    .unwrap();
    let registry = StaticFactories::admit(
        &catalog([("ctor".into(), metadata()), ("metadata".into(), metadata())]),
        [constructors, metadata_panic],
    )
    .unwrap();
    for name in ["ctor", "metadata"] {
        let error = registry
            .mount(&root, name, json!({"label":"x"}))
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::Business);
        assert_eq!(
            error.execution,
            rutis_protocol::error::Execution::NotStarted
        );
    }
    assert_eq!(probe.built.load(Ordering::Relaxed), 0);
    assert!(probe.applied.lock().unwrap().is_empty());
    assert!(root.get::<String>().is_none());
    root.shutdown().await.unwrap();
}

#[test]
fn linked_schema_hash_and_schema_dialect_are_checked_without_constructing_factory() {
    assert_eq!(
        StaticFactory::native::<Config, Factory>("probe", metadata(), b"{}", || unreachable!())
            .err()
            .unwrap()
            .code,
        ErrorCode::InterfaceMismatch
    );
    let schema = br#"{"type":"string","pattern":".*"}"#;
    let mut definition = metadata();
    definition.config_sha256 = digest(schema);
    assert!(StaticFactory::native::<Config, Factory>(
        "probe",
        definition,
        schema,
        || unreachable!()
    )
    .is_err());
}
