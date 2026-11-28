//! Serialized text-scope evidence shapes retained for report compatibility.
//!
//! The text-scope algorithms that filled these fields were part of the removed
//! removed proof engine. The types stay here so existing reports keep their exact
//! serde field names, order, defaults, and skip rules. New documents leave the
//! collections empty; readers must not infer that an empty collection was a
//! completed text-scope search.

use serde::{Deserialize, Serialize};

use super::{LocalViewComparison, NodeId, SourceRef};
use crate::model::PageId;

/// Content of corresponding intervals under the stated comparison convention.
/// Inferred paragraph groups have no certified interval boundaries. Otherwise,
/// the enclosing scope retains the parent correspondence. Boundary indexes
/// refer to its candidate proposals; member nodes retain all source projections.
/// Neither this unit nor its conditional masks owns changed sources or discharges
/// strict coverage. An inferred parent keeps this comparison inferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopeReview {
    pub convention: String,
    /// Whole-node or contiguous-group proposal indexes. Every member of each
    /// boundary group is excluded from the interior. Source-cut ranges carry
    /// their two explicit certificates in `source_cuts` instead of inventing proposal IDs.
    /// Inferred paragraph groups leave this empty and certify no boundaries.
    pub boundaries: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cuts: Option<SourceCutRange>,
    /// Presence in this corresponding interval only. This does not establish
    /// document-wide novelty, deletion or the absence of an external copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<TextScopePresence>,
    /// Explicit tag order and individually closed page regions, when the
    /// enclosing interval crosses pages. Sources remain non-owning references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_regions: Option<NativeRegionChains>,
    /// Complete interior evidence, including unchanged context. These lists are
    /// range locators, never changed masks or exclusive source ownership.
    pub old_sources: Vec<SourceRef>,
    pub new_sources: Vec<SourceRef>,
    pub old_boundaries: [Vec<SourceRef>; 2],
    pub new_boundaries: [Vec<SourceRef>; 2],
    /// Search status of the enclosing supplier universe, including optional
    /// interior correspondence hypotheses. It does not replace the accepted
    /// source-boundary decisions. Older reports leave this observation unknown.
    #[serde(default)]
    pub candidate_search_exhaustive: Option<bool>,
    /// Space provenance in the concatenated review text. Layout-derived entries
    /// retain both spacing interpretations; they are not literal space glyphs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<TextScopeSpacing>,
    pub comparison: LocalViewComparison,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopeSpacing {
    pub convention: String,
    pub old: Vec<SpaceBoundary>,
    pub new: Vec<SpaceBoundary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextScopePresence {
    pub convention: String,
    pub present: super::PresenceSide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceBoundary {
    pub position: usize,
    pub origin: SpaceOrigin,
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceOrigin {
    LiteralGlyph,
    ReconstructedGap,
    LineSeparator,
    PageSeparator,
    Ambiguous,
}

/// A token boundary in an existing retained source view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCut {
    pub node: NodeId,
    pub token_boundary: usize,
}

/// Half-open token interval and its physical source projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFragment {
    pub node: NodeId,
    pub tokens: [usize; 2],
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CutEvidence {
    AcceptedBoundary {
        proposal: usize,
    },
    UniqueNativeFragment {
        old: SourceFragment,
        new: SourceFragment,
    },
    /// A mandatory literal-space edge inside the declared enclosing cuts.
    CorrespondingContentEdge,
}

/// Equality and occurrence closure are local to the declared source universe;
/// this convention does not identify a semantic paragraph or author intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutCorrespondence {
    pub old: SourceCut,
    pub new: SourceCut,
    pub evidence: CutEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutRange {
    pub convention: String,
    pub projection: String,
    pub population: SourceCutPopulation,
    pub entry: CutCorrespondence,
    pub exit: CutCorrespondence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_refinement: Option<Box<SourceCutEdgeRefinement>>,
}

/// A second, non-owning comparison of the content between literal edge spaces.
/// The original interval remains reported, including any space-only change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutEdgeRefinement {
    pub convention: String,
    pub enclosing: [CutCorrespondence; 2],
    pub old_padding: [Vec<SourceFragment>; 2],
    pub new_padding: [Vec<SourceFragment>; 2],
}

/// Only content outside the first or last agreed page boundary is eligible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCutPageEdge {
    Before,
    After,
}

