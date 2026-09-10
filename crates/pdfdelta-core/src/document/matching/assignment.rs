//! Exact partial assignment for validated independent leaf correspondences.
//!
//! Dummy columns represent leaving a vertex unmatched. Scores and potentials
//! remain lexicographic integers; neither cardinality nor a floating-point
//! encoding may override the common correspondence objective.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    CorrespondenceProposal, MatchingAlgorithm, MatchingComponent, NodeId, objective_class,
};

#[cfg(test)]
mod pricing;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Score([i128; 5]);

impl Score {
    fn add(self, other: Self) -> Option<Self> {
        let mut result = self;
        for (value, other) in result.0.iter_mut().zip(other.0) {
            *value = value.checked_add(other)?;
        }
        Some(result)
    }

    fn sub(self, other: Self) -> Option<Self> {
        let mut result = self;
        for (value, other) in result.0.iter_mut().zip(other.0) {
            *value = value.checked_sub(other)?;
        }
        Some(result)
    }
}

#[cfg_attr(test, derive(Clone, Copy))]
struct Edge {
    proposal: usize,
    cost: Score,
}

struct Assignment {
    rows: Vec<BTreeMap<usize, Edge>>,
    real_columns: usize,
}

struct Optimum {
    cost: Score,
    selected: Vec<usize>,
    #[cfg(test)]
    row_potential: Vec<Score>,
    #[cfg(test)]
    column_potential: Vec<Score>,
}

struct Budget {
    remaining: usize,
}

impl Budget {
    fn spend(&mut self, count: usize) -> Option<()> {
        self.remaining = self.remaining.checked_sub(count)?;
        Some(())
    }
}

impl Assignment {
    fn new(
        indices: &[usize],
        proposals: &[CorrespondenceProposal],
        source_premises: &[bool],
        budget: &mut Budget,
    ) -> Option<Self> {
        let mut old = BTreeSet::new();
        let mut new = BTreeSet::new();
        for index in indices {
            budget.spend(1)?;
            old.insert(proposals[*index].old[0]);
            new.insert(proposals[*index].new[0]);
        }
        let transpose = old.len() > new.len();
        let (row_ids, column_ids) = if transpose { (new, old) } else { (old, new) };
        let number = |ids: BTreeSet<NodeId>| {
            ids.into_iter()
                .enumerate()
                .map(|(index, id)| (id, index))
                .collect::<BTreeMap<_, _>>()
        };
        let row_ids = number(row_ids);
        let column_ids = number(column_ids);
        let mut result = Self {
            rows: (0..row_ids.len()).map(|_| BTreeMap::new()).collect(),
            real_columns: column_ids.len(),
        };
        for index in indices {
            budget.spend(1)?;
            let proposal = &proposals[*index];
            let (row, column) = if transpose {
                (proposal.new[0], proposal.old[0])
            } else {
                (proposal.old[0], proposal.new[0])
            };
            let mut cost = Score::default();
            cost.0[objective_class(*index, proposals, source_premises)] =
                -i128::from(proposal.weight);
            result.rows[row_ids[&row]].insert(
                column_ids[&column],
                Edge {
                    proposal: *index,
                    cost,
                },
            );
        }
        Some(result)
    }

