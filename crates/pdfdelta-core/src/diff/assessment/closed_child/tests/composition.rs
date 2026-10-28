use super::super::super::{ComparisonAssessment, Coverage, ResolutionState, validation};
use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Captured {
    relation: usize,
    old: Vec<ComparableToken>,
    new: Vec<ComparableToken>,
    cuts: [usize; 2],
    mandatory_pairs: Vec<(usize, usize)>,
}

fn captured() -> Vec<Captured> {
    serde_json::from_str(include_str!("composition-oracle.json"))
        .expect("independent Python oracle")
}

fn text(tokens: &[ComparableToken]) -> String {
    tokens
        .iter()
        .map(|t| match t {
            ComparableToken::Scalar(c) => *c,
            ComparableToken::Unmapped { .. } => panic!("literal fixture"),
        })
        .collect()
}

fn coverage(parts: &[ResolutionRange], total_tokens: usize) -> Coverage {
    Coverage {
        total_tokens,
        resolved_tokens: parts
            .iter()
            .filter(|p| p.state != ResolutionState::Unresolved)
            .map(|p| p.comparable_range.end - p.comparable_range.start)
            .sum(),
        ratio: None,
    }
}

fn exercise(
    old_text: [&str; 2],
    new_text: [&str; 2],
    mutation: u8,
    budget: usize,
) -> Result<(usize, usize)> {
    let mut old_blocks = [block(1, old_text[0]), block(2, old_text[1])];
    let mut new_blocks = [block(101, new_text[0]), block(102, new_text[1])];
    match mutation {
        1 => new_blocks[0].pages = vec![1],
        2 => old_blocks[1] = block(2, "QZ"),
        3 => {
            let source = old_blocks[0].canonical.source_map[0].source.clone();
            old_blocks[1].raw.source_map[0].source = source.clone();
            old_blocks[1].canonical.source_map[0].source = source;
        }
        4 => old_blocks[0].raw.text.replace_range(..1, "x"),
        _ => {}
    }
    let old = super::super::super::super::SidePlan::inspect("component old", &old_blocks)?
        .materialize()?;
    let new = super::super::super::super::SidePlan::inspect("component new", &new_blocks)?
        .materialize()?;
    let alignment = Alignment {
        spans: Vec::new(),
        main_anchors: Vec::new(),
        move_candidates: Vec::new(),
    };
    let mut assessor = Assessor::new([&old, &new], &alignment, None, DiffOptions::default())?;
    let mut work = 32_000_000;
    let old_parent = light_group_metadata(
        &old,
        0..2,
        BlockSeparator::Concatenate,
        &mut work,
        COARSE_MEMORY_BYTES,
    )
    .expect("whole old")
    .span;
    let new_parent = light_group_metadata(
        &new,
        0..2,
        BlockSeparator::Concatenate,
        &mut work,
        COARSE_MEMORY_BYTES,
    )
    .expect("whole new")
    .span;
    let lengths = [
        old_parent.comparable_range.end,
        new_parent.comparable_range.end,
    ];
    assessor.records.push(RelationAssessment {
        old_span: Some(old_parent.clone()),
        new_span: Some(new_parent.clone()),
        parent: None,
        outcome: RelationOutcome::Established,
        search: SearchCompleteness::Complete,
        assumptions: vec![
            ComparisonAssumption::InputReadingOrder,
            ComparisonAssumption::CanonicalNormalization,
            ComparisonAssumption::LocalEvidenceBoundaries,
        ],
        reasons: Vec::new(),
    });
    let mut domains = vec![(
        DomainKey {
            local: None,
            old: 0..2,
            new: 0..2,
            old_separator: BlockSeparator::Concatenate,
            new_separator: BlockSeparator::Concatenate,
        },
        DomainProof {
            scope: ProofScope::ExactKey,
            relation: 0,
            unique: false,
            search: SearchCompleteness::Complete,
            edits: Vec::new(),
            lengths,
            strict_unique: false,
            stable_events: None,
        },
    )];
    match mutation {
        5 => domains[0].1.search = SearchCompleteness::Incomplete,
        6 => domains[0].1.scope = ProofScope::BroadRootRefusal,
        7 => assessor.records[0]
            .assumptions
            .push(ComparisonAssumption::AlternativeLineBreakNormalization),
        8 => {
            assessor.records[0]
                .old_span
                .as_mut()
                .expect("old")
                .comparable_range
                .start = 1;
        }
        14 => assessor.options.max_assessment_ranges = 2,
        15..=31 => assessor.options.max_assessment_ranges = usize::from(mutation - 12),
        _ => {}
    }
    let components = [
        component_span(&old, 0).expect("old component"),
        component_span(&new, 0).expect("new component"),
    ];
    let mut partitions: [Vec<ResolutionRange>; 2] = std::array::from_fn(|s| {
        let side = [&old, &new][s];
        (0..2)
            .map(|i| ResolutionRange {
                block: side.blocks[i].block,
                canonical_range: ScalarRange {
                    start: 0,
                    end: side.canonical[i].len(),
                },
                comparable_range: TokenRange {
                    start: 0,
                    end: side.canonical[i].len(),
                },
                state: ResolutionState::Unresolved,
            })
            .collect()
    });
    let mut unresolved = vec![UnresolvedRegion {
        old_span: Some(old_parent.clone()),
        new_span: Some(new_parent.clone()),
        evidence: Vec::new(),
    }];
    let mut proven = vec![ProvenChangedRegion {
        old_span: Some(old_parent),
        new_span: Some(new_parent),
        confidence: Confidence::High,
        proof: ChangedRegionProof::ExactTokenMultisetMismatch,
    }];
    let mut candidates = Vec::new();
    let mut owners = [Ownership::new(), Ownership::new()];
    match mutation {
        9 => candidates.push(ChangeCandidate {
            change: Change::single_occurrence(
                ChangeKind::Replacement,
                Some(components[0].clone()),
                Some(components[1].clone()),
                Confidence::High,
                Vec::new(),
            ),
            relation: 0,
            alternative_group: 0,
        }),
        10 => proven.push(ProvenChangedRegion {
            old_span: Some(components[0].clone()),
            new_span: None,
            confidence: Confidence::High,
            proof: ChangedRegionProof::OneSidedNonEmptyRange,
        }),
        11 => owners[0].changed.push(SourceInterval {
            block_index: 0,
            start: 0,
            end: components[0].comparable_range.end,
        }),
        12 => owners[0].accepted.push(SourceInterval {
            block_index: 0,
            start: 0,
            end: components[0].comparable_range.end,
        }),
        13 => proven.push(proven[0].clone()),
        _ => {}
    }
    if matches!(mutation, 11 | 12) {
        partitions[0][0].state = if mutation == 11 {
            ResolutionState::Changed
        } else {
            ResolutionState::Equal
        };
    }
    let before = partitions.clone();
    let parent_before = assessor.records[0].clone();
    assessor.remaining_work = budget;
    let [p0, p1] = &mut partitions;
    let adopted = assessor.recover_source_closed_initial_children(
        &mut owners,
        &candidates,
        &proven,
        local::CoupledPublication {
            domains: &domains,
            partitions: [p0, p1],
            unresolved: &mut unresolved,
        },
        domains.capacity(),
        assessor.options.max_assessment_ranges,
    )?;
    assert_eq!(assessor.records[0], parent_before);
    assert!(assessor.remaining_work <= budget);
    assert!(assessor.localized_edits.is_empty() && assessor.semantic_acceptance.is_empty());
    let resolved = partitions
        .iter()
        .flat_map(|p| p.iter())
        .filter(|p| p.state == ResolutionState::Equal)
        .map(|p| p.comparable_range.end - p.comparable_range.start)
        .sum::<usize>();
    if !adopted {
        assert_eq!(
            assessor.records.len(),
            1,
            "unpublished closure must be removed"
        );
        assert_eq!(partitions, before);
        assert_eq!(
            resolved,
            before
                .iter()
                .flat_map(|p| p.iter())
                .filter(|p| p.state == ResolutionState::Equal)
                .map(|p| p.comparable_range.end - p.comparable_range.start)
                .sum::<usize>()
        );
    } else {
        assert!(resolved > 0);
        assert!(assessor.records.len() < assessor.options.max_assessment_ranges);
        for child in assessor.records.iter().skip(2) {
            let a = child.old_span.as_ref().expect("old equal");
            let b = child.new_span.as_ref().expect("new equal");
            assert_eq!(child.parent, Some(1));
            assert!(a.comparable_range.end <= components[0].comparable_range.end);
            assert!(b.comparable_range.end <= components[1].comparable_range.end);
            assert!(
                old.canonical[0][a.comparable_range.start..a.comparable_range.end]
                    .iter()
                    .all(|t| matches!(t, ComparableToken::Scalar(c) if !c.is_whitespace()))
            );
        }
        let comparison = super::super::super::super::Comparison {
            changes: Vec::new(),
            change_candidates: candidates,
            assessment: None,
            proven_changed_regions: proven,
            formatting_changes: Vec::new(),
            unresolved_regions: unresolved,
            old_coverage: coverage(&partitions[0], old.total_tokens),
            new_coverage: coverage(&partitions[1], new.total_tokens),
        };
        let [old_resolution, new_resolution] = partitions;
        let assessment = ComparisonAssessment {
            relations: assessor.records,
            old_resolution,
            new_resolution,
            policy_version: super::super::super::ASSESSMENT_POLICY_VERSION,
            localized_edits: Vec::new(),
            scoped_change_evidence: Vec::new(),
            evidence_link_work: Default::default(),
            review_units: Vec::new(),
            work_limit: budget,
            work_used: budget - assessor.remaining_work,
            anchor_work: Default::default(),
            local_view_work: Default::default(),
            suffix_reuse_work: Default::default(),
            work_by_stage: super::super::super::AssessmentWork {
                emission: budget - assessor.remaining_work,
                ..Default::default()
            },
            candidates_truncated: false,
        };
        validation::validate([&old, &new], &assessment, &comparison)?;
        for defect in 0..5 {
            let mut altered = assessment.clone();
            match defect {
                0 => altered.relations[1].assumptions.clear(),
                1 => altered.relations[1].parent = None,
                2 => altered.relations[0].search = SearchCompleteness::Incomplete,
                3 => altered.relations[2].assumptions.clear(),
                4 => {
                    altered.relations[1]
                        .old_span
                        .as_mut()
                        .expect("closure")
                        .comparable_range
                        .start = 1;
                }
                _ => unreachable!(),
            }
            assert!(
                validation::validate([&old, &new], &altered, &comparison).is_err(),
                "certificate defect {defect}"
            );
        }
    }
    Ok((
        resolved
            - before
                .iter()
                .flat_map(|p| p.iter())
                .filter(|p| p.state == ResolutionState::Equal)
                .map(|p| p.comparable_range.end - p.comparable_range.start)
                .sum::<usize>(),
        budget - assessor.remaining_work,
    ))
}

