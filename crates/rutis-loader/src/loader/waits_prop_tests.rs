//! Deterministic randomized / property-based graph tests for
//! [`super::WaitGraph`] and [`crate::loader::status::describe_waits`]
//! (#1326, `docs/design-formal-verification-2026-11-08.md` §2.3 & §3.5).

use std::collections::{HashMap, HashSet, VecDeque};

use proptest::prelude::*;
use rutis::host::{FiberState, Snapshot};

use super::{Fact, MissingDep, Wait, WaitGraph, WaitReason};
use crate::loader::status::describe_waits;
use crate::loader::EntryStatus;
use crate::LoaderError;

const FORBIDDEN_WORDS: &[&str] = &[
    "rutis",
    "Cordis",
    "Rust",
    "binding",
    "session",
    "PluginId",
    "fiber",
    "Pending",
    "Loading",
    "Unresolved",
    "announces",
    "CycleChain",
    "CycleLink",
    "UnknownProvider",
];

/// Distinct service names with pairwise edit distance >= 3 so unrelated
/// services never collide with `closest()` typo suggestions unless a test
/// explicitly constructs a misspelt name.
fn svc_name(index: usize) -> String {
    const NAMES: &[&str] = &[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra",
        "tango", "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
    ];
    NAMES[index % NAMES.len()].to_owned()
}

fn row_name(index: usize) -> String {
    format!("r{index}")
}

fn running(state: FiberState) -> EntryStatus {
    EntryStatus::Running(Snapshot {
        generation: 1,
        state,
        error: None,
    })
}

fn status_from_tag(tag: u8) -> EntryStatus {
    match tag % 7 {
        0 => running(FiberState::Pending),
        1 => running(FiberState::Loading),
        2 => running(FiberState::Active),
        3 => running(FiberState::Failed),
        4 => EntryStatus::Disabled,
        5 => EntryStatus::Inactive,
        _ => EntryStatus::Unresolved(LoaderError::NotFound { name: "mod".into() }),
    }
}

fn isolate_label(tag: u8, row_index: usize) -> Option<String> {
    match tag % 4 {
        0 => None,
        1 => Some("shared_a".into()),
        2 => Some("shared_b".into()),
        _ => Some(format!("priv_{row_index}")),
    }
}

/// Independent reachability oracle over `graph`: shortest hop distance from
/// `from` to `target` along sole-live-provider same-isolate Pending edges
/// (`from` itself is treated as Pending; intermediate nodes must be Pending
/// and not waiting on their own runtime).
fn oracle_shortest_distance(graph: &WaitGraph<'_>, from: &str, target: &str) -> Option<usize> {
    let mut visited: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<(&str, usize)> = VecDeque::new();
    visited.insert(from);
    queue.push_back((from, 0));

    while let Some((curr, dist)) = queue.pop_front() {
        let Some(curr_fact) = graph.fact_of(curr) else {
            continue;
        };
        if !curr_fact.is_pending() {
            continue;
        }
        for dep in graph.service_waits(curr) {
            let Some((next, next_fact)) = graph.edge_target(curr, curr_fact, dep) else {
                continue;
            };
            if next == target {
                return Some(dist + 1);
            }
            if next_fact.is_pending() && visited.insert(next) {
                queue.push_back((next, dist + 1));
            }
        }
    }
    None
}

