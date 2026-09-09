//! Independent evaluation of graph correspondences, change units, and masks.
//!
//! Alternatives describe whole acceptable outcomes within a dimension. They are
//! never mixed to manufacture an outcome that no annotator accepted. Node IDs
//! refer to the supplied graph fixtures, not stable identifiers across parsers.

use std::collections::BTreeSet;

use pdfdelta_core::document::{
    Channel, ChannelCoverage, DocumentView, DocumentViewComparison, EdgeKind, ExactPixelMask,
    InterpretationStatus, NodeId, TypedOperation, document_coverage,
};
use serde::{Deserialize, Serialize};

use crate::{BenchError, Result};

pub const GENERALIZATION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Correspondence,
    /// Local content operations; composited page rendering is measured by PixelRegion.
    ChangeUnit,
    /// Review text spans, independent of mandatory changed-token positions.
    DisplayRange,
    TextPosition,
    PixelRegion,
    Relation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fact {
    Relation {
        relation: EdgeKind,
        old: [Vec<NodeId>; 2],
        new: [Vec<NodeId>; 2],
        old_present: bool,
        new_present: bool,
    },
    Correspondence {
        old: Vec<NodeId>,
        new: Vec<NodeId>,
    },
    ChangeUnit {
        /// None leaves correspondence unannotated; an empty group asserts absence.
        old: Option<Vec<NodeId>>,
        new: Option<Vec<NodeId>>,
        operation: TypedOperation,
    },
    TextPosition {
        old: Vec<NodeId>,
        new: Vec<NodeId>,
        convention: String,
        old_side: bool,
        position: usize,
    },
    /// Half-open Unicode scalar offsets in the displayed local value. The
    /// current report displays the entire value; these are not source-token
    /// indices or evidence that every displayed character changed.
    DisplayRange {
        old: Vec<NodeId>,
        new: Vec<NodeId>,
        old_side: bool,
        start: usize,
        end: usize,
    },
    PixelRegion {
        old: Vec<NodeId>,
        new: Vec<NodeId>,
        mask: ExactPixelMask,
    },
}

impl Fact {
    fn work_units(&self) -> usize {
        match self {
            Self::Relation { old, new, .. } => old
                .iter()
                .chain(new)
                .fold(1, |sum, group| sum.saturating_add(group.len())),
            Self::Correspondence { old, new } => {
                1_usize.saturating_add(old.len()).saturating_add(new.len())
            }
            Self::ChangeUnit {
                old,
                new,
                operation,
            } => {
                let content = match operation {
                    TypedOperation::TextChanged { old, new } => old
                        .as_ref()
                        .map_or(0, String::len)
                        .saturating_add(new.as_ref().map_or(0, String::len)),
                    TypedOperation::ValueChanged { old, new } => {
                        value_work_units(old).saturating_add(value_work_units(new))
                    }
                    TypedOperation::RenderedRegionChanged
                    | TypedOperation::PageRenderingChanged => 1,
                };
                content
                    .saturating_add(old.as_ref().map_or(0, Vec::len))
                    .saturating_add(new.as_ref().map_or(0, Vec::len))
                    .saturating_add(1)
            }
            Self::TextPosition {
                old,
                new,
                convention,
                ..
            } => old
                .len()
                .saturating_add(new.len())
                .saturating_add(convention.len())
                .saturating_add(1),
            Self::DisplayRange { old, new, end, .. } => old
                .len()
                .saturating_add(new.len())
                .saturating_add(*end)
                .saturating_add(1),
            Self::PixelRegion { old, new, mask } => old
                .len()
                .saturating_add(new.len())
                .saturating_add(mask.runs.len().saturating_mul(3))
                .saturating_add(1),
        }
    }

    fn dimension(&self) -> Dimension {
        match self {
            Self::Relation { .. } => Dimension::Relation,
            Self::Correspondence { .. } => Dimension::Correspondence,
            Self::ChangeUnit { .. } => Dimension::ChangeUnit,
            Self::DisplayRange { .. } => Dimension::DisplayRange,
            Self::TextPosition { .. } => Dimension::TextPosition,
            Self::PixelRegion { .. } => Dimension::PixelRegion,
        }
    }

