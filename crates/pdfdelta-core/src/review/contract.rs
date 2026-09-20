//! Identity, completeness, reason, and retrieval vocabulary shared by every
//! agent review packet.
//!
//! The vocabulary here is deliberately separate from the comparison report: an
//! external reviewer's selection is a review note, never a proof certificate,
//! so its schema evolves independently of the engine's versioned result.

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    diff::AssessmentReason,
    document::{Channel, EvidenceFailure, SourceRef},
    model::PageId,
    normalize::ScalarRange,
};

/// Schema identifier for review packets produced by the planner.
pub const REVIEW_SCHEMA: &str = "agent-review/v1";

/// Schema identifier for external assessments returned by a host agent.
pub const DECISION_SCHEMA: &str = "agent-decision/v1";

/// Packet policy version.
///
/// It is bound into [`BundleIdentity::digest`], so a policy change produces a
/// different bundle identity even for byte-identical inputs. Identifier
/// stability is promised for one policy version and one engine revision, never
/// across them.
pub const POLICY_VERSION: u32 = 1;

/// Maximum length of any identifier this module mints or accepts.
pub const MAX_IDENTIFIER_BYTES: usize = 64;

/// Why an identifier was rejected.
///
/// Identifiers cross a trust boundary in both directions: they are printed in
/// bounded JSON and read back from an external assessment, so their character
/// set is restricted rather than escaped at every use site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentifierError {
    Empty,
    TooLong { bytes: usize },
    UnsupportedCharacter { position: usize },
}

impl fmt::Display for IdentifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("identifier is empty"),
            Self::TooLong { bytes } => write!(
                formatter,
                "identifier is {bytes} bytes; the limit is {MAX_IDENTIFIER_BYTES}"
            ),
            Self::UnsupportedCharacter { position } => write!(
                formatter,
                "identifier byte {position} is outside [A-Za-z0-9._:-]"
            ),
        }
    }
}

impl std::error::Error for IdentifierError {}

fn validate_identifier(value: &str) -> Result<(), IdentifierError> {
    if value.is_empty() {
        return Err(IdentifierError::Empty);
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(IdentifierError::TooLong { bytes: value.len() });
    }
    for (position, byte) in value.bytes().enumerate() {
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')) {
            return Err(IdentifierError::UnsupportedCharacter { position });
        }
    }
    Ok(())
}

