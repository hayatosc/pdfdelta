//! Exact cardinality-bounded search for wide group components.
//!
//! A component of group candidates can exceed the subset-search proposal bound
//! before any state is visited. Endpoint sharing is a conflict, so every
//! feasible selection consumes at least `min_group_size` distinct endpoints on
//! each side and therefore contains at most `k = min(old_bound, new_bound)`
//! proposals. When the exact decision-tree size for sizes `0..=k` fits the
//! component state budget, enumerating those sizes is complete. Cross-proposal
//! conflicts and alternative partitions are still checked per selection, and
//! the unchanged six-class objective keeps the same all-optima mandatory
//! intersection and forced prefix contract. A budget stop certifies only the
//! previously forced prefix.
//!
//! Endpoint collection, the tree preflight and every decision count are
//! charged to the remaining component assignment work, so a source-only rerun
//! or a hybrid fallback never receives silent extra budget.

use std::collections::BTreeSet;

use super::{
    CorrespondenceProposal, MatchingAlgorithm, MatchingComponent, MatchingLimits, Ownership,
    assignment::Budget, objective_class, partitions_compatible,
};

pub(super) enum Attempt {
    Solved(MatchingComponent),
    /// The exact tree or the preflight did not fit the remaining budgets.
    /// `assignment_work` is the bounded work already charged; the caller adds
    /// it to its fallback and keeps its subset search for `remaining`.
    Unsupported {
        remaining: Vec<usize>,
        assignment_work: usize,
    },
}

/// Enumerates every feasible selection up to the endpoint cardinality bound.
pub(super) fn solve(
    remaining: Vec<usize>,
    proposals: &[CorrespondenceProposal],
    source_premises: &[bool],
    conflicts: &[BTreeSet<usize>],
    ownership: &[(Ownership, Ownership)],
    forced: &BTreeSet<usize>,
    limits: MatchingLimits,
) -> Attempt {
    let work_limit = limits.max_assignment_work_per_component;
    let mut budget = Budget::new(work_limit);
    let Some(bound) = cardinality_bound(&remaining, proposals, &mut budget) else {
        return Attempt::Unsupported {
            remaining,
            assignment_work: work_limit - budget.remaining(),
        };
    };
    if decision_tree_nodes(
        remaining.len(),
        bound,
        limits.max_states_per_component,
        &mut budget,
    )
    .is_none_or(|nodes| nodes > limits.max_states_per_component as u128)
    {
        return Attempt::Unsupported {
            remaining,
            assignment_work: work_limit - budget.remaining(),
        };
    }
    Attempt::Solved(enumerate(
        &remaining,
        bound,
        proposals,
        source_premises,
        conflicts,
        ownership,
        forced,
        limits,
        work_limit - budget.remaining(),
    ))
}

/// Upper bound on the number of proposals in any conflict-free selection:
/// `min(unique_old / min_old_size, unique_new / min_new_size)`, capped at the
/// number of candidates. Endpoint collection is charged per visited endpoint.
fn cardinality_bound(
    remaining: &[usize],
    proposals: &[CorrespondenceProposal],
    budget: &mut Budget,
) -> Option<usize> {
    if remaining.is_empty() {
        return None;
    }
    let mut old = BTreeSet::new();
    let mut new = BTreeSet::new();
    let mut min_old = usize::MAX;
    let mut min_new = usize::MAX;
    for &index in remaining {
        budget.charge(1)?;
        let proposal = &proposals[index];
        if proposal.old.is_empty() || proposal.new.is_empty() {
            return None;
        }
        // The public contract rejects duplicate endpoints inside one group.
        // A malformed group is never admitted to the cardinality bound.
        budget.charge(proposal.old.len().saturating_add(proposal.new.len()))?;
        let mut old_unique = BTreeSet::new();
        let mut new_unique = BTreeSet::new();
        for node in &proposal.old {
            old_unique.insert(*node);
        }
        for node in &proposal.new {
            new_unique.insert(*node);
        }
        if old_unique.len() != proposal.old.len() || new_unique.len() != proposal.new.len() {
            return None;
        }
        min_old = min_old.min(old_unique.len());
        min_new = min_new.min(new_unique.len());
        for node in old_unique {
            budget.charge(1)?;
            old.insert(node);
        }
        for node in new_unique {
            budget.charge(1)?;
            new.insert(node);
        }
    }
    let old_bound = old.len() / min_old;
    let new_bound = new.len() / min_new;
    Some(old_bound.min(new_bound).min(remaining.len()))
}

