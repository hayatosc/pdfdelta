mod candidate;
mod features;
mod ordered;
mod score;

pub use candidate::{
    Candidate, CandidateGenerator, CandidateSource, ExhaustiveCandidateGenerator,
    InvertedIndexCandidateGenerator,
};
pub use features::{
    BlockFeatures, DEFAULT_ANCHOR_MIN_TOKENS, ExactAnchor, ExactHash, NGram, NGramSet,
    build_block_features, dice_similarity, exact_anchors,
};
pub(crate) use features::{estimate_ngram_token_elements, validate_ngram_size};
pub(crate) use ordered::validate_alignment_options;
pub use ordered::{
    Alignment, AlignmentConfidence, AlignmentEvidence, AlignmentKind, AlignmentOptions,
    AlignmentSpan, align_ordered,
};
pub use score::BlockSeparator;