macro_rules! identifier {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Accepts an identifier restricted to `[A-Za-z0-9._:-]`.
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                validate_identifier(&value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

identifier!(
    BundleId,
    "Identity of one exported review bundle.\n\nBound to the inputs, options, policy version, and evidence snapshot; never to\nwall-clock time."
);
identifier!(CaseId, "Identity of one review case inside a bundle.");
identifier!(
    HypothesisId,
    "Identity of one competing hypothesis inside a case."
);
identifier!(GapId, "Identity of one unlocalized gap inside a bundle.");
identifier!(
    SourceAlias,
    "Short bundle-local alias for one source reference.\n\nAliases are collision-checked inside a bundle and are meaningless outside it."
);

/// Which document a reference belongs to.
///
/// Sides are never interchangeable: the same alias text on the other side
/// denotes different evidence, so every reference carries its side explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

impl Side {
    #[must_use]
    pub const fn is_old(self) -> bool {
        matches!(self, Self::Old)
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

/// A side-tagged source reference with its bundle-local alias.
///
/// The alias is display sugar for bounded output; [`EvidenceRef::source`]
/// remains the authority. Source identifiers are execution-local, so an alias
/// resolved against a different bundle is meaningless and must be rejected
/// rather than reinterpreted.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub side: Side,
    pub alias: SourceAlias,
    pub source: SourceRef,
}

impl EvidenceRef {
    /// Renders the `old:E31` form used in external assessments.
    #[must_use]
    pub fn qualified_alias(&self) -> String {
        format!("{}:{}", self.side.label(), self.alias)
    }
}

/// Three-valued completeness.
///
/// `Unknown` is not `Incomplete`: it records that the engine never established
/// the observation, which is a different obligation from a search that ran and
/// stopped short.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Incomplete,
    Unknown,
}

impl Completeness {
    /// Maps a recorded observation, preserving an absent observation as
    /// [`Completeness::Unknown`].
    #[must_use]
    pub const fn from_observation(observed: Option<bool>) -> Self {
        match observed {
            Some(true) => Self::Complete,
            Some(false) => Self::Incomplete,
            None => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn from_flag(complete: bool) -> Self {
        if complete {
            Self::Complete
        } else {
            Self::Incomplete
        }
    }

    /// True only for an established complete observation.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// The four independent completeness observations carried by every case.
///
/// They answer different questions and are never collapsed: evidence discovery,
/// candidate enumeration, solver search, and the response budget each fail on
/// their own. One returned hypothesis is not uniqueness, and a response that
/// omitted alternatives says nothing about whether enumeration finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseCompleteness {
    /// Discovery of the evidence this case depends on.
    pub evidence: Completeness,
    /// Enumeration of competing hypotheses before any solver ran.
    pub candidate_enumeration: Completeness,
    /// The correspondence solver's search over the enumerated candidates.
    pub solver_search: Completeness,
    /// Whether this response carries every field the case holds locally.
    pub response: Completeness,
}

impl CaseCompleteness {
    /// Every observation unknown; the planner narrows what it can establish.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            evidence: Completeness::Unknown,
            candidate_enumeration: Completeness::Unknown,
            solver_search: Completeness::Unknown,
            response: Completeness::Unknown,
        }
    }
}

/// Which comparison contract produced a case.
///
/// The two pipelines have different evidence, different selection channels, and
/// different completeness meanings. Cases record their origin so a reviewer is
/// never told that a native-glyph-only result examined visual or form evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineContract {
    /// The shared evidence pipeline over selected channels.
    SharedEvidence,
    /// The native-glyph text adapter with its own report contract.
    NativeText,
}

/// How the engine classified the material behind a case.
///
/// These classes are reported separately in human review and stay separate
/// here. A non-owning range comparison and an inferred correspondence are not
/// restated as strict results because an agent later agreed with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineClass {
    /// Conditional on an accepted correspondence, with exact source masks.
    Strict,
    /// A non-owning comparison of a corresponding range; it owns no sources.
    NonOwningRange,
    /// An inferred correspondence or an inferred, non-owning observation.
    Inferred,
    /// No engine comparison exists for this material at all.
    Unavailable,
}

/// The question a reviewer is being asked.
///
/// A case is a decision to make, not a rendered difference to approve. The
/// packet never asks whether a computed diff "looks correct"; it asks which
/// correspondence holds, and what changed under the selected one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewQuestion {
    /// Which counterpart, if any, corresponds to this material.
    ResolveCorrespondence,
    /// Whether content changed within an accepted correspondence.
    CompareContent,
    /// Whether a relationship between elements changed.
    CompareRelationship,
    /// What a region contains when no native text was acquired for it.
    InterpretVisualRegion,
    /// Whether a stored value and its appearance agree.
    CheckValueAppearance,
    /// What evidence is missing, and whether it can be acquired at all.
    AcquisitionGap,
}

/// Why material remains unresolved.
///
/// Reasons are attached where the engine records the obligation. Free-text
/// engine messages are retained verbatim in [`ReasonRecord::message`] but are
/// never parsed to derive a classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReviewReason {
    UnknownReadingOrder,
    InferredReadingOrder,
    ExtractionGap,
    NormalizationUncertainty,
    CompetingCorrespondence,
    AmbiguousEditLocation,
    SearchIncomplete,
    DomainNotClosed,
    SourceEvidenceMissing,
    WorkLimit,
    OutputLimit,
    /// Channel discovery never established what else exists.
    InventoryIncomplete,
    /// The channel's interpretation is not implemented.
    UnsupportedChannel,
    /// An acquisition backend failed for this material.
    BackendFailure,
    /// An explicit parser, extraction, or rendering limit stopped acquisition.
    ResourceLimit,
    /// Candidate generation stopped before the supplier universe was closed.
    CandidateEnumerationIncomplete,
    /// The correspondence needs a more specific child scope or group view.
    StructuralRefinementRequired,
    /// Relationship comparison did not resolve.
    RelationSearchIncomplete,
    /// Only a non-owning range comparison exists for this material.
    NonOwningRangeOnly,
    /// The correspondence behind the observation is inferred.
    InferredCorrespondence,
    /// A stored value was compared without checking its rendered appearance.
    ValueAppearanceUnverified,
    /// Pixels exist for the region but no native text was acquired.
    VisualOnlyRegion,
    /// Channel discovery found the reference, but no comparison reached it.
    DiscoveredButUnexamined,
    /// The export stopped before it could describe this material fully.
    ExportBudget,
    /// The engine recorded an obligation this vocabulary does not classify.
    /// The original message is retained so the reason is never discarded.
    Other,
}

