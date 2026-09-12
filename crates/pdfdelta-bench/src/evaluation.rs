//! Provenance, split, and evaluation accounting for benchmark runs.
//!
//! The evaluation model deliberately keeps operational trials separate from
//! quality denominators. A failed or skipped run remains an observed trial,
//! while partial annotations can contribute reviewed recall without silently
//! becoming a document-wide precision claim.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{BenchError, Result};

/// Column names appended to the legacy revision manifest columns.
pub const PROVENANCE_COLUMNS: [&str; 7] = [
    "document_series_id",
    "derivation_group",
    "producer_family",
    "producer_version",
    "annotation_scope",
    "first_evaluated",
    "tuning_use",
];

/// Version for the provenance and evaluation artifact, independent of the
/// legacy revision summary schema.
pub const EVALUATION_SCHEMA_VERSION: u32 = 5;

/// Whether a document was used to change implementation or tuning choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuningUse {
    Unused,
    UsedForFix,
}

impl TuningUse {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unused => "unused",
            Self::UsedForFix => "used_for_fix",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "unused" => Some(Self::Unused),
            "used_for_fix" | "used_for_tuning" => Some(Self::UsedForFix),
            _ => None,
        }
    }
}

/// Provenance that must travel with a revision pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkProvenance {
    pub document_series_id: String,
    pub derivation_group: String,
    pub producer_family: String,
    pub producer_version: String,
    pub annotation_scope: String,
    pub first_evaluated: String,
    pub tuning_use: TuningUse,
}

impl BenchmarkProvenance {
    /// Parses the seven provenance columns appended to a manifest row.
    pub fn parse(columns: &[&str], line_number: usize) -> Result<Self> {
        if columns.len() != PROVENANCE_COLUMNS.len() {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has {} provenance columns, expected {}",
                columns.len(),
                PROVENANCE_COLUMNS.len()
            )));
        }
        let fields = columns
            .iter()
            .zip(PROVENANCE_COLUMNS)
            .map(|(value, name)| require_nonblank(value, name, line_number))
            .collect::<Result<Vec<_>>>()?;
        let tuning_use = TuningUse::parse(&fields[6]).ok_or_else(|| {
            BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has invalid tuning_use {:?}",
                fields[6]
            ))
        })?;
        validate_date(&fields[5], "first_evaluated", line_number)?;
        if !matches!(
            fields[4].as_str(),
            "none" | "complete" | "partial" | "scoped_complete"
        ) {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has invalid annotation_scope {:?}",
                fields[4]
            )));
        }
        Ok(Self {
            document_series_id: fields[0].clone(),
            derivation_group: fields[1].clone(),
            producer_family: fields[2].clone(),
            producer_version: fields[3].clone(),
            annotation_scope: fields[4].clone(),
            first_evaluated: fields[5].clone(),
            tuning_use,
        })
    }

    /// A known producer family remains useful evidence when its version is
    /// unavailable; unknown families do not count as producer validation.
    #[must_use]
    pub fn known_producer(&self) -> bool {
        !self.producer_family.eq_ignore_ascii_case("unknown")
    }
}

/// The metadata needed to validate cross-row split contamination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestProvenanceEntry {
    pub pair_id: String,
    pub split: String,
    pub document_series_id: String,
    pub derivation_group: String,
    pub producer_family: String,
    pub producer_version: String,
    pub tuning_use: TuningUse,
    pub old_sha256: String,
    pub new_sha256: String,
}

/// Validates series, derivation, and byte-level split boundaries.
pub fn validate_manifest_provenance(entries: &[ManifestProvenanceEntry]) -> Result<()> {
    let mut pair_ids = BTreeSet::new();
    let mut series_splits = BTreeMap::<&str, &str>::new();
    let mut derivation_splits = BTreeMap::<&str, &str>::new();
    let mut producer_splits = BTreeMap::<(String, String), String>::new();
    let mut hashes = BTreeMap::<&str, (&str, &str)>::new();

    for entry in entries {
        if !pair_ids.insert(entry.pair_id.as_str()) {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest provenance duplicates pair_id {:?}",
                entry.pair_id
            )));
        }
        validate_split_label(&entry.split)?;
        for (name, value) in [
            ("document_series_id", &entry.document_series_id),
            ("derivation_group", &entry.derivation_group),
        ] {
            if value.trim().is_empty() {
                return Err(BenchError::InvalidInput(format!(
                    "revision manifest pair {:?} has blank {name}",
                    entry.pair_id
                )));
            }
        }
        if entry.split == "holdout" && entry.tuning_use == TuningUse::UsedForFix {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest pair {:?} is marked used_for_fix but remains in holdout",
                entry.pair_id
            )));
        }
        check_group_split(
            &mut series_splits,
            &entry.document_series_id,
            &entry.split,
            "document series",
        )?;
        check_group_split(
            &mut derivation_splits,
            &entry.derivation_group,
            &entry.split,
            "derivation group",
        )?;
        if !entry.producer_family.eq_ignore_ascii_case("unknown")
            && let Some(previous) = producer_splits.insert(
                (
                    entry.producer_family.clone(),
                    entry.producer_version.clone(),
                ),
                entry.split.clone(),
            )
            && previous != entry.split
        {
            return Err(BenchError::InvalidInput(format!(
                "producer {}@{} appears in both {} and {} splits",
                entry.producer_family, entry.producer_version, previous, entry.split
            )));
        }
        for digest in [&entry.old_sha256, &entry.new_sha256] {
            if let Some((split, group)) =
                hashes.insert(digest, (&entry.split, &entry.derivation_group))
            {
                if split != entry.split.as_str() {
                    return Err(BenchError::InvalidInput(format!(
                        "revision manifest byte hash {digest} crosses {} and {} splits",
                        split, entry.split
                    )));
                }
                if group != entry.derivation_group.as_str() {
                    return Err(BenchError::InvalidInput(format!(
                        "revision manifest byte hash {digest} is shared by unrelated derivation groups"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn check_group_split<'a>(
    groups: &mut BTreeMap<&'a str, &'a str>,
    group: &'a str,
    split: &'a str,
    label: &str,
) -> Result<()> {
    if let Some(previous) = groups.insert(group, split)
        && previous != split
    {
        return Err(BenchError::InvalidInput(format!(
            "{label} {group:?} appears in both {previous} and {split} splits"
        )));
    }
    Ok(())
}

fn validate_split_label(split: &str) -> Result<()> {
    if matches!(split, "dev" | "holdout") {
        Ok(())
    } else {
        Err(BenchError::InvalidInput(format!(
            "revision manifest has invalid split {split:?}; expected dev or holdout"
        )))
    }
}

fn require_nonblank(value: &str, column: &str, line_number: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.contains(['\t', '\r', '\n']) {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has blank or invalid {column}"
        )));
    }
    Ok(value.to_owned())
}

fn validate_date(value: &str, column: &str, line_number: usize) -> Result<()> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if !valid_shape {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {column} {value:?}; expected YYYY-MM-DD"
        )));
    }
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    let year = value[0..4].parse::<u16>().unwrap_or_default();
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    };
    if max_day == 0 || !(1..=max_day).contains(&day) {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {column} {value:?}"
        )));
    }
    Ok(())
}

