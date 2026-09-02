use pdfdelta_core::{
    alignment::AlignmentConfidence,
    diff::{
        ExactRangeLeafSample, ExactRangeParentMetrics, ExactRangeParentOutcome,
        ExactRangeParentRelation, ExactRangeParentSample, ExactRangeParentStopReason,
        ExactRangeSeparator, RecoveryOwnership, SectionHeadingEvidence, SectionPairingMetrics,
    },
};
use serde::Serialize;

use super::{RecoveryGapReasonReport, RecoveryLeafKindReport};

const SAMPLE_LIMIT: usize = 64;
const LEAF_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactRangeParentRelationReport {
    SamePairedParent,
    ChangedPairedParent,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactRangeParentStopReasonReport {
    SectionAnalysisUnavailable,
    MissingOwnershipLedger,
    CandidateLimit,
    ComparisonLimit,
    AllocationFailure,
    InvalidOwnership,
    CounterOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactRangeSeparatorReport {
    Concatenate,
    Space,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExactRangeOwnershipReport {
    Accepted,
    Leaf { leaf: RecoveryLeafKindReport },
    Gap { reason: RecoveryGapReasonReport },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactRangeHeadingEvidenceReport {
    Exact,
    NumberStripped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactRangeConfidenceReport {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ExactRangeParentMetricsReport {
    pub old_candidates: usize,
    pub new_candidates: usize,
    pub accepted_candidates: usize,
    pub gap_barriers: usize,
    pub short_evidence_omitted: usize,
    pub hash_matches: usize,
    pub token_verified_matches: usize,
    pub unique_pairs: usize,
    pub same_paired_parent: usize,
    pub changed_paired_parent: usize,
    pub parent_unknown: usize,
    pub overlap_vetoes: usize,
    pub nesting_vetoes: usize,
    pub token_comparisons: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ExactRangeLeafSampleReport {
    pub block: u64,
    pub comparable_start: usize,
    pub comparable_end: usize,
    pub ownership: ExactRangeOwnershipReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExactRangeParentSampleReport {
    pub relation: ExactRangeParentRelationReport,
    pub old_source_token_count: usize,
    pub new_source_token_count: usize,
    pub old_separator: ExactRangeSeparatorReport,
    pub new_separator: ExactRangeSeparatorReport,
    pub old_leaves: Vec<ExactRangeLeafSampleReport>,
    pub new_leaves: Vec<ExactRangeLeafSampleReport>,
    pub old_parent_heading_block: Option<u64>,
    pub new_parent_heading_block: Option<u64>,
    pub old_parent_heading_evidence: Option<ExactRangeHeadingEvidenceReport>,
    pub new_parent_heading_evidence: Option<ExactRangeHeadingEvidenceReport>,
    pub old_parent_confidence: Option<ExactRangeConfidenceReport>,
    pub new_parent_confidence: Option<ExactRangeConfidenceReport>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExactRangeParentOutcomeReport {
    Complete {
        metrics: ExactRangeParentMetricsReport,
        samples: Vec<ExactRangeParentSampleReport>,
    },
    Unavailable {
        reason: ExactRangeParentStopReasonReport,
    },
}

pub fn build_exact_range_parent_outcome(
    outcome: Option<&ExactRangeParentOutcome>,
    section_metrics: Option<SectionPairingMetrics>,
) -> Result<Option<Box<ExactRangeParentOutcomeReport>>, String> {
    let Some(section_metrics) = section_metrics else {
        return if outcome.is_none() {
            Ok(None)
        } else {
            Err("exact-range parent outcome exists without section-pairing metrics".to_owned())
        };
    };
    let outcome = outcome.ok_or_else(|| {
        "section-pairing metrics exist without an exact-range parent outcome".to_owned()
    })?;
    validate_availability(section_metrics, outcome)?;
    let report = match outcome {
        ExactRangeParentOutcome::Complete { metrics, samples } => {
            validate_complete(*metrics, samples)?;
            ExactRangeParentOutcomeReport::Complete {
                metrics: (*metrics).into(),
                samples: samples.iter().map(Into::into).collect(),
            }
        }
        ExactRangeParentOutcome::Unavailable(reason) => {
            ExactRangeParentOutcomeReport::Unavailable {
                reason: (*reason).into(),
            }
        }
    };
    Ok(Some(Box::new(report)))
}

fn validate_availability(
    section_metrics: SectionPairingMetrics,
    outcome: &ExactRangeParentOutcome,
) -> Result<(), String> {
    match (section_metrics.complete, outcome) {
        (
            false,
            ExactRangeParentOutcome::Unavailable(
                ExactRangeParentStopReason::SectionAnalysisUnavailable,
            ),
        ) => Ok(()),
        (false, _) => Err(
            "incomplete section-pairing analysis exposes exact-range parent diagnostics".to_owned(),
        ),
        (
            true,
            ExactRangeParentOutcome::Unavailable(
                ExactRangeParentStopReason::SectionAnalysisUnavailable,
            ),
        ) => Err("complete section-pairing analysis reports itself unavailable".to_owned()),
        (true, _) => Ok(()),
    }
}

fn validate_complete(
    metrics: ExactRangeParentMetrics,
    samples: &[ExactRangeParentSample],
) -> Result<(), String> {
    if samples.len() > SAMPLE_LIMIT {
        return Err("exact-range parent samples exceed the bounded limit".to_owned());
    }
    if metrics.unique_pairs > metrics.token_verified_matches
        || metrics.token_verified_matches > metrics.hash_matches
        || metrics.unique_pairs > metrics.old_candidates
        || metrics.unique_pairs > metrics.new_candidates
    {
        return Err("exact-range parent candidate counters are inconsistent".to_owned());
    }
    let total_candidates = checked_sum(&[metrics.old_candidates, metrics.new_candidates])?;
    if metrics.accepted_candidates > total_candidates {
        return Err("exact-range parent accepted candidates exceed candidates".to_owned());
    }
    let retained = checked_sum(&[
        metrics.same_paired_parent,
        metrics.changed_paired_parent,
        metrics.parent_unknown,
    ])?;
    if checked_sum(&[retained, metrics.overlap_vetoes])? != metrics.unique_pairs {
        return Err("exact-range parent relation partition is inconsistent".to_owned());
    }
    if metrics.nesting_vetoes > metrics.overlap_vetoes || samples.len() > retained {
        return Err("exact-range parent overlap partition is inconsistent".to_owned());
    }

    let mut sampled_relations = [0usize; 3];
    for sample in samples {
        validate_sample(sample)?;
        sampled_relations[relation_index(sample.relation)] = sampled_relations
            [relation_index(sample.relation)]
        .checked_add(1)
        .ok_or_else(|| "exact-range parent sample counters overflow".to_owned())?;
    }
    if sampled_relations[0] > metrics.same_paired_parent
        || sampled_relations[1] > metrics.changed_paired_parent
        || sampled_relations[2] > metrics.parent_unknown
    {
        return Err("exact-range parent samples exceed relation metrics".to_owned());
    }
    for (sampled, total) in sampled_relations.into_iter().zip([
        metrics.same_paired_parent,
        metrics.changed_paired_parent,
        metrics.parent_unknown,
    ]) {
        if (sampled > 0) != (total > 0) {
            return Err("exact-range parent samples do not represent every relation".to_owned());
        }
    }
    Ok(())
}

fn validate_sample(sample: &ExactRangeParentSample) -> Result<(), String> {
    let old_tokens = validate_sample_side(
        &sample.old_leaves,
        sample.old_parent_heading_block,
        sample.old_parent_heading_evidence,
        sample.old_parent_confidence,
    )?;
    let new_tokens = validate_sample_side(
        &sample.new_leaves,
        sample.new_parent_heading_block,
        sample.new_parent_heading_evidence,
        sample.new_parent_confidence,
    )?;
    if old_tokens != sample.old_source_token_count || new_tokens != sample.new_source_token_count {
        return Err("exact-range parent sample token totals are inconsistent".to_owned());
    }
    if matches!(
        sample.relation,
        ExactRangeParentRelation::SamePairedParent | ExactRangeParentRelation::ChangedPairedParent
    ) && (sample.old_parent_heading_evidence.is_none()
        || sample.new_parent_heading_evidence.is_none())
    {
        return Err("paired exact-range relation lacks reciprocal parent evidence".to_owned());
    }
    Ok(())
}

fn validate_sample_side(
    leaves: &[ExactRangeLeafSample],
    parent_heading_block: Option<u64>,
    heading_evidence: Option<SectionHeadingEvidence>,
    confidence: Option<AlignmentConfidence>,
) -> Result<usize, String> {
    if leaves.is_empty() || leaves.len() > LEAF_LIMIT || parent_heading_block.is_none() {
        return Err("exact-range parent sample has invalid leaf or parent evidence".to_owned());
    }
    if heading_evidence.is_some() != confidence.is_some()
        || confidence.is_some_and(|value| value != AlignmentConfidence::High)
    {
        return Err("exact-range parent heading evidence is inconsistent".to_owned());
    }
    leaves.iter().try_fold(0usize, |total, leaf| {
        if leaf.comparable_start >= leaf.comparable_end
            || !matches!(leaf.ownership, RecoveryOwnership::Leaf(_))
        {
            return Err("exact-range parent sample contains an invalid leaf range".to_owned());
        }
        total
            .checked_add(leaf.comparable_end - leaf.comparable_start)
            .ok_or_else(|| "exact-range parent sample token total overflows".to_owned())
    })
}

fn checked_sum(values: &[usize]) -> Result<usize, String> {
    values.iter().try_fold(0usize, |total, value| {
        total
            .checked_add(*value)
            .ok_or_else(|| "exact-range parent counters overflow".to_owned())
    })
}

fn relation_index(relation: ExactRangeParentRelation) -> usize {
    match relation {
        ExactRangeParentRelation::SamePairedParent => 0,
        ExactRangeParentRelation::ChangedPairedParent => 1,
        ExactRangeParentRelation::Unknown => 2,
    }
}

impl From<ExactRangeParentMetrics> for ExactRangeParentMetricsReport {
    fn from(metrics: ExactRangeParentMetrics) -> Self {
        Self {
            old_candidates: metrics.old_candidates,
            new_candidates: metrics.new_candidates,
            accepted_candidates: metrics.accepted_candidates,
            gap_barriers: metrics.gap_barriers,
            short_evidence_omitted: metrics.short_evidence_omitted,
            hash_matches: metrics.hash_matches,
            token_verified_matches: metrics.token_verified_matches,
            unique_pairs: metrics.unique_pairs,
            same_paired_parent: metrics.same_paired_parent,
            changed_paired_parent: metrics.changed_paired_parent,
            parent_unknown: metrics.parent_unknown,
            overlap_vetoes: metrics.overlap_vetoes,
            nesting_vetoes: metrics.nesting_vetoes,
            token_comparisons: metrics.token_comparisons,
        }
    }
}

impl From<&ExactRangeParentSample> for ExactRangeParentSampleReport {
    fn from(sample: &ExactRangeParentSample) -> Self {
        Self {
            relation: sample.relation.into(),
            old_source_token_count: sample.old_source_token_count,
            new_source_token_count: sample.new_source_token_count,
            old_separator: sample.old_separator.into(),
            new_separator: sample.new_separator.into(),
            old_leaves: sample.old_leaves.iter().map(Into::into).collect(),
            new_leaves: sample.new_leaves.iter().map(Into::into).collect(),
            old_parent_heading_block: sample.old_parent_heading_block,
            new_parent_heading_block: sample.new_parent_heading_block,
            old_parent_heading_evidence: sample.old_parent_heading_evidence.map(Into::into),
            new_parent_heading_evidence: sample.new_parent_heading_evidence.map(Into::into),
            old_parent_confidence: sample.old_parent_confidence.map(Into::into),
            new_parent_confidence: sample.new_parent_confidence.map(Into::into),
        }
    }
}

impl From<&ExactRangeLeafSample> for ExactRangeLeafSampleReport {
    fn from(sample: &ExactRangeLeafSample) -> Self {
        Self {
            block: sample.block,
            comparable_start: sample.comparable_start,
            comparable_end: sample.comparable_end,
            ownership: sample.ownership.into(),
        }
    }
}

impl From<ExactRangeParentRelation> for ExactRangeParentRelationReport {
    fn from(relation: ExactRangeParentRelation) -> Self {
        match relation {
            ExactRangeParentRelation::SamePairedParent => Self::SamePairedParent,
            ExactRangeParentRelation::ChangedPairedParent => Self::ChangedPairedParent,
            ExactRangeParentRelation::Unknown => Self::Unknown,
        }
    }
}

impl From<ExactRangeParentStopReason> for ExactRangeParentStopReasonReport {
    fn from(reason: ExactRangeParentStopReason) -> Self {
        match reason {
            ExactRangeParentStopReason::SectionAnalysisUnavailable => {
                Self::SectionAnalysisUnavailable
            }
            ExactRangeParentStopReason::MissingOwnershipLedger => Self::MissingOwnershipLedger,
            ExactRangeParentStopReason::CandidateLimit => Self::CandidateLimit,
            ExactRangeParentStopReason::ComparisonLimit => Self::ComparisonLimit,
            ExactRangeParentStopReason::AllocationFailure => Self::AllocationFailure,
            ExactRangeParentStopReason::InvalidOwnership => Self::InvalidOwnership,
            ExactRangeParentStopReason::CounterOverflow => Self::CounterOverflow,
        }
    }
}

impl From<ExactRangeSeparator> for ExactRangeSeparatorReport {
    fn from(separator: ExactRangeSeparator) -> Self {
        match separator {
            ExactRangeSeparator::Concatenate => Self::Concatenate,
            ExactRangeSeparator::Space => Self::Space,
        }
    }
}

impl From<RecoveryOwnership> for ExactRangeOwnershipReport {
    fn from(ownership: RecoveryOwnership) -> Self {
        match ownership {
            RecoveryOwnership::Accepted => Self::Accepted,
            RecoveryOwnership::Leaf(leaf) => Self::Leaf { leaf: leaf.into() },
            RecoveryOwnership::Gap(reason) => Self::Gap {
                reason: reason.into(),
            },
        }
    }
}

impl From<SectionHeadingEvidence> for ExactRangeHeadingEvidenceReport {
    fn from(evidence: SectionHeadingEvidence) -> Self {
        match evidence {
            SectionHeadingEvidence::Exact => Self::Exact,
            SectionHeadingEvidence::NumberStripped => Self::NumberStripped,
        }
    }
}

impl From<AlignmentConfidence> for ExactRangeConfidenceReport {
    fn from(confidence: AlignmentConfidence) -> Self {
        match confidence {
            AlignmentConfidence::High => Self::High,
            AlignmentConfidence::Medium => Self::Medium,
            AlignmentConfidence::Low => Self::Low,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::diff::{RecoveryLeafKind, SectionPairingStopReason};

    fn complete_section_metrics() -> SectionPairingMetrics {
        SectionPairingMetrics {
            complete: true,
            ..SectionPairingMetrics::default()
        }
    }

    fn sample(relation: ExactRangeParentRelation) -> ExactRangeParentSample {
        let leaf = ExactRangeLeafSample {
            block: 7,
            comparable_start: 2,
            comparable_end: 5,
            ownership: RecoveryOwnership::Leaf(RecoveryLeafKind::TrustedRunResidual),
        };
        ExactRangeParentSample {
            relation,
            old_source_token_count: 3,
            new_source_token_count: 3,
            old_separator: ExactRangeSeparator::Concatenate,
            new_separator: ExactRangeSeparator::Space,
            old_leaves: vec![leaf],
            new_leaves: vec![ExactRangeLeafSample { block: 11, ..leaf }],
            old_parent_heading_block: Some(1),
            new_parent_heading_block: Some(9),
            old_parent_heading_evidence: Some(SectionHeadingEvidence::Exact),
            new_parent_heading_evidence: Some(SectionHeadingEvidence::NumberStripped),
            old_parent_confidence: Some(AlignmentConfidence::High),
            new_parent_confidence: Some(AlignmentConfidence::High),
        }
    }

    #[test]
    fn serializes_complete_outcome_and_all_metric_fields() {
        let metrics = ExactRangeParentMetrics {
            old_candidates: 10,
            new_candidates: 11,
            accepted_candidates: 2,
            gap_barriers: 3,
            short_evidence_omitted: 4,
            hash_matches: 6,
            token_verified_matches: 5,
            unique_pairs: 2,
            changed_paired_parent: 1,
            overlap_vetoes: 1,
            nesting_vetoes: 1,
            token_comparisons: 12,
            ..ExactRangeParentMetrics::default()
        };
        let outcome = ExactRangeParentOutcome::Complete {
            metrics,
            samples: vec![sample(ExactRangeParentRelation::ChangedPairedParent)],
        };
        let report =
            build_exact_range_parent_outcome(Some(&outcome), Some(complete_section_metrics()))
                .expect("valid outcome")
                .expect("report");
        let value = serde_json::to_value(report).expect("report serializes");

        assert_eq!(value["status"], "complete");
        assert_eq!(value["metrics"]["old_candidates"], 10);
        assert_eq!(value["metrics"]["token_comparisons"], 12);
        assert_eq!(value["samples"][0]["relation"], "changed_paired_parent");
        assert_eq!(value["samples"][0]["old_separator"], "concatenate");
        assert_eq!(value["samples"][0]["new_separator"], "space");
        assert_eq!(
            value["samples"][0]["old_leaves"][0]["ownership"]["kind"],
            "leaf"
        );
        assert_eq!(
            value["samples"][0]["old_leaves"][0]["ownership"]["leaf"],
            "trusted_run_residual"
        );
        assert_eq!(value["samples"][0]["old_parent_confidence"], "high");
    }

    #[test]
    fn unavailable_outcome_is_atomic_and_agrees_with_section_status() {
        let stopped = SectionPairingMetrics {
            complete: false,
            stop_reason: Some(SectionPairingStopReason::InvalidInput),
            ..SectionPairingMetrics::default()
        };
        let outcome = ExactRangeParentOutcome::Unavailable(
            ExactRangeParentStopReason::SectionAnalysisUnavailable,
        );
        let report = build_exact_range_parent_outcome(Some(&outcome), Some(stopped))
            .expect("matching unavailable outcome")
            .expect("report");
        assert_eq!(
            serde_json::to_value(report).expect("report serializes"),
            serde_json::json!({
                "status": "unavailable",
                "reason": "section_analysis_unavailable"
            })
        );

        assert!(
            build_exact_range_parent_outcome(Some(&outcome), Some(complete_section_metrics()))
                .is_err()
        );
        assert!(build_exact_range_parent_outcome(Some(&outcome), None).is_err());
    }

    #[test]
    fn rejects_inconsistent_relation_and_sample_partitions() {
        let inconsistent_partition = ExactRangeParentOutcome::Complete {
            metrics: ExactRangeParentMetrics {
                unique_pairs: 2,
                same_paired_parent: 1,
                ..ExactRangeParentMetrics::default()
            },
            samples: Vec::new(),
        };
        assert!(
            build_exact_range_parent_outcome(
                Some(&inconsistent_partition),
                Some(complete_section_metrics()),
            )
            .is_err()
        );

        let invalid_sample = ExactRangeParentOutcome::Complete {
            metrics: ExactRangeParentMetrics {
                unique_pairs: 1,
                parent_unknown: 1,
                ..ExactRangeParentMetrics::default()
            },
            samples: vec![ExactRangeParentSample {
                old_source_token_count: 4,
                ..sample(ExactRangeParentRelation::Unknown)
            }],
        };
        assert!(
            build_exact_range_parent_outcome(
                Some(&invalid_sample),
                Some(complete_section_metrics()),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_inconsistent_candidate_counters_without_overflow() {
        for metrics in [
            ExactRangeParentMetrics {
                old_candidates: 1,
                new_candidates: 1,
                hash_matches: 1,
                token_verified_matches: 1,
                unique_pairs: 2,
                same_paired_parent: 2,
                ..ExactRangeParentMetrics::default()
            },
            ExactRangeParentMetrics {
                old_candidates: 2,
                new_candidates: 2,
                hash_matches: 1,
                token_verified_matches: 2,
                ..ExactRangeParentMetrics::default()
            },
            ExactRangeParentMetrics {
                old_candidates: 1,
                new_candidates: 1,
                accepted_candidates: 3,
                ..ExactRangeParentMetrics::default()
            },
            ExactRangeParentMetrics {
                old_candidates: usize::MAX,
                new_candidates: 1,
                ..ExactRangeParentMetrics::default()
            },
        ] {
            let outcome = ExactRangeParentOutcome::Complete {
                metrics,
                samples: Vec::new(),
            };
            assert!(
                build_exact_range_parent_outcome(Some(&outcome), Some(complete_section_metrics()),)
                    .is_err()
            );
        }
    }

    #[test]
    fn rejects_missing_stratified_relation_sample() {
        let outcome = ExactRangeParentOutcome::Complete {
            metrics: ExactRangeParentMetrics {
                old_candidates: 2,
                new_candidates: 2,
                hash_matches: 2,
                token_verified_matches: 2,
                unique_pairs: 2,
                same_paired_parent: 1,
                changed_paired_parent: 1,
                ..ExactRangeParentMetrics::default()
            },
            samples: vec![sample(ExactRangeParentRelation::SamePairedParent)],
        };

        assert!(
            build_exact_range_parent_outcome(Some(&outcome), Some(complete_section_metrics()),)
                .is_err()
        );
    }
}
