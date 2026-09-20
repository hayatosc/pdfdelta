//! Token-bounded review packets for an external reviewing agent.
//!
//! The engine's comparison is unchanged by everything in this module. A packet
//! projects what the comparison already established, together with the
//! obligations it could not discharge, so that a caller can retrieve only the
//! evidence a particular decision needs.
//!
//! Three separations are load-bearing and are preserved by every type here:
//!
//! * A machine result and an external interpretation are different things. A
//!   reviewer's selection is stored beside the comparison, never merged into
//!   it, and never raises a non-owning or inferred observation to a strict one.
//! * Omission is not resolution. Output budgets, search budgets, candidate
//!   truncation, and unretrieved images are reported independently, so a single
//!   returned hypothesis is never read as uniqueness.
//! * Unresolved material survives having no candidate. Pages with no extracted
//!   text, inventories that never closed, and scopes that were never visited
//!   become cases or explicit gaps rather than disappearing.
//!
//! Document-derived text carried by these types is untrusted data. It is quoted
//! for a reviewer to read; it never becomes an instruction, a file name, or a
//! command argument.

mod case;
mod context;
mod contract;
mod decision;
mod evidence_plan;
mod native_plan;
mod planner;

pub use case::{
    Cardinality, CaseRegion, Hypothesis, OmissionReason, OmittedRun, RequiredEvidence, ReviewCase,
    ReviewText, SideLocation, StructureLabel, TextCoverage, TextOrder, UnmappedMark,
};
pub use context::{CaseContext, ContextItem, ContextKind};
pub use contract::{
    BundleId, BundleIdentity, CaseCompleteness, CaseId, Completeness, Cursor, DECISION_SCHEMA,
    Detail, EngineClass, EvidenceRef, GapId, HypothesisId, IdentifierError, MAX_CURSOR_BYTES,
    MAX_IDENTIFIER_BYTES, POLICY_VERSION, PipelineContract, REVIEW_SCHEMA, ReasonRecord,
    RetrievalAction, ReviewAssumption, ReviewQuestion, ReviewReason, ScalarInterval, Side,
    SidedSource, SourceAlias, TokenInterval,
};
pub use decision::{
    AgentDecision, DecisionChangeKind, DecisionOutcome, DecisionRejection, DecisionStatus,
    RetrievalRequest, validate_decision, validate_decisions,
};
pub use evidence_plan::{SharedEvidenceReview, plan_shared_evidence};
pub use manifest::{
    AgentReviewManifest, CaseCensus, EngineOutcome, EngineStatus, ExportOmission, ExportStatus,
    GapScope, InventoryGap, OmissionKind, OmissionScope, QuestionCount, RetrievalCapability,
    UnlocalizedGap,
};
pub use native_plan::{NativeTextReview, cases_covering, covered_blocks, plan_native_text};
pub use planner::{PlannerLimits, ReviewPlan, evidence_ref, retain_text, source_alias};

mod manifest;

/// Lowercase hexadecimal rendering of a digest.
///
/// Digests identify artifacts and detect drift; they never stand in for a
/// source-equality proof.
#[must_use]
pub fn lowercase_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_rendering_is_lowercase_and_zero_padded() {
        assert_eq!(lowercase_hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert!(lowercase_hex(&[0xab]).chars().all(|c| !c.is_uppercase()));
    }
}