/// Classification of one operational trial. Every value contributes to the
/// operational denominator, including values that contribute no quality data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialStatus {
    #[default]
    Ok,
    Skipped,
    Unsupported,
    Unresolved,
    Limit,
    Fatal,
}

/// Percentile summary of observed trial runtimes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDistribution {
    pub samples: usize,
    pub min_ms: Option<u128>,
    pub p50_ms: Option<u128>,
    pub p95_ms: Option<u128>,
    pub max_ms: Option<u128>,
}

impl RuntimeDistribution {
    fn from_samples(samples: &[u128]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        Self {
            samples: sorted.len(),
            min_ms: sorted.first().copied(),
            p50_ms: percentile(&sorted, 50),
            p95_ms: percentile(&sorted, 95),
            max_ms: sorted.last().copied(),
        }
    }
}

/// Percentile summary of observed peak resident memory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDistribution {
    pub samples: usize,
    pub min_bytes: Option<u64>,
    pub p50_bytes: Option<u64>,
    pub p95_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
}

impl MemoryDistribution {
    fn from_samples(samples: &[u64]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        Self {
            samples: sorted.len(),
            min_bytes: sorted.first().copied(),
            p50_bytes: percentile(&sorted, 50),
            p95_bytes: percentile(&sorted, 95),
            max_bytes: sorted.last().copied(),
        }
    }
}

fn percentile<T: Copy>(sorted: &[T], percentile: usize) -> Option<T> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (sorted.len() * percentile).div_ceil(100).max(1) - 1;
    sorted.get(rank).copied()
}

/// Counts all observed trials by operational outcome and records their
/// measured resource distributions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationalTotals {
    pub trials: usize,
    pub ok: usize,
    pub skipped: usize,
    pub unsupported: usize,
    pub unresolved: usize,
    pub limit: usize,
    pub fatal: usize,
    pub runtime: RuntimeDistribution,
    pub peak_memory: MemoryDistribution,
    #[serde(skip)]
    runtime_samples_ms: Vec<u128>,
    #[serde(skip)]
    peak_memory_samples_bytes: Vec<u64>,
}

impl OperationalTotals {
    fn add(&mut self, status: TrialStatus, runtime_ms: u128, peak_memory_bytes: Option<u64>) {
        self.trials = self.trials.saturating_add(1);
        match status {
            TrialStatus::Ok => self.ok = self.ok.saturating_add(1),
            TrialStatus::Skipped => self.skipped = self.skipped.saturating_add(1),
            TrialStatus::Unsupported => self.unsupported = self.unsupported.saturating_add(1),
            TrialStatus::Unresolved => self.unresolved = self.unresolved.saturating_add(1),
            TrialStatus::Limit => self.limit = self.limit.saturating_add(1),
            TrialStatus::Fatal => self.fatal = self.fatal.saturating_add(1),
        }
        self.runtime_samples_ms.push(runtime_ms);
        self.runtime = RuntimeDistribution::from_samples(&self.runtime_samples_ms);
        if let Some(bytes) = peak_memory_bytes {
            self.peak_memory_samples_bytes.push(bytes);
            self.peak_memory = MemoryDistribution::from_samples(&self.peak_memory_samples_bytes);
        }
    }
}

/// Quality dimensions for one reviewed pair. Precision and document recall
/// are intentionally absent for partial annotations.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QualityEvaluation {
    pub annotation: Option<String>,
    pub expected_changes: Option<usize>,
    pub reported_changes: Option<usize>,
    pub matched_changes: Option<usize>,
    pub kind_correct: Option<usize>,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub document_recall: Option<f64>,
    pub kind_accuracy: Option<f64>,
    /// Recall of retained final candidate events against reviewed changes.
    pub candidate_recall: Option<f64>,
    /// Recall reported by the candidate generator before final diff matching.
    pub generator_candidate_recall_at_k: Option<f64>,
    pub reported_hunks_per_matched_change: Option<f64>,
    pub review_hunks_per_expected_change: Option<f64>,
    pub unmatched_tiny_changes: Option<usize>,
    pub unresolvable_reported_spans: Option<usize>,
}

/// Candidate-generator recall and review workload for one pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub top_k: usize,
    pub annotated_counterparts: usize,
    pub evaluable_counterparts: usize,
    pub recalled_counterparts: usize,
    pub unavailable_counterparts: usize,
    pub recall_at_k: Option<f64>,
    pub candidate_groups: Option<usize>,
    pub candidate_tokens: Option<usize>,
    pub precision: Option<f64>,
    /// Quality of retained candidate events after final matching.
    pub final_event: Option<CandidateEventEvaluation>,
}

/// Event-level quality of retained change candidates against reviewed changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidateEventEvaluation {
    pub expected_changes: usize,
    pub reported_changes: usize,
    pub matched_changes: usize,
    pub recall: Option<f64>,
    pub precision: Option<f64>,
}

/// Work charged by the comparison assessment at each execution stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentWorkEvaluation {
    pub anchor_verification: usize,
    pub local_views: usize,
    pub localization: usize,
    pub emission: usize,
}