#[test]
fn c31_forced_component_composes_without_owning_ambiguous_digits_or_spaces() -> Result<()> {
    let (resolved, _) = exercise(
        ["[22] AUTHOR ", "XY tail"],
        ["[24] AUTHOR ", "XY change"],
        0,
        32_000_000,
    )?;
    assert_eq!(resolved, 16);
    Ok(())
}

#[test]
fn c31_component_keeps_source_ownership_candidate_page_and_scope_vetoes() -> Result<()> {
    for mutation in 1..=14 {
        assert_eq!(
            exercise(
                ["[22] AUTHOR ", "XY tail"],
                ["[24] AUTHOR ", "XY change"],
                mutation,
                32_000_000
            )?
            .0,
            0,
            "mutation {mutation}"
        );
    }
    Ok(())
}

#[test]
fn c31_component_every_budget_stop_is_atomic_and_valid() -> Result<()> {
    let inputs = (["AAB ", "XY"], ["ACB ", "XY"]);
    let (full, work) = exercise(inputs.0, inputs.1, 0, 32_000_000)?;
    assert!(full > 0);
    for budget in 0..=work {
        let (resolved, used) = exercise(inputs.0, inputs.1, 0, budget)?;
        assert!(resolved <= full && used <= budget);
    }
    Ok(())
}

