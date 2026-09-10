//! Source-side normalization families, never selected by the opposing text.

use crate::normalize::{BlockText, ComparableToken};

use super::{SourceRef, TextNormalization, TextView};

/// An immutable source-checked projection. Its private payload prevents a bare
/// optional-position list from authorizing character claims. Certificates are
/// deliberately not deserialized from reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizationCertificate {
    tokens: Vec<ComparableToken>,
    origins: Vec<Vec<SourceRef>>,
    source_backed: Vec<bool>,
    optional_positions: Vec<usize>,
}

impl TextView {
    pub(super) fn certify_normalization(&mut self, block: &BlockText, remaining: &mut usize) {
        if block.issues.is_empty() {
            return;
        }
        let work = block
            .raw
            .text
            .len()
            .saturating_add(block.canonical.text.len())
            .saturating_add(block.raw.source_map.len())
            .saturating_add(block.canonical.source_map.len())
            .saturating_add(block.normalization_events.len())
            .saturating_add(block.canonical.unmapped.len())
            .saturating_mul(block.issues.len().saturating_add(1))
            .saturating_add(self.tokens.len())
            .saturating_add(self.origins.iter().map(Vec::len).sum::<usize>());
        let Some(next) = remaining.checked_sub(work) else {
            self.normalization = TextNormalization::Unresolved {
                reason: "source normalization validation exhausted its work budget".into(),
            };
            *remaining = 0;
            return;
        };
        *remaining = next;
        let Ok(Some(optional_positions)) =
            block.checked_optional_hyphens(&self.tokens, 0..self.tokens.len())
        else {
            return;
        };
        self.bind_optional_positions(optional_positions);
    }

    /// Call only with a source-checked family or a concatenation of families
    /// whose complete projections have already been validated.
    pub(super) fn bind_optional_positions(&mut self, optional_positions: Vec<usize>) {
        self.normalization = TextNormalization::Alternatives {
            certificate: Some(NormalizationCertificate {
                tokens: self.tokens.clone(),
                origins: self.origins.clone(),
                source_backed: self.source_backed.clone(),
                optional_positions: optional_positions.clone(),
            }),
            optional_positions,
        };
    }

    pub(super) fn has_validated_normalization(&self) -> bool {
        match &self.normalization {
            TextNormalization::Exact => true,
            TextNormalization::Alternatives {
                optional_positions,
                certificate: Some(proof),
            } if proof.tokens == self.tokens
                && proof.origins == self.origins
                && proof.source_backed == self.source_backed
                && proof.optional_positions == *optional_positions =>
            {
                true
            }
            _ => false,
        }
    }

    pub(super) fn optional_tokens(&self) -> Option<Vec<bool>> {
        if !self.has_validated_normalization() {
            return None;
        }
        let mut optional = vec![false; self.tokens.len()];
        if let TextNormalization::Alternatives {
            optional_positions, ..
        } = &self.normalization
        {
            for position in optional_positions {
                *optional.get_mut(*position)? = true;
            }
        }
        Some(optional)
    }
}
