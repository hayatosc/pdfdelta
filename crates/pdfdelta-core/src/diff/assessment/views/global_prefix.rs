use super::{LocalDomain, View, anchors::GlobalSeedWitness, charge};
use crate::{
    Result,
    diff::{SentenceRecoveryInput, Side, TextSpan},
};
use std::sync::Arc;

/// A completed global seed's generation. Borrowing all original inputs freezes
/// source characters, normalization, view boundaries and recovery metadata until
/// this assessor finishes. There is no public constructor or serialized form.
pub(in super::super) struct GlobalGeneration(Arc<()>);

struct Generation<'a, 'document> {
    sides: [&'a Side<'document>; 2],
    recovery: SentenceRecoveryInput<'a>,
    view_counts: [usize; 2],
    max_ranges: usize,
}

struct Prefix {
    original: LocalDomain,
    extended: LocalDomain,
    attempted: bool,
    nominated: bool,
}

/// Sparse end-block prefixes retained by the actual discovery invocation.
/// Moving this value to its assessor keeps the same immutable generation alive.
pub(in super::super) struct GlobalPrefixSet<'a, 'document> {
    generation: Generation<'a, 'document>,
    expected_generation: Arc<()>,
    receipt: Option<GlobalGeneration>,
    prefixes: Vec<Prefix>,
    retained_identifiers: usize,
}

/// A newly extended span pair, never an old parent's correspondence flags.
pub(in super::super) struct GlobalExtension {
    pub(in super::super) domain: LocalDomain,
}

fn same_slice<T>(a: &[T], b: &[T]) -> bool {
    std::ptr::eq(a, b)
}

fn same_evidence(
    a: Option<crate::diff::TrustedRunRecoveryInput<'_>>,
    b: Option<crate::diff::TrustedRunRecoveryInput<'_>>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            same_slice(a.descriptors, b.descriptors)
                && same_slice(a.raw_region_edges, b.raw_region_edges)
        }
        _ => false,
    }
}

impl<'a, 'document> GlobalPrefixSet<'a, 'document> {
    pub(super) fn new(
        sides: [&'a Side<'document>; 2],
        recovery: SentenceRecoveryInput<'a>,
        view_counts: [usize; 2],
        max_ranges: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        // Scope header, its eventual move and the empty retention ledger.
        if !charge(remaining, 128) {
            return None;
        }
        let expected_generation = Arc::new(());
        Some(Self {
            receipt: Some(GlobalGeneration(Arc::clone(&expected_generation))),
            expected_generation,
            generation: Generation {
                sides,
                recovery,
                view_counts,
                max_ranges,
            },
            prefixes: Vec::new(),
            retained_identifiers: 0,
        })
    }

    /// Transfers the one prepaid receipt to the owning assessor. The receipt
    /// has no clone, external constructor or serialized representation.
    pub(in super::super) fn take_generation(&mut self) -> Option<GlobalGeneration> {
        self.receipt.take()
    }