/// Compact counts for non-owning review claims in one comparison assessment.
///
/// These counts describe claim evidence and deliberately do not add changed
/// token bounds across units: review domains may overlap, so such a sum would
/// falsely become an owned recall or event count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentClaimEvaluation {
    pub review_unit_count: usize,
    /// Incomplete units are the difference between these two counts.
    pub complete_review_unit_count: usize,
    pub positive_lower_bound_unit_count: usize,
    pub mandatory_old_span_count: usize,
    pub mandatory_new_span_count: usize,
    /// Units with more than the default single normalization interpretation.
    pub normalization_hypothesis_unit_count: usize,
}

/// One expected change's assignment from the official event matcher.
///
/// `matched` counts an assignment in the same way as quality's
/// `matched_changes`; `kind_agrees` keeps kind correctness separate from event
/// matching. An absent collection means that matching was unavailable for the
/// pair, while an entry with `matched == false` is an authoritative miss.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedChangeMatchEvaluation {
    pub expected_id: String,
    pub matched: bool,
    pub actual_index: Option<usize>,
    pub actual_kind: Option<String>,
    pub kind_agrees: Option<bool>,
}

/// Assessment policy and bounded work accounting for one comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentEvaluation {
    pub policy_version: u32,
    pub work_limit: usize,
    pub work_used: usize,
    pub work_by_stage: AssessmentWorkEvaluation,
    pub candidates_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_diagnostics: Option<AssessmentClaimEvaluation>,
}

/// Counts the complete, disjoint token partition for one document side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResolutionCounts {
    pub total: usize,
    pub same: usize,
    pub changed: usize,
    pub unresolved: usize,
}

/// Quality for changed regions whose source coordinates are not established.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProvenEvaluation {
    pub reported_regions: usize,
    pub expected_regions: Option<usize>,
    pub matched_regions: Option<usize>,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub unlocalized_regions: usize,
}

/// Event-level quality over a fully reviewed scope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopedEventEvaluation {
    pub reviewed_scope_count: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

/// Token-level quality over a fully reviewed scope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopedTokenEvaluation {
    pub expected_changed_tokens: usize,
    pub reported_changed_tokens: usize,
    pub true_positive_tokens: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub span_iou: f64,
    pub false_positive_tokens_per_10k_unchanged: Option<f64>,
}

/// Reviewed recall split by evidence strength and change kind.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewedRecallEvaluation {
    pub content_presence_recall: Option<f64>,
    pub exact_localization_recall: Option<f64>,
    pub semantic_relation_recall: Option<f64>,
    pub move_recall: Option<f64>,
}

/// One pair evaluation retained in the reproducible evaluation artifact.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvaluationRecord {
    pub pair_id: String,
    pub document_series_id: Option<String>,
    pub derivation_group: Option<String>,
    pub producer_family: Option<String>,
    pub producer_version: Option<String>,
    pub split: String,
    pub trial_status: TrialStatus,
    pub compared: bool,
    pub extraction_complete: Option<bool>,
    pub comparison_complete: Option<bool>,
    pub coverage_old: Option<f64>,
    pub coverage_new: Option<f64>,
    pub coverage_comparison: Option<f64>,
    pub unresolved_old_token_share: Option<f64>,
    pub unresolved_new_token_share: Option<f64>,
    pub accepted_changes: usize,
    pub candidate_changes: usize,
    pub formatting_only_changes: usize,
    pub uncertain_changes: usize,
    pub unresolved_regions: usize,
    pub unlocalized_changed_regions: usize,
    pub quality_skipped_reason: Option<String>,
    pub trial_runtime_ms: u128,
    pub peak_memory_bytes: Option<u64>,
    pub limit_scale_used: f64,
    pub resolved_old_tokens: Option<usize>,
    pub resolved_new_tokens: Option<usize>,
    pub old_token_resolution: Option<TokenResolutionCounts>,
    pub new_token_resolution: Option<TokenResolutionCounts>,
    pub assessment: Option<AssessmentEvaluation>,
    /// Per-expectation assignments from the same matcher as `quality`.
    pub expected_matches: Option<Vec<ExpectedChangeMatchEvaluation>>,
    pub quality: QualityEvaluation,
    pub candidate: Option<CandidateEvaluation>,
    pub proven: Option<ProvenEvaluation>,
    pub scoped_event: Option<ScopedEventEvaluation>,
    pub scoped_tokens: Option<ScopedTokenEvaluation>,
    pub reviewed_recall: Option<ReviewedRecallEvaluation>,
}

/// Aggregated quality counts for a document, series, or complete corpus.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QualityTotals {
    pub evaluable_trials: usize,
    pub complete_trials: usize,
    pub scoped_complete_trials: usize,
    pub partial_trials: usize,
    pub expected_changes: usize,
    pub reported_changes: usize,
    pub matched_changes: usize,
    pub kind_correct: usize,
    pub candidate_annotated_counterparts: usize,
    pub candidate_evaluable_counterparts: usize,
    pub candidate_recalled_counterparts: usize,
    pub candidate_unavailable_counterparts: usize,
    pub candidate_groups: usize,
    pub candidate_tokens: usize,
    pub candidate_precision: Option<f64>,
    pub candidate_recall: Option<f64>,
    pub generator_candidate_recall_at_k: Option<f64>,
    pub candidate_event_expected_changes: usize,
    pub candidate_event_matched_changes: usize,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub document_recall: Option<f64>,
    pub kind_accuracy: Option<f64>,
}

/// Aggregate counts for one document series group.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvaluationGroupTotals {
    pub group_id: String,
    pub operational: OperationalTotals,
    pub accepted_changes: usize,
    pub candidate_changes: usize,
    pub formatting_only_changes: usize,
    pub uncertain_changes: usize,
    pub unresolved_regions: usize,
    pub unlocalized_changed_regions: usize,
    pub quality_skip_reasons: BTreeMap<String, usize>,
    pub old_token_resolution: TokenResolutionCounts,
    pub new_token_resolution: TokenResolutionCounts,
    pub proven: ProvenEvaluation,
    pub quality: QualityTotals,
}

