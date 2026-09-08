//! Joins final local edits by their emitted event indices, never by text alone.

use pdfdelta_core::{
    diff::{AtomicEdit, RelationOutcome, TokenRange},
    normalize::ScalarRange,
};

use super::{
    ActualChange, ActualChangeOccurrence, ActualRelationTraceStatus, BlockText, ComparableToken,
    Comparison, HashMap, MatchingLimits, MatchingScanBudget, TextSpan, collapse_whitespace,
    resolve_span, semantic_hunk_from_edits, span_group_comparable_tokens,
};

pub(super) fn for_change(
    comparison: &Comparison,
    change_index: usize,
    maps: [&HashMap<u64, &BlockText>; 2],
    budget: &mut MatchingScanBudget,
) -> Option<ActualChange> {
    let assessment = comparison.assessment.as_ref()?;
    let index = assessment
        .localized_edits
        .partition_point(|trace| trace.changes.end <= change_index);
    let trace = assessment.localized_edits.get(index)?;
    if !trace.changes.contains(&change_index) {
        return None;
    }
    let relation = assessment.relations.get(trace.relation)?;
    if relation.outcome != RelationOutcome::Established {
        return None;
    }
    let bounded = [relation.old_span.as_ref()?, relation.new_span.as_ref()?];
    let change = comparison.changes.get(change_index)?;
    let limits = MatchingLimits::default();
    budget
        .charge_visits(
            trace.edits.len().checked_mul(change.occurrences.len())?,
            limits,
        )
        .ok()?;
    // Quote context may extend beyond the proved interval. It locates source
    // annotations; the relation still records only the bounded comparison.
    let contexts = [
        full_context(bounded[0], maps[0], budget, limits)?,
        full_context(bounded[1], maps[1], budget, limits)?,
    ];
    let texts = [
        resolve_span(maps[0], &contexts[0])?,
        resolve_span(maps[1], &contexts[1])?,
    ];
    let bytes = texts[0].0.len().checked_add(texts[1].0.len())?;
    let repetitions = trace
        .edits
        .len()
        .checked_add(8)?
        .checked_mul(change.occurrences.len())?;
    budget
        .charge_text(bytes.checked_mul(repetitions)?, limits)
        .ok()?;
    let edits = trace
        .edits
        .iter()
        .map(|edit| {
            let shift = |range: &std::ops::Range<usize>, offset: usize| {
                Some(range.start.checked_add(offset)?..range.end.checked_add(offset)?)
            };
            Some(AtomicEdit {
                old: shift(&edit.old, bounded[0].comparable_range.start)?,
                new: shift(&edit.new, bounded[1].comparable_range.start)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let occurrences = change
        .occurrences
        .iter()
        .map(|occurrence| {
            let hunk =
                semantic_hunk_from_edits(occurrence, &contexts[0], &contexts[1], &edits, maps)?;
            let lengths = [
                occurrence
                    .old_span
                    .as_ref()
                    .map(|span| span.comparable_range.end - span.comparable_range.start),
                occurrence
                    .new_span
                    .as_ref()
                    .map(|span| span.comparable_range.end - span.comparable_range.start),
            ];
            Some(ActualChangeOccurrence {
                old_text: hunk.old_text.clone(),
                new_text: hunk.new_text.clone(),
                old_relation_context: Some(collapse_whitespace(&texts[0].0)),
                new_relation_context: Some(collapse_whitespace(&texts[1].0)),
                old_relation_context_len: Some(texts[0].1),
                new_relation_context_len: Some(texts[1].1),
                old_comparable_len: lengths[0],
                new_comparable_len: lengths[1],
                old_atomic_changed_tokens: Some(hunk.old_atomic_changed_tokens),
                new_atomic_changed_tokens: Some(hunk.new_atomic_changed_tokens),
                old_semantic_changed_tokens: lengths[0],
                new_semantic_changed_tokens: lengths[1],
                semantic_hunks: Some(vec![hunk]),
                relation_trace: ActualRelationTraceStatus::Assessed {
                    relation: trace.relation,
                },
                resolvable: true,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ActualChange {
        kind: change.kind,
        reported_hunk_count: change.occurrences.len(),
        occurrences,
    })
}

fn full_context(
    bounded: &TextSpan,
    map: &HashMap<u64, &BlockText>,
    budget: &mut MatchingScanBudget,
    limits: MatchingLimits,
) -> Option<TextSpan> {
    budget.charge_visits(bounded.blocks.len(), limits).ok()?;
    for block in &bounded.blocks {
        let canonical = &map.get(&block.0)?.canonical;
        if !canonical.unmapped.is_empty() {
            return None;
        }
        budget
            .charge_text(canonical.text.len().checked_add(1)?, limits)
            .ok()?;
    }
    let tokens = span_group_comparable_tokens(map, bounded)?;
    let scalars = tokens
        .iter()
        .filter(|token| matches!(token, ComparableToken::Scalar(_)))
        .count();
    Some(TextSpan {
        blocks: bounded.blocks.clone(),
        separator: bounded.separator,
        comparable_range: TokenRange {
            start: 0,
            end: tokens.len(),
        },
        canonical_range: ScalarRange {
            start: 0,
            end: scalars,
        },
    })
}

#[cfg(test)]
#[path = "../../examples/support/glyph_fixture.rs"]
mod glyph_fixture;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pdfdelta_core::{
        pipeline::{PipelineOptions, compare_extraction_outcomes},
        source::ExtractionOutcome,
    };

    use super::super::{
        Annotation, ExpectedChange, build_block_map, compute_quality, flatten_actual_changes,
    };
    use super::*;

    #[test]
    fn source_backed_edits_match_contextual_annotations_without_legacy_traces() {
        let read = |side| {
            glyph_fixture::read(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("../../fixtures/issue12/fips-{side}.glyphs.json")),
            )
            .expect("source fixture")
        };
        let outcome = compare_extraction_outcomes(
            ExtractionOutcome::complete(read("old")),
            ExtractionOutcome::complete(read("new")),
            PipelineOptions::default(),
        )
        .expect("normal comparison");
        let maps = [
            build_block_map(&outcome.old_blocks),
            build_block_map(&outcome.new_blocks),
        ];
        let actual = flatten_actual_changes(&outcome.comparison, [&maps[0], &maps[1]], &[], &[]);
        assert_eq!(actual.len(), 5);
        assert!(
            actual
                .iter()
                .flat_map(|change| &change.occurrences)
                .all(|occurrence| matches!(
                    occurrence.relation_trace,
                    ActualRelationTraceStatus::Assessed { .. }
                ))
        );
        // These are independent source-fixture assertions, not additions to the
        // frozen real-world annotation set.
        let expected: Vec<ExpectedChange> = serde_json::from_str(
            r#"[
            {"id":"capitalization","kind":"replacement",
             "old_quote":"This Standard defines methods for digital signature generation",
             "new_quote":"This standard defines methods for digital signature generation",
             "old_changed_ranges":[{"start":5,"end":6}],
             "new_changed_ranges":[{"start":5,"end":6}]},
            {"id":"comma","kind":"deletion",
             "old_quote":"binary data (commonly called a message), and for the verification",
             "old_changed_quote":","}
        ]"#,
        )
        .expect("fixed annotations");
        let quality = compute_quality(Annotation::Partial, &expected, &actual);
        assert_eq!(quality.recall, Some(1.0), "{quality:?}");
        assert_eq!(quality.kind_accuracy, Some(1.0));

        let assessment = outcome.comparison.assessment.as_ref().expect("assessment");
        assessment
            .validate(&outcome.comparison)
            .expect("valid witness");
        for invalid_case in 0..4 {
            let mut invalid = assessment.clone();
            let trace = invalid.localized_edits.first_mut().expect("local witness");
            match invalid_case {
                0 => trace.relation = assessment.relations.len(),
                1 => trace.changes.end = outcome.comparison.changes.len() + 1,
                2 => trace.edits[0].old.end = usize::MAX,
                _ => trace.edits.clear(),
            }
            assert!(invalid.validate(&outcome.comparison).is_err());
        }
        let mut budget = MatchingScanBudget {
            occurrence_visits: usize::MAX,
            text_bytes: 0,
        };
        assert!(for_change(&outcome.comparison, 0, [&maps[0], &maps[1]], &mut budget).is_none());
    }
}
