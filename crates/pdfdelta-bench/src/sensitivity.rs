//! Development-only perturbation matrix for layout, matching, and budget
//! parameters.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    BenchError, Result,
    cases::{BenchmarkCase, built_in_cases},
    evaluator::{default_pipeline_options, evaluate_case_with_options},
    renderers::RendererKind,
};
use pdfdelta_core::pipeline::PipelineOptions;

pub const SENSITIVITY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
struct Scenario {
    name: &'static str,
    parameters: BTreeMap<String, String>,
    options: PipelineOptions,
}

/// One result in the development sensitivity matrix.
#[derive(Clone, Debug, Serialize)]
pub struct SensitivityRecord {
    pub scenario: String,
    pub case_name: String,
    pub renderer: String,
    pub passed: bool,
    pub actual_changes: Option<usize>,
    pub formatting_only_changes: Option<usize>,
    pub extraction_complete: Option<bool>,
    pub comparison_complete: Option<bool>,
    pub event_precision: Option<f64>,
    pub event_recall: Option<f64>,
    pub detail: String,
}

/// Parameters used by one sensitivity scenario.
#[derive(Clone, Debug, Serialize)]
pub struct SensitivityScenario {
    pub name: String,
    pub parameters: BTreeMap<String, String>,
}

/// Reproducible result of the full built-in perturbation matrix.
#[derive(Clone, Debug, Serialize)]
pub struct SensitivityReport {
    pub schema_version: u32,
    pub corpus: String,
    pub scenarios: Vec<SensitivityScenario>,
    pub records: Vec<SensitivityRecord>,
}

/// Runs every built-in case with both available renderers under each fixed
/// development perturbation.
pub fn run_builtin_sensitivity() -> Result<SensitivityReport> {
    let cases = built_in_cases()?;
    run_sensitivity(&cases)
}

fn run_sensitivity(cases: &[BenchmarkCase]) -> Result<SensitivityReport> {
    let scenarios = scenarios();
    let scenario_metadata = scenarios
        .iter()
        .map(|scenario| SensitivityScenario {
            name: scenario.name.to_owned(),
            parameters: scenario.parameters.clone(),
        })
        .collect();
    let mut records = Vec::new();
    for scenario in &scenarios {
        for case in cases {
            for renderer in RendererKind::all() {
                records.push(evaluate_one(case, renderer, scenario));
            }
        }
    }
    Ok(SensitivityReport {
        schema_version: SENSITIVITY_SCHEMA_VERSION,
        corpus: "built-in-acceptance-cases".to_owned(),
        scenarios: scenario_metadata,
        records,
    })
}

fn evaluate_one(
    case: &BenchmarkCase,
    renderer: RendererKind,
    scenario: &Scenario,
) -> SensitivityRecord {
    match evaluate_case_with_options(case, renderer, scenario.options) {
        Ok(result) => SensitivityRecord {
            scenario: scenario.name.to_owned(),
            case_name: case.name().to_owned(),
            renderer: renderer.name().to_owned(),
            passed: result.passed,
            actual_changes: Some(result.actual_changes),
            formatting_only_changes: Some(result.formatting_only_changes),
            extraction_complete: Some(result.extraction_complete),
            comparison_complete: Some(result.comparison_complete),
            event_precision: Some(result.precision.event_precision),
            event_recall: Some(result.precision.event_recall),
            detail: result.detail,
        },
        Err(error) => SensitivityRecord {
            scenario: scenario.name.to_owned(),
            case_name: case.name().to_owned(),
            renderer: renderer.name().to_owned(),
            passed: false,
            actual_changes: None,
            formatting_only_changes: None,
            extraction_complete: None,
            comparison_complete: None,
            event_precision: None,
            event_recall: None,
            detail: error.to_string(),
        },
    }
}

fn scenarios() -> Vec<Scenario> {
    let baseline = default_pipeline_options();
    vec![
        scenario("baseline", baseline, []),
        scenario(
            "line-baseline-tolerant",
            PipelineOptions {
                line: pdfdelta_core::layout::LineOptions {
                    max_baseline_distance_ratio: baseline.line.max_baseline_distance_ratio * 1.5,
                    ..baseline.line
                },
                ..baseline
            },
            [("line.max_baseline_distance_ratio", "0.375")],
        ),
        scenario(
            "block-join-strict",
            PipelineOptions {
                block: pdfdelta_core::layout::BlockOptions {
                    min_join_score: 0.9,
                    ..baseline.block
                },
                ..baseline
            },
            [("block.min_join_score", "0.9")],
        ),
        scenario(
            "match-score-relaxed",
            PipelineOptions {
                alignment: pdfdelta_core::alignment::AlignmentOptions {
                    min_match_score: 0.45,
                    strong_match_score: 0.8,
                    ..baseline.alignment
                },
                ..baseline
            },
            [
                ("alignment.min_match_score", "0.45"),
                ("alignment.strong_match_score", "0.8"),
            ],
        ),
        scenario(
            "score-margin-wide",
            PipelineOptions {
                alignment: pdfdelta_core::alignment::AlignmentOptions {
                    min_score_margin: 0.16,
                    ..baseline.alignment
                },
                ..baseline
            },
            [("alignment.min_score_margin", "0.16")],
        ),
        scenario(
            "candidate-budget-low",
            PipelineOptions {
                alignment: pdfdelta_core::alignment::AlignmentOptions {
                    max_candidate_visits: 4_000,
                    ..baseline.alignment
                },
                ..baseline
            },
            [("alignment.max_candidate_visits", "4000")],
        ),
    ]
}

fn scenario<const N: usize>(
    name: &'static str,
    options: PipelineOptions,
    parameters: [(&str, &str); N],
) -> Scenario {
    Scenario {
        name,
        options,
        parameters: parameters
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    }
}

/// Writes a sensitivity report as JSON while rejecting invalid empty output
/// destinations at the caller boundary.
pub fn serialize_report(report: &SensitivityReport) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(report).map_err(|error| {
        BenchError::Publication(format!("cannot serialize sensitivity report: {error}"))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_covers_layout_matching_and_budget_perturbations() {
        let scenarios = scenarios();
        assert_eq!(scenarios.len(), 6);
        assert!(scenarios.iter().any(|scenario| scenario.name == "baseline"));
        assert!(scenarios.iter().any(|scenario| {
            scenario
                .parameters
                .contains_key("alignment.max_candidate_visits")
        }));
    }

    #[test]
    fn report_serialization_is_stable_and_versioned() {
        let report = SensitivityReport {
            schema_version: SENSITIVITY_SCHEMA_VERSION,
            corpus: "test".to_owned(),
            scenarios: Vec::new(),
            records: Vec::new(),
        };
        let bytes = serialize_report(&report).expect("serialize report");
        assert!(
            std::str::from_utf8(&bytes)
                .expect("UTF-8 report")
                .contains("\"schema_version\": 1")
        );
    }
}
