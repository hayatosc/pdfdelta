//! Test-only retrieval experiment; fixtures and budgets belong to the benchmark manifest.

use super::*;
use crate::document::{IdentityKey, NodeId, solve_correspondence_scope};
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Matrix {
    universe_version: String,
    max_token_visits: usize,
    max_proposals: usize,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    size: usize,
    pattern: String,
}

fn fixture(case: &Case, new: bool) -> DocumentGraph {
    let texts: Vec<_> = (0..case.size)
        .map(|i| match case.pattern.as_str() {
            "near_equal" | "mixed_keys" => {
                format!("Paragraph {i:04}: value {}", if new { 2 } else { 1 })
            }
            "rewrite" => {
                let base = if new { b'A' } else { b'a' };
                (0..6)
                    .map(|p| char::from(base + ((i >> (p * 2)) % 4) as u8))
                    .collect()
            }
            "repeated" => format!("Repeated statement {}", if new { 'B' } else { 'A' }),
            _ => panic!("unknown matrix pattern"),
        })
        .collect();
    let mut graph = tests::graph(&texts.iter().map(String::as_str).collect::<Vec<_>>());
    if case.pattern == "mixed_keys" {
        for (i, node) in graph.nodes.iter_mut().skip(1).enumerate() {
            if i.is_multiple_of(2) {
                node.identity = Some(IdentityKey {
                    namespace: "measurement".into(),
                    value: i.to_string(),
                });
            }
        }
    }
    graph
}

#[test]
#[ignore = "isolated release measurement driven by measure-text.py"]
fn measure_text_retrieval() {
    let matrix: Matrix = serde_json::from_str(include_str!(
        "../../../../../benchmark/realworld/next/text-matrix.json"
    ))
    .expect("registered matrix");
    assert_eq!(matrix.universe_version, "literal-text-retrieval-v1");
    let id = std::env::var("PDFDELTA_TEXT_CASE").expect("case");
    let mode = std::env::var("PDFDELTA_TEXT_MODE").expect("mode");
    assert!(matches!(mode.as_str(), "dense" | "indexed"));
    let case = matrix
        .cases
        .iter()
        .find(|case| case.id == id)
        .expect("registered case");
    assert!(matches!(case.size, 32 | 128 | 512 | 2048));
    let start = std::time::Instant::now();
    let old = fixture(case, false);
    let new = fixture(case, true);
    let limits = TextCandidateLimits {
        max_token_visits: matrix.max_token_visits,
        ..Default::default()
    };
    let matching = MatchingLimits {
        max_proposals: matrix.max_proposals,
        max_pair_checks: matrix.max_proposals,
        ..Default::default()
    };
    let mut search = tests::search();
    let left_nodes: Vec<_> = old.nodes.iter().skip(1).collect();
    let right_nodes: Vec<_> = new.nodes.iter().skip(1).collect();
    let left = features(&left_nodes, &mut search, limits).expect("bounded fixture features");
    let mut right = features(&right_nodes, &mut search, limits).expect("bounded fixture features");
    let mut candidates = tests::candidates();
    if mode == "indexed" {
        append_indexed_pairs(
            &left,
            &mut right,
            &mut candidates,
            &mut search,
            matching,
            limits,
        );
    } else {
        append_dense_pairs(
            &left,
            &right,
            &mut candidates,
            &mut search,
            matching,
            limits,
        );
    }
    let retrieval_seconds = start.elapsed().as_secs_f64();
    let generated = candidates.proposals.len();
    // Mirror the supplier's whole-suffix withdrawal; an incomplete prefix is not a universe.
    if !search.exhaustive {
        candidates.proposals.clear();
    }
    let mut hasher = Sha256::new();
    for proposal in &candidates.proposals {
        hasher.update(serde_json::to_vec(proposal).expect("proposal digest"));
    }
    let candidate_sha256: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let solve_start = std::time::Instant::now();
    let solved = solve_correspondence_scope(
        &old,
        &new,
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        &candidates.proposals,
        matching,
    );
    let optimization_seconds = solve_start.elapsed().as_secs_f64();
    let complete = search.exhaustive
        && solved.as_ref().is_ok_and(|s| {
            s.conflict_search_complete && s.components.iter().all(|component| component.exhaustive)
        });
    println!(
        "TEXT_MEASUREMENT {}",
        serde_json::json!({
            "case": id, "mode": mode, "complete": complete,
            "retrieval_complete": search.exhaustive, "candidate_checks": search.examined_pairs,
            "token_visits": search.token_visits, "feature_entries": search.feature_entries,
            "index_entries": search.index_entries, "generated_proposals": generated,
            "retained_proposals": candidates.proposals.len(), "candidate_sha256": candidate_sha256,
            "weight_one_proposals": candidates.proposals.iter().filter(|p| p.weight == 1).count(),
            "assignment_work": solved.as_ref().ok().map(|s| s.components.iter().map(|c| c.assignment_work).sum::<usize>()),
            "optimization_error": solved.as_ref().err().map(ToString::to_string),
            "pricing_rounds": 0, "optimization_runs": null,
            "optimization_runs_note": "production solver does not expose root/exclusion solve counts",
            "retrieval_seconds": retrieval_seconds, "optimization_seconds": optimization_seconds,
            "kernel_seconds": start.elapsed().as_secs_f64(),
            "mandatory": solved.as_ref().ok().map(|s| s.components.iter().flat_map(|c| c.mandatory.iter()).collect::<Vec<_>>())
        })
    );
}
