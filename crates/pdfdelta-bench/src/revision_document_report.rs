//! Projects shared-route character masks onto independently extracted source text.

use std::collections::{BTreeMap, BTreeSet};

use pdfdelta_core::{
    document::{DocumentViewComparison, InterpretationStatus, SourceRef, TypedOperation},
    normalize::{BlockText, ComparableToken, TextSourceAtom},
};
use serde::Serialize;

use super::{
    Annotation, ExpectedDocument, ExpectedKind, ScopedTokenMetrics,
    revision_scopes::{
        ScopedExpectedTokenEvidence, TokenInterval, evaluate_projected_tokens,
        resolve_revision_scopes, validate_scoped_expected_changes,
    },
};

const MAX_SOURCE_TOKENS: usize = 2_000_000;
const MAX_SOURCE_REFERENCES: usize = 8_000_000;

#[derive(Debug, Serialize)]
pub struct RevisionDocumentEvaluation {
    pub schema_version: u32,
    pub pair: String,
    pub expectations: Vec<RevisionEventProjection>,
    pub scopes: Vec<RevisionScopeProjection>,
    pub projection_errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RevisionEventProjection {
    pub id: String,
    /// None means the annotation or report cannot be projected, not a missed event.
    pub matched: Option<bool>,
    pub operation: Option<[usize; 2]>,
    pub unavailable: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RevisionScopeProjection {
    pub id: String,
    pub tokens: Option<ScopedTokenMetrics>,
    pub unavailable: Option<String>,
}

type SourceIndex = BTreeMap<(Vec<SourceRef>, char), Option<TokenInterval>>;

struct ProjectedOperation {
    index: [usize; 2],
    replacement: bool,
    complete_mask: bool,
    sides: [Vec<TokenInterval>; 2],
}

fn source_index(blocks: &[BlockText]) -> Result<SourceIndex, String> {
    let mut index = SourceIndex::new();
    let mut tokens_used = 0usize;
    let mut references_used = 0usize;
    for (block_order, block) in blocks.iter().enumerate() {
        let required = block
            .canonical
            .text
            .chars()
            .count()
            .saturating_add(block.canonical.unmapped.len());
        if required > MAX_SOURCE_TOKENS.saturating_sub(tokens_used) {
            return Err("source projection token limit".into());
        }
        let tokens = block
            .canonical
            .comparable_tokens_with_sources()
            .map_err(|error| error.to_string())?;
        tokens_used = tokens_used
            .checked_add(tokens.len())
            .filter(|count| *count <= MAX_SOURCE_TOKENS)
            .ok_or("source projection token limit")?;
        for (position, (token, source)) in tokens.into_iter().enumerate() {
            let ComparableToken::Scalar(value) = token else {
                continue;
            };
            let mut refs = BTreeSet::new();
            for atom in source.atoms {
                match atom {
                    TextSourceAtom::Glyph(glyph) => {
                        refs.insert(SourceRef::Native { glyph });
                    }
                    TextSourceAtom::SyntheticSpace {
                        preceding,
                        following,
                    }
                    | TextSourceAtom::LineBreak {
                        preceding,
                        following,
                    } => {
                        refs.insert(SourceRef::Native { glyph: preceding });
                        refs.insert(SourceRef::Native { glyph: following });
                    }
                }
            }
            references_used = references_used
                .checked_add(refs.len())
                .filter(|count| *count <= MAX_SOURCE_REFERENCES)
                .ok_or("source projection reference limit")?;
            if refs.is_empty() {
                continue;
            }
            index
                .entry((refs.into_iter().collect(), value))
                .and_modify(|existing| *existing = None)
                .or_insert(Some(TokenInterval {
                    block_order,
                    start: position,
                    end: position + 1,
                }));
        }
    }
    Ok(index)
}

fn project(
    comparison: &DocumentViewComparison,
    indexes: [&SourceIndex; 2],
) -> (Vec<ProjectedOperation>, Vec<String>) {
    let mut operations = Vec::new();
    let mut errors = Vec::new();
    for (scope_index, scope) in comparison.scopes.iter().enumerate() {
        if scope.interpretation == InterpretationStatus::Inferred {
            continue;
        }
        for (local_index, local) in scope.result.comparisons.iter().enumerate() {
            if local.interpretation == InterpretationStatus::Inferred || !local.compared {
                continue;
            }
            let Some(TypedOperation::TextChanged { old, new }) = &local.operation else {
                continue;
            };
            let index = [scope_index, local_index];
            let Some(mask) = &local.text_mask else {
                errors.push(format!("operation {index:?} has no exact character mask"));
                continue;
            };
            let mut sides = [Vec::new(), Vec::new()];
            let mut valid = true;
            for (side, text, changed, mandatory) in [
                (0, old, &mask.old, &mask.claims.mandatory_old),
                (1, new, &mask.new, &mask.claims.mandatory_new),
            ] {
                let Some(text) = text else {
                    valid = false;
                    continue;
                };
                let scalars = text.chars().collect::<Vec<_>>();
                if scalars.len() != mandatory.len() {
                    valid = false;
                    continue;
                }
                let mut seen = BTreeSet::new();
                for token in changed {
                    let Some(value) = scalars.get(token.position) else {
                        valid = false;
                        continue;
                    };
                    if !mandatory[token.position] || !seen.insert(token.position) {
                        valid = false;
                        continue;
                    }
                    let mut sources = token.sources.clone();
                    sources.sort_unstable();
                    sources.dedup();
                    match indexes[side].get(&(sources, *value)) {
                        Some(Some(coordinate)) => sides[side].push(*coordinate),
                        _ => valid = false,
                    }
                }
                if seen.len() != mandatory.iter().filter(|changed| **changed).count() {
                    valid = false;
                }
            }
            if valid {
                let localized = mask.old.len().saturating_add(mask.new.len());
                operations.push(ProjectedOperation {
                    index,
                    replacement: old.is_some() && new.is_some(),
                    complete_mask: local.unresolved.is_empty()
                        && mask.claims.changed_source_lower == localized
                        && mask.claims.changed_source_upper == localized,
                    sides,
                });
            } else {
                errors.push(format!(
                    "operation {index:?} has ambiguous or unavailable source coordinates"
                ));
            }
        }
    }
    (operations, errors)
}

fn points(
    intervals: &[TokenInterval],
    work: &mut usize,
) -> Result<BTreeSet<(usize, usize)>, String> {
    let mut output = BTreeSet::new();
    for interval in intervals {
        if interval.end < interval.start
            || interval.end - interval.start > MAX_SOURCE_TOKENS.saturating_sub(output.len())
        {
            return Err("event projection token limit".into());
        }
        *work = work
            .checked_sub(interval.end - interval.start)
            .ok_or("event projection work limit")?;
        output.extend(
            (interval.start..interval.end).map(|position| (interval.block_order, position)),
        );
    }
    Ok(output)
}

/// Evaluates source-projected text observations. A replacement requires one
/// correspondence operation with both exact masks, never independent fragments.
/// Unsupported event representations and invalid selectors remain unavailable.
pub(crate) fn evaluate(
    expected: &ExpectedDocument,
    blocks: [&[BlockText]; 2],
    comparison: &DocumentViewComparison,
) -> Result<RevisionDocumentEvaluation, String> {
    let indexes = [source_index(blocks[0])?, source_index(blocks[1])?];
    let (operations, projection_errors) = project(comparison, [&indexes[0], &indexes[1]]);
    let reported = [
        operations
            .iter()
            .flat_map(|operation| operation.sides[0].iter().copied())
            .collect::<Vec<_>>(),
        operations
            .iter()
            .flat_map(|operation| operation.sides[1].iter().copied())
            .collect::<Vec<_>>(),
    ];
    let mut expectations = Vec::new();
    let mut event_work = 32_000_000usize;
    for change in &expected.changes {
        let matched = (|| {
            if !projection_errors.is_empty() {
                return Err("report source projection is incomplete".to_owned());
            }
            let scope_id = change
                .scope
                .as_ref()
                .ok_or("unscoped event projection is unavailable")?;
            let selected = expected
                .scopes
                .iter()
                .filter(|scope| &scope.id == scope_id)
                .cloned()
                .collect::<Vec<_>>();
            if selected.len() != 1 {
                return Err("scope is missing or duplicated".into());
            }
            let scopes = resolve_revision_scopes(&selected, blocks[0], blocks[1])?;
            let evidence = validate_scoped_expected_changes(
                std::slice::from_ref(change),
                &scopes,
                blocks[0],
                blocks[1],
            )?;
            let target = [
                points(&evidence.old, &mut event_work)?,
                points(&evidence.new, &mut event_work)?,
            ];
            if target.iter().all(BTreeSet::is_empty) {
                return Err("empty event mask cannot establish a change".into());
            }
            if !matches!(change.kind, ExpectedKind::Replacement) {
                // Existing shared text operations describe paired views; they do
                // not establish an inserted/deleted/moved element's extent.
                return Ok(None);
            }
            let mut matched = None;
            for operation in &operations {
                if operation.replacement
                    && operation.complete_mask
                    && points(&operation.sides[0], &mut event_work)? == target[0]
                    && points(&operation.sides[1], &mut event_work)? == target[1]
                    && matched.replace(operation.index).is_some()
                {
                    return Err("multiple operations claim the same event".into());
                }
            }
            Ok(matched)
        })();
        expectations.push(match matched {
            Ok(operation) => RevisionEventProjection {
                id: change.id.clone(),
                matched: Some(operation.is_some()),
                operation,
                unavailable: None,
            },
            Err(reason) => RevisionEventProjection {
                id: change.id.clone(),
                matched: None,
                operation: None,
                unavailable: Some(reason),
            },
        });
    }
    let mut scopes = Vec::new();
    for scope in &expected.scopes {
        let score = (|| {
            if !projection_errors.is_empty() {
                return Err("report source projection is incomplete".to_owned());
            }
            if expected.annotation != Annotation::ScopedComplete && scope.completeness.is_none() {
                return Err("scope is not completely annotated".into());
            }
            let resolved =
                resolve_revision_scopes(std::slice::from_ref(scope), blocks[0], blocks[1])?;
            let changes = expected
                .changes
                .iter()
                .filter(|change| change.scope.as_ref() == Some(&scope.id))
                .cloned()
                .collect::<Vec<_>>();
            let evidence: ScopedExpectedTokenEvidence =
                validate_scoped_expected_changes(&changes, &resolved, blocks[0], blocks[1])?;
            evaluate_projected_tokens(reported.clone(), evidence, &resolved, blocks)
        })();
        scopes.push(match score {
            Ok(tokens) => RevisionScopeProjection {
                id: scope.id.clone(),
                tokens: Some(tokens),
                unavailable: None,
            },
            Err(reason) => RevisionScopeProjection {
                id: scope.id.clone(),
                tokens: None,
                unavailable: Some(reason),
            },
        });
    }
    Ok(RevisionDocumentEvaluation {
        schema_version: 1,
        pair: expected.pair.clone(),
        expectations,
        scopes,
        projection_errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::{
        layout::{BlockId, BlockRole},
        model::GlyphId,
        normalize::{MappedText, ScalarRange, SourceMapEntry, TextSource},
    };
    use serde_json::{Value, json};

    fn block(text: &str, first: u64) -> BlockText {
        let mapped = MappedText {
            text: text.into(),
            unmapped: Vec::new(),
            source_map: text
                .chars()
                .enumerate()
                .map(|(index, _)| SourceMapEntry {
                    output_range: ScalarRange {
                        start: index,
                        end: index + 1,
                    },
                    source: TextSource {
                        atoms: [TextSourceAtom::Glyph(GlyphId(first + index as u64))].into(),
                    },
                })
                .collect(),
        };
        BlockText {
            block: BlockId(0),
            role: BlockRole::Body,
            raw: mapped.clone(),
            canonical: mapped,
            matching: text.into(),
            matching_tokens: text.chars().map(ComparableToken::Scalar).collect(),
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: None,
            position_signatures: None,
            line_breaks: None,
            page_breaks: None,
        }
    }

    fn annotation() -> ExpectedDocument {
        serde_json::from_value(json!({
            "version": 1, "pair": "projection", "reviewed_on": "2026-09-10", "annotation": "scoped_complete",
            "scopes": [{"id": "scope", "old": {"start_quote":"left A right", "end_quote":"left A right"},
                "new": {"start_quote":"left B right", "end_quote":"left B right"}}],
            "changes": [{"id":"edit", "kind":"replacement", "scope":"scope", "old_quote":"left A right", "new_quote":"left B right",
                "old_changed_ranges":[{"start":5,"end":6}], "new_changed_ranges":[{"start":5,"end":6}]}],
        })).expect("annotation")
    }

    fn operation(old: &str, new: &str, changed: [bool; 2]) -> Value {
        let mut mandatory = [
            vec![false; old.chars().count()],
            vec![false; new.chars().count()],
        ];
        for (side, change) in changed.into_iter().enumerate() {
            if change {
                mandatory[side][5] = true;
            }
        }
        let masks = changed
            .into_iter()
            .enumerate()
            .map(|(side, changed)| {
                if changed {
                    json!([{"position":5,"sources":[{"origin":"native","glyph":105 + side * 100}]}])
                } else {
                    json!([])
                }
            })
            .collect::<Vec<_>>();
        json!({
            "old":[1], "new":[2], "interpretation":"conditional_on_correspondence", "compared":true,
            "operation":{"kind":"text_changed","old":old,"new":new}, "pixel_mask":null, "unresolved":[],
            "text_mask":{"convention":"literal-minimal-source-tokens-v1", "old":masks[0], "new":masks[1],
                "claims":{"changed_source_lower":2,"changed_source_upper":2,"mandatory_old":mandatory[0],"mandatory_new":mandatory[1],"normalization_pairs":1}}
        })
    }

    fn comparison(operations: Vec<Value>, inferred: bool) -> DocumentViewComparison {
        serde_json::from_value(json!({
            "relations":[], "relation_unresolved":[], "scopes":[{
                "parent":null,"depth":0,"interpretation":if inferred {"inferred"} else {"conditional_on_correspondence"},
                "result":{
                    "candidates":{"proposals":[],"examined_pairs":0,"group_token_checks":0,"group_constraint_checks":0,"exhaustive":true},
                    "matching":{"channels":{"text":true,"visual":false,"forms":false,"relations":false,"presentation":false},
                        "objective":"scoped_identity_then_literal_then_inferred_structure_v3", "scope":{"old":0,"new":0},
                        "components":[],"conflict_checks":0,"ownership_visits":0,"source_only_mandatory":[],"inferred_proposals":[],"conflict_search_complete":true},
                    "visual_search":{"examined_pairs":0,"compared_pixels":0,"exhaustive":true},
                    "text_search":{"examined_pairs":0,"source_ownership_visits":0,"source_conflict_checks":0,"source_search_states":0,
                        "protected_correspondences":[],"old_nodes":[],"new_nodes":[],"token_visits":0,"feature_entries":0,"exhaustive":true},
                    "comparisons":operations,"accepted_correspondences":[],"structural_correspondences":[],"unresolved":[]
                }
            }]
        })).expect("report comparison")
    }

    #[test]
    fn unlocalized_changes_prevent_whole_event_recovery_but_keep_token_evidence() {
        let old = [block("left A right x", 100)];
        let new = [block("left B right xx", 200)];
        let mut local = operation("left A right x", "left B right xx", [true, true]);
        // The inserted x has two equally optimal locations. Only A and B
        // belong to the mandatory mask, despite the exact changed count of 3.
        local["text_mask"]["claims"]["changed_source_lower"] = json!(3);
        local["text_mask"]["claims"]["changed_source_upper"] = json!(3);
        let result = evaluate(&annotation(), [&old, &new], &comparison(vec![local], false))
            .expect("project partially localized change");
        assert!(result.projection_errors.is_empty());
        assert_eq!(result.expectations[0].matched, Some(false));
        assert_eq!(
            result.scopes[0]
                .tokens
                .as_ref()
                .expect("scope metrics")
                .true_positive_tokens,
            2
        );
    }

    #[test]
    fn exact_masks_require_one_operation_and_inference_never_counts() {
        let old = [block("left A right", 100)];
        let new = [block("left B right", 200)];
        let joint = operation("left A right", "left B right", [true, true]);
        for inferred in [false, true] {
            let result = evaluate(
                &annotation(),
                [&old, &new],
                &comparison(vec![joint.clone()], inferred),
            )
            .expect("project");
            assert!(result.projection_errors.is_empty());
            assert_eq!(result.expectations[0].matched, Some(!inferred));
            assert_eq!(
                result.scopes[0]
                    .tokens
                    .as_ref()
                    .expect("scope metrics")
                    .true_positive_tokens,
                if inferred { 0 } else { 2 }
            );
        }
        let fragmented = comparison(
            vec![
                operation("left A right", "left  right", [true, false]),
                operation("left  right", "left B right", [false, true]),
            ],
            false,
        );
        let result = evaluate(&annotation(), [&old, &new], &fragmented).expect("project fragments");
        assert_eq!(result.expectations[0].matched, Some(false));
        assert_eq!(
            result.scopes[0]
                .tokens
                .as_ref()
                .expect("token recovery")
                .true_positive_tokens,
            2
        );
    }

    #[test]
    fn a_repeated_scalar_in_one_glyph_has_no_unique_source_coordinate() {
        let mut repeated = block("ff", 7);
        repeated.canonical.source_map[1].source = repeated.canonical.source_map[0].source.clone();
        let index = source_index(&[repeated]).expect("index");
        assert_eq!(
            index.get(&(vec![SourceRef::Native { glyph: GlyphId(7) }], 'f')),
            Some(&None)
        );
    }

    #[test]
    fn unchanged_source_false_positives_and_unprojectable_masks_remain_distinct() {
        let old = [block("left A right", 100)];
        let new = [block("left B right", 200)];
        let mut wrong = operation("left A right", "left B right", [true, true]);
        wrong["text_mask"]["old"][0]["position"] = json!(0);
        wrong["text_mask"]["old"][0]["sources"][0]["glyph"] = json!(100);
        wrong["text_mask"]["claims"]["mandatory_old"][5] = json!(false);
        wrong["text_mask"]["claims"]["mandatory_old"][0] = json!(true);
        let result = evaluate(
            &annotation(),
            [&old, &new],
            &comparison(vec![wrong.clone()], false),
        )
        .expect("project wrong mask");
        let tokens = result.scopes[0]
            .tokens
            .as_ref()
            .expect("known incorrect source");
        assert_eq!(
            tokens.reported_changed_tokens - tokens.true_positive_tokens,
            1
        );
        assert_eq!(result.expectations[0].matched, Some(false));
        wrong["text_mask"]["old"][0]["sources"][0]["glyph"] = json!(9999);
        let result = evaluate(&annotation(), [&old, &new], &comparison(vec![wrong], false))
            .expect("retain unknown projection");
        assert_eq!(result.expectations[0].matched, None);
        assert!(result.scopes[0].tokens.is_none());
        assert!(!result.projection_errors.is_empty());
    }
}
