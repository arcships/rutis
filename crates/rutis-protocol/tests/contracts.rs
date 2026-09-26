use rutis_protocol::contract::{validate_wire, AdmittedBundle, TypeExpr, WireValue};
use serde::Deserialize;
use serde_json::{json, Value};

const BUNDLE: &[u8] = include_bytes!("../../../protocol/fixtures/database.bundle.json");
#[derive(Deserialize)]
struct Case {
    name: String,
    #[serde(rename = "type")]
    contract: TypeExpr,
    value: Value,
    references: Vec<String>,
    error: Option<String>,
}

#[test]
fn t22_shared_contract_corpus() {
    let cases: Vec<Case> = serde_json::from_slice(include_bytes!(
        "../../../protocol/fixtures/contract-corpus.json"
    ))
    .unwrap();
    for case in cases {
        let result = serde_json::from_value::<WireValue>(case.value)
            .map_err(|_| "InvalidParams".into())
            .and_then(|v| {
                validate_wire(&case.contract, &v, &case.references)
                    .map_err(|e| format!("{:?}", e.code))
            });
        assert_eq!(result.err(), case.error, "{}", case.name);
    }
}

#[test]
fn complete_object_graph_descriptor_and_exact_bundle_identity() {
    let admitted = AdmittedBundle::parse(BUNDLE).unwrap();
    assert_eq!(admitted.bundle.interfaces.len(), 4);
    admitted
        .require_identity("1.0.0", &admitted.sha256)
        .unwrap();
    assert!(admitted
        .require_identity("1.0.1", &admitted.sha256)
        .is_err());
    assert!(admitted.require_identity("1.0.0", &"0".repeat(64)).is_err());
    let mut changed = BUNDLE.to_vec();
    changed.push(b' ');
    assert_ne!(
        admitted.sha256,
        AdmittedBundle::parse(&changed).unwrap().sha256
    );
}

#[test]
fn unsupported_extensions_and_schema_keywords_are_rejected_at_prepare() {
    let bundle: Value = serde_json::from_slice(BUNDLE).unwrap();
    for (path, value, code) in [
        (
            "/required_capabilities",
            json!(["object.delegate"]),
            "UnsupportedCapability",
        ),
        (
            "/events/session~1event/modes",
            json!(["waterfall"]),
            "UnsupportedCapability",
        ),
        (
            "/interfaces/Database/methods/withCallback/params/ownership",
            json!("scope"),
            "UnsupportedCapability",
        ),
        (
            "/interfaces/Database/methods/connect/result",
            json!({"kind":"stream","item":{"kind":"value","schema":{"type":"string"}}}),
            "UnsupportedCapability",
        ),
        (
            "/interfaces/Database/methods/connect/params/schema",
            json!({"type":"string","pattern":".*"}),
            "UnsupportedCapability",
        ),
        (
            "/interfaces/Database/methods/connect/params/schema",
            json!({"$ref":"https://example.invalid/schema"}),
            "UnsupportedCapability",
        ),
        (
            "/interfaces/Database/methods/connect/result/interface",
            json!("missing"),
            "InvalidParams",
        ),
        (
            "/interfaces/Database/methods/connect/params/schema",
            json!({"$defs":{"X":{"$ref":"#/$defs/X"}},"$ref":"#/$defs/X"}),
            "InvalidParams",
        ),
    ] {
        let mut changed = bundle.clone();
        *changed.pointer_mut(path).unwrap() = value;
        let result = AdmittedBundle::parse(&serde_json::to_vec(&changed).unwrap()).unwrap_err();
        assert_eq!(format!("{:?}", result.code), code, "{path}");
    }
}