    /// Retains only a last adjacent comparison ending inside the final original
    /// member block. The first seed was globally unique after a completed scan;
    /// exact equality of the longer prefix therefore proves its uniqueness.
    pub(super) fn retain(
        &mut self,
        witness: GlobalSeedWitness,
        views: [&View; 2],
        prefix: &LocalDomain,
        remaining: &mut usize,
    ) -> Result<()> {
        if !charge(remaining, 32) {
            return Ok(());
        }
        let first = witness.anchor();
        let starts = [first.old_start, first.new_start];
        let spans = [&prefix.old_span, &prefix.new_span];
        let width = self.generation.recovery.min_tokens;
        if width == 0 || self.prefixes.len() >= self.generation.max_ranges {
            return Ok(());
        }
        for side in 0..2 {
            let Some(last) = views[side].block_ranges.last() else {
                return Ok(());
            };
            let range = spans[side].comparable_range;
            if views[side].source_bounded
                || views[side].group.blocks.len() < 2
                || range.start != starts[side]
                || starts[side]
                    .checked_add(width)
                    .is_none_or(|end| end > range.end)
                || range.start >= last.start
                || range.end <= last.start
                || range.end >= last.end
                || last.end != views[side].group.tokens.len()
            {
                return Ok(());
            }
        }
        let lengths = spans.map(|span| span.comparable_range.end - span.comparable_range.start);
        if lengths[0] != lengths[1]
            || !charge(remaining, lengths[0].saturating_mul(6).saturating_add(8))
        {
            return Ok(());
        }
        if views
            .iter()
            .zip(spans)
            .zip(starts)
            .any(|((view, span), start)| {
                view.group.tokens[start..span.comparable_range.end]
                    .iter()
                    .any(|token| !token.is_scalar())
            })
        {
            return Ok(());
        }
        if views[0].group.tokens[starts[0]..spans[0].comparable_range.end]
            != views[1].group.tokens[starts[1]..spans[1].comparable_range.end]
        {
            return Ok(());
        }
        let identifiers = views
            .iter()
            .map(|view| view.group.blocks.len())
            .sum::<usize>();
        let slots = self
            .prefixes
            .capacity()
            .saturating_add(self.prefixes.len())
            .saturating_add(1);
        let bytes = slots
            .saturating_mul(std::mem::size_of::<Prefix>())
            .saturating_add(
                self.retained_identifiers
                    .saturating_add(identifiers.saturating_mul(2))
                    .saturating_mul(8),
            )
            .saturating_add(std::mem::size_of::<Self>())
            .saturating_add(32);
        if bytes > 64 * 1024 * 1024
            || !charge(
                remaining,
                self.prefixes
                    .len()
                    .saturating_mul(std::mem::size_of::<Prefix>().div_ceil(8))
                    .saturating_add(identifiers.saturating_mul(8))
                    .saturating_add(128),
            )
        {
            return Ok(());
        }
        let old_span = views[0]
            .group
            .try_span(starts[0], views[0].group.tokens.len())
            .ok_or_else(|| super::super::allocation_error("global prefix old source span"))?;
        let new_span = views[1]
            .group
            .try_span(starts[1], views[1].group.tokens.len())
            .ok_or_else(|| super::super::allocation_error("global prefix new source span"))?;
        let copy = |span: &TextSpan| -> Result<TextSpan> {
            Ok(TextSpan {
                blocks: crate::diff::try_copy_slice(&span.blocks).ok_or_else(|| {
                    super::super::allocation_error("global prefix source identifiers")
                })?,
                separator: span.separator,
                canonical_range: span.canonical_range,
                comparable_range: span.comparable_range,
            })
        };
        self.prefixes
            .try_reserve_exact(1)
            .map_err(|_| super::super::allocation_error("global prefix retention"))?;
        // The preflight includes the old and new capacities while reallocation
        // can keep both alive. Record actual capacity before making a row usable.
        if self
            .prefixes
            .capacity()
            .saturating_mul(std::mem::size_of::<Prefix>())
            .saturating_add(
                self.retained_identifiers
                    .saturating_add(identifiers.saturating_mul(2))
                    .saturating_mul(8),
            )
            > bytes
        {
            return Ok(());
        }
        self.retained_identifiers = self
            .retained_identifiers
            .saturating_add(identifiers.saturating_mul(2));
        self.prefixes.push(Prefix {
            original: LocalDomain {
                old_span: copy(spans[0])?,
                new_span: copy(spans[1])?,
                source_bounded: false,
            },
            extended: LocalDomain {
                old_span,
                new_span,
                source_bounded: false,
            },
            attempted: false,
            nominated: false,
        });
        Ok(())
    }

