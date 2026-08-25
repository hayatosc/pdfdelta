mod anchor;
mod candidate;
mod features;
mod ordered;
mod score;

pub use anchor::{
    AnchorIntervalWindow, DEFAULT_ANCHOR_MIN_TOKENS, ExactAnchor, MonotoneAnchorChain,
    exact_anchors, partition_anchor_windows, select_monotone_anchor_chain,
};
pub use candidate::{
    Candidate, CandidateGenerator, CandidateSource, CandidateVisitBreakdown,
    CandidateVisitEstimate, ExhaustiveCandidateGenerator, InvertedIndexCandidateGenerator,
    MinHashLshCandidateGenerator, MinHashLshOptions,
};
pub(crate) use features::validate_ngram_size;
pub use features::{
    BlockFeatures, ExactHash, NGram, NGramSet, build_block_features, dice_similarity,
    estimate_ngram_token_elements,
};
pub(crate) use ordered::align_ordered_with_metrics;
pub(crate) use ordered::validate_alignment_options;
pub use ordered::{
    Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentOptions,
    AlignmentSpan, align_ordered,
};
pub use score::BlockSeparator;
