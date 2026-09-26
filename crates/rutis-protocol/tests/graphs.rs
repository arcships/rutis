use rutis_protocol::contract::{callback_key, AdmittedBundle, TypeExpr};
use rutis_protocol::graph::{DecodedValue, GraphScopes, WireGraph};
use rutis_protocol::imports::{ImportControl, Imports};
use serde_json::{json, Value};

fn bundle() -> AdmittedBundle {
    AdmittedBundle::parse(include_bytes!(
        "../../../protocol/fixtures/database.bundle.json"
    ))
    .unwrap()
}
fn scopes() -> GraphScopes {
    let scope = serde_json::from_value(
        json!({"activation":{"runtime":"node","epoch":"1","activation":"1"},"scope":"1"}),
    )
    .unwrap();
    let borrow = serde_json::from_value(
        json!({"activation":{"runtime":"node","epoch":"1","activation":"1"},"scope":"2"}),
    )
    .unwrap();
    GraphScopes { scope, borrow }
}
fn graph_value(bundle: &AdmittedBundle, mut value: Value) -> Value {
    if let Some(references) = value.get_mut("references").and_then(Value::as_array_mut) {
        for reference in references {
            let view = &mut reference["delivery"]["view"];
            if view["bundle_sha256"] == "$bundle" {
                view["bundle_sha256"] = json!(bundle.sha256());
            }
            if view["interface"] == "$callback" {
                view["interface"] = json!(callback_key(
                    &bundle.bundle().interfaces["Database"].methods["withCallback"].params
                ));
            }
        }
    }
    value
}
fn cycle(bundle: &AdmittedBundle) -> WireGraph {
    serde_json::from_value(graph_value(
        bundle,
        serde_json::from_slice(include_bytes!(
            "../../../protocol/fixtures/session.graph.json"
        ))
        .unwrap(),
    ))
    .unwrap()
}
fn session_expr(bundle: &AdmittedBundle) -> &TypeExpr {
    &bundle.bundle().interfaces["Agent"].properties["session"]
}
fn imports(scopes: &GraphScopes) -> Imports {
    let imports = Imports::default();
    imports.open_scope(scopes.scope.clone(), None).unwrap();
    imports
        .open_scope(
            scopes.borrow.clone(),
            (scopes.borrow.activation == scopes.scope.activation).then(|| scopes.scope.clone()),
        )
        .unwrap();
    imports
}
fn renew(graph: &mut WireGraph, start: u64) {
    for (i, reference) in graph.references.iter_mut().enumerate() {
        reference.delivery.id.0 = start + i as u64;
        reference.delivery.token = format!("fixture-{}", reference.delivery.id.0);
    }
}