    /// Nominates a retained extension by missing end-block length. This reads
    /// only prepaid scalar bounds; latest ownership and all proofs run later.
    pub(in super::super) fn nominate(
        &mut self,
        remaining: &mut usize,
    ) -> Result<Option<LocalDomain>> {
        if !charge(
            remaining,
            self.prefixes
                .len()
                .saturating_mul(16)
                .saturating_add(self.retained_identifiers.saturating_mul(8))
                .saturating_add(64),
        ) {
            return Ok(None);
        }
        let Some(row) = self
            .prefixes
            .iter_mut()
            .filter(|row| !row.nominated && !row.attempted)
            .max_by_key(|row| {
                row.extended
                    .old_span
                    .comparable_range
                    .end
                    .saturating_sub(row.original.old_span.comparable_range.end)
            })
        else {
            return Ok(None);
        };
        let copy = |span: &TextSpan| -> Result<TextSpan> {
            Ok(TextSpan {
                blocks: crate::diff::try_copy_slice(&span.blocks).ok_or_else(|| {
                    super::super::allocation_error("global nomination identifiers")
                })?,
                separator: span.separator,
                canonical_range: span.canonical_range,
                comparable_range: span.comparable_range,
            })
        };
        let result = LocalDomain {
            old_span: copy(&row.original.old_span)?,
            new_span: copy(&row.original.new_span)?,
            source_bounded: false,
        };
        row.nominated = true;
        Ok(Some(result))
    }

    /// The only consumer is the owning assessor. Foreign or changed generations
    /// cannot use a retained prefix, even when their visible text is identical.
    pub(in super::super) fn extension(
        &mut self,
        generation: &GlobalGeneration,
        sides: [&Side<'_>; 2],
        recovery: SentenceRecoveryInput<'_>,
        original: [&TextSpan; 2],
        max_ranges: usize,
        remaining: &mut usize,
    ) -> Result<Option<GlobalExtension>> {
        let identifiers = self.retained_identifiers;
        if !charge(
            remaining,
            96usize
                .saturating_add(identifiers.saturating_mul(4))
                .saturating_add(self.prefixes.len().saturating_mul(16)),
        ) {
            return Ok(None);
        }
        let frozen = &self.generation;
        let input = frozen.recovery;
        if !Arc::ptr_eq(&self.expected_generation, &generation.0) {
            return Ok(None);
        }
        if !sides
            .iter()
            .zip(frozen.sides)
            .all(|(a, b)| std::ptr::eq(*a, b))
            || !same_slice(
                input.old_trusted_run_intervals,
                recovery.old_trusted_run_intervals,
            )
            || !same_slice(
                input.new_trusted_run_intervals,
                recovery.new_trusted_run_intervals,
            )
            || !same_slice(
                input.old_native_order_blocks,
                recovery.old_native_order_blocks,
            )
            || !same_slice(
                input.new_native_order_blocks,
                recovery.new_native_order_blocks,
            )
            || !same_evidence(
                input.old_trusted_run_evidence,
                recovery.old_trusted_run_evidence,
            )
            || !same_evidence(
                input.new_trusted_run_evidence,
                recovery.new_trusted_run_evidence,
            )
            || input.min_tokens != recovery.min_tokens
            || input.enable_known_span_sentence_shadow != recovery.enable_known_span_sentence_shadow
            || input.enable_sentence_edge_gate_shadow != recovery.enable_sentence_edge_gate_shadow
            || frozen.max_ranges != max_ranges
            || frozen.view_counts.contains(&0)
        {
            return Ok(None);
        }
        let Some(row) = self.prefixes.iter_mut().find(|row| {
            !row.attempted
                && &row.original.old_span == original[0]
                && &row.original.new_span == original[1]
        }) else {
            return Ok(None);
        };
        let n = row
            .extended
            .old_span
            .blocks
            .len()
            .saturating_add(row.extended.new_span.blocks.len());
        if !charge(remaining, n.saturating_mul(12).saturating_add(64)) {
            return Ok(None);
        }
        let copy = |span: &TextSpan| -> Result<TextSpan> {
            Ok(TextSpan {
                blocks: crate::diff::try_copy_slice(&span.blocks).ok_or_else(|| {
                    super::super::allocation_error("global extension source identifiers")
                })?,
                separator: span.separator,
                canonical_range: span.canonical_range,
                comparable_range: span.comparable_range,
            })
        };
        let domain = LocalDomain {
            old_span: copy(&row.extended.old_span)?,
            new_span: copy(&row.extended.new_span)?,
            source_bounded: false,
        };
        row.attempted = true;
        Ok(Some(GlobalExtension { domain }))
    }
}