/// Unweighted quality means over independent groups, preserving the
/// document/series distinction from micro-averaged token and event totals.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MacroQualityTotals {
    pub groups: usize,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub document_recall: Option<f64>,
    pub kind_accuracy: Option<f64>,
    pub candidate_recall: Option<f64>,
    pub generator_candidate_recall_at_k: Option<f64>,
}

/// Evaluation summary embedded in the revision summary and reusable as a
/// standalone artifact.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvaluationSummary {
    pub schema_version: u32,
    pub records: Vec<EvaluationRecord>,
    pub document_totals: Vec<EvaluationGroupTotals>,
    pub series_totals: Vec<EvaluationGroupTotals>,
    pub producer_totals: Vec<EvaluationGroupTotals>,
    pub split_totals: Vec<EvaluationGroupTotals>,
    pub totals: EvaluationGroupTotals,
    pub lineage_macro: MacroQualityTotals,
    pub producer_macro: MacroQualityTotals,
    pub baselines: Vec<BaselineRecord>,
}

impl EvaluationSummary {
    #[must_use]
    pub fn from_records(records: Vec<EvaluationRecord>) -> Self {
        let document_totals = records
            .iter()
            .map(|record| aggregate_group(record.pair_id.clone(), std::slice::from_ref(record)))
            .collect();
        let mut by_series = BTreeMap::<String, Vec<EvaluationRecord>>::new();
        for record in &records {
            let key = record
                .document_series_id
                .clone()
                .unwrap_or_else(|| format!("pair:{}", record.pair_id));
            by_series.entry(key).or_default().push(record.clone());
        }
        let series_totals = by_series
            .into_iter()
            .map(|(group, records)| aggregate_group(group, &records))
            .collect::<Vec<_>>();
        let mut by_producer = BTreeMap::<String, Vec<EvaluationRecord>>::new();
        for record in &records {
            // Unknown producer families are retained in one bucket but do not
            // create a producer-diversity claim. A known family remains
            // identifiable when its version is unavailable.
            let key = match &record.producer_family {
                Some(family) if !family.eq_ignore_ascii_case("unknown") => {
                    let version = record.producer_version.as_deref().unwrap_or("unknown");
                    format!("{family}@{version}")
                }
                _ => "unknown".to_owned(),
            };
            by_producer.entry(key).or_default().push(record.clone());
        }
        let producer_totals = by_producer
            .into_iter()
            .map(|(group, records)| aggregate_group(group, &records))
            .collect::<Vec<_>>();
        let mut by_split = BTreeMap::<String, Vec<EvaluationRecord>>::new();
        for record in &records {
            by_split
                .entry(record.split.clone())
                .or_default()
                .push(record.clone());
        }
        let split_totals = by_split
            .into_iter()
            .map(|(group, records)| aggregate_group(group, &records))
            .collect();
        let totals = aggregate_group("all".to_owned(), &records);
        let lineage_macro = macro_quality(&series_totals);
        let producer_macro = macro_quality(&producer_totals);
        Self {
            schema_version: EVALUATION_SCHEMA_VERSION,
            records,
            document_totals,
            series_totals,
            producer_totals,
            split_totals,
            totals,
            lineage_macro,
            producer_macro,
            baselines: default_baselines(),
        }
    }

    #[must_use]
    pub fn with_baselines(mut self, baselines: Vec<BaselineRecord>) -> Self {
        self.baselines = baselines;
        self
    }
}

