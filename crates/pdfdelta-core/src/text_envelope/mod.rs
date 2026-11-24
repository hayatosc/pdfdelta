//! Bounded literal-token claims and joint alignment envelopes.
//!
//! A caller supplies two token sequences and masks that are already established
//! by its own evidence. Results describe only the alignment policy named by the
//! inputs; they never establish document correspondence, source ownership, or
//! extraction completeness.
//!
//! The exported kernel is deliberately small and self-contained. It keeps only
//! bounded DP state and returns explicit refusals rather than an empty or
//! partial claim.

pub(crate) mod claims;
mod document_claims;
mod envelope;
#[cfg(test)]
mod envelope_tests;
pub(crate) mod hypotheses;

pub use document_claims::{LocalTextClaims, LocalTextSide, local_text_claims, local_text_envelope};
pub use envelope::{
    AlignmentDagRow, EnvelopeUnavailable, LocalTextEnvelope, LocalTextEnvelopeLimits,
    LocalTextEnvelopeResult, MAX_ENVELOPE_CELLS, OptimalAlignmentDag, TokenAlternatives,
};

/// Enumerates every binary word up to `max_length`, shortest first.
///
/// Test-only oracle shared by the envelope tests; it is not a production
/// vocabulary or a bound on caller tokens.
#[cfg(test)]
fn all_words(max_length: usize) -> Vec<Vec<u8>> {
    fn append(words: &mut Vec<Vec<u8>>, current: &mut Vec<u8>, remaining: usize) {
        if remaining == 0 {
            words.push(current.clone());
            return;
        }
        for token in 0..=1 {
            current.push(token);
            append(words, current, remaining - 1);
            current.pop();
        }
    }
    let mut words = Vec::new();
    for length in 0..=max_length {
        append(&mut words, &mut Vec::new(), length);
    }
    words
}