impl From<AssessmentReason> for ReviewReason {
    /// Maps the native-text adapter's vocabulary onto the shared one.
    ///
    /// The match is exhaustive on purpose: a new engine reason must be
    /// classified here rather than silently collapsing into
    /// [`ReviewReason::Other`].
    fn from(reason: AssessmentReason) -> Self {
        match reason {
            AssessmentReason::UnknownReadingOrder => Self::UnknownReadingOrder,
            AssessmentReason::InferredReadingOrder => Self::InferredReadingOrder,
            AssessmentReason::ExtractionGap => Self::ExtractionGap,
            AssessmentReason::NormalizationUncertainty => Self::NormalizationUncertainty,
            AssessmentReason::CompetingCorrespondence => Self::CompetingCorrespondence,
            AssessmentReason::AmbiguousEditLocation => Self::AmbiguousEditLocation,
            AssessmentReason::SearchIncomplete => Self::SearchIncomplete,
            AssessmentReason::DomainNotClosed => Self::DomainNotClosed,
            AssessmentReason::SourceEvidenceMissing => Self::SourceEvidenceMissing,
            AssessmentReason::WorkLimit => Self::WorkLimit,
            AssessmentReason::OutputLimit => Self::OutputLimit,
        }
    }
}

impl From<EvidenceFailure> for ReviewReason {
    fn from(failure: EvidenceFailure) -> Self {
        match failure {
            EvidenceFailure::Unsupported => Self::UnsupportedChannel,
            EvidenceFailure::Unresolved => Self::SourceEvidenceMissing,
            EvidenceFailure::ResourceLimit => Self::ResourceLimit,
            EvidenceFailure::BackendFailure => Self::BackendFailure,
        }
    }
}

/// One classified obligation with its original message and locator.
///
/// `message` is untrusted when it quotes document-derived text; it is data for
/// the reviewer, never an instruction. An empty `sources` list means the
/// obligation could not be localized, not that no evidence is involved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasonRecord {
    pub reason: ReviewReason,
    /// The engine's own wording, retained for continuity with existing reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<EvidenceRef>,
}

impl ReasonRecord {
    /// A classified reason with no locator and no engine message.
    #[must_use]
    pub fn new(reason: ReviewReason) -> Self {
        Self {
            reason,
            message: None,
            channel: None,
            page: None,
            sources: Vec::new(),
        }
    }

    /// Retains the engine's message beside the classification.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    #[must_use]
    pub fn with_channel(mut self, channel: Channel) -> Self {
        self.channel = Some(channel);
        self
    }

    #[must_use]
    pub fn with_page(mut self, page: Option<PageId>) -> Self {
        self.page = page;
        self
    }

    #[must_use]
    pub fn with_sources(mut self, sources: Vec<EvidenceRef>) -> Self {
        self.sources = sources;
        self
    }
}