fn aggregate_group(group_id: String, records: &[EvaluationRecord]) -> EvaluationGroupTotals {
    let mut result = EvaluationGroupTotals {
        group_id,
        ..EvaluationGroupTotals::default()
    };
    let mut precision_numerator = 0usize;
    let mut precision_denominator = 0usize;
    let mut recall_expected = 0usize;
    let mut recall_detected = 0usize;
    let mut document_expected = 0usize;
    let mut document_detected = 0usize;
    let mut partial_expected = 0usize;
    let mut partial_detected = 0usize;
    let mut kind_matched = 0usize;
    let mut kind_correct = 0usize;
    let mut generator_candidate_sum = 0.0;
    let mut generator_candidate_count = 0usize;
    let mut candidate_precision_sum = 0.0;
    let mut candidate_precision_count = 0usize;
    let mut proven_expected_known = true;
    let mut proven_precision_known = true;
    let mut proven_has_values = false;

    for record in records {
        result.operational.add(
            record.trial_status,
            record.trial_runtime_ms,
            record.peak_memory_bytes,
        );
        result.accepted_changes = result
            .accepted_changes
            .saturating_add(record.accepted_changes);
        result.candidate_changes = result
            .candidate_changes
            .saturating_add(record.candidate_changes);
        result.formatting_only_changes = result
            .formatting_only_changes
            .saturating_add(record.formatting_only_changes);
        result.uncertain_changes = result
            .uncertain_changes
            .saturating_add(record.uncertain_changes);
        result.unresolved_regions = result
            .unresolved_regions
            .saturating_add(record.unresolved_regions);
        result.unlocalized_changed_regions = result
            .unlocalized_changed_regions
            .saturating_add(record.unlocalized_changed_regions);
        if let Some(old) = record.old_token_resolution {
            add_token_counts(&mut result.old_token_resolution, old);
        }
        if let Some(new) = record.new_token_resolution {
            add_token_counts(&mut result.new_token_resolution, new);
        }
        if let Some(proven) = record.proven {
            proven_has_values = true;
            proven_precision_known &= proven.precision.is_some();
            result.proven.reported_regions = result
                .proven
                .reported_regions
                .saturating_add(proven.reported_regions);
            result.proven.unlocalized_regions = result
                .proven
                .unlocalized_regions
                .saturating_add(proven.unlocalized_regions);
            match (
                proven_expected_known,
                proven.expected_regions,
                proven.matched_regions,
            ) {
                (true, Some(expected), Some(matched)) => {
                    result.proven.expected_regions = Some(
                        result
                            .proven
                            .expected_regions
                            .unwrap_or_default()
                            .saturating_add(expected),
                    );
                    result.proven.matched_regions = Some(
                        result
                            .proven
                            .matched_regions
                            .unwrap_or_default()
                            .saturating_add(matched),
                    );
                }
                _ => proven_expected_known = false,
            }
        }
        if let Some(reason) = &record.quality_skipped_reason {
            let count = result
                .quality_skip_reasons
                .entry(reason.clone())
                .or_default();
            *count = count.saturating_add(1);
        }
        if let Some(candidate) = &record.candidate {
            result.quality.candidate_annotated_counterparts = result
                .quality
                .candidate_annotated_counterparts
                .saturating_add(candidate.annotated_counterparts);
            result.quality.candidate_evaluable_counterparts = result
                .quality
                .candidate_evaluable_counterparts
                .saturating_add(candidate.evaluable_counterparts);
            result.quality.candidate_recalled_counterparts = result
                .quality
                .candidate_recalled_counterparts
                .saturating_add(candidate.recalled_counterparts);
            result.quality.candidate_unavailable_counterparts = result
                .quality
                .candidate_unavailable_counterparts
                .saturating_add(candidate.unavailable_counterparts);
            result.quality.candidate_groups = result
                .quality
                .candidate_groups
                .saturating_add(candidate.candidate_groups.unwrap_or_default());
            result.quality.candidate_tokens = result
                .quality
                .candidate_tokens
                .saturating_add(candidate.candidate_tokens.unwrap_or_default());
            if let Some(final_event) = candidate.final_event {
                result.quality.candidate_event_expected_changes = result
                    .quality
                    .candidate_event_expected_changes
                    .saturating_add(final_event.expected_changes);
                result.quality.candidate_event_matched_changes = result
                    .quality
                    .candidate_event_matched_changes
                    .saturating_add(final_event.matched_changes);
            }
            if let Some(value) = candidate.precision {
                candidate_precision_sum += value;
                candidate_precision_count = candidate_precision_count.saturating_add(1);
            }
        }
        let quality = &record.quality;
        let Some(annotation) = quality.annotation.as_deref() else {
            continue;
        };
        let Some(expected) = quality.expected_changes else {
            continue;
        };
        result.quality.evaluable_trials = result.quality.evaluable_trials.saturating_add(1);
        result.quality.expected_changes = result.quality.expected_changes.saturating_add(expected);
        result.quality.reported_changes = result
            .quality
            .reported_changes
            .saturating_add(quality.reported_changes.unwrap_or_default());
        result.quality.matched_changes = result
            .quality
            .matched_changes
            .saturating_add(quality.matched_changes.unwrap_or_default());
        result.quality.kind_correct = result
            .quality
            .kind_correct
            .saturating_add(quality.kind_correct.unwrap_or_default());
        match annotation {
            "complete" => {
                result.quality.complete_trials = result.quality.complete_trials.saturating_add(1);
                recall_expected = recall_expected.saturating_add(expected);
                recall_detected =
                    recall_detected.saturating_add(quality.matched_changes.unwrap_or_default());
                document_expected = document_expected.saturating_add(expected);
                document_detected =
                    document_detected.saturating_add(quality.matched_changes.unwrap_or_default());
            }
            "scoped_complete" => {
                result.quality.scoped_complete_trials =
                    result.quality.scoped_complete_trials.saturating_add(1);
                recall_expected = recall_expected.saturating_add(expected);
                recall_detected =
                    recall_detected.saturating_add(quality.matched_changes.unwrap_or_default());
            }
            "partial" => {
                result.quality.partial_trials = result.quality.partial_trials.saturating_add(1);
                partial_expected = partial_expected.saturating_add(expected);
                partial_detected =
                    partial_detected.saturating_add(quality.matched_changes.unwrap_or_default());
            }
            _ => {}
        }
        if annotation != "partial" {
            precision_numerator =
                precision_numerator.saturating_add(quality.matched_changes.unwrap_or_default());
            precision_denominator =
                precision_denominator.saturating_add(quality.reported_changes.unwrap_or_default());
        }
        kind_matched = kind_matched.saturating_add(quality.matched_changes.unwrap_or_default());
        kind_correct = kind_correct.saturating_add(quality.kind_correct.unwrap_or_default());
        if let Some(value) = quality.generator_candidate_recall_at_k {
            generator_candidate_sum += value;
            generator_candidate_count = generator_candidate_count.saturating_add(1);
        }
    }
    result.quality.precision = ratio(precision_numerator, precision_denominator);
    result.quality.recall = ratio(recall_detected, recall_expected);
    result.quality.document_recall = ratio(document_detected, document_expected);
    result.quality.kind_accuracy = ratio(kind_correct, kind_matched);
    result.quality.candidate_recall = ratio(
        result.quality.candidate_event_matched_changes,
        result.quality.candidate_event_expected_changes,
    );
    result.quality.generator_candidate_recall_at_k =
        if result.quality.candidate_evaluable_counterparts > 0 {
            ratio(
                result.quality.candidate_recalled_counterparts,
                result.quality.candidate_evaluable_counterparts,
            )
        } else {
            (generator_candidate_count > 0)
                .then(|| generator_candidate_sum / generator_candidate_count as f64)
        };
    result.quality.candidate_precision = (candidate_precision_count > 0)
        .then(|| candidate_precision_sum / candidate_precision_count as f64);
    if proven_has_values && proven_expected_known {
        result.proven.precision = proven_precision_known
            .then(|| {
                ratio(
                    result.proven.matched_regions.unwrap_or_default(),
                    result.proven.reported_regions,
                )
            })
            .flatten();
        result.proven.recall = ratio(
            result.proven.matched_regions.unwrap_or_default(),
            result.proven.expected_regions.unwrap_or_default(),
        );
    } else if proven_has_values {
        result.proven.expected_regions = None;
        result.proven.matched_regions = None;
        result.proven.precision = None;
        result.proven.recall = None;
    }
    // Partial recall is retained in `recall` only when no complete scope was
    // observed; it never supplies the document-wide denominator.
    if recall_expected == 0 {
        result.quality.recall = ratio(partial_detected, partial_expected);
        result.quality.document_recall = None;
    }
    result
}

