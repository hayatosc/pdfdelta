//! Projection of a native-glyph text comparison into review cases.
//!
//! This adapter has a different contract from the shared-evidence one and does
//! not pretend otherwise: it examines extracted glyphs and nothing else, so its
//! cases never suggest that visual, form, or relationship evidence was
//! considered. Its obligations are the comparison's own exclusive partition of
//! comparable tokens, so a case is accounted for by the token intervals it
//! covers rather than by channel source references.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    BundleIdentity, CaseCompleteness, CaseId, Completeness, Detail, EngineClass, EngineOutcome,
    EvidenceRef, GapScope, Hypothesis, HypothesisId, PlannerLimits, ReasonRecord, RequiredEvidence,
    RetrievalAction, ReviewAssumption, ReviewCase, ReviewPlan, ReviewQuestion, ReviewReason,
    ReviewText, ScalarInterval, Side, SideLocation, TextCoverage, TextOrder, TokenInterval,
    UnlocalizedGap,
    case::{Cardinality, UnmappedMark},
    planner::{Assembly, Budget, CaseKey, Identifiers, assemble, evidence_ref},
};
use crate::{
    alignment::AlignmentEvidence,
    diff::{
        Comparison, ComparisonAssumption, RelationAssessment, RelationOutcome, ResolutionState,
        SearchCompleteness, TextSpan,
    },
    document::{Channel, SourceRef},
    layout::BlockId,
    model::{GlyphEvidence, PageId},
    normalize::BlockText,
    report::{DocumentSide, ExtractionStatus, SideIndex, SpanSourceEvidence, SpanSourceProjector},
    source::{ExtractionIssueKind, ExtractionScope},
};

/// One native-glyph comparison, ready to project.
///
/// The blocks and glyph evidence must be the ones the comparison was produced
/// from: spans are indexes into that exact normalization.
pub struct NativeTextReview<'a> {
    pub identity: BundleIdentity,
    pub outcome: EngineOutcome,
    pub comparison: &'a Comparison,
    pub old_blocks: &'a [BlockText],
    pub new_blocks: &'a [BlockText],
    pub old_glyphs: &'a [GlyphEvidence],
    pub new_glyphs: &'a [GlyphEvidence],
    pub extraction: &'a ExtractionStatus,
}

struct SideResolver<'a> {
    index: Option<SideIndex<'a>>,
    projector: Option<SpanSourceProjector<'a>>,
}

struct Planner<'a> {
    input: &'a NativeTextReview<'a>,
    limits: PlannerLimits,
    budget: Budget,
    identifiers: Identifiers,
    cases: Vec<ReviewCase>,
    gaps: Vec<UnlocalizedGap>,
    old: SideResolver<'a>,
    new: SideResolver<'a>,
    covered: BTreeSet<(Side, u64, usize)>,
}

/// Projects a native-glyph comparison into a bounded review plan.
///
/// The comparison is neither modified nor recomputed.
#[must_use]
pub fn plan_native_text(input: &NativeTextReview<'_>, limits: PlannerLimits) -> ReviewPlan {
    let planner = Planner {
        input,
        limits,
        budget: Budget::new(limits),
        identifiers: Identifiers::new(),
        cases: Vec::new(),
        gaps: Vec::new(),
        old: SideResolver {
            index: SideIndex::new(input.old_blocks).ok(),
            projector: SpanSourceProjector::new(
                input.old_blocks,
                input.old_glyphs,
                Default::default(),
            )
            .ok(),
        },
        new: SideResolver {
            index: SideIndex::new(input.new_blocks).ok(),
            projector: SpanSourceProjector::new(
                input.new_blocks,
                input.new_glyphs,
                Default::default(),
            )
            .ok(),
        },
        covered: BTreeSet::new(),
    };
    planner.plan()
}

impl<'a> Planner<'a> {
    fn plan(mut self) -> ReviewPlan {
        self.unresolved_regions();
        self.candidate_groups();
        self.tentative_relations();
        self.residual_partition();
        self.acquisition();
        self.enumeration_limits();
        assemble(
            Assembly {
                // Structural context comes from the evidence graph, which this
                // contract does not build.
                contexts: Vec::new(),
                identity: self.input.identity.clone(),
                outcome: self.input.outcome.clone(),
                channels: &BTreeSet::from([Channel::Text]),
                inventory_gaps: Vec::new(),
                unlocalized_gaps: self.gaps,
                cases: self.cases,
                // This contract acquires no rasters, so no case can be answered
                // from an image here.
                visual_available: false,
            },
            &self.budget,
        )
    }

