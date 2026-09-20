//! One review case: a decision a reviewer can make, with the minimum evidence
//! the decision needs and an explicit account of what was left out.
//!
//! A case is a non-owning view. Two cases may quote the same material as
//! context; that overlap is declared through [`ReviewCase::related_cases`] and
//! [`ReviewCase::conflicts_with`] so the same source is never counted as
//! resolved twice.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::contract::{
    CaseCompleteness, CaseId, Cursor, EngineClass, EvidenceRef, HypothesisId, PipelineContract,
    ReasonRecord, RetrievalAction, ReviewAssumption, ReviewQuestion, ScalarInterval, Side,
    TokenInterval,
};
use crate::{
    document::{Channel, NodeKind},
    model::PageId,
};

/// How the scalars of a review text were ordered.
///
/// Painting order is what the PDF instructs; a reading order is an
/// interpretation of it. When neither is established the scalars are not
/// concatenated into a single asserted sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOrder {
    /// The order in which the content stream paints the material.
    RenderOrder,
    /// The order the document's own structure tree declares. This is the
    /// document's assertion about reading order, not a verified property of
    /// the painted material.
    DeclaredStructureOrder,
    /// A layout interpretation of the painting order.
    InferredReadingOrder,
    /// No order was established; the runs are reported separately.
    Unknown,
}

/// A token whose character identity is unresolved.
///
/// Unmapped material is never replaced by a substitute character, because a
/// substitute would silently assert a reading that the font evidence does not
/// support. Its position is a scalar position in the accompanying text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmappedMark {
    /// Scalar position at which the unmapped token sits.
    pub position: usize,
    /// Raw code points from the content stream, when the extractor retained them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_codes: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<EvidenceRef>,
}

/// Why a run of text is absent from a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    /// The run was independently confirmed equal on both sides.
    ConfirmedEqual,
    /// The response budget could not carry the run.
    ResponseBudget,
    /// The export budget stopped before the run was written to the bundle.
    ExportBudget,
    /// The caller asked for a detail level that excludes the run.
    DetailLevel,
}

/// A run that is not in the packet, with its length and the way to get it.
///
/// Folding is only permitted over runs that were independently confirmed equal;
/// an omitted run always keeps its interval, its length, and a retrieval action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedRun {
    pub range: ScalarInterval,
    pub scalars: usize,
    pub reason: OmissionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand_with: Option<RetrievalAction>,
}

/// Retained text for one side of a case.
///
/// The text is evidence quoted for review, not a normalized rendering: digits,
/// negations, units, dates, and spacing are preserved as the engine retained
/// them. Positions count Unicode scalars.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewText {
    pub side: Side,
    /// Retained scalars. Unmapped tokens contribute no scalar here and appear
    /// in [`ReviewText::unmapped`] instead.
    pub text: String,
    /// Total scalars the case covers, including omitted runs.
    pub scalar_count: usize,
    pub order: TextOrder,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmapped: Vec<UnmappedMark>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<OmittedRun>,
    /// Comparable-token interval this text projects onto, when the pipeline
    /// established one. Token positions are not scalar positions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_range: Option<TokenInterval>,
    /// Canonical scalar interval inside the owning block, when established.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_range: Option<ScalarInterval>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<EvidenceRef>,
}

impl ReviewText {
    /// Empty retained text for a side with no acquired characters.
    ///
    /// An empty text is not a claim that the side is blank; the case's reasons
    /// and completeness record why nothing was acquired.
    #[must_use]
    pub fn empty(side: Side, order: TextOrder) -> Self {
        Self {
            side,
            text: String::new(),
            scalar_count: 0,
            order,
            unmapped: Vec::new(),
            omitted: Vec::new(),
            token_range: None,
            canonical_range: None,
            sources: Vec::new(),
        }
    }
}

/// A structural label that locates material for a reader.
///
/// Labels are quoted document text and are therefore untrusted data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureLabel {
    pub kind: NodeKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<EvidenceRef>,
}