/// A model premise retained separately from source facts.
///
/// Assumptions travel with a case so that a reviewer can see which premises the
/// engine's own comparison already depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReviewAssumption {
    /// The retained block order is a supported reading order for this domain.
    InputReadingOrder,
    /// Comparison used reversible canonical normalization rather than raw text.
    CanonicalNormalization,
    /// Unmapped token identity depends on a supplied font-program identity.
    UnmappedFontIdentity,
    /// A layout view supplied a synthetic separator that owns no glyph.
    ReconstructedSpacing,
    /// The correspondence behind this case was accepted, not proved unique.
    AcceptedCorrespondence,
    /// The scope containing this case was itself established by inference.
    InferredParentScope,
    /// Page rasters were produced by a declared rendering profile, which is an
    /// observation of that renderer rather than of the document's content.
    RenderedObservation,
}

/// How much of a case the caller is asking for.
///
/// Retrieval order is not fixed: a case with no native text answers at
/// [`Detail::Text`] with a visual requirement and continues at
/// [`Detail::Visual`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    /// Case identity, question, location, and required evidence kinds only.
    Index,
    /// The question, both sides' retained text, reasons, and hypotheses.
    Text,
    /// Headings, neighbours, table labels, footnotes, and other occurrences.
    Context,
    /// The remaining competing hypotheses and their counterevidence.
    Alternatives,
    /// Locally rendered region images and their source bindings.
    Visual,
}

/// A retrieval the caller can perform next.
///
/// Actions are typed so a host never has to construct a command line from
/// document-derived text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RetrievalAction {
    /// List further cases from the index.
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<Cursor>,
    },
    /// Read one case at one detail level.
    Show {
        case: CaseId,
        detail: Detail,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<Cursor>,
    },
    /// Produce local images for one case.
    Render { case: CaseId },
}

/// An opaque continuation token.
///
/// A cursor is bound to one bundle identity and one query shape. Replaying it
/// against another bundle or another query is rejected rather than silently
/// reinterpreted, because record ordering differs between them.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(String);

impl Cursor {
    /// Accepts a token restricted to `[A-Za-z0-9._:-]`.
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        if value.len() > MAX_CURSOR_BYTES {
            return Err(IdentifierError::TooLong { bytes: value.len() });
        }
        if value.is_empty() {
            return Err(IdentifierError::Empty);
        }
        for (position, byte) in value.bytes().enumerate() {
            if !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')) {
                return Err(IdentifierError::UnsupportedCharacter { position });
            }
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Maximum length of a continuation token.
pub const MAX_CURSOR_BYTES: usize = 128;

/// Half-open interval of Unicode scalars.
///
/// Positions count Unicode scalar values in the referenced review text. They are
/// not UTF-8 byte offsets and not grapheme clusters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScalarInterval {
    pub start: usize,
    pub end: usize,
}

