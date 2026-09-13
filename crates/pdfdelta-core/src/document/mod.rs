//! Evidence-backed document comparison across native, visual, and structured sources.
//!
//! Text-only glyph comparisons are one adapter, not a claim that every selected
//! channel of a PDF has been examined. Interpretation and correspondence remain
//! distinct from source facts and from conditional exact character masks.

mod comparison;
mod counterpart_tables;
mod coverage;
mod dependencies;
mod evidence;
mod extraction_dependencies;
pub(crate) mod footers;
mod form_readings;
mod forms;
mod graph;
mod groups;
mod key_presence;
mod matching;
mod native;
mod normalization;
mod operations;
mod providers;
mod recognition;
mod relations;
mod ruled_tables;
mod scoped_keys;
mod structure_candidates;
mod structures;
mod tables;
mod text_candidates;
mod text_scopes;
mod visual;
mod widgets;

pub use comparison::*;
pub use counterpart_tables::*;
pub use coverage::*;
pub use evidence::*;
pub use extraction_dependencies::{ExtractionDependency, ExtractionDependencyLimits};
pub use form_readings::*;
pub use forms::*;
pub use graph::*;
pub use key_presence::*;
pub use matching::*;
pub use normalization::NormalizationCertificate;
pub use operations::*;
pub use relations::RelationComparison;
pub use scoped_keys::*;
pub use structures::*;
pub use text_candidates::{TextCandidateLimits, TextCandidateSearch};
pub use text_scopes::{
    CutCorrespondence, CutEvidence, NativeRegion, NativeRegionChain, NativeRegionChains,
    NativeTransition, SourceCut, SourceCutBoundaryPadding, SourceCutEdgeRefinement,
    SourceCutPopulation, SourceCutRange, SourceCutRowEndpoint, SourceCutRowOrder, SourceCutSearch,
    SourceFragment, SpaceBoundary, SpaceOrigin, TextScopePresence, TextScopeReview,
    TextScopeSpacing,
};
pub use visual::{VisualCandidateLimits, VisualCandidateSearch};