/// Where a case sits in one document.
///
/// Page numbers are reported in both bases because the two are routinely
/// confused: `page_number` is what a reader sees, `page_index` is the engine's
/// own zero-based identifier. Absent fields mean the location is unknown, never
/// that the material is at the start of the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideLocation {
    pub side: Side,
    /// One-based page number for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<u32>,
    /// Zero-based engine page identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_index: Option<PageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<NodeKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<StructureLabel>,
}

impl SideLocation {
    /// A location with only its side established.
    #[must_use]
    pub fn unknown(side: Side) -> Self {
        Self {
            side,
            page_number: None,
            page_index: None,
            view: None,
            labels: Vec::new(),
        }
    }

    /// Records a page in both bases from the engine identifier.
    #[must_use]
    pub fn with_page(mut self, page: Option<PageId>) -> Self {
        self.page_index = page;
        self.page_number = page.map(|page| page.0.saturating_add(1));
        self
    }

    /// Records which kind of view the material belongs to.
    #[must_use]
    pub fn view(mut self, kind: NodeKind) -> Self {
        self.view = Some(kind);
        self
    }
}

/// How many elements each side of a hypothesis carries.
///
/// A missing counterpart is local to this case's scope: `OldToNone` states that
/// no counterpart was found inside the examined range, never that the material
/// is absent from the whole document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
    OldToNone,
    NoneToNew,
}

impl Cardinality {
    /// Classifies a hypothesis from its member counts.
    #[must_use]
    pub const fn from_counts(old: usize, new: usize) -> Self {
        match (old, new) {
            (0, _) => Self::NoneToNew,
            (_, 0) => Self::OldToNone,
            (1, 1) => Self::OneToOne,
            (1, _) => Self::OneToMany,
            (_, 1) => Self::ManyToOne,
            _ => Self::ManyToMany,
        }
    }
}

/// One competing answer to a case's question.
///
/// A hypothesis is never the only admissible answer: "none of these" and
/// "insufficient evidence" remain available for every case, so a single
/// returned hypothesis does not make a correspondence unique.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: HypothesisId,
    pub cardinality: Cardinality,
    /// The search supplier that proposed this hypothesis.
    pub supplier: String,
    /// The solver's declared objective weight.
    ///
    /// This is a search objective, not a probability and not a confidence. It
    /// orders candidates under one stated objective and nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_weight: Option<u32>,
    /// Whether the solver found this hypothesis in every optimum it examined.
    /// Mandatory membership under a truncated search is not uniqueness.
    pub mandatory_in_examined_optima: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<SideLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<SideLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<ReviewText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<ReviewText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
    /// Hypotheses in this case that cannot hold together with this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<HypothesisId>,
    /// Facts that argue against this hypothesis.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counterevidence: Vec<ReasonRecord>,
}

/// What kind of evidence a case still needs before it can be decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredEvidence {
    /// Retained native text is sufficient to answer the question.
    NativeText,
    /// Surrounding structure is needed: headings, labels, neighbours.
    Context,
    /// The remaining competing hypotheses are needed.
    Alternatives,
    /// A rendered region is needed because no native text exists for it.
    Visual,
    /// No further retrieval can help; the evidence does not exist locally.
    Unavailable,
}

/// What the engine established about this case's material before it stopped.
///
/// This is the engine's own finding, carried so a reviewer can tell apart a
/// range where a difference is already proved and only its position is open,
/// from material that was never compared at all. It is always read together
/// with the case's [`EngineClass`]: a difference established under an inferred
/// correspondence is still inferred, and stays so whatever a reviewer decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseFinding {
    /// A difference was established for this material, even where its exact
    /// position within the range remains unresolved.
    DifferenceEstablished,
    /// The material was compared and found equal within the examined range.
    /// This says nothing about the rest of the document.
    EqualityEstablished,
    /// Nothing was established: the material was never compared, or the
    /// comparison could not conclude.
    NotEstablished,
}

impl CaseFinding {
    /// Listing rank, so what the engine already settled is offered first.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::DifferenceEstablished => 0,
            Self::NotEstablished => 1,
            Self::EqualityEstablished => 2,
        }
    }
}