    /// Rectangular Hungarian augmentation with absent edges, zero-cost dummy
    /// columns, and exact potentials in the ordered additive score group.
    fn optimum(&self, forbidden: Option<usize>, budget: &mut Budget) -> Option<Optimum> {
        let rows = self.rows.len();
        let columns = self.real_columns.checked_add(rows)?;
        budget.spend(rows.checked_add(columns)?)?;
        let mut row_potential = vec![Score::default(); rows + 1];
        let mut column_potential = vec![Score::default(); columns + 1];
        let mut matched_row = vec![0; columns + 1];
        for row in 1..=rows {
            budget.spend(columns)?;
            matched_row[0] = row;
            let mut column = 0;
            let mut distance: Vec<Option<Score>> = vec![None; columns + 1];
            let mut previous = vec![0; columns + 1];
            let mut visited = vec![false; columns + 1];
            loop {
                visited[column] = true;
                let current_row = matched_row[column];
                let mut next: Option<(Score, usize)> = None;
                for candidate in 1..=columns {
                    budget.spend(1)?;
                    if visited[candidate] {
                        continue;
                    }
                    let cost = if candidate > self.real_columns {
                        Some(Score::default())
                    } else {
                        self.rows[current_row - 1]
                            .get(&(candidate - 1))
                            .filter(|edge| Some(edge.proposal) != forbidden)
                            .map(|edge| edge.cost)
                    };
                    if let Some(cost) = cost {
                        let reduced = cost
                            .sub(row_potential[current_row])?
                            .sub(column_potential[candidate])?;
                        if distance[candidate].is_none_or(|prior| reduced < prior) {
                            distance[candidate] = Some(reduced);
                            previous[candidate] = column;
                        }
                    }
                    // A previous row may have reached this column even if the
                    // current row has no edge to it.
                    if let Some(distance) = distance[candidate]
                        && next.is_none_or(|(best, prior)| {
                            distance < best
                                || (distance == best
                                    && matched_row[candidate] == 0
                                    && matched_row[prior] != 0)
                        })
                    {
                        next = Some((distance, candidate));
                    }
                }
                let (delta, next_column) = next?;
                for candidate in 0..=columns {
                    budget.spend(1)?;
                    if visited[candidate] {
                        let row = matched_row[candidate];
                        row_potential[row] = row_potential[row].add(delta)?;
                        column_potential[candidate] = column_potential[candidate].sub(delta)?;
                    } else if let Some(prior) = distance[candidate] {
                        distance[candidate] = Some(prior.sub(delta)?);
                    }
                }
                column = next_column;
                if matched_row[column] == 0 {
                    break;
                }
            }
            while column != 0 {
                budget.spend(1)?;
                let prior = previous[column];
                matched_row[column] = matched_row[prior];
                column = prior;
            }
        }
        let mut result = Optimum {
            cost: Score::default(),
            selected: Vec::new(),
            #[cfg(test)]
            row_potential,
            #[cfg(test)]
            column_potential,
        };
        for (column, row) in matched_row
            .iter()
            .enumerate()
            .skip(1)
            .take(self.real_columns)
        {
            budget.spend(1)?;
            if *row == 0 {
                continue;
            }
            let edge = self.rows[*row - 1].get(&(column - 1))?;
            result.cost = result.cost.add(edge.cost)?;
            result.selected.push(edge.proposal);
        }
        Some(result)
    }
}

