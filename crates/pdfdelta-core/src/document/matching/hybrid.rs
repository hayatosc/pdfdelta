//! Exact bounded search for mixed conflict components.
//!
//! One declared group candidate prevents the single-edge assignment solver from
//! covering a whole component. When the group candidates are few and are plain
//! leaf groups, every feasibility constraint remains pairwise: each
//! conflict-free group selection is evaluated once, the remaining independent
//! one-to-one correspondences are solved by the exact assignment solver, and
//! the shared ordered objective compares both parts. Mandatory proposals are
//! the intersection over every optimal selection, never one arbitrary optimum.
//!
//! Source ownership can also remove a one-to-one proposal from the assignment
//! model without changing its plain leaf shape. Such proposals stay leaves only
//! after checking the complete conflict matrix: every leaf-to-leaf conflict must
//! already be endpoint sharing, which the assignment solver enforces. Shapes
//! with alternative partitions, nested groups, or unverified leaf conflicts
//! fall back to the component subset search.

use std::collections::BTreeSet;

use super::{
    CorrespondenceProposal, Ownership,
    assignment::{Assignment, Budget, Optimum, Score},
    objective_class,
};

/// A residual component that the mixed search decides exactly.
pub(super) struct Mixed {
    /// Grouped candidates; the caller bounds their count by the component limit.
    pub(super) groups: Vec<usize>,
    /// Independent leaf correspondences solved by assignment.
    pub(super) leaves: Vec<usize>,
}

pub(super) struct Outcome {
    /// The forced prefix plus every proposal common to all optima. Empty of
    /// newly certified proposals unless `exhaustive`.
    pub(super) mandatory: Vec<usize>,
    pub(super) explored_states: usize,
    pub(super) assignment_work: usize,
    pub(super) exhaustive: bool,
}

/// A mixed search attempt. `Unsupported` reports the work already charged to
/// the shared budget so the caller's fallback keeps the component accounting
/// exact.
pub(super) enum Attempt {
    Unsupported { assignment_work: usize },
    Solved(Outcome),
}

/// Classifies a residual component, returning `None` when this search cannot
/// decide it exactly. Plain leaves are proposals with the assignment shape even
/// when source ownership removed them from the assignment model; groups must be
/// plain leaf groups with no alternative partition membership and no owned
/// descendants beyond their endpoints.
pub(super) fn classify(
    remaining: &[usize],
    assignment_shape: &[bool],
    ownership: &[(Ownership, Ownership)],
    proposals: &[CorrespondenceProposal],
) -> Option<Mixed> {
    let mut groups = Vec::new();
    let mut leaves = Vec::new();
    for &index in remaining {
        if assignment_shape[index] {
            leaves.push(index);
            continue;
        }
        let proposal = &proposals[index];
        if proposal.old.len() == 1 && proposal.new.len() == 1 {
            return None;
        }
        let (old, new) = &ownership[index];
        if !old.partitions.is_empty()
            || !new.partitions.is_empty()
            || old.nodes.len() != proposal.old.len()
            || new.nodes.len() != proposal.new.len()
        {
            return None;
        }
        groups.push(index);
    }
    (!groups.is_empty() && !leaves.is_empty()).then_some(Mixed { groups, leaves })
}