#[test]
fn t22_shared_graph_contract_and_ownership_corpus() {
    let bundle = bundle();
    let cases: Value = serde_json::from_slice(include_bytes!(
        "../../../protocol/fixtures/graph-corpus.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let scopes = GraphScopes {
            scope: serde_json::from_value(case["scopes"]["scope"].clone()).unwrap(),
            borrow: serde_json::from_value(case["scopes"]["borrow"].clone()).unwrap(),
        };
        let imports = imports(&scopes);
        let expr: TypeExpr = serde_json::from_value(case["type"].clone()).unwrap();
        let value = rutis_protocol::json::decode(
            &serde_json::to_vec(&graph_value(&bundle, case["graph"].clone())).unwrap(),
        )
        .unwrap();
        let result = imports
            .receive_graph_value(&bundle, &expr, value, &scopes)
            .map_err(|e| format!("{:?}", e.code));
        assert_eq!(
            result.err().as_deref(),
            case["error"].as_str(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn t04_t10_cycles_and_reordered_deliveries_keep_identity_without_arc_cycles() {
    let bundle = bundle();
    let scopes = scopes();
    let imports = imports(&scopes);
    let first = imports
        .receive_graph(&bundle, session_expr(&bundle), cycle(&bundle), &scopes)
        .unwrap()
        .into_object()
        .unwrap();
    let agent = first.property("agent").unwrap().into_object().unwrap();
    let back = agent.property("session").unwrap().into_object().unwrap();
    assert!(first.same_wrapper(&back));
    assert_eq!(imports.retained_objects(), 2);
    assert!(first.property("unknown").is_err());
    // Wire indexes are local to a message. A different ordering and fresh
    // delivery ids do not change the underlying immutable relationship.
    let mut graph = cycle(&bundle);
    renew(&mut graph, 3);
    graph.references.swap(0, 1);
    graph.root = serde_json::from_value(json!({"kind":"ref","index":1})).unwrap();
    *graph.references[0].properties.get_mut("session").unwrap() =
        serde_json::from_value(json!({"kind":"ref","index":1})).unwrap();
    *graph.references[1].properties.get_mut("agent").unwrap() =
        serde_json::from_value(json!({"kind":"ref","index":0})).unwrap();
    let repeated = imports
        .receive_graph(&bundle, session_expr(&bundle), graph, &scopes)
        .unwrap()
        .into_object()
        .unwrap();
    assert!(first.same_wrapper(&repeated));
    agent.release();
    assert!(first.property("agent").is_err());
    assert!(back.same_wrapper(&first));
    assert_eq!(imports.retained_objects(), 1);
    let mut next = cycle(&bundle);
    renew(&mut next, 5);
    let fresh_parent = imports
        .receive_graph(&bundle, session_expr(&bundle), next, &scopes)
        .unwrap()
        .into_object()
        .unwrap();
    let fresh_agent = fresh_parent
        .property("agent")
        .unwrap()
        .into_object()
        .unwrap();
    assert!(first.same_wrapper(&fresh_parent));
    assert!(agent.same_object(&fresh_agent));
    assert!(!agent.same_wrapper(&fresh_agent));
    assert!(agent.delivery().is_err());
    imports.close_scope(&scopes.scope);
    assert_eq!(imports.retained_objects(), 0);
    assert!(fresh_agent.property("session").is_err());
    assert!(first.property("agent").is_err());
}

#[test]
fn t07_invalid_snapshot_releases_new_grants_without_changing_existing_graph() {
    let bundle = bundle();
    let scopes = scopes();
    let imports = imports(&scopes);
    let first = imports
        .receive_graph(&bundle, session_expr(&bundle), cycle(&bundle), &scopes)
        .unwrap()
        .into_object()
        .unwrap();
    let original = first.property("agent").unwrap().into_object().unwrap();
    imports.take_controls();
    let mut changed = cycle(&bundle);
    renew(&mut changed, 3);
    changed.references[1].delivery.object.object.0 = 99;
    assert!(imports
        .receive_graph(&bundle, session_expr(&bundle), changed, &scopes)
        .is_err());
    let controls = imports.take_controls();
    assert_eq!(controls.len(), 2);
    assert!(controls
        .iter()
        .all(|control| matches!(control, ImportControl::Release { .. })));
    assert!(first
        .property("agent")
        .unwrap()
        .into_object()
        .unwrap()
        .same_wrapper(&original));
    assert!(first.delivery().is_ok());
    assert!(original.delivery().is_ok());
    assert_eq!(imports.retained_objects(), 2);
}

#[test]
fn t06_t08_borrow_graph_expires_without_closing_persistent_aliases() {
    let bundle = bundle();
    let scopes = scopes();
    let imports = imports(&scopes);
    let cases: Value = serde_json::from_slice(include_bytes!(
        "../../../protocol/fixtures/graph-corpus.json"
    ))
    .unwrap();
    let case = cases
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "same identity with independent durable and borrow grants")
        .unwrap();
    let graph: WireGraph =
        serde_json::from_value(graph_value(&bundle, case["graph"].clone())).unwrap();
    let expr: TypeExpr = serde_json::from_value(case["type"].clone()).unwrap();
    let DecodedValue::Record(mut fields) = imports
        .receive_graph(&bundle, &expr, graph, &scopes)
        .unwrap()
    else {
        panic!("record")
    };
    let durable = fields.remove("durable").unwrap().into_object().unwrap();
    let temporary = fields.remove("temporary").unwrap().into_object().unwrap();
    assert!(durable.same_object(&temporary));
    assert!(!durable.same_wrapper(&temporary));
    let child = temporary.property("agent").unwrap().into_object().unwrap();
    assert_eq!(child.scope(), scopes.borrow);
    imports.close_scope(&scopes.borrow);
    assert!(temporary.delivery().is_err());
    assert!(child.delivery().is_err());
    assert!(durable.property("agent").is_ok());
    assert_eq!(imports.retained_objects(), 2);
    imports.close_scope(&scopes.scope);
    assert_eq!(imports.retained_objects(), 0);
}

#[test]
fn a_result_without_references_is_still_rejected_after_receiving_scope_closes() {
    let bundle = bundle();
    let scopes = scopes();
    let imports = imports(&scopes);
    imports.close_scope(&scopes.scope);
    let graph =
        serde_json::from_value(json!({"root":{"kind":"value","value":null},"references":[]}))
            .unwrap();
    let expr = serde_json::from_value(json!({"kind":"value","schema":{"type":"null"}})).unwrap();
    assert!(imports
        .receive_graph(&bundle, &expr, graph, &scopes)
        .is_err());
}