pub(super) fn solve(
    indices: Vec<usize>,
    proposals: &[CorrespondenceProposal],
    source_premises: &[bool],
    work_limit: usize,
) -> MatchingComponent {
    let mut budget = Budget {
        remaining: work_limit,
    };
    let mut result = MatchingComponent {
        proposals: indices,
        mandatory: Vec::new(),
        explored_states: 0,
        assignment_work: 0,
        algorithm: MatchingAlgorithm::BipartiteAssignment,
        exhaustive: false,
    };
    let mut certificate = || -> Option<Vec<usize>> {
        let problem = Assignment::new(&result.proposals, proposals, source_premises, &mut budget)?;
        let optimum = problem.optimum(None, &mut budget)?;
        let mut mandatory = Vec::new();
        for edge in optimum.selected {
            let without = problem.optimum(Some(edge), &mut budget)?;
            match without.cost.cmp(&optimum.cost) {
                std::cmp::Ordering::Greater => mandatory.push(edge),
                std::cmp::Ordering::Equal => {}
                std::cmp::Ordering::Less => return None,
            }
        }
        mandatory.sort_unstable();
        Some(mandatory)
    };
    if let Some(mandatory) = certificate() {
        result.mandatory = mandatory;
        result.exhaustive = true;
    }
    result.assignment_work = work_limit - budget.remaining;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ProposalBasis;

    fn brute_force(problem: &Assignment) -> Optimum {
        fn visit(
            problem: &Assignment,
            row: usize,
            used: &mut BTreeSet<usize>,
            selected: &mut BTreeSet<usize>,
            cost: Score,
            best: &mut Option<Optimum>,
        ) {
            if row == problem.rows.len() {
                match best {
                    Some(best) if best.cost < cost => {}
                    Some(best) if best.cost == cost => {
                        best.selected.retain(|edge| selected.contains(edge));
                    }
                    best => {
                        *best = Some(Optimum {
                            cost,
                            selected: selected.iter().copied().collect(),
                            row_potential: Vec::new(),
                            column_potential: Vec::new(),
                        });
                    }
                }
                return;
            }
            visit(problem, row + 1, used, selected, cost, best);
            for (column, edge) in &problem.rows[row] {
                if used.insert(*column) {
                    selected.insert(edge.proposal);
                    visit(
                        problem,
                        row + 1,
                        used,
                        selected,
                        cost.add(edge.cost).expect("small reference score fits"),
                        best,
                    );
                    selected.remove(&edge.proposal);
                    used.remove(column);
                }
            }
        }
        let mut best = None;
        visit(
            problem,
            0,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            Score::default(),
            &mut best,
        );
        best.expect("empty matching is feasible")
    }

    fn check(matrix: &[Vec<Option<(usize, u32)>>]) {
        let mut proposals = Vec::new();
        let mut source = Vec::new();
        for (old, row) in matrix.iter().enumerate() {
            for (new, entry) in row.iter().enumerate() {
                let Some((class, weight)) = entry else {
                    continue;
                };
                proposals.push(CorrespondenceProposal {
                    old: vec![NodeId(old as u64)],
                    new: vec![NodeId(new as u64)],
                    basis: match class {
                        0 => ProposalBasis::ScopedIdentity,
                        1 | 3 => ProposalBasis::LiteralContent,
                        2 => ProposalBasis::StructuralNeighbor,
                        _ => ProposalBasis::Model,
                    },
                    supplier: "exhaustive-reference".into(),
                    weight: *weight,
                });
                source.push(*class < 2);
            }
        }
        let indices: Vec<_> = (0..proposals.len()).collect();
        let problem = Assignment::new(
            &indices,
            &proposals,
            &source,
            &mut Budget {
                remaining: usize::MAX,
            },
        )
        .expect("bounded fixture construction");
        let expected = brute_force(&problem);
        let actual = problem
            .optimum(
                None,
                &mut Budget {
                    remaining: usize::MAX,
                },
            )
            .expect("bounded fixture optimum");
        assert_eq!(expected.cost, actual.cost, "{matrix:?}");
        let result = solve(indices, &proposals, &source, 1_000_000);
        assert!(result.exhaustive, "{matrix:?}");
        assert_eq!(expected.selected, result.mandatory, "{matrix:?}");
        let priced = pricing::trial(&problem, 1_000_000);
        assert!(priced.complete, "{matrix:?}");
        assert_eq!(priced.cost, Some(expected.cost), "{matrix:?}");
        assert_eq!(priced.mandatory, expected.selected, "{matrix:?}");
        let dense = pricing::dense_trial(&problem, 1_000_000);
        assert!(dense.complete, "{matrix:?}");
        assert_eq!(dense.cost, Some(expected.cost), "{matrix:?}");
        assert_eq!(dense.mandatory, expected.selected, "{matrix:?}");
    }

    #[test]
    fn assignment_certificates_match_exhaustive_partial_matchings() {
        // Every sparse 2x3 matrix over absent/weight-one/weight-two edges.
        for mut encoding in 0..729 {
            let mut matrix = vec![vec![None; 3]; 2];
            for row in &mut matrix {
                for entry in row {
                    let value = encoding % 3;
                    encoding /= 3;
                    *entry = (value > 0).then_some((4, value));
                }
            }
            check(&matrix);
        }
        // Deterministic mixed priorities, zero weights, large weights, and
        // rectangular transposition exercise the ordered objective itself.
        let mut state = 7_u64;
        for (rows, columns, cases) in [(3, 3, 300), (4, 4, 100), (4, 2, 100)] {
            for _ in 0..cases {
                let mut matrix = vec![vec![None; columns]; rows];
                for row in &mut matrix {
                    for entry in row {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        if !state.is_multiple_of(7) {
                            *entry = Some(((state >> 12) as usize % 5, (state >> 32) as u32));
                        }
                    }
                }
                check(&matrix);
            }
        }
        check(&[vec![Some((4, 0))]]);
        check(&[vec![None, None]]);
        check(&[]);
        check(&[
            vec![Some((0, 1)), Some((4, u32::MAX))],
            vec![Some((4, u32::MAX)), None],
        ]);
    }
}