    fn accepts(&self, observed: &Self) -> bool {
        if let (
            Self::ChangeUnit {
                old,
                new,
                operation,
            },
            Self::ChangeUnit {
                old: actual_old,
                new: actual_new,
                operation: actual_operation,
            },
        ) = (self, observed)
        {
            return operation == actual_operation
                && old
                    .as_ref()
                    .is_none_or(|old| Some(old) == actual_old.as_ref())
                && new
                    .as_ref()
                    .is_none_or(|new| Some(new) == actual_new.as_ref());
        }
        self == observed
    }
}

fn value_work_units(value: &pdfdelta_core::document::FieldValue) -> usize {
    use pdfdelta_core::document::FieldValue;
    match value {
        FieldValue::Text(text) => text.len(),
        FieldValue::Name(bytes) => bytes.len(),
        FieldValue::Choices(choices) => choices.iter().fold(1_usize, |sum, choice| {
            sum.saturating_add(choice.len()).saturating_add(1)
        }),
        FieldValue::Unresolved { raw_bytes, reason } => raw_bytes
            .as_ref()
            .map_or(0, Vec::len)
            .saturating_add(reason.len()),
        FieldValue::Selected(_) | FieldValue::Empty => 1,
    }
}

/// A missing dimension is unannotated; one empty alternative asserts no facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DimensionAnnotation {
    pub dimension: Dimension,
    pub alternatives: Vec<Vec<Fact>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralizationAnnotation {
    pub schema_version: u32,
    pub channels: BTreeSet<Channel>,
    pub dimensions: Vec<DimensionAnnotation>,
}

/// Counts use one-to-one multiset matching, so repeated reports are false positives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlternativeScore {
    pub true_positive: usize,
    pub false_positive: usize,
    pub false_negative: usize,
}

impl AlternativeScore {
    pub fn precision(&self) -> Option<f64> {
        ratio(self.true_positive, self.true_positive + self.false_positive)
    }

    pub fn recall(&self) -> Option<f64> {
        ratio(self.true_positive, self.true_positive + self.false_negative)
    }
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DimensionScore {
    pub dimension: Dimension,
    pub alternatives: Vec<AlternativeScore>,
    /// Inferred reports are counted separately and cannot satisfy exact annotations.
    pub inferred_reports: usize,
    /// The same annotations scored against inferred reports only, without promotion.
    pub inferred_alternatives: Vec<AlternativeScore>,
}

/// Recall, discovery completeness, and source coverage remain distinct measures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralizationScore {
    pub schema_version: u32,
    pub dimensions: Vec<DimensionScore>,
    pub search_resolved: bool,
    pub coverage: Vec<ChannelCoverage>,
    pub comparison_complete: bool,
    pub unresolved_local_comparisons: usize,
    pub old_evidence_issues: usize,
    pub new_evidence_issues: usize,
}

/// Evaluate annotated dimensions against the common graph comparison output.
///
/// Each acceptable alternative receives its own score. An exact match requires
/// both zero false positives and zero false negatives for one whole alternative.
/// Unannotated dimensions are absent, not reported as successful zero-change cases.
/// Stores and graphs must be the validated inputs used to produce `comparison`.
///
/// # Errors
/// Rejects unknown versions, duplicate dimensions, empty alternative lists,
/// misplaced facts, and inputs exceeding the bounded evaluation work budget.
pub fn evaluate(
    annotation: &GeneralizationAnnotation,
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    comparison: &DocumentViewComparison,
) -> Result<GeneralizationScore> {
    evaluate_observations(
        annotation,
        comparison,
        document_coverage(old, new, comparison, &annotation.channels),
        old.evidence
            .issues
            .iter()
            .filter(|issue| annotation.channels.contains(&issue.channel))
            .count(),
        new.evidence
            .issues
            .iter()
            .filter(|issue| annotation.channels.contains(&issue.channel))
            .count(),
    )
}

