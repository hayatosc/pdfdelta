//! Test-only entry point for the benchmark-owned immutable coefficient matrix.

use std::collections::BTreeSet;

use super::{Assignment, Budget, Edge, PricingPass, Score, Trial, trial_with};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Matrix {
    universe_version: String,
    contract: String,
    work_limit: usize,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    size: usize,
    pattern: Pattern,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Pattern {
    NearEqual,
    Rewrite,
    Repeated,
    MixedKeys,
}

impl Case {
    fn edge(&self, row: usize, column: usize) -> Option<Edge> {
        let strong = if row == column { 1_000_001 } else { 1 };
        let (class, weight) = match self.pattern {
            Pattern::NearEqual => (4, strong),
            Pattern::Rewrite => (4, 1),
            Pattern::Repeated => (1, 1),
            Pattern::MixedKeys if row.is_multiple_of(2) || column.is_multiple_of(2) => {
                if !row.is_multiple_of(2) || !column.is_multiple_of(2) || row / 4 != column / 4 {
                    return None;
                }
                (0, 1)
            }
            Pattern::MixedKeys => (4, strong),
        };
        let mut cost = Score::default();
        cost.0[class] = -weight;
        Some(Edge {
            proposal: row * self.size + column,
            cost,
        })
    }
}

/// Builds the entire same universe once, then uses the production optimizer
/// for the root and each forbidden edge. Construction and all solves share a
/// work budget, as in the priced trial. This is a kernel reference, not PDF I/O.
pub(super) fn dense_trial_with(
    rows: usize,
    columns: usize,
    edge_at: impl Fn(usize, usize) -> Option<Edge>,
    work_limit: usize,
) -> Trial {
    let mut budget = Budget {
        remaining: work_limit,
    };
    let mut result = Trial {
        cost: None,
        mandatory: Vec::new(),
        complete: false,
        passes: vec![PricingPass::default()],
        work: 0,
    };
    let mut certify = || -> Option<Vec<usize>> {
        budget.spend(rows.checked_add(columns)?)?;
        let mut problem = Assignment {
            rows: (0..rows).map(|_| Default::default()).collect(),
            real_columns: columns,
        };
        let construction = &mut result.passes[0];
        for (row, entries) in problem.rows.iter_mut().enumerate() {
            for column in 0..columns {
                budget.spend(1)?;
                construction.candidate_checks += 1;
                construction.universe_lookups += 1;
                if let Some(edge) = edge_at(row, column) {
                    entries.insert(column, edge);
                    construction.max_active_edges += 1;
                }
            }
        }
        construction.optimization_runs = 1;
        let optimum = problem.optimum(&BTreeSet::new(), &mut budget)?;
        construction.complete = true;
        result.cost = Some(optimum.cost);
        let active_edges = construction.max_active_edges;
        let mut mandatory = Vec::new();
        for proposal in optimum.selected {
            let without = problem.optimum(&BTreeSet::from([proposal]), &mut budget);
            result.passes.push(PricingPass {
                forbidden: Some(proposal),
                optimization_runs: 1,
                complete: without.is_some(),
                max_active_edges: active_edges,
                ..PricingPass::default()
            });
            match without?.cost.cmp(&optimum.cost) {
                std::cmp::Ordering::Greater => mandatory.push(proposal),
                std::cmp::Ordering::Equal => {}
                std::cmp::Ordering::Less => return None,
            }
        }
        mandatory.sort_unstable();
        Some(mandatory)
    };
    if let Some(mandatory) = certify() {
        result.mandatory = mandatory;
        result.complete = true;
    }
    result.work = work_limit - budget.remaining;
    result
}

#[test]
#[ignore = "explicit benchmark driver supplies case and mode; not a regression test"]
fn measure_assignment_pricing() {
    let matrix: Matrix = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../benchmark/microbench/pricing-matrix.json"
    )))
    .expect("versioned benchmark manifest");
    assert_eq!(matrix.universe_version, "independent-coefficients-v1");
    let id = std::env::var("PDFDELTA_PRICING_CASE").expect("case ID");
    let mode = std::env::var("PDFDELTA_PRICING_MODE").expect("dense or priced");
    let case = matrix
        .cases
        .iter()
        .find(|case| case.id == id)
        .expect("registered case");
    assert!((1..=2048).contains(&case.size));
    let started = std::time::Instant::now();
    let result = match mode.as_str() {
        "dense" => dense_trial_with(
            case.size,
            case.size,
            |r, c| case.edge(r, c),
            matrix.work_limit,
        ),
        "priced" => trial_with(
            case.size,
            case.size,
            |r, c| case.edge(r, c),
            matrix.work_limit,
        ),
        _ => panic!("unknown measurement mode"),
    };
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "PRICING_MEASUREMENT {}",
        serde_json::json!({
            "case": id,
            "mode": mode,
            "universe_version": matrix.universe_version,
            "contract": matrix.contract,
            "rows": case.size,
            "columns": case.size,
            "work_limit": matrix.work_limit,
            "work": result.work,
            "complete": result.complete,
            "cost": result.cost.map(|score| score.0),
            "mandatory": result.mandatory,
            "kernel_seconds": elapsed,
            "passes": result.passes,
        })
    );
}
