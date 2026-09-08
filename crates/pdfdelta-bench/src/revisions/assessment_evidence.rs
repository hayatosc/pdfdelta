//! Joins final local edits by their emitted event indices, never by text alone.

use pdfdelta_core::{
    diff::{AtomicEdit, ChangeEvent, RelationOutcome, TokenRange},
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
    if change.occurrences.is_empty() {
        return None;
    }
    let limits = MatchingLimits::default();
    budget.charge_visits(trace.edits.len(), limits).ok()?;
    // Quote context may extend beyond the proved interval. It locates source
    // annotations; the relation still records only the bounded comparison.
    let contexts = [
        full_context(bounded[0], maps[0], budget, limits)?,
        full_context(bounded[1], maps[1], budget, limits)?,
    ];
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
    from_witness(
        change,
        [&contexts[0], &contexts[1]],
        &edits,
        maps,
        ActualRelationTraceStatus::Assessed {
            relation: trace.relation,
        },
        budget,
    )
}

/// Coalesces only the hunks of one event with one independently selected witness.
pub(super) fn from_witness(
    change: &ChangeEvent,
    contexts: [&TextSpan; 2],
    edits: &[AtomicEdit],
    maps: [&HashMap<u64, &BlockText>; 2],
    relation_trace: ActualRelationTraceStatus,
    budget: &mut MatchingScanBudget,
) -> Option<ActualChange> {
    if change.occurrences.is_empty() {
        return None;
    }
    let limits = MatchingLimits::default();
    budget
        .charge_visits(edits.len().checked_mul(change.occurrences.len())?, limits)
        .ok()?;
    let texts = [
        resolve_span(maps[0], contexts[0])?,
        resolve_span(maps[1], contexts[1])?,
    ];
    let bytes = texts[0].0.len().checked_add(texts[1].0.len())?;
    let repetitions = edits
        .len()
        .checked_add(8)?
        .checked_mul(change.occurrences.len())?;
    budget
        .charge_text(bytes.checked_mul(repetitions)?, limits)
        .ok()?;
    let hunks = change
        .occurrences
        .iter()
        .map(|occurrence| {
            semantic_hunk_from_edits(occurrence, contexts[0], contexts[1], edits, maps)
        })
        .collect::<Option<Vec<_>>>()?;
    let old = envelope_text(
        change
            .occurrences
            .iter()
            .filter_map(|part| part.old_span.as_ref()),
        maps[0],
    )?;
    let new = envelope_text(
        change
            .occurrences
            .iter()
            .filter_map(|part| part.new_span.as_ref()),
        maps[1],
    )?;
    let atomic_counts = hunks
        .iter()
        .try_fold((0usize, 0usize), |(old, new), hunk| {
            Some((
                old.checked_add(hunk.old_atomic_changed_tokens)?,
                new.checked_add(hunk.new_atomic_changed_tokens)?,
            ))
        })?;
    let owned_counts =
        change
            .occurrences
            .iter()
            .try_fold((0usize, 0usize), |(old, new), part| {
                let count = |span: &Option<TextSpan>| {
                    span.as_ref().map_or(Some(0), |span| {
                        span.comparable_range
                            .end
                            .checked_sub(span.comparable_range.start)
                    })
                };
                Some((
                    old.checked_add(count(&part.old_span)?)?,
                    new.checked_add(count(&part.new_span)?)?,
                ))
            })?;
    // The event and one independently selected relation establish this grouping.
    // Disjoint exact masks are hunks of one logical occurrence, not repeated
    // occurrences inferred from equal text. Public fragmentation stays separate.
    Some(ActualChange {
        kind: change.kind,
        reported_hunk_count: change.occurrences.len(),
        occurrences: vec![ActualChangeOccurrence {
            old_text: old.0,
            new_text: new.0,
            old_relation_context: Some(collapse_whitespace(&texts[0].0)),
            new_relation_context: Some(collapse_whitespace(&texts[1].0)),
            old_relation_context_len: Some(texts[0].1),
            new_relation_context_len: Some(texts[1].1),
            old_comparable_len: old.1,
            new_comparable_len: new.1,
            old_atomic_changed_tokens: Some(atomic_counts.0),
            new_atomic_changed_tokens: Some(atomic_counts.1),
            old_semantic_changed_tokens: Some(owned_counts.0),
            new_semantic_changed_tokens: Some(owned_counts.1),
            semantic_hunks: Some(hunks),
            relation_trace,
            resolvable: true,
        }],
    })
}

/// Builds display text only; the exact hunk masks retain changed-token ownership.
/// All spans already passed the same relation-context check above.
fn envelope_text<'a>(
    mut spans: impl Iterator<Item = &'a TextSpan>,
    map: &HashMap<u64, &BlockText>,
) -> Option<(Option<String>, Option<usize>)> {
    let Some(mut envelope) = spans.next().cloned() else {
        return Some((None, None));
    };
    for span in spans {
        if span.comparable_range.start < envelope.comparable_range.end
            || span.canonical_range.start < envelope.canonical_range.end
        {
            return None;
        }
        envelope.comparable_range.end = span.comparable_range.end;
        envelope.canonical_range.end = span.canonical_range.end;
    }
    let (text, length) = resolve_span(map, &envelope)?;
    Some((Some(collapse_whitespace(&text)), Some(length)))
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