#[test]
fn c31_captured_parent_pairs_match_independent_python_oracle() -> Result<()> {
    for case in captured() {
        let old = case.old.iter().collect::<Vec<_>>();
        let new = case.new.iter().collect::<Vec<_>>();
        let mut budget = 32_000_000;
        let analysis = semantic::mandatory_match_analysis_with_memory_limit(
            &old,
            &new,
            &mut budget,
            COARSE_MEMORY_BYTES,
        )?
        .expect("bounded complete real frame");
        assert_eq!(
            analysis.pairs(),
            case.mandatory_pairs,
            "relation {}",
            case.relation
        );
        let a = [
            text(&case.old[..case.cuts[0]]),
            text(&case.old[case.cuts[0]..]),
        ];
        let b = [
            text(&case.new[..case.cuts[1]]),
            text(&case.new[case.cuts[1]..]),
        ];
        let (resolved, work) = exercise(
            [&a[0], &a[1]],
            [&b[0], &b[1]],
            u8::from(case.relation == 1723),
            32_000_000,
        )?;
        if case.relation == 1721 {
            assert!(resolved > 0);
        } else {
            assert_eq!(resolved, 0);
        }
        println!(
            "C31 modeled relation={} resolved_positions={resolved} work={work}",
            case.relation
        );
    }
    Ok(())
}

#[test]
fn c31_empty_and_repeated_boundaries_supply_no_component_certificate() -> Result<()> {
    let blocks = [block(1, ""), block(2, "AB")];
    let side =
        super::super::super::super::SidePlan::inspect("empty component", &blocks)?.materialize()?;
    assert!(component_span(&side, 0).is_none());
    let old = "AAA"
        .chars()
        .map(ComparableToken::Scalar)
        .collect::<Vec<_>>();
    let new = "AAAA"
        .chars()
        .map(ComparableToken::Scalar)
        .collect::<Vec<_>>();
    let mut work = 32_000_000;
    let analysis = semantic::mandatory_match_analysis_with_memory_limit(
        &old.iter().collect::<Vec<_>>(),
        &new.iter().collect::<Vec<_>>(),
        &mut work,
        COARSE_MEMORY_BYTES,
    )?
    .expect("complete repeated input");
    assert!(
        analysis.pairs().is_empty(),
        "equal boundary text does not force a cut"
    );
    Ok(())
}

#[test]
fn c31_component_retains_the_output_stop_slot_at_small_limits() -> Result<()> {
    let mut held = 0;
    let mut published = 0;
    for mutation in 15..=31 {
        let (resolved, _) = exercise(
            ["[22] AUTHOR ", "XY tail"],
            ["[24] AUTHOR ", "XY change"],
            mutation,
            32_000_000,
        )?;
        assert!(resolved <= 16);
        if resolved == 0 {
            held += 1;
        } else {
            published += 1;
        }
    }
    // Diagnostic capacity can refuse before the relation slot is reached.
    // Both refusal and publication must preserve the reserved output slot.
    assert!(held > 0 && published > 0);
    Ok(())
}