fn add_token_counts(total: &mut TokenResolutionCounts, value: TokenResolutionCounts) {
    total.total = total.total.saturating_add(value.total);
    total.same = total.same.saturating_add(value.same);
    total.changed = total.changed.saturating_add(value.changed);
    total.unresolved = total.unresolved.saturating_add(value.unresolved);
}

fn macro_quality(groups: &[EvaluationGroupTotals]) -> MacroQualityTotals {
    let mut result = MacroQualityTotals::default();
    let mut precision = Vec::new();
    let mut recall = Vec::new();
    let mut document_recall = Vec::new();
    let mut kind_accuracy = Vec::new();
    let mut candidate_recall = Vec::new();
    let mut generator_candidate_recall_at_k = Vec::new();
    let considered_groups = groups
        .iter()
        .filter(|group| group.group_id != "unknown")
        .count();
    for group in groups.iter().filter(|group| group.group_id != "unknown") {
        let quality = &group.quality;
        if let Some(value) = quality.precision {
            precision.push(value);
        }
        if let Some(value) = quality.recall {
            recall.push(value);
        }
        if let Some(value) = quality.document_recall {
            document_recall.push(value);
        }
        if let Some(value) = quality.kind_accuracy {
            kind_accuracy.push(value);
        }
        if let Some(value) = quality.candidate_recall {
            candidate_recall.push(value);
        }
        if let Some(value) = quality.generator_candidate_recall_at_k {
            generator_candidate_recall_at_k.push(value);
        }
    }
    result.groups = considered_groups;
    result.precision = mean(&precision);
    result.recall = mean(&recall);
    result.document_recall = mean(&document_recall);
    result.kind_accuracy = mean(&kind_accuracy);
    result.candidate_recall = mean(&candidate_recall);
    result.generator_candidate_recall_at_k = mean(&generator_candidate_recall_at_k);
    result
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

/// Baseline identity recorded alongside the evaluation output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineRecord {
    pub name: String,
    pub kind: String,
    pub source_revision: String,
    pub options: BTreeMap<String, String>,
    pub policy_sha256: Option<String>,
    pub corpus_sha256: Option<String>,
    pub artifact_path: Option<String>,
    pub artifact_sha256: Option<String>,
    pub raw_result_sha256: Option<String>,
}

/// Returns no baseline identities until a capture records their provenance.
///
/// Executable paths and hashes belong to a specific run and must be supplied
/// through [`EvaluationSummary::with_baselines`] rather than embedded in
/// library defaults.
#[must_use]
pub fn default_baselines() -> Vec<BaselineRecord> {
    Vec::new()
}

/// Reproduction metadata for one published evaluation artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproducibleArtifact {
    pub schema_version: u32,
    pub command: Vec<String>,
    pub manifest_sha256: String,
    pub annotation_sha256: String,
    pub policy_sha256: String,
    pub corpus_sha256: String,
    pub source_revision: String,
    pub options: BTreeMap<String, String>,
    pub baselines: Vec<BaselineRecord>,
    pub raw_result_path: String,
    pub raw_result_sha256: String,
    pub summary_sha256: Option<String>,
}

/// Formats bytes as lowercase hexadecimal.
#[must_use]
pub fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Hashes bytes using the same lowercase SHA-256 representation as the
/// revision manifest.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

/// Hashes a reproducibility input without accepting a directory or symlink.
pub fn hash_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "cannot inspect artifact input {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(BenchError::InvalidInput(format!(
            "artifact input {} must be a regular file",
            path.display()
        )));
    }
    let bytes = fs::read(path).map_err(|error| {
        BenchError::InvalidInput(format!(
            "cannot read artifact input {}: {error}",
            path.display()
        ))
    })?;
    Ok(sha256_hex(&bytes))
}

