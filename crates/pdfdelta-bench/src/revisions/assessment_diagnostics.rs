//! Bounded joins between reviewed source locations and final assessment records.

use std::collections::BTreeSet;

use pdfdelta_core::diff::{Change, RelationAssessment, RelationOutcome, SearchCompleteness};
use serde::Serialize;

use super::{
    DiagnosticBudget, DiagnosticLimits, DiagnosticScanError, DiagnosticScanResult, ExpectedChange,
    ExpectedQuoteLocations, QuoteLocateOutcome, QuoteLocation, location, required_location_mask,
    span_location_range,
};
use crate::revisions::{assessment_evaluation, change_kind_name};

/// Final-pipeline evidence, separate from candidate-generator diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FinalAssessmentDiagnostics {
    pub complete: bool,
    pub assessment: crate::evaluation::AssessmentEvaluation,
    pub records: Vec<ExpectedAssessmentTrace>,
}

/// Location overlap identifies records to inspect, not a correct semantic match.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExpectedAssessmentTrace {
    pub expected_id: String,
    pub old: AssessmentQuoteLocation,
    pub new: AssessmentQuoteLocation,
    pub scan_complete: bool,
    pub candidates: Vec<AssessmentEventReference>,
    pub accepted_changes: Vec<AssessmentEventReference>,
    pub relations: Vec<AssessmentRelationTrace>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AssessmentQuoteLocation {
    pub status: &'static str,
    pub block: Option<u64>,
    pub canonical_range: Option<[usize; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AssessmentEventReference {
    pub index: usize,
    pub kind: &'static str,
    pub relation: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AssessmentRelationTrace {
    pub index: usize,
    pub parent: Option<usize>,
    pub outcome: &'static str,
    pub search: &'static str,
    pub reasons: Vec<String>,
    pub assumptions: Vec<String>,
    pub old: Option<AssessmentSpanSummary>,
    pub new: Option<AssessmentSpanSummary>,
}

/// Wide parent domains retain their extent without copying every source block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AssessmentSpanSummary {
    pub block_count: usize,
    pub first_block: Option<u64>,
    pub last_block: Option<u64>,
    pub blocks: Option<Vec<u64>>,
    pub separator: Option<&'static str>,
    pub canonical_range: [usize; 2],
    pub comparable_range: [usize; 2],
}

type BlockMaps<'a> =
    [&'a std::collections::HashMap<u64, &'a pdfdelta_core::normalize::BlockText>; 2];

pub(super) fn evaluate(
    expected: &[ExpectedChange],
    locations: &[ExpectedQuoteLocations],
    comparison: &pdfdelta_core::diff::Comparison,
    blocks: BlockMaps<'_>,
    limits: DiagnosticLimits,
    all_expected_processed: bool,
) -> Result<Option<FinalAssessmentDiagnostics>, String> {
    let Some(assessment) = &comparison.assessment else {
        return Ok(None);
    };
    // This scan has its own diagnostic budget. It cannot consume comparison
    // work or change the legacy diagnostic's availability and metrics.
    let mut budget = DiagnosticBudget::default();
    let mut records = Vec::new();
    for (expected, locations) in expected.iter().zip(locations) {
        if budget.charge_output(limits).is_err() {
            break;
        }
        let mut trace = ExpectedAssessmentTrace {
            expected_id: expected.id.clone(),
            old: quote_location(locations.old.as_ref()),
            new: quote_location(locations.new.as_ref()),
            scan_complete: false,
            candidates: Vec::new(),
            accepted_changes: Vec::new(),
            relations: Vec::new(),
        };
        match trace_records(
            &mut trace,
            locations,
            comparison,
            blocks,
            &mut budget,
            limits,
        ) {
            Ok(()) => trace.scan_complete = true,
            Err(DiagnosticScanError::Limited) => {}
            Err(DiagnosticScanError::Invalid(error)) => return Err(error),
        }
        records.push(trace);
        if budget.limited {
            break;
        }
    }
    Ok(Some(FinalAssessmentDiagnostics {
        complete: all_expected_processed && !budget.limited && records.len() == expected.len(),
        assessment: assessment_evaluation(assessment),
        records,
    }))
}

fn trace_records(
    trace: &mut ExpectedAssessmentTrace,
    locations: &ExpectedQuoteLocations,
    comparison: &pdfdelta_core::diff::Comparison,
    blocks: BlockMaps<'_>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<()> {
    let Some(assessment) = &comparison.assessment else {
        return Ok(());
    };
    if [&locations.old, &locations.new]
        .into_iter()
        .any(|located| matches!(located, Some(QuoteLocateOutcome::Limited)))
    {
        budget.limited = true;
        return Err(DiagnosticScanError::Limited);
    }
    if required_location_mask(locations).is_none() {
        return Ok(());
    }
    let mut selected = BTreeSet::new();
    for (index, candidate) in comparison.change_candidates.iter().enumerate() {
        budget.charge_scan(1, limits)?;
        if event_overlaps(&candidate.change, None, locations, blocks, budget, limits)? {
            budget.charge_output(limits)?;
            trace.candidates.push(AssessmentEventReference {
                index,
                kind: change_kind_name(candidate.change.kind),
                relation: Some(candidate.relation),
            });
            selected.insert(candidate.relation);
        }
    }
    for (index, change) in comparison.changes.iter().enumerate() {
        budget.charge_scan(1, limits)?;
        let script_index = assessment
            .localized_edits
            .partition_point(|script| script.changes.end <= index);
        let relation_index = assessment
            .localized_edits
            .get(script_index)
            .filter(|script| script.changes.contains(&index))
            .map(|script| script.relation);
        let context = relation_index.and_then(|index| assessment.relations.get(index));
        if event_overlaps(change, context, locations, blocks, budget, limits)? {
            budget.charge_output(limits)?;
            trace.accepted_changes.push(AssessmentEventReference {
                index,
                kind: change_kind_name(change.kind),
                relation: relation_index,
            });
            if let Some(relation) = relation_index {
                selected.insert(relation);
            }
        }
    }
    for (index, relation) in assessment.relations.iter().enumerate() {
        budget.charge_region(limits)?;
        if spans_overlap(
            [relation.old_span.as_ref(), relation.new_span.as_ref()],
            locations,
            blocks,
            budget,
            limits,
        )? {
            selected.insert(index);
        }
    }
    // Parent indices must decrease, as required by comparison validation.
    // Resolve the chain explicitly so a malformed caller-built record cannot loop.
    for index in selected.iter().copied().collect::<Vec<_>>() {
        let mut current = index;
        loop {
            budget.charge_scan(1, limits)?;
            let relation = assessment.relations.get(current).ok_or_else(|| {
                DiagnosticScanError::Invalid("final diagnostic references missing relation".into())
            })?;
            let Some(parent) = relation.parent else { break };
            if parent >= current {
                return Err(DiagnosticScanError::Invalid(
                    "final diagnostic parent must precede its child".into(),
                ));
            }
            current = parent;
            if !selected.insert(parent) {
                break;
            }
        }
    }
    for index in selected {
        budget.charge_output(limits)?;
        let relation = &assessment.relations[index];
        budget.charge_scan(
            relation
                .reasons
                .len()
                .saturating_add(relation.assumptions.len()),
            limits,
        )?;
        trace
            .relations
            .push(relation_trace(index, &assessment.relations[index]));
    }
    Ok(())
}

fn event_overlaps(
    change: &Change,
    context: Option<&RelationAssessment>,
    locations: &ExpectedQuoteLocations,
    blocks: BlockMaps<'_>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<bool> {
    for occurrence in &change.occurrences {
        budget.charge_scan(1, limits)?;
        if spans_overlap(
            // An absent side is located by the actual established parent, so
            // a deletion can be inspected against a paired context quote.
            // This is a diagnostic join, not a change-kind or quality match.
            [
                occurrence
                    .old_span
                    .as_ref()
                    .or_else(|| context.and_then(|relation| relation.old_span.as_ref())),
                occurrence
                    .new_span
                    .as_ref()
                    .or_else(|| context.and_then(|relation| relation.new_span.as_ref())),
            ],
            locations,
            blocks,
            budget,
            limits,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn spans_overlap(
    spans: [Option<&pdfdelta_core::diff::TextSpan>; 2],
    locations: &ExpectedQuoteLocations,
    blocks: BlockMaps<'_>,
    budget: &mut DiagnosticBudget,
    limits: DiagnosticLimits,
) -> DiagnosticScanResult<bool> {
    for (side, quote) in [location(&locations.old), location(&locations.new)]
        .into_iter()
        .enumerate()
    {
        let Some(quote) = quote else { continue };
        let Some(span) = spans[side] else {
            return Ok(false);
        };
        let Some(range) = span_location_range(span, quote, blocks[side], budget, limits)? else {
            return Ok(false);
        };
        let span_range = span.canonical_range;
        let overlaps = if span_range.start == span_range.end {
            range.start <= span_range.start && span_range.start <= range.end
        } else {
            span_range.start < range.end && range.start < span_range.end
        };
        if !overlaps {
            return Ok(false);
        }
    }
    Ok(true)
}

fn quote_location(outcome: Option<&QuoteLocateOutcome>) -> AssessmentQuoteLocation {
    let (status, found) = match outcome {
        None => ("not_applicable", None),
        Some(QuoteLocateOutcome::Unique(found)) => ("unique", Some(found)),
        Some(QuoteLocateOutcome::Missing) => ("missing", None),
        Some(QuoteLocateOutcome::Segmented) => ("segmented", None),
        Some(QuoteLocateOutcome::Ambiguous) => ("ambiguous", None),
        Some(QuoteLocateOutcome::Indeterminate) => ("indeterminate", None),
        Some(QuoteLocateOutcome::Limited) => ("limited", None),
    };
    AssessmentQuoteLocation {
        status,
        block: found.map(|QuoteLocation { block, .. }| block.0),
        canonical_range: found.map(|found| [found.scalar_range.start, found.scalar_range.end]),
    }
}

fn relation_trace(index: usize, relation: &RelationAssessment) -> AssessmentRelationTrace {
    AssessmentRelationTrace {
        index,
        parent: relation.parent,
        outcome: match relation.outcome {
            RelationOutcome::Established => "established",
            RelationOutcome::Tentative => "tentative",
        },
        search: match relation.search {
            SearchCompleteness::Complete => "complete",
            SearchCompleteness::Incomplete => "incomplete",
        },
        reasons: relation
            .reasons
            .iter()
            .map(|reason| format!("{reason:?}"))
            .collect(),
        assumptions: relation
            .assumptions
            .iter()
            .map(|assumption| format!("{assumption:?}"))
            .collect(),
        old: relation.old_span.as_ref().map(span_summary),
        new: relation.new_span.as_ref().map(span_summary),
    }
}

fn span_summary(span: &pdfdelta_core::diff::TextSpan) -> AssessmentSpanSummary {
    AssessmentSpanSummary {
        block_count: span.blocks.len(),
        first_block: span.blocks.first().map(|block| block.0),
        last_block: span.blocks.last().map(|block| block.0),
        blocks: (span.blocks.len() <= 32)
            .then(|| span.blocks.iter().map(|block| block.0).collect()),
        separator: span.separator.map(|separator| match separator {
            pdfdelta_core::alignment::BlockSeparator::Space => "space",
            pdfdelta_core::alignment::BlockSeparator::Concatenate => "concatenate",
        }),
        canonical_range: [span.canonical_range.start, span.canonical_range.end],
        comparable_range: [span.comparable_range.start, span.comparable_range.end],
    }
}