/// A comparable-token interval one case accounts for.
///
/// The native-glyph contract partitions each side's comparable tokens
/// exclusively, so a case is accounted for by the intervals it covers. The
/// shared-evidence contract expresses the same accounting through
/// [`ReviewCase::evidence`] source references instead, and leaves this empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextCoverage {
    pub side: Side,
    /// Block identifier inside the side's normalization.
    pub block: u64,
    pub token_range: TokenInterval,
}

/// Where one case's material sits on a page, in PDF user space.
///
/// Bounds are the union of the retained geometry of the case's sources on that
/// page. `None` means geometry was not retained, which is different from a
/// zero-size region: no crop can be derived from it, and a reader is given the
/// whole page instead of a box that was guessed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaseRegion {
    pub side: Side,
    pub page_number: u32,
    pub page_index: PageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[f64; 4]>,
}

/// One review case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewCase {
    pub case_id: CaseId,
    /// Content digest over the canonical case key, for tamper and drift checks.
    pub content_digest: String,
    pub question: ReviewQuestion,
    pub pipeline: PipelineContract,
    pub engine_class: EngineClass,
    /// What the engine established here before it stopped.
    pub finding: CaseFinding,
    pub channels: BTreeSet<Channel>,
    pub completeness: CaseCompleteness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<ReasonRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<ReviewAssumption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<SideLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<SideLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<ReviewText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<ReviewText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hypotheses: Vec<Hypothesis>,
    /// Total competing hypotheses, when enumeration finished.
    ///
    /// `None` means the total is unknown because enumeration did not close, not
    /// that the returned hypotheses are all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternatives_total: Option<usize>,
    pub alternatives_returned: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<OmittedRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_evidence: Vec<RequiredEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_cases: Vec<CaseId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<CaseId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_actions: Vec<RetrievalAction>,
    /// Every source this case quotes, on either side.
    ///
    /// Membership here is a locator, never ownership: the same source may be
    /// quoted by another case as context.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
    /// Token intervals this case accounts for under the native-glyph contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered: Vec<TextCoverage>,
    /// Pages and bounds a rendered view of this case would cover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<CaseRegion>,
}

impl ReviewCase {
    /// Sources this case quotes on one side, in retained order.
    pub fn sources(&self, side: Side) -> impl Iterator<Item = &EvidenceRef> {
        self.evidence
            .iter()
            .filter(move |reference| reference.side == side)
    }

    /// Whether the case can be decided from the evidence it already carries.
    ///
    /// A case that still requires retrieval is not undecidable; it is
    /// incomplete at the current detail level.
    #[must_use]
    pub fn needs_retrieval(&self) -> bool {
        self.required_evidence
            .iter()
            .any(|required| !matches!(required, RequiredEvidence::Unavailable))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinality_distinguishes_missing_counterparts_from_groups() {
        assert_eq!(Cardinality::from_counts(1, 1), Cardinality::OneToOne);
        assert_eq!(Cardinality::from_counts(1, 3), Cardinality::OneToMany);
        assert_eq!(Cardinality::from_counts(3, 1), Cardinality::ManyToOne);
        assert_eq!(Cardinality::from_counts(2, 2), Cardinality::ManyToMany);
        assert_eq!(Cardinality::from_counts(1, 0), Cardinality::OldToNone);
        assert_eq!(Cardinality::from_counts(0, 1), Cardinality::NoneToNew);
    }

    #[test]
    fn side_location_reports_both_page_bases() {
        let location = SideLocation::unknown(Side::New).with_page(Some(PageId(0)));
        assert_eq!(location.page_index, Some(PageId(0)));
        assert_eq!(location.page_number, Some(1));

        let unknown = SideLocation::unknown(Side::New).with_page(None);
        assert_eq!(unknown.page_number, None);
    }

    #[test]
    fn empty_text_preserves_its_declared_order() {
        let text = ReviewText::empty(Side::Old, TextOrder::Unknown);
        assert_eq!(text.scalar_count, 0);
        assert_eq!(text.order, TextOrder::Unknown);
        assert!(text.text.is_empty());
    }
}