impl ScalarInterval {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

impl From<ScalarRange> for ScalarInterval {
    fn from(range: ScalarRange) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

/// Half-open interval of comparable tokens.
///
/// Token positions are the comparison pipeline's own unit and do not coincide
/// with scalar positions wherever normalization folds or expands material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TokenInterval {
    pub start: usize,
    pub end: usize,
}

impl From<crate::diff::TokenRange> for TokenInterval {
    fn from(range: crate::diff::TokenRange) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

/// Immutable identity of the comparison a bundle was exported from.
///
/// Every field is either an input fact or an execution contract. Wall-clock
/// time, durations, and host paths are deliberately absent so that two runs of
/// the same build over the same inputs produce the same identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleIdentity {
    pub policy_version: u32,
    pub schema: String,
    /// Lowercase hex SHA-256 of the exact acquired bytes.
    pub old_sha256: String,
    pub new_sha256: String,
    pub old_bytes: usize,
    pub new_bytes: usize,
    /// Canonical rendering of the comparison options that select evidence.
    pub options: String,
    /// Evidence snapshot revisions, one per side.
    pub old_revision: String,
    pub new_revision: String,
    /// Backend name, version, and profile triples in retained order.
    pub backends: Vec<String>,
    pub pipeline: PipelineContract,
    pub selected_channels: BTreeSet<Channel>,
}

impl BundleIdentity {
    /// Lowercase hex SHA-256 over a canonical encoding of this identity.
    ///
    /// The encoding is length-prefixed per field so that no two distinct
    /// identities can produce the same byte string by shifting a delimiter.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        let mut field = |bytes: &[u8]| {
            hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(bytes);
        };
        field(self.schema.as_bytes());
        field(&self.policy_version.to_be_bytes());
        field(self.old_sha256.as_bytes());
        field(self.new_sha256.as_bytes());
        field(
            &u64::try_from(self.old_bytes)
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        field(
            &u64::try_from(self.new_bytes)
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        field(self.options.as_bytes());
        field(self.old_revision.as_bytes());
        field(self.new_revision.as_bytes());
        for backend in &self.backends {
            field(backend.as_bytes());
        }
        field(match self.pipeline {
            PipelineContract::SharedEvidence => b"shared_evidence",
            PipelineContract::NativeText => b"native_text",
        });
        for channel in &self.selected_channels {
            field(format!("{channel:?}").as_bytes());
        }
        crate::review::lowercase_hex(&hasher.finalize())
    }

    /// The bundle identifier derived from [`BundleIdentity::digest`].
    ///
    /// # Panics
    /// Never: the derived text is hex and within the identifier limit.
    #[must_use]
    pub fn bundle_id(&self) -> BundleId {
        let digest = self.digest();
        BundleId::new(format!("b{}", &digest[..32]))
            .expect("a hex digest prefix is a valid identifier")
    }
}

/// A source reference paired with the side it belongs to, before aliasing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SidedSource {
    pub side: Side,
    pub source: SourceRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> BundleIdentity {
        BundleIdentity {
            policy_version: POLICY_VERSION,
            schema: REVIEW_SCHEMA.into(),
            old_sha256: "00".repeat(32),
            new_sha256: "11".repeat(32),
            old_bytes: 10,
            new_bytes: 11,
            options: "channels=text".into(),
            old_revision: "rev-a".into(),
            new_revision: "rev-b".into(),
            backends: vec!["native/1.0/profile".into()],
            pipeline: PipelineContract::SharedEvidence,
            selected_channels: BTreeSet::from([Channel::Text]),
        }
    }

    #[test]
    fn identifiers_reject_shell_and_control_characters() {
        assert!(CaseId::new("R17").is_ok());
        assert!(CaseId::new("").is_err());
        assert!(CaseId::new("R 17").is_err());
        assert!(CaseId::new("R17;rm").is_err());
        assert!(CaseId::new("R\u{1b}[0m").is_err());
        assert!(CaseId::new("R".repeat(MAX_IDENTIFIER_BYTES + 1)).is_err());
    }

    #[test]
    fn bundle_identity_is_stable_and_field_separated() {
        let first = identity();
        assert_eq!(first.digest(), identity().digest());

        // Moving a character across a field boundary must change the digest.
        let mut shifted = identity();
        shifted.old_revision = "rev-ar".into();
        shifted.new_revision = "ev-b".into();
        assert_ne!(first.digest(), shifted.digest());

        let mut other_policy = identity();
        other_policy.policy_version = POLICY_VERSION + 1;
        assert_ne!(first.digest(), other_policy.digest());
    }

    #[test]
    fn unknown_completeness_is_not_incomplete() {
        assert_eq!(Completeness::from_observation(None), Completeness::Unknown);
        assert_eq!(
            Completeness::from_observation(Some(false)),
            Completeness::Incomplete
        );
        assert!(!Completeness::Unknown.is_complete());
        assert!(!Completeness::Incomplete.is_complete());
    }

    #[test]
    fn unclassified_engine_reasons_are_retained_as_other() {
        let record = ReasonRecord::new(ReviewReason::Other)
            .with_message("counterpart table refinement search is incomplete");
        assert_eq!(record.reason, ReviewReason::Other);
        assert_eq!(
            record.message.as_deref(),
            Some("counterpart table refinement search is incomplete")
        );
    }

    #[test]
    fn evidence_alias_keeps_its_side() {
        let reference = EvidenceRef {
            side: Side::Old,
            alias: SourceAlias::new("E31").expect("alias"),
            source: SourceRef::Native {
                glyph: crate::model::GlyphId(31),
            },
        };
        assert_eq!(reference.qualified_alias(), "old:E31");
    }
}