/// Enumerates every conflict-free group selection within `states_limit` and
/// solves each residual assignment within the shared `work_limit`. Both bounds
/// are the component's remaining budgets; the search never exceeds them.
/// Returns `Unsupported` when a leaf-to-leaf conflict is not endpoint sharing,
/// so the caller can keep the component subset search for that unverified shape
/// with the verification work still charged.
pub(super) fn solve(
    mixed: &Mixed,
    proposals: &[CorrespondenceProposal],
    source_premises: &[bool],
    conflicts: &[BTreeSet<usize>],
    forced: &BTreeSet<usize>,
    states_limit: usize,
    work_limit: usize,
) -> Attempt {
    let mut budget = Budget::new(work_limit);
    // Every leaf-to-leaf conflict must be endpoint sharing; the assignment
    // solver represents exactly that constraint. A conflict through physical
    // sources would otherwise be silently dropped by the assignment model.
    let leaf_set: BTreeSet<usize> = mixed.leaves.iter().copied().collect();
    for &leaf in &mixed.leaves {
        for &other in &conflicts[leaf] {
            let Some(()) = budget.charge(1) else {
                return Attempt::Solved(Outcome {
                    mandatory: forced.iter().copied().collect(),
                    explored_states: 0,
                    assignment_work: work_limit - budget.remaining(),
                    exhaustive: false,
                });
            };
            if other <= leaf || !leaf_set.contains(&other) {
                continue;
            }
            let (a, b) = (&proposals[leaf], &proposals[other]);
            if a.old[0] != b.old[0] && a.new[0] != b.new[0] {
                return Attempt::Unsupported {
                    assignment_work: work_limit - budget.remaining(),
                };
            }
        }
    }
    let Some(problem) = Assignment::new(&mixed.leaves, proposals, source_premises, &mut budget)
    else {
        return Attempt::Solved(Outcome {
            mandatory: forced.iter().copied().collect(),
            explored_states: 0,
            assignment_work: work_limit - budget.remaining(),
            exhaustive: false,
        });
    };
    let mut search = Search {
        proposals,
        source_premises,
        conflicts,
        problem,
        budget,
        work_limit,
        states: 0,
        best: None,
        mandatory: BTreeSet::new(),
    };
    // Each independent group selection is evaluated exactly once: the stack
    // restores the parent prefix, then extends it by one compatible successor.
    let mut selected = Vec::new();
    let mut stack = vec![(0_usize, 0_usize, None::<usize>)];
    while let Some((offset, prefix, appended)) = stack.pop() {
        selected.truncate(prefix);
        if let Some(group) = appended {
            selected.push(group);
        }
        if search.states == states_limit {
            return Attempt::Solved(finish(forced, false, &search));
        }
        search.states += 1;
        if !search.evaluate(&selected) {
            return Attempt::Solved(finish(forced, false, &search));
        }
        for next in (offset..mixed.groups.len()).rev() {
            let group = mixed.groups[next];
            match search.compatible(&selected, group) {
                Some(true) => stack.push((next + 1, selected.len(), Some(group))),
                Some(false) => {}
                None => return Attempt::Solved(finish(forced, false, &search)),
            }
        }
    }
    Attempt::Solved(finish(forced, true, &search))
}

struct Search<'a> {
    proposals: &'a [CorrespondenceProposal],
    source_premises: &'a [bool],
    conflicts: &'a [BTreeSet<usize>],
    problem: Assignment,
    budget: Budget,
    work_limit: usize,
    states: usize,
    best: Option<Score>,
    mandatory: BTreeSet<usize>,
}

