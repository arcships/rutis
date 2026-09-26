use rutis_protocol::contract::{callback_key, validate_wire, AdmittedBundle, TypeExpr, WireValue};
use rutis_protocol::json::{decode, MAX_JSON_BYTES};
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
fn t22_strict_json_and_descriptor_corpora() {
    let cases: Value = serde_json::from_slice(include_bytes!(
        "../../../protocol/fixtures/json-corpus.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let hex = case["hex"].as_str().unwrap();
        let bytes: Vec<u8> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let error = decode(&bytes).err().map(|e| format!("{:?}", e.code));
        assert_eq!(error.as_deref(), case["error"].as_str(), "{}", case["name"]);
    }
    assert!(decode(&vec![b' '; MAX_JSON_BYTES + 1]).is_err());
    let cases: Value = serde_json::from_slice(include_bytes!(
        "../../../protocol/fixtures/descriptor-corpus.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let bytes = if let Some(raw) = case["raw"].as_str() {
            raw.as_bytes().to_vec()
        } else {
            let mut bundle: Value = serde_json::from_slice(BUNDLE).unwrap();
            let path = case["path"].as_array().unwrap();
            let mut parent = &mut bundle;
            for part in &path[..path.len() - 1] {
                parent = parent.get_mut(part.as_str().unwrap()).unwrap();
            }
            parent.as_object_mut().unwrap().insert(
                path.last().unwrap().as_str().unwrap().into(),
                case["value"].clone(),
            );
            serde_json::to_vec(&bundle).unwrap()
        };
        let error = AdmittedBundle::parse(&bytes)
            .err()
            .map(|e| format!("{:?}", e.code));
        assert_eq!(error.as_deref(), case["error"].as_str(), "{}", case["name"]);
    }
}

#[test]
fn callback_signature_is_independent_of_number_spelling_and_unicode_key_order() {
    let cases: Value = serde_json::from_slice(include_bytes!(
        "../../../protocol/fixtures/callback-corpus.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let expr: TypeExpr = serde_json::from_value(case["type"].clone()).unwrap();
        assert_eq!(
            callback_key(&expr),
            case["fingerprint"].as_str().unwrap(),
            "{}",
            case["name"]
        );
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
