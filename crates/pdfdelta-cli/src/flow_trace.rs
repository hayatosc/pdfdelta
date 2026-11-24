//! Phase trace for the flow text engine.
//!
//! The compact report never contains timing. This trace records bounded counts
//! and `duration_us` for each flow phase. `duration_us` is nondeterministic and
//! is documented as such in the schema and in `docs/usage.md`; no other field
//! depends on wall time.

use std::{collections::BTreeMap, io::Write, path::Path, time::Duration};

use pdfdelta_flow::FlowError;
use serde::Serialize;

/// Which document side a phase measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraceSide {
    Old,
    New,
}

/// Outcome of one phase in the flow trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraceStatus {
    Completed,
    Incomplete,
    Failed,
    Skipped,
}

/// Converts a measured interval into the saturating `duration_us` metric.
pub(crate) fn duration_metric(duration: Duration) -> usize {
    usize::try_from(duration.as_micros()).unwrap_or(usize::MAX)
}

const FLOW_TRACE_SCHEMA: &str = "pdfdelta-flow-trace/1";
const FLOW_TRACE_VERSION: u8 = 1;
const MAX_ERROR_MESSAGE_BYTES: usize = 2_048;

#[derive(Debug, Serialize)]
struct TraceCommand {
    kind: &'static str,
    old_path: String,
    new_path: String,
}