/// Exact number of decision-tree nodes with a cardinality cap. The closed form
/// `sum_{j=0..=k} C(count + 1, j + 1)` is evaluated term by term with checked
/// arithmetic and stops as soon as the count exceeds `budget`; every term
/// consumes one unit of the shared work budget.
fn decision_tree_nodes(
    count: usize,
    bound: usize,
    budget_state: usize,
    budget: &mut Budget,
) -> Option<u128> {
    let budget_state = budget_state as u128;
    let top = count as u128 + 1;
    let mut term: u128 = 1;
    let mut total: u128 = 0;
    for step in 0..=bound {
        budget.charge(1)?;
        term = term.checked_mul(top.checked_sub(step as u128)?)? / (step as u128 + 1);
        total = total.checked_add(term)?;
        if total > budget_state {
            return None;
        }
    }
    Some(total)
}

#[allow(clippy::too_many_arguments)]
fn enumerate(
    remaining: &[usize],
    bound: usize,
    proposals: &[CorrespondenceProposal],
    source_premises: &[bool],
    conflicts: &[BTreeSet<usize>],
    ownership: &[(Ownership, Ownership)],
    forced: &BTreeSet<usize>,
    limits: MatchingLimits,
    assignment_work: usize,
) -> MatchingComponent {
    let limit = limits.max_states_per_component;
    let mut states = 0usize;
    let base = forced.len();
    let mut pending = vec![(0_usize, [0_u64; 6], forced.clone())];
    let mut best = [0_u64; 6];
    let mut mandatory: Option<BTreeSet<usize>> = None;
    let mut partition_budget = limits.max_ownership_visits;
    while let Some((offset, score, selected)) = pending.pop() {
        if states == limit {
            return truncated(states, forced, assignment_work);
        }
        states += 1;
        if offset == remaining.len() {
            if mandatory.is_none() || score > best {
                best = score;
                mandatory = Some(selected);
            } else if score == best
                && let Some(shared) = &mut mandatory
            {
                shared.retain(|index| selected.contains(index));
            }
            continue;
        }
        // Every selection is reached by skipping to the end; the include
        // branch stops at the endpoint cardinality bound.
        pending.push((offset + 1, score, selected.clone()));
        let index = remaining[offset];
        if selected.len().saturating_sub(base) >= bound || !conflicts[index].is_disjoint(&selected)
        {
            continue;
        }
        match partitions_compatible(index, &selected, ownership, &mut partition_budget) {
            Some(true) => {}
            Some(false) => continue,
            None => return truncated(states, forced, assignment_work),
        }
        let mut included = selected;
        included.insert(index);
        let mut score = score;
        let class = objective_class(index, proposals, source_premises);
        let Some(sum) = score[class].checked_add(u64::from(proposals[index].weight)) else {
            // The validated objective cannot overflow; an impossible sum is
            // reported as an incomplete component, never as a certificate.
            return truncated(states, forced, assignment_work);
        };
        score[class] = sum;
        pending.push((offset + 1, score, included));
    }
    MatchingComponent {
        proposals: Vec::new(),
        mandatory: mandatory.unwrap_or_default().into_iter().collect(),
        explored_states: states,
        assignment_work,
        algorithm: MatchingAlgorithm::CardinalityChoice,
        exhaustive: true,
    }
}