    fn side(&self, side: Side) -> &SideResolver<'a> {
        match side {
            Side::Old => &self.old,
            Side::New => &self.new,
        }
    }

    /// Retained text, unmapped tokens, and pages for one span.
    fn span_text(&self, side: Side, span: &TextSpan) -> Option<(ReviewText, Vec<u32>)> {
        let resolved = self.side(side).index.as_ref()?.resolve(span).ok()?;
        let text = ReviewText {
            side,
            scalar_count: resolved.text.chars().count(),
            text: resolved.text,
            // Block order is the supplied reading order for this contract; it
            // is an interpretation of the painting order, not a proof of it.
            order: TextOrder::InferredReadingOrder,
            unmapped: resolved
                .unmapped
                .iter()
                .map(|token| UnmappedMark {
                    position: token.scalar_offset,
                    raw_codes: vec![u32::from(token.glyph_id)],
                    sources: Vec::new(),
                })
                .collect(),
            omitted: Vec::new(),
            token_range: Some(TokenInterval::from(span.comparable_range)),
            canonical_range: Some(ScalarInterval::from(span.canonical_range)),
            sources: self
                .span_sources(side, span)
                .into_iter()
                .take(self.limits.max_sources_per_case)
                .map(|source| evidence_ref(side, source))
                .collect(),
        };
        Some((text, resolved.pages))
    }

    /// Glyph references a span selects.
    ///
    /// Synthetic spaces and line breaks reference their neighbours without
    /// owning a glyph of their own, so they contribute no reference here.
    fn span_sources(&self, side: Side, span: &TextSpan) -> BTreeSet<SourceRef> {
        let Some(projector) = self.side(side).projector.as_ref() else {
            return BTreeSet::new();
        };
        projector
            .project(span)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|evidence| match evidence {
                SpanSourceEvidence::Glyph { glyph_id, .. } => {
                    Some(SourceRef::Native { glyph: glyph_id })
                }
                SpanSourceEvidence::SyntheticSpace { .. }
                | SpanSourceEvidence::LineBreak { .. }
                | SpanSourceEvidence::BlockSeparatorSpace => None,
            })
            .collect()
    }

    fn location(side: Side, pages: &[u32]) -> SideLocation {
        SideLocation::unknown(side).with_page(pages.first().map(|page| PageId(*page)))
    }

    /// The token positions a span occupies, for partition accounting.
    fn cover(&mut self, side: Side, span: &TextSpan) -> Vec<TextCoverage> {
        let mut covered = Vec::new();
        for block in &span.blocks {
            for position in span.comparable_range.start..span.comparable_range.end {
                self.covered.insert((side, block.0, position));
            }
            covered.push(TextCoverage {
                side,
                block: block.0,
                token_range: TokenInterval::from(span.comparable_range),
            });
        }
        covered
    }

    fn push(&mut self, case: ReviewCase) {
        if !self.budget.accept_case() {
            return;
        }
        self.cases.push(case);
    }

    fn case(&mut self, draft: Draft) -> ReviewCase {
        let Draft {
            question,
            engine_class,
            old_span,
            new_span,
            reasons,
            assumptions,
            completeness,
            hypotheses,
            alternatives_total,
        } = draft;
        let mut key = CaseKey::new(question);
        key.debug(&engine_class);
        for span in [old_span.as_ref(), new_span.as_ref()].into_iter().flatten() {
            key.debug(&span.blocks);
            key.debug(&span.comparable_range);
            key.debug(&span.canonical_range);
        }
        for reason in &reasons {
            key.debug(&reason.reason);
        }
        let case_id = self.identifiers.case(&key.finish());
        let digest = case_id.as_str()[1..].to_owned();
        let old = old_span
            .as_ref()
            .and_then(|span| self.span_text(Side::Old, span));
        let new = new_span
            .as_ref()
            .and_then(|span| self.span_text(Side::New, span));
        let mut covered = Vec::new();
        if let Some(span) = &old_span {
            covered.extend(self.cover(Side::Old, span));
        }
        if let Some(span) = &new_span {
            covered.extend(self.cover(Side::New, span));
        }
        let evidence: Vec<EvidenceRef> = old
            .as_ref()
            .map(|(text, _)| text.sources.clone())
            .unwrap_or_default()
            .into_iter()
            .chain(
                new.as_ref()
                    .map(|(text, _)| text.sources.clone())
                    .unwrap_or_default(),
            )
            .collect();
        let readable = old.as_ref().is_some_and(|(text, _)| !text.text.is_empty())
            || new.as_ref().is_some_and(|(text, _)| !text.text.is_empty());
        let mut available_actions = vec![RetrievalAction::Show {
            case: case_id.clone(),
            detail: Detail::Text,
            cursor: None,
        }];
        if !hypotheses.is_empty() || alternatives_total.is_none() {
            available_actions.push(RetrievalAction::Show {
                case: case_id.clone(),
                detail: Detail::Alternatives,
                cursor: None,
            });
        }
        ReviewCase {
            content_digest: digest,
            question,
            pipeline: self.input.identity.pipeline,
            engine_class,
            channels: BTreeSet::from([Channel::Text]),
            completeness,
            reasons,
            assumptions,
            old: old
                .as_ref()
                .map(|(_, pages)| Self::location(Side::Old, pages)),
            new: new
                .as_ref()
                .map(|(_, pages)| Self::location(Side::New, pages)),
            old_text: old.map(|(text, _)| text),
            new_text: new.map(|(text, _)| text),
            alternatives_returned: hypotheses.len(),
            hypotheses,
            alternatives_total,
            omitted: Vec::new(),
            next_cursor: None,
            required_evidence: if readable {
                Vec::new()
            } else {
                // No raster exists under this contract, so nothing further can
                // be retrieved for material with no extracted text.
                vec![RequiredEvidence::Unavailable]
            },
            related_cases: Vec::new(),
            conflicts_with: Vec::new(),
            available_actions,
            evidence,
            covered,
            // This contract retains no rasters, so no rendered view exists.
            regions: Vec::new(),
            case_id,
        }
    }

    fn unresolved_regions(&mut self) {
        for region in &self.input.comparison.unresolved_regions {
            if !self.budget.visit_sources(1) {
                return;
            }
            let mut reasons: Vec<ReasonRecord> = region
                .evidence
                .iter()
                .filter_map(|evidence| alignment_reason(*evidence))
                .map(ReasonRecord::new)
                .collect();
            if reasons.is_empty() {
                reasons.push(
                    ReasonRecord::new(ReviewReason::Other)
                        .with_message(format!("alignment evidence: {:?}", region.evidence)),
                );
            }
            let draft = Draft {
                question: ReviewQuestion::CompareContent,
                engine_class: EngineClass::Unavailable,
                old_span: region.old_span.clone(),
                new_span: region.new_span.clone(),
                reasons,
                assumptions: vec![ReviewAssumption::InputReadingOrder],
                completeness: CaseCompleteness {
                    evidence: Completeness::Unknown,
                    candidate_enumeration: Completeness::Unknown,
                    solver_search: Completeness::Incomplete,
                    response: Completeness::Complete,
                },
                hypotheses: Vec::new(),
                alternatives_total: None,
            };
            let case = self.case(draft);
            self.push(case);
        }
    }

    /// Competing localized edits, grouped into the choice they represent.
    fn candidate_groups(&mut self) {
        let mut grouped: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (index, candidate) in self.input.comparison.change_candidates.iter().enumerate() {
            grouped
                .entry((candidate.relation, candidate.alternative_group))
                .or_default()
                .push(index);
        }
        for ((relation, _), members) in grouped {
            if !self.budget.visit_candidates(members.len()) {
                return;
            }
            let assessment = self
                .input
                .comparison
                .assessment
                .as_ref()
                .and_then(|assessment| assessment.relations.get(relation));
            let hypotheses: Vec<Hypothesis> = members
                .iter()
                .take(self.limits.max_hypotheses_per_case)
                .filter_map(|index| self.candidate_hypothesis(*index, &members))
                .collect();
            let (reasons, assumptions, search) = assessment.map_or_else(
                || {
                    (
                        vec![ReasonRecord::new(ReviewReason::AmbiguousEditLocation)],
                        vec![ReviewAssumption::InputReadingOrder],
                        SearchCompleteness::Incomplete,
                    )
                },
                |assessment| {
                    (
                        relation_reasons(assessment),
                        relation_assumptions(assessment),
                        assessment.search,
                    )
                },
            );
            let (old_span, new_span) = assessment.map_or((None, None), |assessment| {
                (assessment.old_span.clone(), assessment.new_span.clone())
            });
            let draft = Draft {
                question: ReviewQuestion::ResolveCorrespondence,
                engine_class: EngineClass::Unavailable,
                old_span,
                new_span,
                reasons,
                assumptions,
                completeness: CaseCompleteness {
                    evidence: Completeness::Unknown,
                    candidate_enumeration: Completeness::from_flag(
                        !self
                            .input
                            .comparison
                            .assessment
                            .as_ref()
                            .is_some_and(|assessment| assessment.candidates_truncated),
                    ),
                    solver_search: Completeness::from_flag(search == SearchCompleteness::Complete),
                    response: Completeness::Complete,
                },
                alternatives_total: (members.len() <= self.limits.max_hypotheses_per_case)
                    .then_some(members.len()),
                hypotheses,
            };
            let case = self.case(draft);
            self.push(case);
        }
    }

    fn candidate_hypothesis(&self, index: usize, group: &[usize]) -> Option<Hypothesis> {
        let candidate = self.input.comparison.change_candidates.get(index)?;
        let occurrence = candidate.change.occurrences.first()?;
        let old = occurrence
            .old_span
            .as_ref()
            .and_then(|span| self.span_text(Side::Old, span));
        let new = occurrence
            .new_span
            .as_ref()
            .and_then(|span| self.span_text(Side::New, span));
        Some(Hypothesis {
            id: HypothesisId::new(format!("H{index}")).ok()?,
            cardinality: Cardinality::from_counts(
                usize::from(occurrence.old_span.is_some()),
                usize::from(occurrence.new_span.is_some()),
            ),
            supplier: format!("{:?}", candidate.change.kind),
            // This contract ranks candidates with no numeric objective; a
            // confidence label is not one and is reported as counterevidence
            // context instead.
            objective_weight: None,
            mandatory_in_examined_optima: false,
            old: old
                .as_ref()
                .map(|(_, pages)| Self::location(Side::Old, pages)),
            new: new
                .as_ref()
                .map(|(_, pages)| Self::location(Side::New, pages)),
            evidence: old
                .as_ref()
                .map(|(text, _)| text.sources.clone())
                .unwrap_or_default()
                .into_iter()
                .chain(
                    new.as_ref()
                        .map(|(text, _)| text.sources.clone())
                        .unwrap_or_default(),
                )
                .collect(),
            old_text: old.map(|(text, _)| text),
            new_text: new.map(|(text, _)| text),
            conflicts_with: group
                .iter()
                .filter(|other| **other != index)
                .filter_map(|other| HypothesisId::new(format!("H{other}")).ok())
                .collect(),
            counterevidence: Vec::new(),
        })
    }

    /// Relations the engine could not establish, and that no candidate covers.
    fn tentative_relations(&mut self) {
        let Some(assessment) = self.input.comparison.assessment.as_ref() else {
            return;
        };
        let with_candidates: BTreeSet<usize> = self
            .input
            .comparison
            .change_candidates
            .iter()
            .map(|candidate| candidate.relation)
            .collect();
        for (index, relation) in assessment.relations.iter().enumerate() {
            if relation.outcome != RelationOutcome::Tentative || with_candidates.contains(&index) {
                continue;
            }
            if !self.budget.visit_sources(1) {
                return;
            }
            let draft = Draft {
                question: ReviewQuestion::ResolveCorrespondence,
                engine_class: EngineClass::Unavailable,
                old_span: relation.old_span.clone(),
                new_span: relation.new_span.clone(),
                reasons: relation_reasons(relation),
                assumptions: relation_assumptions(relation),
                completeness: CaseCompleteness {
                    evidence: Completeness::Unknown,
                    candidate_enumeration: Completeness::from_flag(
                        !assessment.candidates_truncated,
                    ),
                    solver_search: Completeness::from_flag(
                        relation.search == SearchCompleteness::Complete,
                    ),
                    response: Completeness::Complete,
                },
                hypotheses: Vec::new(),
                alternatives_total: None,
            };
            let case = self.case(draft);
            self.push(case);
        }
    }

    /// Unresolved intervals of the comparison's own exclusive partition that no
    /// case above already accounts for.
    fn residual_partition(&mut self) {
        let Some(assessment) = self.input.comparison.assessment.as_ref() else {
            return;
        };
        for (side, ranges) in [
            (Side::Old, &assessment.old_resolution),
            (Side::New, &assessment.new_resolution),
        ] {
            for range in ranges {
                if range.state != ResolutionState::Unresolved {
                    continue;
                }
                if !self.budget.visit_sources(1) {
                    return;
                }
                let uncovered = (range.comparable_range.start..range.comparable_range.end)
                    .any(|position| !self.covered.contains(&(side, range.block.0, position)));
                if !uncovered {
                    continue;
                }
                let span = TextSpan {
                    blocks: vec![range.block],
                    separator: None,
                    canonical_range: range.canonical_range,
                    comparable_range: range.comparable_range,
                };
                let (old_span, new_span) = match side {
                    Side::Old => (Some(span), None),
                    Side::New => (None, Some(span)),
                };
                let draft = Draft {
                    question: ReviewQuestion::ResolveCorrespondence,
                    engine_class: EngineClass::Unavailable,
                    old_span,
                    new_span,
                    reasons: vec![
                        ReasonRecord::new(ReviewReason::DiscoveredButUnexamined)
                            .with_channel(Channel::Text),
                    ],
                    assumptions: vec![ReviewAssumption::InputReadingOrder],
                    completeness: CaseCompleteness {
                        evidence: Completeness::Unknown,
                        candidate_enumeration: Completeness::Unknown,
                        solver_search: Completeness::Unknown,
                        response: Completeness::Complete,
                    },
                    hypotheses: Vec::new(),
                    alternatives_total: None,
                };
                let case = self.case(draft);
                self.push(case);
            }
        }
    }

    fn acquisition(&mut self) {
        for issue in &self.input.extraction.issues {
            let side = match issue.side {
                DocumentSide::Old => Side::Old,
                DocumentSide::New => Side::New,
            };
            let page = match issue.scope {
                ExtractionScope::Page(page) | ExtractionScope::PageGlyphGap { page, .. } => {
                    Some(page)
                }
                _ => None,
            };
            let reason = ReasonRecord::new(match issue.kind {
                ExtractionIssueKind::Unsupported => ReviewReason::UnsupportedChannel,
                ExtractionIssueKind::Unresolved => ReviewReason::ExtractionGap,
            })
            .with_message(issue.description.clone())
            .with_channel(Channel::Text)
            .with_page(page);
            let mut key = CaseKey::new(ReviewQuestion::AcquisitionGap);
            key.field(side.label().as_bytes());
            key.debug(&issue.scope);
            key.field(issue.description.as_bytes());
            let case_id = self.identifiers.case(&key.finish());
            let digest = case_id.as_str()[1..].to_owned();
            let location = SideLocation::unknown(side).with_page(page);
            let case = ReviewCase {
                content_digest: digest,
                question: ReviewQuestion::AcquisitionGap,
                pipeline: self.input.identity.pipeline,
                engine_class: EngineClass::Unavailable,
                channels: BTreeSet::from([Channel::Text]),
                completeness: CaseCompleteness {
                    evidence: Completeness::Incomplete,
                    candidate_enumeration: Completeness::Unknown,
                    solver_search: Completeness::Unknown,
                    response: Completeness::Complete,
                },
                reasons: vec![reason],
                assumptions: Vec::new(),
                old: (side == Side::Old).then(|| location.clone()),
                new: (side == Side::New).then_some(location),
                old_text: None,
                new_text: None,
                hypotheses: Vec::new(),
                alternatives_total: None,
                alternatives_returned: 0,
                omitted: Vec::new(),
                next_cursor: None,
                required_evidence: vec![RequiredEvidence::Unavailable],
                related_cases: Vec::new(),
                conflicts_with: Vec::new(),
                available_actions: Vec::new(),
                evidence: Vec::new(),
                covered: Vec::new(),
                regions: Vec::new(),
                case_id,
            };
            self.push(case);
        }
    }

    /// Enumeration that stopped inside the engine, which no case can localize.
    fn enumeration_limits(&mut self) {
        let Some(assessment) = self.input.comparison.assessment.as_ref() else {
            return;
        };
        if !assessment.candidates_truncated {
            return;
        }
        let mut key = CaseKey::new(ReviewQuestion::ResolveCorrespondence);
        key.field(b"candidates-truncated");
        let gap_id = self.identifiers.gap(&key.finish());
        self.gaps.push(UnlocalizedGap {
            gap_id,
            scope: GapScope::Document,
            channel: Some(Channel::Text),
            side: None,
            evidence_complete: Completeness::Unknown,
            reasons: vec![ReasonRecord::new(ReviewReason::OutputLimit).with_message(
                "candidate descriptions were truncated within the assessment output limit",
            )],
            sources: Vec::new(),
        });
    }
}