#[derive(Debug, Serialize)]
struct TraceResult {
    status: &'static str,
    exit_code: u8,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TraceError {
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

impl TraceError {
    fn from_flow(error: &FlowError) -> Self {
        let limit = error.limit();
        Self {
            kind: error.class().label(),
            message: bounded_message(error.message()),
            resource: limit.map(|limit| limit.resource),
            limit: limit.map(|limit| limit.limit),
        }
    }
}

#[derive(Debug, Serialize)]
struct TracePhase {
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<TraceSide>,
    status: TraceStatus,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    metrics: BTreeMap<&'static str, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_us: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TraceError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skip_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FlowTrace {
    trace_schema: &'static str,
    trace_schema_version: u8,
    engine: &'static str,
    command: TraceCommand,
    result: TraceResult,
    nondeterministic_fields: Vec<&'static str>,
    phases: Vec<TracePhase>,
}

impl FlowTrace {
    pub(crate) fn new(old_path: &Path, new_path: &Path) -> Self {
        Self {
            trace_schema: FLOW_TRACE_SCHEMA,
            trace_schema_version: FLOW_TRACE_VERSION,
            engine: "flow",
            command: TraceCommand {
                kind: "compare",
                old_path: old_path.to_string_lossy().into_owned(),
                new_path: new_path.to_string_lossy().into_owned(),
            },
            result: TraceResult {
                status: "failed",
                exit_code: 2,
            },
            nondeterministic_fields: vec!["duration_us"],
            phases: Vec::new(),
        }
    }

    pub(crate) fn complete(
        &mut self,
        name: &'static str,
        side: Option<TraceSide>,
        metrics: impl IntoIterator<Item = (&'static str, usize)>,
        duration: Duration,
    ) {
        self.record(
            name,
            side,
            TraceStatus::Completed,
            metrics,
            Some(duration),
            None,
        );
    }

    pub(crate) fn incomplete(
        &mut self,
        name: &'static str,
        side: Option<TraceSide>,
        metrics: impl IntoIterator<Item = (&'static str, usize)>,
        duration: Duration,
        error: &FlowError,
    ) {
        self.record(
            name,
            side,
            TraceStatus::Incomplete,
            metrics,
            Some(duration),
            Some(TraceError::from_flow(error)),
        );
    }

    pub(crate) fn fail(&mut self, name: &'static str, side: Option<TraceSide>, error: &FlowError) {
        self.record(
            name,
            side,
            TraceStatus::Failed,
            [],
            None,
            Some(TraceError::from_flow(error)),
        );
    }

    /// Marks the command result. Missing expected phases are recorded as
    /// skipped, so a stopped run never looks like an empty completed phase
    /// list.
    pub(crate) fn finish(&mut self, exit_code: u8, incomplete: bool, channels: bool) {
        self.result = TraceResult {
            status: if incomplete {
                "incomplete"
            } else if exit_code == 2 {
                "failed"
            } else {
                "completed"
            },
            exit_code,
        };
        for (name, side) in expected_phases() {
            if !self
                .phases
                .iter()
                .any(|phase| phase.name == name && phase.side == side)
            {
                self.phases.push(TracePhase {
                    name,
                    side,
                    status: TraceStatus::Skipped,
                    metrics: BTreeMap::new(),
                    duration_us: None,
                    error: None,
                    // The channel phase only exists for a multi-channel run. A
                    // text-only run never starts it, which is a different fact
                    // from a phase that a failed predecessor prevented.
                    skip_reason: Some(if name == "channel_comparison" && !channels {
                        "text_only"
                    } else {
                        "prior_phase_did_not_complete"
                    }),
                });
            }
        }
    }

    pub(crate) fn write_json<W: Write + ?Sized>(
        &self,
        writer: &mut W,
    ) -> Result<(), serde_json::Error> {
        serde_json::to_writer_pretty(&mut *writer, self)?;
        writer.write_all(b"\n").map_err(serde_json::Error::io)
    }

    fn record(
        &mut self,
        name: &'static str,
        side: Option<TraceSide>,
        status: TraceStatus,
        metrics: impl IntoIterator<Item = (&'static str, usize)>,
        duration: Option<Duration>,
        error: Option<TraceError>,
    ) {
        self.phases.push(TracePhase {
            name,
            side,
            status,
            metrics: metrics.into_iter().collect(),
            duration_us: duration.map(duration_metric),
            error,
            skip_reason: None,
        });
    }
}

fn bounded_message(message: &str) -> String {
    if message.len() <= MAX_ERROR_MESSAGE_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_ERROR_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

fn expected_phases() -> Vec<(&'static str, Option<TraceSide>)> {
    let phases = vec![
        ("input_reading", Some(TraceSide::Old)),
        ("input_reading", Some(TraceSide::New)),
        ("parse", Some(TraceSide::Old)),
        ("parse", Some(TraceSide::New)),
        ("glyph_extraction", Some(TraceSide::Old)),
        ("glyph_extraction", Some(TraceSide::New)),
        ("tokenization_normalization", Some(TraceSide::Old)),
        ("tokenization_normalization", Some(TraceSide::New)),
        ("furniture_order", None),
        ("flow_diff", None),
        ("assessment_formatting", None),
        // Always expected: a text-only run records it explicitly as skipped
        // for `text_only`, and a multi-channel run records it completed.
        ("channel_comparison", None),
        ("report", None),
    ];
    phases
}

/// Writes a flow trace atomically. A destination that exists is refused by the
/// shared output publisher, exactly like every other report path.
pub(crate) fn write_atomically(path: &Path, trace: &FlowTrace) -> Result<(), FlowError> {
    crate::fs::write_output_atomically(path, "flow trace report", |output| {
        trace
            .write_json(output)
            .map_err(|error| format!("cannot render flow trace for {}: {error}", path.display()))
    })
    .map_err(FlowError::backend)
}

/// Bounded metric helper for issue kinds.
pub(crate) fn issue_metrics(
    issues: &[pdfdelta_core::source::ExtractionIssue],
) -> BTreeMap<&'static str, usize> {
    let mut metrics = BTreeMap::new();
    metrics.insert("issues", issues.len());
    metrics.insert(
        "unsupported_issues",
        issues
            .iter()
            .filter(|issue| issue.kind() == pdfdelta_core::source::ExtractionIssueKind::Unsupported)
            .count(),
    );
    metrics.insert(
        "unresolved_issues",
        issues
            .iter()
            .filter(|issue| issue.kind() == pdfdelta_core::source::ExtractionIssueKind::Unresolved)
            .count(),
    );
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_kinds_come_from_the_typed_class_not_the_message() {
        let mut trace = FlowTrace::new(Path::new("old.pdf"), Path::new("new.pdf"));
        let errors = [
            (
                "backend",
                FlowError::backend("limit exceeded; unsupported input; max_ bytes"),
            ),
            (
                "unsupported",
                FlowError::unsupported("backend failure and a limit exceeded"),
            ),
            (
                "unresolved",
                FlowError::unresolved("unsupported wording with exceeded limits"),
            ),
            (
                "limit_exceeded",
                FlowError::limit_exceeded(
                    "PDF input bytes",
                    16,
                    "unsupported backend failure without limit wording",
                ),
            ),
        ];
        for (name, error) in &errors {
            trace.fail(name, None, error);
        }
        let mut output = Vec::new();
        trace.write_json(&mut output).expect("trace serializes");
        let value: serde_json::Value =
            serde_json::from_slice(&output).expect("trace is valid JSON");
        for (index, (expected, _)) in errors.iter().enumerate() {
            assert_eq!(
                value["phases"][index]["error"]["kind"], *expected,
                "{value}"
            );
        }
        assert_eq!(value["phases"][3]["error"]["resource"], "PDF input bytes");
        assert_eq!(value["phases"][3]["error"]["limit"], 16);
    }
}