/// Publishes reproduction metadata atomically and refuses to overwrite an
/// existing artifact.
pub fn write_reproducible_artifact(path: &Path, artifact: &ReproducibleArtifact) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    if !parent.is_dir() {
        return Err(BenchError::Publication(format!(
            "artifact parent directory does not exist: {}",
            parent.display()
        )));
    }
    if path.symlink_metadata().is_ok() {
        return Err(BenchError::Publication(format!(
            "artifact destination already exists: {}",
            path.display()
        )));
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            BenchError::Publication(format!(
                "artifact destination has no valid file name: {}",
                path.display()
            ))
        })?;
    let mut bytes = serde_json::to_vec_pretty(artifact).map_err(|error| {
        BenchError::Publication(format!("cannot serialize reproducible artifact: {error}"))
    })?;
    bytes.push(b'\n');
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|error| {
            BenchError::Publication(format!("cannot create temporary artifact: {error}"))
        })?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes).map_err(|error| {
            BenchError::Publication(format!("cannot write temporary artifact: {error}"))
        })?;
        file.sync_all().map_err(|error| {
            BenchError::Publication(format!("cannot sync temporary artifact: {error}"))
        })?;
        fs::hard_link(&temp_path, path).map_err(|error| {
            BenchError::Publication(format!("cannot publish reproducible artifact: {error}"))
        })?;
        Ok(())
    })();
    let _ = fs::remove_file(&temp_path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        pair_id: &str,
        split: &str,
        series: &str,
        derivation: &str,
        tuning_use: TuningUse,
        old_sha256: &str,
        new_sha256: &str,
    ) -> ManifestProvenanceEntry {
        ManifestProvenanceEntry {
            pair_id: pair_id.to_owned(),
            split: split.to_owned(),
            document_series_id: series.to_owned(),
            derivation_group: derivation.to_owned(),
            producer_family: "unknown".to_owned(),
            producer_version: "unknown".to_owned(),
            tuning_use,
            old_sha256: old_sha256.to_owned(),
            new_sha256: new_sha256.to_owned(),
        }
    }

    fn artifact() -> ReproducibleArtifact {
        ReproducibleArtifact {
            schema_version: EVALUATION_SCHEMA_VERSION,
            command: vec!["pdfbench".to_owned()],
            manifest_sha256: sha256_hex(b"manifest"),
            annotation_sha256: sha256_hex(b"annotation"),
            policy_sha256: sha256_hex(b"policy"),
            corpus_sha256: sha256_hex(b"corpus"),
            source_revision: "test".to_owned(),
            options: BTreeMap::new(),
            baselines: Vec::new(),
            raw_result_path: "raw.json".to_owned(),
            raw_result_sha256: sha256_hex(b"raw"),
            summary_sha256: None,
        }
    }

    #[test]
    fn provenance_parser_requires_complete_date_and_tuning_metadata() {
        let fields = [
            "series",
            "derivation",
            "unknown",
            "unknown",
            "scoped_complete",
            "2026-09-08",
            "unused",
        ];
        let parsed = BenchmarkProvenance::parse(&fields, 2).expect("valid metadata");
        assert_eq!(parsed.annotation_scope, "scoped_complete");
        assert!(!parsed.known_producer());
        let known_family = BenchmarkProvenance::parse(
            &[
                "series",
                "derivation",
                "LaTeX with hyperref",
                "unknown",
                "none",
                "2026-09-08",
                "unused",
            ],
            2,
        )
        .expect("known producer family with unknown version is valid");
        assert!(known_family.known_producer());
        assert!(
            BenchmarkProvenance::parse(
                &[
                    "series",
                    "derivation",
                    "unknown",
                    "unknown",
                    "none",
                    "2026-1-8",
                    "unused"
                ],
                3
            )
            .is_err()
        );
        assert!(
            BenchmarkProvenance::parse(
                &[
                    "series",
                    "derivation",
                    "unknown",
                    "unknown",
                    "unexpected",
                    "2026-09-08",
                    "unused"
                ],
                4
            )
            .is_err()
        );
        assert!(
            BenchmarkProvenance::parse(
                &[
                    "series",
                    "derivation",
                    "unknown",
                    "unknown",
                    "none",
                    "2024-02-30",
                    "unused"
                ],
                5
            )
            .is_err()
        );
    }

    #[test]
    fn split_validation_rejects_series_derivation_byte_and_tuning_contamination() {
        let mut rows = vec![entry(
            "dev-pair",
            "dev",
            "series-a",
            "derivation-a",
            TuningUse::UsedForFix,
            "a",
            "b",
        )];
        rows.push(entry(
            "holdout-pair",
            "holdout",
            "series-a",
            "derivation-b",
            TuningUse::Unused,
            "c",
            "d",
        ));
        assert!(validate_manifest_provenance(&rows).is_err());

        rows[1].document_series_id = "series-b".to_owned();
        rows[1].old_sha256 = "a".to_owned();
        assert!(validate_manifest_provenance(&rows).is_err());

        rows[1].old_sha256 = "c".to_owned();
        rows[1].derivation_group = "derivation-a".to_owned();
        assert!(validate_manifest_provenance(&rows).is_err());
    }

    #[test]
    fn split_validation_keeps_known_producer_series_in_one_split() {
        let mut first = entry(
            "dev-pair",
            "dev",
            "series-a",
            "derivation-a",
            TuningUse::Unused,
            "a",
            "b",
        );
        first.producer_family = "typst".to_owned();
        first.producer_version = "0.14".to_owned();
        let mut second = first.clone();
        second.pair_id = "holdout-pair".to_owned();
        second.split = "holdout".to_owned();
        second.document_series_id = "series-b".to_owned();
        second.derivation_group = "derivation-b".to_owned();
        second.old_sha256 = "c".to_owned();
        second.new_sha256 = "d".to_owned();
        assert!(validate_manifest_provenance(&[first, second]).is_err());
    }

    #[test]
    fn known_producer_family_with_unknown_version_has_a_producer_group() {
        let summary = EvaluationSummary::from_records(vec![EvaluationRecord {
            pair_id: "known-family".to_owned(),
            producer_family: Some("LaTeX with hyperref".to_owned()),
            producer_version: Some("unknown".to_owned()),
            ..EvaluationRecord::default()
        }]);

        assert_eq!(summary.producer_macro.groups, 1);
        assert_eq!(
            summary.producer_totals[0].group_id,
            "LaTeX with hyperref@unknown"
        );
    }

    #[test]
    fn evaluation_keeps_candidates_out_of_accepted_counts_and_failures_in_denominator() {
        let records = vec![
            EvaluationRecord {
                pair_id: "changed".to_owned(),
                split: "dev".to_owned(),
                trial_status: TrialStatus::Ok,
                compared: true,
                accepted_changes: 1,
                candidate_changes: 2,
                unresolved_regions: 1,
                candidate: Some(CandidateEvaluation {
                    top_k: 0,
                    annotated_counterparts: 0,
                    evaluable_counterparts: 0,
                    recalled_counterparts: 0,
                    unavailable_counterparts: 0,
                    recall_at_k: None,
                    candidate_groups: None,
                    candidate_tokens: None,
                    precision: None,
                    final_event: Some(CandidateEventEvaluation {
                        expected_changes: 2,
                        reported_changes: 2,
                        matched_changes: 1,
                        recall: Some(0.5),
                        precision: None,
                    }),
                }),
                quality: QualityEvaluation {
                    annotation: Some("partial".to_owned()),
                    expected_changes: Some(2),
                    reported_changes: Some(1),
                    matched_changes: Some(1),
                    kind_correct: Some(1),
                    ..QualityEvaluation::default()
                },
                ..EvaluationRecord::default()
            },
            EvaluationRecord {
                pair_id: "limit".to_owned(),
                split: "holdout".to_owned(),
                trial_status: TrialStatus::Limit,
                candidate: Some(CandidateEvaluation {
                    top_k: 5,
                    annotated_counterparts: 3,
                    evaluable_counterparts: 2,
                    recalled_counterparts: 1,
                    unavailable_counterparts: 1,
                    recall_at_k: Some(0.5),
                    candidate_groups: None,
                    candidate_tokens: None,
                    precision: None,
                    final_event: None,
                }),
                ..EvaluationRecord::default()
            },
        ];
        let summary = EvaluationSummary::from_records(records);
        assert_eq!(summary.totals.accepted_changes, 1);
        assert_eq!(summary.totals.candidate_changes, 2);
        assert_eq!(summary.totals.operational.trials, 2);
        assert_eq!(summary.totals.operational.limit, 1);
        assert_eq!(summary.totals.quality.partial_trials, 1);
        assert_eq!(summary.totals.quality.precision, None);
        assert_eq!(summary.totals.quality.document_recall, None);
        assert_eq!(summary.totals.quality.recall, Some(0.5));
        assert_eq!(summary.totals.quality.kind_accuracy, Some(1.0));
        assert_eq!(summary.totals.quality.candidate_recall, Some(0.5));
        assert_eq!(
            summary.totals.quality.generator_candidate_recall_at_k,
            Some(0.5)
        );
        assert_eq!(summary.totals.quality.candidate_event_expected_changes, 2);
        assert_eq!(summary.totals.quality.candidate_event_matched_changes, 1);
        let serialized = serde_json::to_value(&summary).expect("evaluation summary serializes");
        assert_eq!(
            serialized["records"][0]["candidate"]["final_event"]["reported_changes"],
            2
        );
        assert_eq!(summary.totals.quality.candidate_evaluable_counterparts, 2);
        assert_eq!(summary.split_totals.len(), 2);
    }

    #[test]
    fn historical_assessment_without_claim_diagnostics_stays_unavailable() {
        let assessment: AssessmentEvaluation = serde_json::from_value(serde_json::json!({
            "policy_version": 1,
            "work_limit": 10,
            "work_used": 0,
            "work_by_stage": {
                "anchor_verification": 0,
                "local_views": 0,
                "localization": 0,
                "emission": 0
            },
            "candidates_truncated": false
        }))
        .expect("historical assessment deserializes");

        assert_eq!(assessment.claim_diagnostics, None);
        let serialized = serde_json::to_value(assessment).expect("assessment serializes");
        assert!(
            !serialized
                .as_object()
                .expect("assessment object")
                .contains_key("claim_diagnostics")
        );
    }

    #[test]
    fn expected_match_results_serialize_with_the_evaluation_record() {
        let summary = EvaluationSummary::from_records(vec![EvaluationRecord {
            pair_id: "matched".to_owned(),
            expected_matches: Some(vec![ExpectedChangeMatchEvaluation {
                expected_id: "change-1".to_owned(),
                matched: true,
                actual_index: Some(3),
                actual_kind: Some("replacement".to_owned()),
                kind_agrees: Some(true),
            }]),
            ..EvaluationRecord::default()
        }]);

        let serialized = serde_json::to_value(summary).expect("evaluation summary serializes");
        assert_eq!(
            serialized["records"][0]["expected_matches"][0],
            serde_json::json!({
                "expected_id": "change-1",
                "matched": true,
                "actual_index": 3,
                "actual_kind": "replacement",
                "kind_agrees": true,
            })
        );
    }

    #[test]
    fn proven_totals_discard_partial_expected_counts_independent_of_order() {
        let known = EvaluationRecord {
            pair_id: "known".to_owned(),
            proven: Some(ProvenEvaluation {
                reported_regions: 2,
                expected_regions: Some(2),
                matched_regions: Some(1),
                precision: Some(0.5),
                recall: Some(0.5),
                unlocalized_regions: 2,
            }),
            ..EvaluationRecord::default()
        };
        let unknown = EvaluationRecord {
            pair_id: "unknown".to_owned(),
            proven: Some(ProvenEvaluation {
                reported_regions: 3,
                expected_regions: None,
                matched_regions: None,
                precision: None,
                recall: None,
                unlocalized_regions: 3,
            }),
            ..EvaluationRecord::default()
        };
        let first = EvaluationSummary::from_records(vec![known.clone(), unknown.clone()]);
        let second = EvaluationSummary::from_records(vec![unknown, known]);

        assert_eq!(first.totals.proven.reported_regions, 5);
        assert_eq!(first.totals.proven.expected_regions, None);
        assert_eq!(first.totals.proven.matched_regions, None);
        assert_eq!(first.totals.proven.precision, None);
        assert_eq!(first.totals.proven.recall, None);
        assert_eq!(first.totals.proven, second.totals.proven);
    }

    #[test]
    fn operational_and_resolution_totals_keep_observed_resource_and_token_states() {
        let summary = EvaluationSummary::from_records(vec![
            EvaluationRecord {
                pair_id: "one".to_owned(),
                trial_status: TrialStatus::Ok,
                trial_runtime_ms: 4,
                peak_memory_bytes: Some(200),
                old_token_resolution: Some(TokenResolutionCounts {
                    total: 5,
                    same: 2,
                    changed: 2,
                    unresolved: 1,
                }),
                new_token_resolution: Some(TokenResolutionCounts {
                    total: 6,
                    same: 3,
                    changed: 2,
                    unresolved: 1,
                }),
                quality_skipped_reason: Some("incomplete_extraction".to_owned()),
                ..EvaluationRecord::default()
            },
            EvaluationRecord {
                pair_id: "two".to_owned(),
                trial_status: TrialStatus::Limit,
                trial_runtime_ms: 10,
                peak_memory_bytes: Some(100),
                ..EvaluationRecord::default()
            },
        ]);
        let operational = &summary.totals.operational;
        assert_eq!(operational.trials, 2);
        assert_eq!(operational.limit, 1);
        assert_eq!(operational.runtime.p50_ms, Some(4));
        assert_eq!(operational.runtime.p95_ms, Some(10));
        assert_eq!(operational.peak_memory.p50_bytes, Some(100));
        assert_eq!(summary.totals.old_token_resolution.unresolved, 1);
        assert_eq!(summary.totals.new_token_resolution.changed, 2);
        assert_eq!(
            summary.totals.quality_skip_reasons["incomplete_extraction"],
            1
        );
    }

    #[test]
    fn reproducible_artifact_refuses_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "pdfbench-evaluation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temporary root");
        let path = root.join("run.json");
        write_reproducible_artifact(&path, &artifact()).expect("publish artifact");
        assert!(write_reproducible_artifact(&path, &artifact()).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reproducible_artifact_accepts_a_relative_destination() {
        let name = format!(
            "pdfbench-relative-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        write_reproducible_artifact(Path::new(&name), &artifact())
            .expect("a bare file name resolves against the working directory");
        let _ = fs::remove_file(&name);
    }
}