/// A stopped search keeps the algorithm that actually ran and certifies only
/// the forced prefix; completeness is carried by `exhaustive`.
fn truncated(states: usize, forced: &BTreeSet<usize>, assignment_work: usize) -> MatchingComponent {
    MatchingComponent {
        proposals: Vec::new(),
        mandatory: forced.iter().copied().collect(),
        explored_states: states,
        assignment_work,
        algorithm: MatchingAlgorithm::CardinalityChoice,
        exhaustive: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::document::{NodeId, ProposalBasis};

    fn proposal(
        old: &[u64],
        new: &[u64],
        basis: ProposalBasis,
        weight: u32,
    ) -> CorrespondenceProposal {
        CorrespondenceProposal {
            old: old.iter().copied().map(NodeId).collect(),
            new: new.iter().copied().map(NodeId).collect(),
            basis,
            supplier: "cardinality-fixture".into(),
            weight,
        }
    }

    fn endpoint_conflicts(proposals: &[CorrespondenceProposal]) -> Vec<BTreeSet<usize>> {
        let mut result = vec![BTreeSet::new(); proposals.len()];
        for a in 0..proposals.len() {
            for b in a + 1..proposals.len() {
                let overlaps = proposals[a]
                    .old
                    .iter()
                    .any(|id| proposals[b].old.contains(id))
                    || proposals[a]
                        .new
                        .iter()
                        .any(|id| proposals[b].new.contains(id));
                if overlaps {
                    result[a].insert(b);
                    result[b].insert(a);
                }
            }
        }
        result
    }

    fn class_of(basis: ProposalBasis, source: bool) -> usize {
        match (source, basis) {
            (true, ProposalBasis::ScopedIdentity | ProposalBasis::TableCellIdentity) => 0,
            (true, ProposalBasis::LiteralContentWithPadding) => 2,
            (true, _) => 1,
            (
                false,
                ProposalBasis::ScopedIdentity
                | ProposalBasis::TableCellIdentity
                | ProposalBasis::StructuralNeighbor,
            ) => 3,
            (false, ProposalBasis::LiteralContent) => 4,
            (false, _) => 5,
        }
    }

    fn owner(nodes: &[u64], partitions: &[(usize, &[usize])]) -> Ownership {
        Ownership {
            nodes: nodes.iter().copied().map(NodeId).collect(),
            sources: BTreeSet::new(),
            partitions: partitions
                .iter()
                .map(|(group, allowed)| (*group, allowed.iter().copied().collect()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn sides(nodes: &[u64], partitions: &[(usize, &[usize])]) -> (Ownership, Ownership) {
        (owner(nodes, partitions), owner(nodes, partitions))
    }

    /// Exhaustive oracle over every subset of `remaining`, honoring forced
    /// membership, endpoint/synthetic conflicts and partition compatibility.
    fn brute(
        remaining: &[usize],
        forced: &BTreeSet<usize>,
        proposals: &[CorrespondenceProposal],
        source: &[bool],
        conflicts: &[BTreeSet<usize>],
        ownership: &[(Ownership, Ownership)],
    ) -> (Vec<usize>, [u64; 6]) {
        let mut best = [0_u64; 6];
        let mut tied: Option<BTreeSet<usize>> = None;
        for mask in 0..(1_u32 << remaining.len()) {
            let mut chosen = forced.clone();
            let mut feasible = forced
                .iter()
                .all(|a| forced.iter().all(|b| a == b || !conflicts[*a].contains(b)));
            'next: for (offset, index) in remaining.iter().enumerate() {
                if mask & (1 << offset) == 0 {
                    continue;
                }
                for prior in &chosen {
                    if conflicts[*prior].contains(index) {
                        feasible = false;
                        break 'next;
                    }
                }
                chosen.insert(*index);
            }
            if !feasible || !partitions_feasible(&chosen, ownership) {
                continue;
            }
            let mut score = [0_u64; 6];
            for &index in &chosen {
                score[class_of(proposals[index].basis, source[index])] +=
                    u64::from(proposals[index].weight);
            }
            match &mut tied {
                Some(current) if score > best => {
                    best = score;
                    *current = chosen;
                }
                Some(current) if score == best => {
                    current.retain(|index| chosen.contains(index));
                }
                Some(_) => {}
                None => {
                    best = score;
                    tied = Some(chosen);
                }
            }
        }
        (
            tied.expect("empty selection is feasible")
                .into_iter()
                .collect(),
            best,
        )
    }

    fn partitions_feasible(chosen: &BTreeSet<usize>, ownership: &[(Ownership, Ownership)]) -> bool {
        // Old and new partition groups are unrelated namespaces; each side is
        // intersected separately, matching `partitions_compatible`.
        for old in [true, false] {
            let side = |index: usize| -> &Ownership {
                if old {
                    &ownership[index].0
                } else {
                    &ownership[index].1
                }
            };
            for &index in chosen {
                for (group, allowed) in &side(index).partitions {
                    let possible = allowed.iter().any(|partition| {
                        chosen.iter().all(|other| {
                            side(*other)
                                .partitions
                                .get(group)
                                .is_none_or(|choices| choices.contains(partition))
                        })
                    });
                    if !possible {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn solved(attempt: Attempt) -> MatchingComponent {
        match attempt {
            Attempt::Solved(component) => component,
            Attempt::Unsupported { .. } => panic!("fixture should fit the state budget"),
        }
    }

    fn next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        *state >> 33
    }

    #[test]
    fn cardinality_search_matches_exhaustive_subsets() {
        let mut state = 11_u64;
        for case in 0..512 {
            let mut proposals = Vec::new();
            let mut source = Vec::new();
            for _ in 0..4 + next(&mut state) % 7 {
                let old_len = (1 + next(&mut state) % 2) as usize;
                let new_len = (1 + next(&mut state) % 2) as usize;
                let mut old_set = BTreeSet::new();
                while old_set.len() < old_len {
                    old_set.insert(next(&mut state) % 5);
                }
                let mut new_set = BTreeSet::new();
                while new_set.len() < new_len {
                    new_set.insert(10 + next(&mut state) % 5);
                }
                let old: Vec<u64> = old_set.into_iter().collect();
                let new: Vec<u64> = new_set.into_iter().collect();
                let basis = match next(&mut state) % 4 {
                    0 => ProposalBasis::ScopedIdentity,
                    1 => ProposalBasis::LiteralContent,
                    2 => ProposalBasis::StructuralNeighbor,
                    _ => ProposalBasis::TextSimilarity,
                };
                proposals.push(proposal(&old, &new, basis, (next(&mut state) % 5) as u32));
                source.push(next(&mut state).is_multiple_of(2));
            }
            let mut conflicts = endpoint_conflicts(&proposals);
            for a in 0..proposals.len() {
                if next(&mut state).is_multiple_of(8) {
                    let b = (next(&mut state) as usize) % proposals.len();
                    if a != b {
                        conflicts[a].insert(b);
                        conflicts[b].insert(a);
                    }
                }
            }
            let ownership: Vec<_> = (0..proposals.len()).map(|_| sides(&[], &[])).collect();
            let remaining: Vec<usize> = (0..proposals.len()).collect();
            let forced = BTreeSet::new();
            let expected = brute(
                &remaining, &forced, &proposals, &source, &conflicts, &ownership,
            )
            .0;
            let actual = solved(solve(
                remaining.clone(),
                &proposals,
                &source,
                &conflicts,
                &ownership,
                &forced,
                MatchingLimits::default(),
            ));
            assert!(actual.exhaustive, "case={case}");
            assert_eq!(actual.mandatory, expected, "case={case}");
        }
    }

    #[test]
    fn ties_zero_weights_and_class_priorities_follow_the_common_objective() {
        let proposals = vec![
            proposal(&[0], &[10], ProposalBasis::TextSimilarity, 2),
            proposal(&[1], &[11], ProposalBasis::TextSimilarity, 2),
            proposal(&[2], &[12], ProposalBasis::ScopedIdentity, 1),
            proposal(&[3], &[13], ProposalBasis::TextSimilarity, 0),
        ];
        let source = [false, false, true, false];
        let conflicts = endpoint_conflicts(&proposals);
        let ownership: Vec<_> = (0..proposals.len()).map(|_| sides(&[], &[])).collect();
        let remaining: Vec<usize> = (0..4).collect();
        let forced = BTreeSet::new();
        let expected = brute(
            &remaining, &forced, &proposals, &source, &conflicts, &ownership,
        );
        let actual = solved(solve(
            remaining,
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &forced,
            MatchingLimits::default(),
        ));
        assert_eq!(actual.mandatory, expected.0);
        // The class-zero match leads the objective, both positive class-five
        // matches join every optimum, and the zero-weight candidate never
        // enters the mandatory intersection.
        assert_eq!(actual.mandatory, [0, 1, 2]);
    }

    #[test]
    fn synthetic_source_conflicts_are_respected() {
        let proposals = vec![
            proposal(&[0], &[10], ProposalBasis::TextSimilarity, 5),
            proposal(&[1], &[11], ProposalBasis::TextSimilarity, 4),
        ];
        let source = [false, false];
        let mut conflicts = endpoint_conflicts(&proposals);
        conflicts[0].insert(1);
        conflicts[1].insert(0);
        let ownership: Vec<_> = (0..2).map(|_| sides(&[], &[])).collect();
        let remaining: Vec<usize> = (0..2).collect();
        let actual = solved(solve(
            remaining.clone(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &BTreeSet::new(),
            MatchingLimits::default(),
        ));
        let expected = brute(
            &remaining,
            &BTreeSet::new(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
        );
        assert_eq!(actual.mandatory, expected.0);
        assert_eq!(actual.mandatory, [0]);
    }

    #[test]
    fn incompatible_partitions_prevent_joint_selection() {
        let proposals = vec![
            proposal(&[0], &[10], ProposalBasis::TextSimilarity, 5),
            proposal(&[1], &[11], ProposalBasis::TextSimilarity, 4),
        ];
        let source = [false, false];
        let conflicts = endpoint_conflicts(&proposals);
        let ownership = vec![sides(&[0, 10], &[(0, &[0])]), sides(&[1, 11], &[(0, &[1])])];
        let remaining: Vec<usize> = (0..2).collect();
        let actual = solved(solve(
            remaining.clone(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &BTreeSet::new(),
            MatchingLimits::default(),
        ));
        let expected = brute(
            &remaining,
            &BTreeSet::new(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
        );
        assert_eq!(actual.mandatory, expected.0);
        assert_eq!(actual.mandatory, [0]);
    }

    #[test]
    fn forced_prefix_is_retained_and_partition_stops_truncate() {
        let proposals = vec![
            proposal(&[0], &[10], ProposalBasis::TextSimilarity, 5),
            proposal(&[1], &[11], ProposalBasis::TextSimilarity, 4),
            proposal(&[2], &[12], ProposalBasis::TextSimilarity, 3),
        ];
        let source = [false; 3];
        let conflicts = endpoint_conflicts(&proposals);
        let ownership = vec![
            sides(&[0, 10], &[(0, &[0])]),
            sides(&[1, 11], &[(0, &[1])]),
            sides(&[2, 12], &[]),
        ];
        let forced = BTreeSet::from([0]);
        let remaining = vec![1, 2];
        let component = solved(solve(
            remaining.clone(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &forced,
            MatchingLimits {
                max_ownership_visits: 0,
                ..MatchingLimits::default()
            },
        ));
        assert!(!component.exhaustive);
        assert_eq!(component.mandatory, [0]);
        assert!(matches!(
            component.algorithm,
            MatchingAlgorithm::CardinalityChoice
        ));
        let expected = brute(
            &remaining, &forced, &proposals, &source, &conflicts, &ownership,
        );
        let complete = solved(solve(
            remaining,
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &forced,
            MatchingLimits::default(),
        ));
        assert_eq!(complete.mandatory, expected.0);
        assert!(complete.mandatory.contains(&0));
    }

    #[test]
    fn wide_components_without_a_small_bound_fall_back() {
        let proposals: Vec<_> = (0..40)
            .map(|index| proposal(&[index], &[100 + index], ProposalBasis::TextSimilarity, 1))
            .collect();
        let source = vec![false; proposals.len()];
        let conflicts = endpoint_conflicts(&proposals);
        let ownership: Vec<_> = (0..proposals.len()).map(|_| sides(&[], &[])).collect();
        let attempt = solve(
            (0..proposals.len()).collect(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &BTreeSet::new(),
            MatchingLimits::default(),
        );
        assert!(matches!(attempt, Attempt::Unsupported { .. }));
    }

    #[test]
    fn exhausted_work_budget_reports_consumed_work_on_fallback() {
        let proposals = vec![
            proposal(&[0], &[10], ProposalBasis::TextSimilarity, 1),
            proposal(&[1], &[11], ProposalBasis::TextSimilarity, 1),
        ];
        let source = [false; 2];
        let conflicts = endpoint_conflicts(&proposals);
        let ownership: Vec<_> = (0..2).map(|_| sides(&[], &[])).collect();
        for limit in [0, 1] {
            let attempt = solve(
                (0..2).collect(),
                &proposals,
                &source,
                &conflicts,
                &ownership,
                &BTreeSet::new(),
                MatchingLimits {
                    max_assignment_work_per_component: limit,
                    ..MatchingLimits::default()
                },
            );
            match attempt {
                Attempt::Unsupported {
                    assignment_work, ..
                } => assert!(assignment_work <= limit, "limit={limit}"),
                Attempt::Solved(_) => panic!("limit={limit} should not fit"),
            }
        }
    }

    #[test]
    fn cardinality_bound_counts_unique_endpoints() {
        let proposals = vec![
            proposal(&[0, 1], &[10], ProposalBasis::TextSimilarity, 1),
            proposal(&[1, 2], &[11], ProposalBasis::TextSimilarity, 1),
            proposal(&[2, 0], &[12], ProposalBasis::TextSimilarity, 1),
        ];
        let remaining: Vec<usize> = (0..3).collect();
        let mut budget = Budget::new(usize::MAX);
        // Three old endpoints, minimum group size two, one new endpoint per
        // group size one: the old side bounds any selection to one proposal.
        assert_eq!(
            cardinality_bound(&remaining, &proposals, &mut budget),
            Some(1)
        );
    }

    #[test]
    fn old_and_new_partition_groups_are_separate_namespaces() {
        let proposals = vec![
            proposal(&[0], &[10], ProposalBasis::TextSimilarity, 5),
            proposal(&[1], &[11], ProposalBasis::TextSimilarity, 4),
        ];
        let source = [false, false];
        let conflicts = endpoint_conflicts(&proposals);
        // Old group 0 and new group 0 are unrelated; both sides allow the
        // joint selection even though their numeric group names coincide.
        let ownership = vec![
            (owner(&[0, 10], &[(0, &[0])]), owner(&[0, 10], &[(0, &[1])])),
            (owner(&[1, 11], &[(0, &[0])]), owner(&[1, 11], &[(0, &[1])])),
        ];
        let remaining: Vec<usize> = (0..2).collect();
        let actual = solved(solve(
            remaining.clone(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &BTreeSet::new(),
            MatchingLimits::default(),
        ));
        let expected = brute(
            &remaining,
            &BTreeSet::new(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
        );
        assert_eq!(actual.mandatory, expected.0);
        assert_eq!(actual.mandatory, [0, 1]);
    }

    #[test]
    fn duplicate_endpoints_inside_one_group_are_rejected() {
        let proposals = vec![
            proposal(&[0, 0], &[10], ProposalBasis::TextSimilarity, 1),
            proposal(&[1], &[11], ProposalBasis::TextSimilarity, 1),
        ];
        let source = [false; 2];
        let conflicts = endpoint_conflicts(&proposals);
        let ownership: Vec<_> = (0..2).map(|_| sides(&[], &[])).collect();
        let attempt = solve(
            (0..2).collect(),
            &proposals,
            &source,
            &conflicts,
            &ownership,
            &BTreeSet::new(),
            MatchingLimits::default(),
        );
        assert!(matches!(attempt, Attempt::Unsupported { .. }));
    }

    #[test]
    fn decision_tree_nodes_matches_the_prefix_count() {
        // Four candidates with a two-selection cap: 1 + 2 + 4 + 7 + 11 prefix
        // states, and a zero cap still visits only the empty prefixes.
        let mut budget = Budget::new(usize::MAX);
        assert_eq!(decision_tree_nodes(4, 2, 1_000_000, &mut budget), Some(25));
        assert_eq!(decision_tree_nodes(4, 0, 1_000_000, &mut budget), Some(5));
        assert_eq!(decision_tree_nodes(4, 4, 3, &mut budget), None);
    }
}