struct Draft {
    question: ReviewQuestion,
    engine_class: EngineClass,
    old_span: Option<TextSpan>,
    new_span: Option<TextSpan>,
    reasons: Vec<ReasonRecord>,
    assumptions: Vec<ReviewAssumption>,
    completeness: CaseCompleteness,
    hypotheses: Vec<Hypothesis>,
    alternatives_total: Option<usize>,
}

fn relation_reasons(relation: &RelationAssessment) -> Vec<ReasonRecord> {
    if relation.reasons.is_empty() {
        return vec![ReasonRecord::new(ReviewReason::CompetingCorrespondence)];
    }
    relation
        .reasons
        .iter()
        .map(|reason| ReasonRecord::new(ReviewReason::from(*reason)))
        .collect()
}

fn relation_assumptions(relation: &RelationAssessment) -> Vec<ReviewAssumption> {
    let mut assumptions: Vec<ReviewAssumption> = relation
        .assumptions
        .iter()
        .filter_map(|assumption| match assumption {
            ComparisonAssumption::InputReadingOrder => Some(ReviewAssumption::InputReadingOrder),
            ComparisonAssumption::CanonicalNormalization => {
                Some(ReviewAssumption::CanonicalNormalization)
            }
            ComparisonAssumption::UnmappedFontIdentity => {
                Some(ReviewAssumption::UnmappedFontIdentity)
            }
            ComparisonAssumption::ReconstructedSpacing => {
                Some(ReviewAssumption::ReconstructedSpacing)
            }
            // The remaining premises describe how one correspondence was
            // established rather than a modelling choice a reviewer must adopt.
            _ => None,
        })
        .collect();
    if relation.outcome == RelationOutcome::Tentative {
        assumptions.push(ReviewAssumption::AcceptedCorrespondence);
    }
    assumptions.sort_unstable();
    assumptions.dedup();
    assumptions
}

