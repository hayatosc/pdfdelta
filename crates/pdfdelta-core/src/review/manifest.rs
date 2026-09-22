//! The small index an agent reads first: what the engine established, what it
//! could not, and how to retrieve the rest.
//!
//! The manifest never carries document text in bulk. It carries identity,
//! counts, gaps, and typed retrieval actions, so that the first read is bounded
//! regardless of document size.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::contract::{
    BundleId, BundleIdentity, CaseId, Completeness, Detail, EvidenceRef, GapId, PipelineContract,
    ReasonRecord, RetrievalAction, ReviewQuestion, Side,
};
use crate::{document::Channel, model::PageId};

/// The engine's own verdict, independent of any external assessment.
///
/// An external review never rewrites these fields. The command-line policy maps
/// [`EngineStatus`] to process exit codes; the mapping lives with the CLI so
/// this model stays free of process concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    /// Every selected obligation was discharged and no content change was found.
    CompleteUnchanged,
    /// Every selected obligation was discharged and content changes were found.
    CompleteChanged,
    /// At least one selected obligation remains; changes may also be present.
    Incomplete,
}

/// Counts the engine produced, copied without reinterpretation.
///
/// The strict, non-owning, and inferred counts stay separate here exactly as
/// they do in the comparison report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineOutcome {
    pub status: EngineStatus,
    pub comparison_complete: bool,
    pub typed_changes: usize,
    pub inferred_changes: usize,
    pub scope_content_changes: usize,
    pub inferred_scope_changes: usize,
}

/// One channel's discovery state on one side.
///
/// `discovered_sources` is not a denominator: when `inventory_complete` is
/// false, the amount of undiscovered evidence is unknown and no ratio over
/// these counts describes document coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryGap {
    pub side: Side,
    pub channel: Channel,
    pub inventory_complete: bool,
    pub discovered_sources: usize,
    pub compared_sources: usize,
    pub uncompared_sources: usize,
}

/// An unresolved obligation that could not be attached to any located case.
///
/// A gap with no sources is the honest representation of material the engine
/// knows it failed to examine but cannot point at. It is never dropped because
/// no candidate exists for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlocalizedGap {
    pub gap_id: GapId,
    pub scope: GapScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    pub evidence_complete: Completeness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<ReasonRecord>,
    /// Sources involved, when any could be named at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<EvidenceRef>,
}

/// How far an unlocalized gap reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum GapScope {
    /// The whole document is affected.
    Document,
    /// One page is affected, in both bases.
    Page {
        page_number: u32,
        page_index: PageId,
    },
    /// One channel is affected without a page locator.
    Channel,
}

/// What the export itself could not carry.
///
/// Export budgets are separate from comparison completeness: a fully complete
/// comparison can still produce a truncated export, and that truncation is
/// never reported as an unresolved comparison.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOmission {
    pub scope: OmissionScope,
    pub kind: OmissionKind,
    /// How many records were left out, when the count is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand_with: Option<RetrievalAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ReasonRecord>,
}

/// What an omission applies to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum OmissionScope {
    Bundle,
    Case {
        case: CaseId,
    },
    Channel {
        channel: Channel,
    },
    Page {
        page_number: u32,
        page_index: PageId,
    },
}

/// The kind of material an omission withheld.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionKind {
    /// Cases exist that the export could not write.
    CasesNotExported,
    /// A case's competing hypotheses were truncated.
    AlternativesTruncated,
    /// Retained text was withheld.
    TextOmitted,
    /// Surrounding structure was withheld.
    ContextOmitted,
    /// No image was produced for a case that needs one.
    ImageNotRendered,
    /// A gap's source list was shortened; the gap itself is still reported.
    GapSourcesOmitted,
}

/// The work the export was allowed to do, and whether it finished.
///
/// `export_complete` answers a different question from
/// [`EngineOutcome::comparison_complete`] and the two are never merged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportStatus {
    pub export_complete: bool,
    pub source_visits: usize,
    pub source_visit_limit: usize,
    pub candidate_visits: usize,
    pub candidate_visit_limit: usize,
    pub cases_planned: usize,
    pub case_limit: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omissions: Vec<ExportOmission>,
}

/// How many cases exist, broken down by question.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseCensus {
    pub total: usize,
    pub by_question: Vec<QuestionCount>,
}

/// One question's case count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionCount {
    pub question: ReviewQuestion,
    pub cases: usize,
}

/// A retrieval this bundle can actually serve.
///
/// A host that cannot display images still receives the visual capability
/// listing; what it must not receive is a case silently downgraded to a text
/// answer that the evidence does not support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalCapability {
    pub detail: Detail,
    pub available: bool,
}

/// The index an agent reads before requesting anything else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReviewManifest {
    pub schema: String,
    pub bundle_id: BundleId,
    pub identity: BundleIdentity,
    pub pipeline: PipelineContract,
    pub selected_channels: BTreeSet<Channel>,
    pub engine: EngineOutcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inventory_gaps: Vec<InventoryGap>,
    pub cases: CaseCensus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unlocalized_gaps: Vec<UnlocalizedGap>,
    pub export: ExportStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<RetrievalCapability>,
    /// The first retrieval a caller should perform.
    pub next: RetrievalAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_completeness_is_independent_of_comparison_completeness() {
        let engine = EngineOutcome {
            status: EngineStatus::CompleteChanged,
            comparison_complete: true,
            typed_changes: 3,
            inferred_changes: 0,
            scope_content_changes: 0,
            inferred_scope_changes: 0,
        };
        let export = ExportStatus {
            export_complete: false,
            source_visits: 10,
            source_visit_limit: 10,
            candidate_visits: 0,
            candidate_visit_limit: 100,
            cases_planned: 4,
            case_limit: 4,
            omissions: vec![ExportOmission {
                scope: OmissionScope::Bundle,
                kind: OmissionKind::CasesNotExported,
                omitted: None,
                expand_with: None,
                reason: None,
            }],
        };
        assert!(engine.comparison_complete);
        assert!(!export.export_complete);
    }

    #[test]
    fn an_incomplete_inventory_has_no_denominator() {
        let gap = InventoryGap {
            side: Side::Old,
            channel: Channel::Text,
            inventory_complete: false,
            discovered_sources: 120,
            compared_sources: 120,
            uncompared_sources: 0,
        };
        // Every discovered reference was compared, yet discovery itself never
        // closed, so the channel is not complete.
        assert_eq!(gap.uncompared_sources, 0);
        assert!(!gap.inventory_complete);
    }
}
