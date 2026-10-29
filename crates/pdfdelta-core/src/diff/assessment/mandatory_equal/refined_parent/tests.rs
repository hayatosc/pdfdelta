use super::*;
use crate::{
    alignment::{Alignment, BlockSeparator},
    diff::{
        Change, ChangeCandidate, ChangeKind, ChangedRegionProof, Comparison, Confidence,
        DiffOptions, ProvenChangedRegion, SidePlan,
        assessment::{
            ASSESSMENT_POLICY_VERSION, AssessmentWork, BlockId, ComparisonAssessment,
            ComparisonAssumption, Coverage, ProofScope, RelationAssessment, RelationOutcome,
            ScalarRange, SearchCompleteness, SourceInterval, TextSpan, TokenRange,
            coarse_reviewed_domain_bytes, validation,
        },
    },
    layout::BlockRole,
    model::GlyphId,
    normalize::{BlockText, MappedText, SourceMapEntry, TextSource, TextSourceAtom},
};

fn block(id: u64, text: &str, page: u32) -> BlockText {
    let mapped = MappedText {
        text: text.into(),
        unmapped: Vec::new(),
        source_map: text
            .chars()
            .enumerate()
            .map(|(i, _)| SourceMapEntry {
                output_range: ScalarRange {
                    start: i,
                    end: i + 1,
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(id * 1000 + i as u64 + 1))].into(),
                },
            })
            .collect(),
    };
    BlockText {
        block: BlockId(id),
        role: BlockRole::Body,
        raw: mapped.clone(),
        matching_tokens: mapped.comparable_tokens().expect("fixture"),
        canonical: mapped,
        matching: text.into(),
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: vec![page],
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: Some(Vec::new()),
        page_breaks: Some(Vec::new()),
    }
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