impl Search<'_> {
    /// Returns `None` when the shared budget stopped the check.
    fn compatible(&mut self, selected: &[usize], group: usize) -> Option<bool> {
        for &prior in selected {
            self.budget.charge(1)?;
            if self.conflicts[prior].contains(&group) {
                return Some(false);
            }
        }
        Some(true)
    }

    /// Evaluates one group selection. Returns false when a bound stopped it.
    fn evaluate(&mut self, selected: &[usize]) -> bool {
        let mut score = Score::default();
        let mut forbidden = BTreeSet::new();
        for &index in selected {
            let Some(()) = self.budget.charge(self.conflicts[index].len()) else {
                return false;
            };
            forbidden.extend(self.conflicts[index].iter().copied());
            let class = objective_class(index, self.proposals, self.source_premises);
            let Some(updated) = score.penalize(class, self.proposals[index].weight) else {
                return false;
            };
            score = updated;
        }
        let Some(optimum) = self.problem.optimum(&forbidden, &mut self.budget) else {
            return false;
        };
        let Some(total) = score.add(optimum.cost) else {
            return false;
        };
        if self.best.as_ref().is_some_and(|current| total > *current) {
            return true;
        }
        let Some(certified) = self.certify(&forbidden, &optimum, selected) else {
            return false;
        };
        if self.best == Some(total) {
            self.mandatory.retain(|index| certified.contains(index));
        } else {
            self.best = Some(total);
            self.mandatory = certified;
        }
        true
    }

    /// Certifies the proposals in every optimum of the current selection by
    /// forbidding each chosen leaf edge and comparing the exact optima.
    fn certify(
        &mut self,
        forbidden: &BTreeSet<usize>,
        optimum: &Optimum,
        selected: &[usize],
    ) -> Option<BTreeSet<usize>> {
        self.budget
            .charge(forbidden.len().saturating_add(selected.len()))?;
        let mut mandatory: BTreeSet<usize> = selected.iter().copied().collect();
        for &proposal in &optimum.selected {
            self.budget.charge(forbidden.len())?;
            let mut without = forbidden.clone();
            without.insert(proposal);
            let alternative = self.problem.optimum(&without, &mut self.budget)?;
            match alternative.cost.cmp(&optimum.cost) {
                std::cmp::Ordering::Greater => {
                    mandatory.insert(proposal);
                }
                std::cmp::Ordering::Equal => {}
                std::cmp::Ordering::Less => return None,
            }
        }
        Some(mandatory)
    }
}