/// The finite universe in which cut occurrences were counted. An interval's
/// correspondence remains conditional on its accepted enclosing boundaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceCutPopulation {
    CompletePage,
    /// Both pages have complete, disjoint native source paths. Their pairing
    /// remains conditional on this accepted text boundary, not page numbers.
    AnchoredPage {
        boundary: usize,
        /// Multiple local anchors permit only the outer side of this boundary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edge: Option<SourceCutPageEdge>,
        old_page: PageId,
        new_page: PageId,
        old: Vec<NodeId>,
        new: Vec<NodeId>,
        /// Other accepted boundaries stay in the occurrence census but cannot
        /// supply cuts or compared content. This includes counterparts outside
        /// the page pair and local anchors excluded by an outer-edge view.
        external_boundaries: Vec<usize>,
    },
    MatchedInterval {
        boundaries: [usize; 2],
        old: Vec<NodeId>,
        new: Vec<NodeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_regions: Option<Box<NativeRegionChains>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boundary_padding: Option<Box<SourceCutBoundaryPadding>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_order: Option<Box<SourceCutRowOrder>>,
    },
}

/// Monoline outer endpoints of a horizontal source interval. Spatial prefix
/// order uses exact baselines; raw paint order permits an eight-machine-epsilon
/// relative envelope for row classification without rounding any coordinates.
/// The convention selects the order of disjoint row pieces. Only glyphs
/// outside the endpoint cuts in that declared order may leave the census;
/// the intervening source and paint band must still close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutRowOrder {
    pub convention: String,
    pub old: Option<[SourceCutRowEndpoint; 2]>,
    pub new: Option<[SourceCutRowEndpoint; 2]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutRowEndpoint {
    pub node: NodeId,
    pub sources: Vec<SourceRef>,
}

/// Partially clipped outer padding participates in the occurrence census with
/// both presence choices, but cannot be consumed by any compared source range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutBoundaryPadding {
    pub convention: String,
    pub old: Vec<SourceRef>,
    pub new: Vec<SourceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCutSearch {
    pub convention: String,
    pub exhaustive: bool,
    pub examined_fragments: usize,
    pub paired_boundaries: usize,
    pub work: usize,
    pub budget_at_entry: usize,
}

/// A declared native tag sequence can order independently closed page regions.
/// Page adjacency and render order alone never establish these transitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRegion {
    pub page: PageId,
    pub nodes: Vec<NodeId>,
    pub bounded_paint: bool,
    /// A page-local row census may order disjoint same-baseline boundary nodes.
    /// The native membership must still bind the complete inter-page sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_order: Option<String>,
}

/// The two adjacent glyphs have consecutive positions in the retained native
/// structure membership. The full path is checked against that sequence too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTransition {
    pub before: SourceRef,
    pub after: SourceRef,
    pub structure: SourceRef,
    pub position: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRegionChain {
    pub convention: String,
    pub regions: Vec<NativeRegion>,
    pub transitions: Vec<NativeTransition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRegionChains {
    pub old: Option<NativeRegionChain>,
    pub new: Option<NativeRegionChain>,
}

/// Equality of owned native source fragments under the enclosing scope premise.
///
/// Each entire parent passes local native inventory, order, paint and rival-view
/// closure. Only the source-backed unpadded interior is owned. Complements remain
/// explicit obligations; this proves neither paragraph identity nor page inventory.
/// It is independent of non-owning range reviews and supplies no relationship map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTextDomainEquality {
    proposal: usize,
    old: SourceFragment,
    new: SourceFragment,
    old_complement: Vec<SourceRef>,
    new_complement: Vec<SourceRef>,
    /// Source-side row conventions are descriptive; deserialization does not
    /// restore their validation state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_row_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new_row_order: Option<String>,
    /// Reports describe this validation result but cannot recreate it in
    /// process.
    #[serde(skip)]
    verified: bool,
}

impl NativeTextDomainEquality {
    pub(crate) fn sources(&self, old: bool) -> &[SourceRef] {
        if !self.verified {
            return &[];
        }
        if old {
            &self.old.sources
        } else {
            &self.new.sources
        }
    }
}

/// A source-conserving native interval under two admitted boundary premises.
/// Boundary sources remain outside the owned interior. This certificate does not
/// discharge paragraph identity, relationship search, or global inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeTextIntervalComparison {
    boundaries: [usize; 2],
    old_sources: Vec<SourceRef>,
    new_sources: Vec<SourceRef>,
    comparison: LocalViewComparison,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cut_partition: Option<CutPartition>,
    #[serde(skip)]
    verified: bool,
}

impl NativeTextIntervalComparison {
    /// Exact local result under the retained boundary premises.
    #[must_use]
    pub fn comparison(&self) -> &LocalViewComparison {
        &self.comparison
    }

    pub(crate) fn sources(&self, old: bool) -> &[SourceRef] {
        if !self.verified {
            &[]
        } else if old {
            &self.old_sources
        } else {
            &self.new_sources
        }
    }
}

/// Serialized cut partition retained as part of an interval's report shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CutPartition {
    range: SourceCutRange,
    old_remainder: Vec<Remainder>,
    new_remainder: Vec<Remainder>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Remainder {
    parent: NodeId,
    sources: Vec<SourceRef>,
}