fn exercise(mutation: u8, budget: usize, limit: usize) -> Result<(usize, usize)> {
    let old_blocks = [block(1, "AB CD", 7), block(2, "EF", 7)];
    let new_blocks = [
        block(101, "AB", 5),
        block(102, "CD", 5),
        block(103, "EF", 5),
    ];
    let old = SidePlan::inspect("refined old", &old_blocks)?.materialize()?;
    let new = SidePlan::inspect("refined new", &new_blocks)?.materialize()?;
    let sides = [&old, &new];
    let alignment = Alignment {
        spans: Vec::new(),
        main_anchors: Vec::new(),
        move_candidates: Vec::new(),
    };
    let mut assessor = Assessor::new(sides, &alignment, None, DiffOptions::default())?;
    assessor.options.max_assessment_ranges = limit;
    let mut setup = 1_000_000;
    let parent_spans = [
        light_group_metadata(
            &old,
            0..2,
            BlockSeparator::Space,
            &mut setup,
            COARSE_MEMORY_BYTES,
        )
        .expect("old")
        .span,
        light_group_metadata(
            &new,
            0..3,
            BlockSeparator::Space,
            &mut setup,
            COARSE_MEMORY_BYTES,
        )
        .expect("new")
        .span,
    ];
    assessor.records.push(RelationAssessment {
        old_span: Some(parent_spans[0].clone()),
        new_span: Some(parent_spans[1].clone()),
        parent: None,
        outcome: RelationOutcome::Established,
        search: SearchCompleteness::Complete,
        assumptions: vec![
            ComparisonAssumption::InputReadingOrder,
            ComparisonAssumption::CanonicalNormalization,
            ComparisonAssumption::ReconstructedSpacing,
            ComparisonAssumption::LocalEvidenceBoundaries,
        ],
        reasons: Vec::new(),
    });
    let domains = vec![(
        DomainKey {
            local: None,
            old: 0..2,
            new: 0..3,
            old_separator: BlockSeparator::Space,
            new_separator: BlockSeparator::Space,
        },
        DomainProof {
            scope: ProofScope::ExactKey,
            relation: 0,
            unique: true,
            strict_unique: true,
            search: SearchCompleteness::Complete,
            edits: Vec::new(),
            lengths: [8, 8],
            stable_events: None,
        },
    )];
    let bytes =
        coarse_reviewed_domain_bytes(&domains, domains.capacity(), &mut setup).expect("ledger");
    let mut parts: [Vec<ResolutionRange>; 2] = std::array::from_fn(|s| {
        sides[s]
            .blocks
            .iter()
            .enumerate()
            .map(|(i, block)| ResolutionRange {
                block: block.block,
                canonical_range: ScalarRange {
                    start: 0,
                    end: sides[s].canonical[i].len(),
                },
                comparable_range: TokenRange {
                    start: 0,
                    end: sides[s].canonical[i].len(),
                },
                state: ResolutionState::Unresolved,
            })
            .collect()
    });
    let blocked_span = TextSpan {
        blocks: vec![BlockId(1)],
        separator: None,
        canonical_range: ScalarRange { start: 0, end: 5 },
        comparable_range: TokenRange { start: 0, end: 5 },
    };
    let mut owners = [Ownership::new(), Ownership::new()];
    let mut candidates = Vec::new();
    let mut proven = Vec::new();
    match mutation {
        1 => candidates.push(ChangeCandidate {
            change: Change::single_occurrence(
                ChangeKind::Deletion,
                Some(blocked_span.clone()),
                None,
                Confidence::High,
                Vec::new(),
            ),
            relation: 0,
            alternative_group: 0,
        }),
        2 => proven.push(ProvenChangedRegion {
            old_span: Some(blocked_span),
            new_span: None,
            confidence: Confidence::High,
            proof: ChangedRegionProof::OneSidedNonEmptyRange,
        }),
        3 | 4 => {
            let interval = SourceInterval {
                block_index: 0,
                start: 0,
                end: 5,
            };
            if mutation == 3 {
                owners[0].changed.push(interval);
                parts[0][0].state = ResolutionState::Changed;
            } else {
                owners[0].accepted.push(interval);
                parts[0][0].state = ResolutionState::Equal;
            }
        }
        5 => assessor.records[0]
            .assumptions
            .push(ComparisonAssumption::AlternativeLineBreakNormalization),
        _ => {}
    }
    let before = parts.clone();
    let prior = assessor.records[0].clone();
    assessor.remaining_work = budget;
    let [a, b] = &mut parts;
    let adopted = assessor.recover_refined_equal_parents(
        &domains,
        bytes,
        &mut owners,
        [a, b],
        EqualConstraints {
            candidates: &candidates,
            regions: &proven,
            original: &[],
        },
    )?;
    assert_eq!(assessor.records[0], prior);
    assert!(assessor.remaining_work <= budget);
    assert!(assessor.localized_edits.is_empty() && assessor.semantic_acceptance.is_empty());
    let mut gained = 0;
    for side in 0..2 {
        for p in &parts[side] {
            for i in p.comparable_range.start..p.comparable_range.end {
                let was = before[side]
                    .iter()
                    .find(|q| {
                        q.block == p.block
                            && q.comparable_range.start <= i
                            && i < q.comparable_range.end
                    })
                    .expect("prior token");
                if was.state != ResolutionState::Unresolved {
                    assert_eq!(p.state, was.state, "old ownership is immutable");
                } else if p.state == ResolutionState::Equal {
                    gained += 1;
                    if (1..=4).contains(&mutation) && side == 0 {
                        assert_ne!(p.block, BlockId(1), "competing or owned source is held");
                    }
                    assert!(
                        matches!(sides[side].canonical[sides[side].index[&p.block]][i],
                        crate::normalize::ComparableToken::Scalar(c) if !c.is_whitespace())
                    );
                }
            }
        }
    }
    assert_eq!(adopted, gained > 0);
    assert_eq!(
        gained % 4,
        0,
        "both sides publish each two-character run atomically"
    );
    assert!(assessor.records.len() <= limit.max(1));
    for i in 1..assessor.records.len() {
        assert!(compatible_refined_child(sides, &assessor.records, i));
        assert_eq!(assessor.records[i].parent, Some(0));
    }
    if mutation == 0 {
        let comparison = Comparison {
            changes: Vec::new(),
            change_candidates: Vec::new(),
            assessment: None,
            proven_changed_regions: Vec::new(),
            formatting_changes: Vec::new(),
            unresolved_regions: Vec::new(),
            old_coverage: coverage(&parts[0], old.total_tokens),
            new_coverage: coverage(&parts[1], new.total_tokens),
        };
        let [old_resolution, new_resolution] = parts;
        let assessment = ComparisonAssessment {
            policy_version: ASSESSMENT_POLICY_VERSION,
            relations: assessor.records,
            old_resolution,
            new_resolution,
            localized_edits: Vec::new(),
            scoped_change_evidence: Vec::new(),
            evidence_link_work: Default::default(),
            review_units: Vec::new(),
            work_limit: budget,
            work_used: budget - assessor.remaining_work,
            anchor_work: Default::default(),
            local_view_work: Default::default(),
            suffix_reuse_work: Default::default(),
            work_by_stage: AssessmentWork {
                emission: budget - assessor.remaining_work,
                ..Default::default()
            },
            candidates_truncated: false,
        };
        validation::validate(sides, &assessment, &comparison)?;
        if assessment.relations.len() > 1 {
            for defect in 0..6 {
                let mut altered = assessment.clone();
                match defect {
                    0 => altered.relations[1].parent = None,
                    1 => altered.relations[0].assumptions.clear(),
                    2 => altered.relations[1]
                        .assumptions
                        .retain(|a| *a != ComparisonAssumption::MandatoryMatchingEquality),
                    3 => altered.relations[1]
                        .assumptions
                        .push(ComparisonAssumption::PageShiftedMandatoryMatchingEquality),
                    4 => {
                        altered.relations[0]
                            .old_span
                            .as_mut()
                            .expect("parent")
                            .comparable_range
                            .start = 1;
                    }
                    5 => {
                        let span = altered.relations[1].new_span.as_mut().expect("child");
                        span.blocks[0] = BlockId(102);
                    }
                    _ => unreachable!(),
                }
                assert!(
                    validation::validate(sides, &altered, &comparison).is_err(),
                    "defect {defect}"
                );
            }
        }
    }
    Ok((gained, budget - assessor.remaining_work))
}

#[test]
fn c34_refined_publication_keeps_parent_and_whitespace() -> Result<()> {
    assert_eq!(exercise(0, 1_000_000, 1000)?.0, 12);
    Ok(())
}

#[test]
fn c34_refined_publication_preserves_every_competing_obligation() -> Result<()> {
    for mutation in 1..=4 {
        assert_eq!(
            exercise(mutation, 1_000_000, 1000)?.0,
            4,
            "mutation {mutation}"
        );
    }
    assert_eq!(exercise(5, 1_000_000, 1000)?.0, 0);
    Ok(())
}

#[test]
fn c34_refined_publication_is_atomic_at_every_budget_and_output_cut() -> Result<()> {
    let full = exercise(0, 1_000_000, 1000)?;
    assert!(full.1 < 100_000);
    for budget in 0..=full.1 {
        let (gained, used) = exercise(0, budget, 1000)?;
        assert!(gained <= full.0 && used <= budget);
    }
    for limit in 0..20 {
        assert!(exercise(0, 1_000_000, limit)?.0 <= full.0);
    }
    Ok(())
}