pub(super) fn evaluate_observations(
    annotation: &GeneralizationAnnotation,
    comparison: &DocumentViewComparison,
    coverage: Vec<ChannelCoverage>,
    old_evidence_issues: usize,
    new_evidence_issues: usize,
) -> Result<GeneralizationScore> {
    let invalid = |message: &str| BenchError::InvalidInput(message.into());
    if annotation.schema_version != GENERALIZATION_SCHEMA_VERSION {
        return Err(invalid("unsupported generalization annotation version"));
    }
    if annotation.channels.is_empty() {
        return Err(invalid(
            "generalization evaluation needs a selected channel",
        ));
    }
    let mut dimensions = Vec::new();
    let mut work = 0_usize;
    for (index, annotated) in annotation.dimensions.iter().enumerate() {
        if annotation.dimensions[..index]
            .iter()
            .any(|other| other.dimension == annotated.dimension)
        {
            return Err(invalid("duplicate generalization dimension"));
        }
        if annotated.alternatives.is_empty() {
            return Err(invalid("an annotated dimension needs an alternative"));
        }
        let mut observed = Vec::new();
        let mut inferred = Vec::new();
        if annotated.dimension == Dimension::Relation {
            for relation in comparison.relations().filter(|relation| relation.changed()) {
                let fact = Fact::Relation {
                    relation: relation.kind,
                    old: relation.old.clone(),
                    new: relation.new.clone(),
                    old_present: relation.old_present,
                    new_present: relation.new_present,
                };
                work = work.saturating_add(fact.work_units());
                if work > 1_000_000 {
                    return Err(invalid("generalization evaluation work limit"));
                }
                if relation.interpretation == InterpretationStatus::Inferred {
                    inferred.push(fact);
                } else {
                    observed.push(fact);
                }
            }
        }
        for pair in comparison.comparisons() {
            let mut record = |fact: Fact| -> Result<()> {
                work = work.saturating_add(fact.work_units());
                if work > 1_000_000 {
                    return Err(invalid("generalization evaluation work limit"));
                }
                if pair.interpretation == InterpretationStatus::Inferred {
                    inferred.push(fact);
                } else {
                    observed.push(fact);
                }
                Ok(())
            };
            match annotated.dimension {
                Dimension::Relation => {}
                Dimension::Correspondence => record(Fact::Correspondence {
                    old: pair.old.clone(),
                    new: pair.new.clone(),
                })?,
                Dimension::ChangeUnit => {
                    if let Some(operation) = &pair.operation
                        && !matches!(operation, TypedOperation::PageRenderingChanged)
                    {
                        record(Fact::ChangeUnit {
                            old: Some(pair.old.clone()),
                            new: Some(pair.new.clone()),
                            operation: operation.clone(),
                        })?;
                    }
                }
                Dimension::TextPosition => {
                    if let Some(mask) = &pair.text_mask {
                        for (old_side, tokens) in [(true, &mask.old), (false, &mask.new)] {
                            for token in tokens {
                                record(Fact::TextPosition {
                                    old: pair.old.clone(),
                                    new: pair.new.clone(),
                                    convention: mask.convention.clone(),
                                    old_side,
                                    position: token.position,
                                })?;
                            }
                        }
                    }
                }
                Dimension::DisplayRange => {
                    let displayed = match &pair.operation {
                        Some(TypedOperation::TextChanged { old, new }) => {
                            [old.as_deref(), new.as_deref()]
                        }
                        Some(TypedOperation::ValueChanged { old, new }) => {
                            [old, new].map(|value| match value {
                                pdfdelta_core::document::FieldValue::Text(text) => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                        }
                        _ => [None, None],
                    };
                    for (old_side, text) in [true, false].into_iter().zip(displayed) {
                        if let Some(text) = text {
                            record(Fact::DisplayRange {
                                old: pair.old.clone(),
                                new: pair.new.clone(),
                                old_side,
                                start: 0,
                                end: text.chars().take(1_000_001).count(),
                            })?;
                        }
                    }
                }
                Dimension::PixelRegion => {
                    if let Some(mask) = &pair.pixel_mask {
                        record(Fact::PixelRegion {
                            old: pair.old.clone(),
                            new: pair.new.clone(),
                            mask: mask.clone(),
                        })?;
                    }
                }
            }
        }
        let mut alternatives = Vec::new();
        let mut inferred_alternatives = Vec::new();
        for expected in &annotated.alternatives {
            if expected.iter().any(|fact| {
                fact.dimension() != annotated.dimension
                    || matches!(
                        fact,
                        Fact::ChangeUnit {
                            operation: TypedOperation::PageRenderingChanged,
                            ..
                        }
                    )
            }) {
                return Err(invalid("fact belongs to a different dimension"));
            }
            if expected
                .iter()
                .any(|fact| matches!(fact, Fact::DisplayRange { start, end, .. } if start > end))
            {
                return Err(invalid("display range start exceeds end"));
            }
            alternatives.push(score(expected, &observed, &mut work)?);
            inferred_alternatives.push(score(expected, &inferred, &mut work)?);
        }
        dimensions.push(DimensionScore {
            dimension: annotated.dimension,
            alternatives,
            inferred_reports: inferred.len(),
            inferred_alternatives,
        });
    }
    let search_resolved = comparison.search_resolved();
    Ok(GeneralizationScore {
        schema_version: GENERALIZATION_SCHEMA_VERSION,
        dimensions,
        search_resolved,
        comparison_complete: search_resolved && coverage.iter().all(|channel| channel.complete),
        coverage,
        unresolved_local_comparisons: comparison
            .comparisons()
            .filter(|pair| !pair.compared || !pair.unresolved.is_empty())
            .count(),
        old_evidence_issues,
        new_evidence_issues,
    })
}

fn score(expected: &[Fact], observed: &[Fact], work: &mut usize) -> Result<AlternativeScore> {
    let invalid = || BenchError::InvalidInput("generalization evaluation work limit".into());
    let expected_units = expected
        .iter()
        .fold(1_usize, |sum, fact| sum.saturating_add(fact.work_units()));
    let observed_units = observed
        .iter()
        .fold(0_usize, |sum, fact| sum.saturating_add(fact.work_units()));
    *work = work.saturating_add(
        expected_units
            .saturating_mul(observed.len().saturating_add(1))
            .saturating_add(observed_units.saturating_mul(expected.len())),
    );
    if *work > 1_000_000 {
        return Err(invalid());
    }
    let edges: Vec<Vec<usize>> = expected
        .iter()
        .map(|expected| {
            observed
                .iter()
                .enumerate()
                .filter_map(|(index, actual)| expected.accepts(actual).then_some(index))
                .collect()
        })
        .collect();
    let edge_count = edges
        .iter()
        .fold(0_usize, |sum, edges| sum.saturating_add(edges.len()));
    *work = work.saturating_add(
        edge_count
            .saturating_add(expected.len())
            .saturating_add(observed.len())
            .saturating_mul(expected.len()),
    );
    if *work > 1_000_000 {
        return Err(invalid());
    }
    let true_positive = crate::evaluator::maximum_cardinality_matching(&edges, observed.len());
    Ok(AlternativeScore {
        true_positive,
        false_positive: observed.len() - true_positive,
        false_negative: expected.len() - true_positive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::{
        document::{DocumentGraph, EvidenceStore, RelationComparison},
        model::Document,
    };

    fn evaluate_fixture(
        annotation: &GeneralizationAnnotation,
        comparison: &DocumentViewComparison,
    ) -> Result<GeneralizationScore> {
        let evidence = EvidenceStore {
            revision: "empty".into(),
            native: Document::new(Vec::new()),
            backends: Vec::new(),
            pages: Vec::new(),
            rendered: Vec::new(),
            structured: Vec::new(),
            inventories: Vec::new(),
            issues: Vec::new(),
        };
        let graph = DocumentGraph::default();
        let view = DocumentView {
            evidence: &evidence,
            graph: &graph,
        };
        evaluate(annotation, view, view, comparison)
    }

    fn fact(id: u64) -> Fact {
        Fact::Correspondence {
            old: vec![NodeId(id)],
            new: vec![NodeId(id)],
        }
    }

    #[test]
    fn missing_and_duplicate_reports_are_not_perfect_scores() {
        let expected = [fact(1), fact(2)];
        let empty = score(&expected, &[], &mut 0).expect("score empty output");
        assert_eq!(empty.recall(), Some(0.0));
        assert_eq!(empty.precision(), None);
        let duplicate = score(&expected, &[fact(1), fact(1)], &mut 0).expect("score duplicates");
        assert_eq!(duplicate.true_positive, 1);
        assert_eq!(duplicate.false_positive, 1);
        assert_eq!(duplicate.false_negative, 1);
    }

    #[test]
    fn unscoped_expectations_do_not_steal_the_only_match_for_a_scoped_event() {
        let change = |node: Option<u64>| Fact::ChangeUnit {
            old: node.map(|id| vec![NodeId(id)]),
            new: node.map(|id| vec![NodeId(id)]),
            operation: TypedOperation::TextChanged {
                old: Some("100".into()),
                new: Some("20".into()),
            },
        };
        let result = score(
            &[change(None), change(Some(1))],
            &[change(Some(1)), change(Some(2))],
            &mut 0,
        )
        .expect("match scoped and unscoped annotations");
        assert_eq!(result.true_positive, 2);
        assert_eq!(result.false_positive, 0);
        assert_eq!(result.false_negative, 0);
        let mut absent = change(None);
        if let Fact::ChangeUnit { old, .. } = &mut absent {
            *old = Some(Vec::new());
        }
        assert!(!absent.accepts(&change(Some(1))));
        assert!(!change(Some(1)).accepts(&change(Some(2))));
    }

    #[test]
    fn relation_alternatives_stay_separate_and_inference_cannot_satisfy_them() {
        let relation = RelationComparison {
            kind: EdgeKind::Contains,
            old: [vec![NodeId(1)], vec![NodeId(2)]],
            new: [vec![NodeId(1)], vec![NodeId(2)]],
            old_present: true,
            new_present: false,
            old_sources: Vec::new(),
            new_sources: Vec::new(),
            dependencies: Vec::new(),
            interpretation: InterpretationStatus::ConditionalOnCorrespondence,
        };
        let expected = Fact::Relation {
            relation: relation.kind,
            old: relation.old.clone(),
            new: relation.new.clone(),
            old_present: true,
            new_present: false,
        };
        let annotation = GeneralizationAnnotation {
            schema_version: GENERALIZATION_SCHEMA_VERSION,
            channels: BTreeSet::from([Channel::Relations]),
            dimensions: vec![DimensionAnnotation {
                dimension: Dimension::Relation,
                alternatives: vec![vec![], vec![expected]],
            }],
        };
        let mut comparison = DocumentViewComparison {
            scopes: Vec::new(),
            relations: vec![relation],
            relation_unresolved: Vec::new(),
        };
        let report = evaluate_fixture(&annotation, &comparison).expect("score alternatives");
        assert_eq!(report.dimensions[0].alternatives[0].false_positive, 1);
        assert_eq!(report.dimensions[0].alternatives[1].true_positive, 1);
        comparison.relations[0].interpretation = InterpretationStatus::Inferred;
        let report = evaluate_fixture(&annotation, &comparison).expect("separate inference");
        assert_eq!(report.dimensions[0].inferred_reports, 1);
        assert_eq!(report.dimensions[0].alternatives[1].true_positive, 0);
        assert_eq!(report.dimensions[0].alternatives[1].false_negative, 1);
        assert_eq!(
            report.dimensions[0].inferred_alternatives[1].true_positive,
            1
        );
    }

    #[test]
    fn absent_annotations_are_not_zero_change_expectations() {
        let comparison = DocumentViewComparison {
            scopes: Vec::new(),
            relations: Vec::new(),
            relation_unresolved: vec!["missing inventory".into()],
        };
        let mut annotation = GeneralizationAnnotation {
            schema_version: GENERALIZATION_SCHEMA_VERSION,
            channels: BTreeSet::from([Channel::Text]),
            dimensions: Vec::new(),
        };
        let report = evaluate_fixture(&annotation, &comparison).expect("unannotated dimensions");
        assert!(report.dimensions.is_empty());
        assert!(!report.search_resolved);
        annotation.dimensions.push(DimensionAnnotation {
            dimension: Dimension::Correspondence,
            alternatives: Vec::new(),
        });
        assert!(evaluate_fixture(&annotation, &comparison).is_err());
        annotation.dimensions[0].alternatives.push(vec![fact(1)]);
        assert!(evaluate_fixture(&annotation, &comparison).is_ok());
        annotation.dimensions.push(annotation.dimensions[0].clone());
        assert!(evaluate_fixture(&annotation, &comparison).is_err());
    }

    #[test]
    fn annotation_payload_counts_toward_the_evaluation_budget() {
        let annotation = GeneralizationAnnotation {
            schema_version: GENERALIZATION_SCHEMA_VERSION,
            channels: BTreeSet::from([Channel::Text]),
            dimensions: vec![DimensionAnnotation {
                dimension: Dimension::ChangeUnit,
                alternatives: vec![vec![Fact::ChangeUnit {
                    old: Some(vec![NodeId(1)]),
                    new: Some(Vec::new()),
                    operation: TypedOperation::TextChanged {
                        old: Some("A".repeat(1_000_000)),
                        new: None,
                    },
                }]],
            }],
        };
        let comparison = DocumentViewComparison {
            scopes: Vec::new(),
            relations: Vec::new(),
            relation_unresolved: Vec::new(),
        };
        assert!(
            matches!(evaluate_fixture(&annotation, &comparison), Err(BenchError::InvalidInput(message)) if message.contains("work limit"))
        );
    }
}