/// A stopped search never certifies freshly enumerated selections: only the
/// already forced prefix remains established.
fn finish(forced: &BTreeSet<usize>, exhaustive: bool, search: &Search<'_>) -> Outcome {
    let mut mandatory = if exhaustive {
        search.mandatory.clone()
    } else {
        BTreeSet::new()
    };
    mandatory.extend(forced.iter().copied());
    Outcome {
        mandatory: mandatory.into_iter().collect(),
        explored_states: search.states,
        assignment_work: search.work_limit - search.budget.remaining(),
        exhaustive,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

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
            supplier: "mixed-fixture".into(),
            weight,
        }
    }

    fn conflicts(proposals: &[CorrespondenceProposal]) -> Vec<BTreeSet<usize>> {
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

    /// Independent mirror of the production objective classes.
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

    /// Exhaustive independent-set oracle: maximum ordered class sums and the
    /// intersection of every maximizing subset.
    fn brute(
        proposals: &[CorrespondenceProposal],
        source: &[bool],
        conflicts: &[BTreeSet<usize>],
    ) -> (Vec<usize>, [u64; 6]) {
        let mut best = [0_u64; 6];
        let mut tied: Option<BTreeSet<usize>> = None;
        for mask in 0..(1_u32 << proposals.len()) {
            let mut chosen: BTreeSet<usize> = BTreeSet::new();
            let mut feasible = true;
            'next: for index in 0..proposals.len() {
                if mask & (1 << index) == 0 {
                    continue;
                }
                for prior in &chosen {
                    if conflicts[*prior].contains(&index) {
                        feasible = false;
                        break 'next;
                    }
                }
                chosen.insert(index);
            }
            if !feasible {
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

    fn solved(attempt: Attempt) -> Outcome {
        match attempt {
            Attempt::Solved(outcome) => outcome,
            Attempt::Unsupported { .. } => panic!("fixture shape should be supported"),
        }
    }
    fn side(nodes: &[u64], partitions: &[usize]) -> Ownership {
        Ownership {
            nodes: nodes.iter().copied().map(NodeId).collect(),
            sources: BTreeSet::new(),
            partitions: partitions
                .iter()
                .map(|group| (*group, BTreeSet::new()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn owner(nodes: &[u64], partitions: &[usize]) -> (Ownership, Ownership) {
        (side(nodes, partitions), side(nodes, partitions))
    }

    fn next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        *state >> 33
    }

    #[test]
    fn mixed_search_matches_exhaustive_subsets() {
        let mut state = 7_u64;
        for case in 0..512 {
            let mut proposals = Vec::new();
            let mut source = Vec::new();
            for old in 0..3_u64 {
                for new in 0..3_u64 {
                    if next(&mut state).is_multiple_of(4) {
                        continue;
                    }
                    let basis = match next(&mut state) % 4 {
                        0 => ProposalBasis::ScopedIdentity,
                        1 => ProposalBasis::LiteralContent,
                        2 => ProposalBasis::StructuralNeighbor,
                        _ => ProposalBasis::TextSimilarity,
                    };
                    proposals.push(proposal(
                        &[old],
                        &[new],
                        basis,
                        1 + (next(&mut state) % 4) as u32,
                    ));
                    source.push(next(&mut state).is_multiple_of(2));
                }
            }
            let leaves: Vec<usize> = (0..proposals.len()).collect();
            let mut groups = Vec::new();
            for _ in 0..=(next(&mut state) % 2) {
                if next(&mut state).is_multiple_of(2) {
                    let old = next(&mut state) % 3;
                    let first = next(&mut state) % 3;
                    let second = (first + 1) % 3;
                    proposals.push(proposal(
                        &[old],
                        &[first, second],
                        ProposalBasis::TextSimilarity,
                        1 + (next(&mut state) % 3) as u32,
                    ));
                } else {
                    let new = next(&mut state) % 3;
                    let first = next(&mut state) % 3;
                    let second = (first + 1) % 3;
                    proposals.push(proposal(
                        &[first, second],
                        &[new],
                        ProposalBasis::TextSimilarity,
                        1 + (next(&mut state) % 3) as u32,
                    ));
                }
                source.push(false);
                groups.push(proposals.len() - 1);
            }
            let conflicts = conflicts(&proposals);
            let mixed = Mixed { groups, leaves };
            let expected = brute(&proposals, &source, &conflicts).0;
            let outcome = solve(
                &mixed,
                &proposals,
                &source,
                &conflicts,
                &BTreeSet::new(),
                100_000,
                32_000_000,
            );
            let outcome = solved(outcome);
            assert!(outcome.exhaustive, "case={case}");
            assert_eq!(outcome.mandatory, expected, "case={case}");
        }
    }

    #[test]
    fn tied_optima_keep_only_shared_mandatory_proposals() {
        let proposals = vec![
            proposal(&[0], &[0], ProposalBasis::TextSimilarity, 2),
            proposal(&[1], &[1], ProposalBasis::TextSimilarity, 2),
            proposal(&[0], &[1, 2], ProposalBasis::TextSimilarity, 2),
            proposal(&[1, 2], &[0], ProposalBasis::TextSimilarity, 2),
        ];
        let source = [false; 4];
        let conflicts = conflicts(&proposals);
        let mixed = Mixed {
            groups: vec![2, 3],
            leaves: vec![0, 1],
        };
        let expected = brute(&proposals, &source, &conflicts).0;
        assert!(expected.is_empty());
        let outcome = solve(
            &mixed,
            &proposals,
            &source,
            &conflicts,
            &BTreeSet::new(),
            100_000,
            32_000_000,
        );
        let outcome = solved(outcome);
        assert!(outcome.exhaustive);
        assert!(outcome.mandatory.is_empty());
    }

    #[test]
    fn higher_objective_class_outweighs_any_lower_weight_sum() {
        let proposals = vec![
            proposal(&[0], &[0], ProposalBasis::ScopedIdentity, 1),
            proposal(&[0], &[1], ProposalBasis::TextSimilarity, 1_000),
            proposal(&[1], &[0], ProposalBasis::TextSimilarity, 1_000),
        ];
        let source = [true, false, false];
        let conflicts = conflicts(&proposals);
        let mixed = Mixed {
            groups: vec![1],
            leaves: vec![0, 2],
        };
        let (expected, score) = brute(&proposals, &source, &conflicts);
        assert_eq!(score, [1, 0, 0, 0, 0, 0]);
        assert_eq!(expected, [0]);
        let outcome = solve(
            &mixed,
            &proposals,
            &source,
            &conflicts,
            &BTreeSet::new(),
            100_000,
            32_000_000,
        );
        let outcome = solved(outcome);
        assert!(outcome.exhaustive);
        assert_eq!(outcome.mandatory, expected);
    }

    #[test]
    fn budget_limits_stop_without_certifying_newly_seen_selections() {
        let proposals = vec![
            proposal(&[0], &[0], ProposalBasis::TextSimilarity, 2),
            proposal(&[1], &[1], ProposalBasis::TextSimilarity, 2),
            proposal(&[3], &[3], ProposalBasis::TextSimilarity, 1),
        ];
        let source = [false; 3];
        let conflicts = conflicts(&proposals);
        let mixed = Mixed {
            groups: vec![2],
            leaves: vec![0, 1],
        };
        let forced = BTreeSet::from([0]);
        for (states_limit, work_limit) in [(0, 32_000_000), (1, 32_000_000), (100_000, 0)] {
            let outcome = solve(
                &mixed,
                &proposals,
                &source,
                &conflicts,
                &forced,
                states_limit,
                work_limit,
            );
            let outcome = solved(outcome);
            assert!(!outcome.exhaustive, "{states_limit}/{work_limit}");
            assert_eq!(outcome.mandatory, [0], "{states_limit}/{work_limit}");
            assert!(outcome.explored_states <= states_limit);
            assert!(outcome.assignment_work <= work_limit);
        }
    }

    #[test]
    fn leaf_conflicts_without_endpoint_sharing_reject_the_shape() {
        let proposals = vec![
            proposal(&[0], &[0], ProposalBasis::TextSimilarity, 1),
            proposal(&[1], &[1], ProposalBasis::TextSimilarity, 1),
            proposal(&[5], &[5, 6], ProposalBasis::TextSimilarity, 1),
        ];
        let source = [false; 3];
        let mut conflicts = conflicts(&proposals);
        assert!(conflicts[0].is_empty());
        conflicts[0].insert(1);
        conflicts[1].insert(0);
        let mixed = Mixed {
            groups: vec![2],
            leaves: vec![0, 1],
        };
        let outcome = solve(
            &mixed,
            &proposals,
            &source,
            &conflicts,
            &BTreeSet::new(),
            100_000,
            32_000_000,
        );
        assert!(matches!(outcome, Attempt::Unsupported { .. }));
    }

    #[test]
    fn classify_accepts_only_verified_plain_shapes() {
        let leaf = proposal(&[0], &[0], ProposalBasis::TextSimilarity, 1);
        let group = proposal(&[0], &[1, 2], ProposalBasis::TextSimilarity, 1);
        let narrow = proposal(&[3], &[3], ProposalBasis::TextSimilarity, 1);

        let mixed = classify(
            &[0, 1],
            &[true, false],
            &[owner(&[0], &[]), (side(&[0], &[]), side(&[1, 2], &[]))],
            &[leaf.clone(), group.clone()],
        )
        .expect("plain leaf group");
        assert_eq!(mixed.groups, [1]);
        assert_eq!(mixed.leaves, [0]);

        assert!(
            classify(
                &[0, 1],
                &[true, false],
                &[owner(&[0], &[]), (side(&[0], &[]), side(&[1, 2], &[0])),],
                &[leaf.clone(), group.clone()],
            )
            .is_none(),
            "partition membership"
        );
        assert!(
            classify(
                &[0, 1],
                &[true, false],
                &[owner(&[0], &[]), (side(&[0], &[]), side(&[1, 2, 9], &[])),],
                &[leaf.clone(), group.clone()],
            )
            .is_none(),
            "owned descendants"
        );
        assert!(
            classify(
                &[0, 1],
                &[true, false],
                &[owner(&[0], &[]), (side(&[0], &[]), side(&[1], &[])),],
                &[leaf.clone(), narrow],
            )
            .is_none(),
            "one-to-one without assignment shape"
        );
        assert!(
            classify(
                &[0],
                &[true],
                &[owner(&[0], &[])],
                std::slice::from_ref(&leaf)
            )
            .is_none(),
            "no groups"
        );
        assert!(
            classify(
                &[0, 1],
                &[false, false],
                &[
                    (side(&[0], &[]), side(&[1, 2], &[])),
                    (side(&[0], &[]), side(&[1, 2], &[])),
                ],
                &[group.clone(), group],
            )
            .is_none(),
            "no leaves"
        );
    }

    #[test]
    fn schedule_se_component_matches_independent_endpoint_oracle() {
        // The residual Schedule SE scope contains 121 one-to-one text
        // candidates over eleven paragraph labels and 22 retained-order group
        // candidates around the 83/84 pair. An independent endpoint-packing
        // oracle over the same weights reports the optimal weight 8_301_289 and
        // seven mandatory one-to-one diagonals. The deterministic enumeration
        // must reproduce the intersection rather than one of the tied optima.
        let labels: [u64; 11] = [76, 80, 83, 84, 87, 99, 7, 48, 50, 52, 75];
        let weights: [[u32; 11]; 11] = [
            [
                961_539, 10_811, 1, 15_038, 5848, 943_397, 1, 34_189, 1, 1, 735_295,
            ],
            [
                10_811, 943_397, 12_270, 248_121, 526_316, 10_753, 1, 32_001, 1, 76_272, 9951,
            ],
            [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            [
                15_038, 240_602, 1, 943_926, 165_485, 14_926, 1, 70_708, 1, 43_479, 13_423,
            ],
            [
                5848, 526_316, 6251, 174_941, 984_178, 5831, 18_182, 63_883, 1, 76_336, 5587,
            ],
            [
                943_397, 10_753, 1, 14_926, 5831, 925_926, 1, 33_899, 1, 1, 724_638,
            ],
            [1, 1, 1, 1, 18_182, 1, 928_572, 38_096, 1, 43_957, 1],
            [
                34_189, 32_001, 1, 70_708, 63_883, 33_899, 38_096, 967_033, 1, 369_048, 30_076,
            ],
            [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            [
                1, 76_272, 1, 43_479, 76_336, 1, 43_957, 369_048, 1, 922_078, 1,
            ],
            [
                943_397, 10_753, 1, 14_926, 5831, 925_926, 1, 33_899, 1, 1, 724_638,
            ],
        ];
        let one_to_two: [u32; 11] = [
            14_389, 242_648, 1, 918_182, 172_495, 14_286, 1, 68_628, 1, 42_106, 14_286,
        ];
        let two_to_one: [u32; 11] = [
            14_493, 236_163, 1, 922_375, 163_552, 14_389, 1, 68_966, 1, 42_329, 12_988,
        ];
        let mut proposals = Vec::new();
        let mut source = Vec::new();
        for (index, &label) in labels.iter().enumerate() {
            proposals.push(proposal(
                &[label],
                &[labels[2], labels[3]],
                ProposalBasis::TextSimilarity,
                one_to_two[index],
            ));
            source.push(false);
        }
        for (index, &label) in labels.iter().enumerate() {
            proposals.push(proposal(
                &[labels[2], labels[3]],
                &[label],
                ProposalBasis::TextSimilarity,
                two_to_one[index],
            ));
            source.push(false);
        }
        for old in 0..11 {
            for new in 0..11 {
                proposals.push(proposal(
                    &[labels[old]],
                    &[labels[new]],
                    ProposalBasis::TextSimilarity,
                    weights[old][new],
                ));
                source.push(false);
            }
        }
        let conflicts = conflicts(&proposals);
        let mixed = Mixed {
            groups: (0..22).collect(),
            leaves: (22..143).collect(),
        };
        let outcome = solve(
            &mixed,
            &proposals,
            &source,
            &conflicts,
            &BTreeSet::new(),
            100_000,
            32_000_000,
        );
        let outcome = solved(outcome);
        assert!(outcome.exhaustive);
        assert_eq!(outcome.mandatory, [22, 34, 58, 70, 94, 106, 130]);
        assert_eq!(outcome.explored_states, 104);
    }
}