/// Structural, formatting, bounding, and forbidden-word invariants asserted
/// on every evaluated row of every generated graph.
fn assert_valid_and_bounded(graph: &WaitGraph<'_>, row_id: &str, waits: &[Wait]) {
    let none = |_: &str| None;
    let full_line = describe_waits(waits, &none);
    assert!(!full_line.is_empty());
    // `rutis-host check` is the legitimate CLI command name in pre-existing
    // `WaitReason::NoEnabledProvider`; no other mention of `rutis` or any
    // internal rutis vocabulary may appear.
    let scrubbed_full = full_line.replace("rutis-host check", "");
    for word in FORBIDDEN_WORDS {
        assert!(
            !scrubbed_full.contains(word),
            "forbidden word {word:?} in {full_line:?} (row={row_id}, waits={waits:?})"
        );
    }

    for wait in waits {
        let single = describe_waits(std::slice::from_ref(wait), &none);
        assert!(
            single.len() <= 1024,
            "single wait line unexpectedly large ({} bytes): {single}",
            single.len()
        );
        if matches!(
            wait.reason,
            WaitReason::Cycle { .. }
                | WaitReason::CycleChain { .. }
                | WaitReason::UnknownProvider { .. }
        ) {
            for word in FORBIDDEN_WORDS {
                assert!(
                    !single.contains(word),
                    "forbidden word {word:?} in {single:?} (row={row_id}, wait={wait:?})"
                );
            }
        }
        match &wait.reason {
            WaitReason::Cycle { row, service } => {
                assert_ne!(row, row_id);
                let prov_fact = graph.fact_of(row).expect("cycle provider in facts");
                assert!(prov_fact.is_pending());
                assert!(
                    graph
                        .deps_of(row)
                        .iter()
                        .any(|dep| dep.service() == service),
                    "2-row cycle provider {row} must wait for {service}"
                );
                assert_eq!(
                    single,
                    format!(
                        "waiting for {} (row {row} waits for {service} too: they inject each other)",
                        wait.service
                    )
                );
            }
            WaitReason::CycleChain { links } => {
                assert!(
                    links.len() >= 3,
                    "CycleChain must have >= 3 links: {links:?}"
                );
                assert_eq!(links[0].row, row_id);
                assert_eq!(links[0].service, wait.service);
                let mut seen_rows = HashSet::new();
                for (i, link) in links.iter().enumerate() {
                    assert!(
                        seen_rows.insert(link.row.as_str()),
                        "CycleChain must be a simple cycle: {links:?}"
                    );
                    let next = &links[(i + 1) % links.len()];
                    assert_eq!(
                        link.provider, next.row,
                        "CycleChain links must form a closed loop: {links:?}"
                    );
                    let fact = graph.fact_of(&link.row).expect("cycle member in facts");
                    assert!(
                        fact.is_pending(),
                        "cycle member {} must be Pending",
                        link.row
                    );
                }
                assert!(single.contains("wait for each other:"));
                assert!(single.contains("break the cycle in their inject"));
                if links.len() > 8 {
                    assert!(single.contains("more wait for each other:"));
                    assert!(single.contains("more steps back to"));
                }
            }
            WaitReason::UnknownProvider {
                chain,
                missing,
                candidates,
            } => {
                assert!(
                    !candidates.is_empty(),
                    "UnknownProvider must have candidates"
                );
                let unique: HashSet<&str> = candidates.iter().map(String::as_str).collect();
                assert_eq!(unique.len(), candidates.len(), "candidates must be unique");

                let terminal = if chain.is_empty() {
                    assert_eq!(missing, &wait.service);
                    row_id
                } else {
                    assert_eq!(chain[0].row, row_id);
                    assert_eq!(chain[0].service, wait.service);
                    let mut seen_chain = HashSet::new();
                    for i in 0..chain.len() {
                        assert!(
                            seen_chain.insert(chain[i].row.as_str()),
                            "chain must be acyclic: {chain:?}"
                        );
                        if i + 1 < chain.len() {
                            assert_eq!(chain[i].provider, chain[i + 1].row);
                        }
                    }
                    let last = chain.last().unwrap().provider.as_str();
                    assert!(!seen_chain.contains(last));
                    last
                };

                assert!(
                    !candidates.iter().any(|c| c == terminal),
                    "terminal waiter {terminal} must not appear in its own candidates"
                );
                let terminal_fact = graph.fact_of(terminal);
                assert!(
                    graph.closest_for(terminal, missing).is_none(),
                    "UnknownProvider must never override typo suggestions for {missing}"
                );
                for cand in candidates {
                    let cand_fact = graph.fact_of(cand).expect("candidate in facts");
                    assert!(
                        cand_fact.is_unstarted_unknown(),
                        "candidate {cand} must be unstarted unknown"
                    );
                    assert!(
                        WaitGraph::same_isolate(terminal_fact, cand_fact, missing),
                        "candidate {cand} must match terminal isolate for {missing}"
                    );
                }
                assert!(
                    !single.contains("wait for each other")
                        && !single.contains("they inject each other"),
                    "UnknownProvider must never claim a cycle: {single}"
                );
                if candidates.len() > 3 {
                    assert!(single.contains("more may provide services once they run"));
                }
                if chain.len() > 8 {
                    assert!(single.contains("more steps,"));
                }
                if !chain.is_empty() && candidates.iter().any(|c| c == row_id) {
                    assert!(
                        !single.contains(&format!("unstarted row {row_id}")),
                        "queried row {row_id} must not be referred to in third person: {single}"
                    );
                    assert!(
                        !single.contains(&format!("why {row_id} has not started")),
                        "queried row {row_id} must not be referred to in third person: {single}"
                    );
                    if candidates.len() == 1 {
                        assert!(
                            single.contains(
                                "; this row has not started and may provide services once it runs; \
                                 declare provides on this row or break the inject chain"
                            ),
                            "expected self-only UnknownProvider phrasing: {single}"
                        );
                    } else {
                        assert!(
                            single.contains("; unstarted rows this row, "),
                            "expected `this row` first in multi-candidate UnknownProvider phrasing: {single}"
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

fn eval_all_pending(graph: &WaitGraph<'_>) {
    for (id, fact) in graph.facts {
        if fact.is_pending() {
            let waits = graph.waits_for_row(id, graph.deps_of(id));
            assert_valid_and_bounded(graph, id, &waits);
        }
    }
}

// ---------------------------------------------------------------------------
// 1. Random DAGs: ZERO Cycle / CycleChain false positives
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

    #[test]
    fn prop_random_dags_never_report_cycles(
        n in 2usize..=14,
        status_tags in proptest::collection::vec(any::<u8>(), 14),
        announces_flags in proptest::collection::vec(any::<bool>(), 14),
        unknown_flags in proptest::collection::vec(any::<bool>(), 14),
        iso_tags in proptest::collection::vec(any::<u8>(), 14),
        edge_bits in proptest::collection::vec(any::<bool>(), 14 * 14),
        missing_flags in proptest::collection::vec(any::<bool>(), 14),
    ) {
        let mut facts = Vec::with_capacity(n);
        let mut deps: HashMap<String, Vec<MissingDep>> = HashMap::new();

        for i in 0..n {
            let status = status_from_tag(status_tags[i]);
            let is_disabled = matches!(status, EntryStatus::Disabled);
            let unknown_decl = !is_disabled && unknown_flags[i];
            let unknown_off = is_disabled && unknown_flags[i];
            let provides = if unknown_decl || unknown_off {
                Vec::new()
            } else {
                vec![svc_name(i)]
            };
            let mut isolates = Vec::new();
            if let Some(label) = isolate_label(iso_tags[i], i) {
                for j in 0..n {
                    isolates.push((svc_name(j), label.clone()));
                }
                isolates.push(("unmatched_target".into(), label));
            }
            facts.push((
                row_name(i),
                Fact {
                    status,
                    plugin: None,
                    provides,
                    announces: announces_flags[i],
                    unknown_decl,
                    unknown_off,
                    isolates,
                },
            ));
        }

        // Strict topological order: row i may only wait for services of j > i
        // (guaranteed acyclic DAG) plus an optional undeclared missing service.
        for i in 0..n {
            if !facts[i].1.is_pending() {
                continue;
            }
            let mut row_deps = Vec::new();
            for j in (i + 1)..n {
                if edge_bits[i * 14 + j] {
                    row_deps.push(MissingDep::Missing(svc_name(j)));
                }
            }
            if missing_flags[i] {
                row_deps.push(MissingDep::Missing("unmatched_target".into()));
            }
            deps.insert(row_name(i), row_deps);
        }

        let no_unexported = |_: &str| None;
        let graph = WaitGraph {
            facts: &facts,
            deps: &deps,
            binding_names: &[],
            catalog_names: &[],
            unexported: &no_unexported,
        };

        for (id, fact) in &facts {
            if fact.is_pending() {
                let waits = graph.waits_for_row(id, graph.deps_of(id));
                assert_valid_and_bounded(&graph, id, &waits);
                for wait in &waits {
                    prop_assert!(
                        !matches!(wait.reason, WaitReason::Cycle { .. } | WaitReason::CycleChain { .. }),
                        "DAG falsely reported cycle for {id}: {waits:?}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. 2..=12-node sole-provider cycles + lead-in chains + distractors
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

    #[test]
    fn prop_sole_provider_cycles_2_to_12_nodes_and_lead_ins(
        cycle_len in 2usize..=12,
        lead_len in 0usize..=4,
        distractors in 0usize..=4,
        cycle_iso in 0u8..3,
        distractor_tags in proptest::collection::vec(any::<u8>(), 4),
        distractor_ann in proptest::collection::vec(any::<bool>(), 4),
    ) {
        let shared_iso = match cycle_iso {
            0 => None,
            1 => Some("scope_one".to_owned()),
            _ => Some("scope_two".to_owned()),
        };

        let mut facts = Vec::new();
        let mut deps: HashMap<String, Vec<MissingDep>> = HashMap::new();

        // Cycle rows c_0 .. c_{cycle_len - 1}:
        // c_i provides svc(i) and waits for svc((i + 1) % cycle_len).
        for i in 0..cycle_len {
            let mut isolates = Vec::new();
            if let Some(ref label) = shared_iso {
                for j in 0..cycle_len {
                    isolates.push((svc_name(j), label.clone()));
                }
            }
            let id = format!("c{i}");
            facts.push((
                id.clone(),
                Fact {
                    status: running(FiberState::Pending),
                    plugin: None,
                    provides: vec![svc_name(i)],
                    announces: false,
                    unknown_decl: false,
                    unknown_off: false,
                    isolates,
                },
            ));
            deps.insert(id, vec![MissingDep::Missing(svc_name((i + 1) % cycle_len))]);
        }

        // Lead-in chain: l_0 -> l_1 -> ... -> l_{lead_len - 1} -> c_0.
        // l_k provides svc(cycle_len + k) and waits for:
        //   if k + 1 < lead_len: svc(cycle_len + k + 1)
        //   else: svc(0) (provided by c_0)
        for k in 0..lead_len {
            let mut isolates = Vec::new();
            if let Some(ref label) = shared_iso {
                isolates.push((svc_name(0), label.clone()));
                for m in 0..lead_len {
                    isolates.push((svc_name(cycle_len + m), label.clone()));
                }
            }
            let id = format!("l{k}");
            let target_svc = if k + 1 < lead_len {
                svc_name(cycle_len + k + 1)
            } else {
                svc_name(0)
            };
            facts.push((
                id.clone(),
                Fact {
                    status: running(FiberState::Pending),
                    plugin: None,
                    provides: vec![svc_name(cycle_len + k)],
                    announces: false,
                    unknown_decl: false,
                    unknown_off: false,
                    isolates,
                },
            ));
            deps.insert(id, vec![MissingDep::Missing(target_svc)]);
        }

        // Distractor rows (Disabled, Failed, Active, or announcing in an
        // unrelated isolate) must not break cycle detection or lead-in behavior.
        for d in 0..distractors {
            let status = status_from_tag(distractor_tags[d]);
            let id = format!("x{d}");
            facts.push((
                id.clone(),
                Fact {
                    status,
                    plugin: None,
                    provides: vec![],
                    announces: distractor_ann[d],
                    unknown_decl: false,
                    unknown_off: matches!(status_from_tag(distractor_tags[d]), EntryStatus::Disabled),
                    isolates: vec![(svc_name(0), format!("other_iso_{d}"))],
                },
            ));
            if facts.last().unwrap().1.is_pending() {
                deps.insert(id, vec![MissingDep::Missing("unrelated_svc".into())]);
            }
        }

        let no_unexported = |_: &str| None;
        let graph = WaitGraph {
            facts: &facts,
            deps: &deps,
            binding_names: &[],
            catalog_names: &[],
            unexported: &no_unexported,
        };

        // Check every cycle member.
        for i in 0..cycle_len {
            let id = format!("c{i}");
            let waits = graph.waits_for_row(&id, graph.deps_of(&id));
            assert_valid_and_bounded(&graph, &id, &waits);
            prop_assert_eq!(waits.len(), 1);
            if cycle_len == 2 {
                let other = format!("c{}", (i + 1) % 2);
                prop_assert_eq!(
                    &waits[0].reason,
                    &WaitReason::Cycle {
                        row: other,
                        service: svc_name(i),
                    }
                );
            } else {
                match &waits[0].reason {
                    WaitReason::CycleChain { links } => {
                        prop_assert_eq!(links.len(), cycle_len);
                        prop_assert_eq!(&links[0].row, &id);
                        prop_assert_eq!(&links.last().unwrap().provider, &id);
                    }
                    other => prop_assert!(false, "expected CycleChain for {id}, got {other:?}"),
                }
            }
        }

        // Check every lead-in row stays ProviderWaiting (never Cycle / CycleChain).
        for k in 0..lead_len {
            let id = format!("l{k}");
            let waits = graph.waits_for_row(&id, graph.deps_of(&id));
            assert_valid_and_bounded(&graph, &id, &waits);
            prop_assert_eq!(waits.len(), 1);
            let expected_provider = if k + 1 < lead_len {
                format!("l{}", k + 1)
            } else {
                "c0".to_owned()
            };
            match &waits[0].reason {
                WaitReason::ProviderWaiting { row, .. } => {
                    prop_assert_eq!(row, &expected_provider);
                }
                other => prop_assert!(
                    false,
                    "lead-in row {id} must stay ProviderWaiting, got {other:?}"
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Arbitrary directed graphs vs independent reachability oracle (IFF)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

    #[test]
    fn prop_arbitrary_directed_graphs_terminate_and_match_oracle(
        n in 2usize..=12,
        status_tags in proptest::collection::vec(any::<u8>(), 12),
        announces_flags in proptest::collection::vec(any::<bool>(), 12),
        unknown_flags in proptest::collection::vec(any::<bool>(), 12),
        iso_tags in proptest::collection::vec(any::<u8>(), 12),
        own_rt_flags in proptest::collection::vec(0u8..12, 12),
        provide_bits in proptest::collection::vec(any::<bool>(), 12 * 8),
        wait_bits in proptest::collection::vec(any::<bool>(), 12 * 8),
    ) {
        let num_svcs = 8usize;
        let mut facts = Vec::with_capacity(n);
        let mut deps: HashMap<String, Vec<MissingDep>> = HashMap::new();

        for i in 0..n {
            // Bias towards Pending (~50%) so rich wait graphs are generated.
            let status = if status_tags[i] % 2 == 0 {
                running(FiberState::Pending)
            } else {
                status_from_tag(status_tags[i] / 2)
            };
            let is_disabled = matches!(status, EntryStatus::Disabled);
            let unknown_decl = !is_disabled && unknown_flags[i] && (i % 3 == 0);
            let unknown_off = is_disabled && unknown_flags[i];
            let provides: Vec<String> = if unknown_decl || unknown_off {
                Vec::new()
            } else {
                (0..num_svcs)
                    .filter(|&s| provide_bits[i * num_svcs + s])
                    .map(svc_name)
                    .collect()
            };
            let mut isolates = Vec::new();
            if let Some(label) = isolate_label(iso_tags[i], i) {
                for s in 0..num_svcs {
                    isolates.push((svc_name(s), label.clone()));
                }
            }
            facts.push((
                row_name(i),
                Fact {
                    status,
                    plugin: None,
                    provides,
                    announces: announces_flags[i],
                    unknown_decl,
                    unknown_off,
                    isolates,
                },
            ));
        }

        for i in 0..n {
            if !facts[i].1.is_pending() {
                continue;
            }
            let mut row_deps = Vec::new();
            if own_rt_flags[i] == 0 {
                row_deps.push(MissingDep::OwnRuntime {
                    service: "py".into(),
                    runtime: "py".into(),
                });
            }
            for s in 0..num_svcs {
                if wait_bits[i * num_svcs + s] {
                    row_deps.push(MissingDep::Missing(svc_name(s)));
                }
            }
            deps.insert(row_name(i), row_deps);
        }

        let no_unexported = |_: &str| None;
        let graph = WaitGraph {
            facts: &facts,
            deps: &deps,
            binding_names: &[],
            catalog_names: &[],
            unexported: &no_unexported,
        };

        // Validate every Pending row against both structural invariants and
        // the independent cycle reachability oracle.
        for (id, fact) in &facts {
            if !fact.is_pending() {
                continue;
            }
            let row_deps = graph.deps_of(id);
            let waits = graph.waits_for_row(id, row_deps);
            assert_valid_and_bounded(&graph, id, &waits);

            for (dep, wait) in row_deps.iter().zip(&waits) {
                let MissingDep::Missing(svc) = dep else {
                    continue;
                };
                let entry_fact = graph.fact_of(id);
                let sole = graph.sole_live_provider(id, entry_fact, svc);
                let expected_cycle_len = match sole {
                    Some((prov, prov_fact)) if prov_fact.is_pending() => {
                        let prov_waits_rt = graph
                            .deps_of(prov)
                            .iter()
                            .any(|d| matches!(d, MissingDep::OwnRuntime { .. }));
                        if prov_waits_rt {
                            None
                        } else {
                            oracle_shortest_distance(&graph, prov, id).map(|d| d + 1)
                        }
                    }
                    _ => None,
                };
                match expected_cycle_len {
                    None => prop_assert!(
                        !matches!(wait.reason, WaitReason::Cycle { .. } | WaitReason::CycleChain { .. }),
                        "row {id} waiting on {svc} must NOT report cycle: {wait:?}"
                    ),
                    Some(2) => prop_assert!(
                        matches!(wait.reason, WaitReason::Cycle { .. }),
                        "row {id} waiting on {svc} has 2-cycle, expected Cycle, got {wait:?}"
                    ),
                    Some(k) => match &wait.reason {
                        WaitReason::CycleChain { links } => prop_assert_eq!(links.len(), k),
                        other => prop_assert!(
                            false,
                            "row {id} waiting on {svc} has {k}-cycle, got {other:?}"
                        ),
                    },
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Unknown-provider chains: never claim a cycle, rank suspects first,
//    and never override typo suggestions (`did you mean`)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

    #[test]
    fn prop_unknown_provider_chains_never_claim_cycle_and_respect_typos(
        chain_len in 0usize..=10,
        suspect_count in 0usize..=3,
        unrelated_count in 1usize..=5,
        use_typo in any::<bool>(),
        shared_iso_tag in 0u8..3,
    ) {
        let shared_iso = match shared_iso_tag {
            0 => None,
            1 => Some("iso_x".to_owned()),
            _ => Some("iso_y".to_owned()),
        };
        let mut facts = Vec::new();
        let mut deps: HashMap<String, Vec<MissingDep>> = HashMap::new();

        // Missing service at the end of the chain:
        // When `use_typo` is true, wait for "waether" while an Active row
        // declares "weather" (edit distance 1 -> typo must win!).
        let missing_svc = if use_typo { "waether" } else { "unannounced_leaf" };

        let make_isolates = |extra: &[&str]| -> Vec<(String, String)> {
            let Some(ref label) = shared_iso else {
                return Vec::new();
            };
            let mut out = vec![(missing_svc.to_owned(), label.clone())];
            for s in extra {
                out.push(((*s).to_owned(), label.clone()));
            }
            for i in 0..=chain_len {
                out.push((format!("hop_svc_{i}"), label.clone()));
            }
            out
        };

        // Active row declaring "weather" (for typo case) + announces: true
        // (Active rows must NEVER be included in unstarted candidates).
        facts.push((
            "active_weather".into(),
            Fact {
                status: running(FiberState::Active),
                plugin: None,
                provides: vec!["weather".into()],
                announces: true,
                unknown_decl: false,
                unknown_off: false,
                isolates: make_isolates(&["weather"]),
            },
        ));

        // Unrelated unstarted announcing rows placed BEFORE suspects in tree
        // order so we prove Tier-1 suspect ranking beats tree order.
        for u in 0..unrelated_count {
            let id = format!("unrel{u}");
            let status = match u % 3 {
                0 => running(FiberState::Pending),
                1 => running(FiberState::Loading),
                _ => EntryStatus::Unresolved(LoaderError::NotFound { name: "x".into() }),
            };
            let is_pending = matches!(status, EntryStatus::Running(Snapshot { state: FiberState::Pending, .. }));
            facts.push((
                id.clone(),
                Fact {
                    status,
                    plugin: None,
                    provides: vec![],
                    announces: true,
                    unknown_decl: false,
                    unknown_off: false,
                    isolates: make_isolates(&[]),
                },
            ));
            if is_pending {
                deps.insert(id, vec![MissingDep::Missing("some_other_missing".into())]);
            }
        }

        // Chain: h_0 -> hop_svc_1 -> h_1 -> ... -> hop_svc_{chain_len} -> h_{chain_len} -> missing_svc.
        for i in 0..=chain_len {
            let id = format!("h{i}");
            let provides = if i == 0 {
                vec![]
            } else {
                vec![format!("hop_svc_{i}")]
            };
            let waits_on = if i < chain_len {
                format!("hop_svc_{}", i + 1)
            } else {
                missing_svc.to_owned()
            };
            facts.push((
                id.clone(),
                Fact {
                    status: running(FiberState::Pending),
                    plugin: None,
                    provides,
                    announces: false,
                    unknown_decl: false,
                    unknown_off: false,
                    isolates: make_isolates(&[]),
                },
            ));
            deps.insert(id, vec![MissingDep::Missing(waits_on)]);
        }

        // Suspect unstarted announcing rows that wait on `h_{chain_len}`
        // (directly reaching the terminal waiter when chain_len >= 1).
        let mut expected_suspects = Vec::new();
        if chain_len >= 1 {
            for s in 0..suspect_count {
                let id = format!("suspect{s}");
                expected_suspects.push(id.clone());
                facts.push((
                    id.clone(),
                    Fact {
                        status: running(FiberState::Pending),
                        plugin: None,
                        provides: vec![],
                        announces: true,
                        unknown_decl: false,
                        unknown_off: false,
                        isolates: make_isolates(&[]),
                    },
                ));
                deps.insert(id, vec![MissingDep::Missing(format!("hop_svc_{chain_len}"))]);
            }
        }

        let no_unexported = |_: &str| None;
        let graph = WaitGraph {
            facts: &facts,
            deps: &deps,
            binding_names: &[],
            catalog_names: &[],
            unexported: &no_unexported,
        };

        eval_all_pending(&graph);

        let head_waits = graph.waits_for_row("h0", graph.deps_of("h0"));
        let tail_id = format!("h{chain_len}");
        let tail_waits = graph.waits_for_row(&tail_id, graph.deps_of(&tail_id));

        if use_typo {
            // Terminal row MUST get typo suggestion, NEVER UnknownProvider.
            prop_assert_eq!(
                &tail_waits[0].reason,
                &WaitReason::NoProvider {
                    like: Some("weather".into()),
                }
            );
            // Upstream row must NOT emit UnknownProvider for a typo.
            prop_assert!(
                !matches!(head_waits[0].reason, WaitReason::UnknownProvider { .. }),
                "typo must not produce UnknownProvider on upstream row: {head_waits:?}"
            );
        } else {
            match &head_waits[0].reason {
                WaitReason::UnknownProvider {
                    chain,
                    missing,
                    candidates,
                } => {
                    prop_assert_eq!(chain.len(), chain_len);
                    prop_assert_eq!(missing, missing_svc);
                    prop_assert!(!candidates.contains(&"active_weather".to_owned()));
                    // Every suspect (Tier 1) must appear strictly before all
                    // unrelated candidates (Tier 2).
                    for (idx, suspect) in expected_suspects.iter().enumerate() {
                        prop_assert_eq!(&candidates[idx], suspect);
                    }
                }
                other => prop_assert!(
                    false,
                    "expected UnknownProvider for h0, got {other:?}"
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Cross-isolate rings never report false cycles
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn prop_cross_isolate_rings_never_form_false_cycles(
        ring_len in 2usize..=8,
        broken_edge in 0usize..8,
    ) {
        let broken = broken_edge % ring_len;
        let mut facts = Vec::new();
        let mut deps: HashMap<String, Vec<MissingDep>> = HashMap::new();

        for i in 0..ring_len {
            let id = format!("r{i}");
            let next_svc = svc_name((i + 1) % ring_len);
            let mut isolates = Vec::new();
            if i == broken {
                // Row `broken` isolates the service it waits for, so the next
                // row's unisolated declaration cannot satisfy it.
                isolates.push((next_svc.clone(), format!("priv_{i}")));
            }
            facts.push((
                id.clone(),
                Fact {
                    status: running(FiberState::Pending),
                    plugin: None,
                    provides: vec![svc_name(i)],
                    announces: false,
                    unknown_decl: false,
                    unknown_off: false,
                    isolates,
                },
            ));
            deps.insert(id, vec![MissingDep::Missing(next_svc)]);
        }

        let no_unexported = |_: &str| None;
        let graph = WaitGraph {
            facts: &facts,
            deps: &deps,
            binding_names: &[],
            catalog_names: &[],
            unexported: &no_unexported,
        };

        for i in 0..ring_len {
            let id = format!("r{i}");
            let waits = graph.waits_for_row(&id, graph.deps_of(&id));
            assert_valid_and_bounded(&graph, &id, &waits);
            for wait in &waits {
                prop_assert!(
                    !matches!(wait.reason, WaitReason::Cycle { .. } | WaitReason::CycleChain { .. }),
                    "cross-isolate ring falsely reported cycle for {id}: {waits:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 6. Alternative live provider suppresses false cycles
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn prop_alternative_live_provider_suppresses_false_cycles(
        ring_len in 2usize..=8,
        alt_svc_idx in 0usize..8,
        alt_state_tag in 0u8..3,
    ) {
        let alt_idx = alt_svc_idx % ring_len;
        let mut facts = Vec::new();
        let mut deps: HashMap<String, Vec<MissingDep>> = HashMap::new();

        for i in 0..ring_len {
            let id = format!("r{i}");
            facts.push((
                id.clone(),
                Fact {
                    status: running(FiberState::Pending),
                    plugin: None,
                    provides: vec![svc_name(i)],
                    announces: false,
                    unknown_decl: false,
                    unknown_off: false,
                    isolates: vec![],
                },
            ));
            deps.insert(id, vec![MissingDep::Missing(svc_name((i + 1) % ring_len))]);
        }

        // Second live provider of `svc_name(alt_idx)` that is NOT part of the
        // ring (Loading, Active, or Pending on an external service).
        let alt_status = match alt_state_tag {
            0 => running(FiberState::Loading),
            1 => running(FiberState::Active),
            _ => running(FiberState::Pending),
        };
        facts.push((
            "alt".into(),
            Fact {
                status: alt_status,
                plugin: None,
                provides: vec![svc_name(alt_idx)],
                announces: false,
                unknown_decl: false,
                unknown_off: false,
                isolates: vec![],
            },
        ));
        if alt_state_tag == 2 {
            deps.insert("alt".into(), vec![MissingDep::Missing("external_dep".into())]);
        }

        let no_unexported = |_: &str| None;
        let graph = WaitGraph {
            facts: &facts,
            deps: &deps,
            binding_names: &[],
            catalog_names: &[],
            unexported: &no_unexported,
        };

        for i in 0..ring_len {
            let id = format!("r{i}");
            let waits = graph.waits_for_row(&id, graph.deps_of(&id));
            assert_valid_and_bounded(&graph, &id, &waits);
            for wait in &waits {
                prop_assert!(
                    !matches!(wait.reason, WaitReason::Cycle { .. } | WaitReason::CycleChain { .. }),
                    "ring with alternative live provider falsely reported cycle for {id}: {waits:?}"
                );
            }
        }
    }
}
