//! Experimental pricing of independent 1:1 assignments, excluded from production.
//!
//! A fixed immutable universe is queried by coordinate. The active assignment
//! stores only selected/priced edges; rows, columns and dummy choices never shrink.
//! This does not close any earlier candidate supplier's incomplete universe.

use std::collections::BTreeSet;

use super::{Assignment, Budget, Edge, Optimum, Score};

mod measurement;

#[derive(Default, serde::Serialize)]
pub(super) struct PricingPass {
    pub forbidden: Option<usize>,
    pub complete: bool,
    pub optimization_runs: usize,
    pub pricing_rounds: usize,
    pub candidate_checks: usize,
    pub universe_lookups: usize,
    pub added_edges: usize,
    pub max_active_edges: usize,
    /// Nonnegative omitted edges in the final, complete dual check only.
    pub safely_omitted_edges: usize,
}

pub(super) struct Trial {
    pub cost: Option<Score>,
    pub mandatory: Vec<usize>,
    pub complete: bool,
    pub passes: Vec<PricingPass>,
    pub work: usize,
}

/// Diagonal seeds are a search convention, not a correspondence assertion.
/// Every omitted non-forbidden edge is checked against the current exact duals.
fn price(
    rows: usize,
    columns: usize,
    edge_at: &impl Fn(usize, usize) -> Option<Edge>,
    forbidden: &BTreeSet<usize>,
    budget: &mut Budget,
    pass: &mut PricingPass,
) -> Option<Optimum> {
    budget.spend(rows.checked_add(columns)?)?;
    let mut active = Assignment {
        rows: (0..rows).map(|_| Default::default()).collect(),
        real_columns: columns,
    };
    let mut active_edges = 0;
    for row in 0..rows.min(columns) {
        budget.spend(1)?;
        pass.candidate_checks += 1;
        pass.universe_lookups += 1;
        if let Some(edge) = edge_at(row, row)
            && !forbidden.contains(&edge.proposal)
        {
            active.rows[row].insert(row, edge);
            active_edges += 1;
            pass.max_active_edges = active_edges;
        }
    }
    loop {
        pass.optimization_runs += 1;
        let optimum = active.optimum(forbidden, budget)?;
        pass.pricing_rounds += 1;
        let mut additions = 0usize;
        let mut safely_omitted = 0usize;
        for (row, active_row) in active.rows.iter_mut().enumerate() {
            for column in 0..columns {
                budget.spend(1)?;
                pass.candidate_checks += 1;
                if active_row.contains_key(&column) {
                    continue;
                }
                pass.universe_lookups += 1;
                let Some(edge) = edge_at(row, column) else {
                    continue;
                };
                if forbidden.contains(&edge.proposal) {
                    continue;
                }
                let reduced = edge
                    .cost
                    .sub(optimum.row_potential[row + 1])?
                    .sub(optimum.column_potential[column + 1])?;
                if reduced < Score::default() {
                    active_row.insert(column, edge);
                    additions += 1;
                    pass.added_edges += 1;
                    active_edges += 1;
                    pass.max_active_edges = active_edges;
                } else {
                    safely_omitted += 1;
                }
            }
        }
        if additions == 0 {
            pass.safely_omitted_edges = safely_omitted;
            pass.complete = true;
            return Some(optimum);
        }
    }
}

pub(super) fn trial(problem: &Assignment, work_limit: usize) -> Trial {
    let edge_at = |row: usize, column| problem.rows[row].get(&column).copied();
    trial_with(
        problem.rows.len(),
        problem.real_columns,
        edge_at,
        work_limit,
    )
}

pub(super) fn dense_trial(problem: &Assignment, work_limit: usize) -> Trial {
    measurement::dense_trial_with(
        problem.rows.len(),
        problem.real_columns,
        |row, column| problem.rows[row].get(&column).copied(),
        work_limit,
    )
}

/// The caller supplies a fixed, independent universe. This callback must not
/// change cost, presence or proposal IDs between calls. Groups, shared sources
/// and alternative partitions are outside this experiment's model.
fn trial_with(
    rows: usize,
    columns: usize,
    edge_at: impl Fn(usize, usize) -> Option<Edge>,
    work_limit: usize,
) -> Trial {
    let mut budget = Budget::new(work_limit);
    let mut result = Trial {
        cost: None,
        mandatory: Vec::new(),
        complete: false,
        passes: Vec::new(),
        work: 0,
    };
    let mut certify = || -> Option<Vec<usize>> {
        result.passes.push(PricingPass::default());
        let optimum = price(
            rows,
            columns,
            &edge_at,
            &BTreeSet::new(),
            &mut budget,
            &mut result.passes[0],
        )?;
        result.cost = Some(optimum.cost);
        let mut mandatory = Vec::new();
        for proposal in optimum.selected {
            let mut pass = PricingPass {
                forbidden: Some(proposal),
                ..PricingPass::default()
            };
            let without = price(
                rows,
                columns,
                &edge_at,
                &BTreeSet::from([proposal]),
                &mut budget,
                &mut pass,
            );
            result.passes.push(pass);
            let without = without?;
            match without.cost.cmp(&optimum.cost) {
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
    result.work = work_limit - budget.remaining();
    result
}

#[test]
fn omitted_zero_reduced_cost_edges_destroy_restricted_necessity() {
    let edge_at = |row, column| {
        Some(Edge {
            proposal: row * 2 + column,
            cost: Score([0, 0, 0, 0, 0, -1]),
        })
    };
    let result = trial_with(2, 2, edge_at, 100_000);
    assert!(result.complete);
    assert_eq!(result.cost, Some(Score([0, 0, 0, 0, 0, -2])));
    assert!(result.mandatory.is_empty());
    // The original objective needs no added edge. Reusing its certificate to
    // claim necessity would incorrectly certify both diagonal seeds.
    assert_eq!(result.passes[0].added_edges, 0);
    assert_eq!(result.passes[0].safely_omitted_edges, 2);
    assert_eq!(result.passes.len(), 3);
    for pass in &result.passes[1..] {
        assert!(pass.forbidden.is_some() && pass.complete);
        assert!(pass.added_edges > 0 && pass.pricing_rounds > 1);
    }
}

#[test]
fn incomplete_pricing_never_certifies_mandatory_edges() {
    let edge_at = |row, column| {
        Some(Edge {
            proposal: row * 3 + column,
            cost: Score([0, 0, 0, 0, 0, if row == column { -2 } else { -1 }]),
        })
    };
    let complete = trial_with(3, 3, edge_at, 1_000_000);
    assert!(complete.complete);
    assert_eq!(complete.mandatory, [0, 4, 8]);
    for limit in 0..complete.work {
        let result = trial_with(3, 3, edge_at, limit);
        assert!(!result.complete, "limit={limit}");
        assert!(result.mandatory.is_empty());
        assert!(result.work <= limit);
        assert!(result.passes.iter().any(|pass| !pass.complete));
    }
}

#[test]
fn zero_weight_and_absent_edges_keep_dummy_choices() {
    let zero = trial_with(
        1,
        1,
        |_, _| {
            Some(Edge {
                proposal: 0,
                cost: Score::default(),
            })
        },
        1000,
    );
    assert!(zero.complete && zero.mandatory.is_empty());
    assert_eq!(zero.cost, Some(Score::default()));
    let absent = trial_with(2, 3, |_, _| None, 10_000);
    assert!(absent.complete && absent.mandatory.is_empty());
    assert_eq!(absent.cost, Some(Score::default()));
}