/// Alignment evidence that explains why a region stayed unresolved.
///
/// Supporting evidence such as an anchor or a similarity score is not a reason
/// and is deliberately not restated as one.
fn alignment_reason(evidence: AlignmentEvidence) -> Option<ReviewReason> {
    match evidence {
        AlignmentEvidence::CandidateSetEmpty => Some(ReviewReason::NoCandidateCounterpart),
        AlignmentEvidence::CandidateScoringRejected
        | AlignmentEvidence::DiffRejectedAsImplausible => Some(ReviewReason::CandidatesRejected),
        AlignmentEvidence::CandidateCompetition => Some(ReviewReason::CompetingCorrespondence),
        AlignmentEvidence::DiffEditDistanceExceeded => Some(ReviewReason::WorkLimit),
        AlignmentEvidence::SearchIncomplete => Some(ReviewReason::SearchIncomplete),
        AlignmentEvidence::NormalizationIssue => Some(ReviewReason::NormalizationUncertainty),
        AlignmentEvidence::ExtractionGap => Some(ReviewReason::ExtractionGap),
        AlignmentEvidence::ReadingOrderUnknown => Some(ReviewReason::UnknownReadingOrder),
        AlignmentEvidence::ReadingOrderInferred => Some(ReviewReason::InferredReadingOrder),
        AlignmentEvidence::ExactCanonical
        | AlignmentEvidence::TextSimilarity
        | AlignmentEvidence::Anchor
        | AlignmentEvidence::AnchorInterval
        | AlignmentEvidence::NeighborConsistency
        | AlignmentEvidence::NumericMask
        | AlignmentEvidence::SplitMerge
        | AlignmentEvidence::MoveCandidate
        | AlignmentEvidence::CandidateSource(_) => None,
    }
}

/// Blocks named by a coverage entry, for callers auditing the partition.
#[must_use]
pub fn covered_blocks(case: &ReviewCase) -> BTreeSet<(Side, BlockId)> {
    case.covered
        .iter()
        .map(|coverage| (coverage.side, BlockId(coverage.block)))
        .collect()
}

/// Identifiers of the cases that cover one token position, if any.
#[must_use]
pub fn cases_covering(
    plan: &ReviewPlan,
    side: Side,
    block: BlockId,
    position: usize,
) -> Vec<CaseId> {
    plan.cases
        .iter()
        .filter(|case| {
            case.covered.iter().any(|coverage| {
                coverage.side == side
                    && coverage.block == block.0
                    && coverage.token_range.start <= position
                    && position < coverage.token_range.end
            })
        })
        .map(|case| case.case_id.clone())
        .collect()
}
