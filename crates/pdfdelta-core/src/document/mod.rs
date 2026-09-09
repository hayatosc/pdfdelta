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
pub(crate) mod footers;
mod form_readings;
mod forms;
mod graph;
mod groups;
mod matching;
mod native;
mod operations;
mod providers;
mod recognition;
mod relations;
mod ruled_tables;
mod structure_candidates;
mod structures;
mod tables;
mod text_candidates;
mod visual;
mod widgets;

pub use comparison::*;
pub use counterpart_tables::*;
pub use coverage::*;
pub use evidence::*;
pub use form_readings::*;
pub use forms::*;
pub use graph::*;
pub use matching::*;
pub use operations::*;
pub use relations::RelationComparison;
pub use structures::*;
pub use text_candidates::{TextCandidateLimits, TextCandidateSearch};
pub use visual::{VisualCandidateLimits, VisualCandidateSearch};
