//! Deferred anchor discovery; prior results and source ownership are immutable.
//!
//! Additional domains use the original closure and proof predicates. Discovery
//! records correspondence evidence only: it never edits candidates, emits a
//! change, or resolves a source token from equal text alone.

use super::{Assessor, DomainKey, DomainState, Result, charge_work, views};

impl Assessor<'_, '_> {
    pub(super) fn discover_deferred_domains(&mut self) -> Result<()> {
        if self.remaining_work == 0
            || self.output_stop.is_some()
            || !self.local_domains.is_empty()
            || !self.local_anchors.is_empty()
            || self.root_reasons.is_empty()
            || self.root_relation.is_none()
        {
            return Ok(());
        }
        let Some(recovery) = self.recovery else {
            return Ok(());
        };
        let available_records = self
            .options
            .max_assessment_ranges
            .saturating_sub(self.records.len());
        if available_records < 2 {
            return Ok(());
        }
        let discovery = views::discover_deferred(
            self.sides,
            recovery,
            &mut self.remaining_work,
            self.options.max_assessment_ranges,
        )?;
        // Proof construction uses whole source groups before slicing. Charge
        // those complete populations and their copies before invoking the
        // unchanged proof path. A refused domain publishes no ownership.
        for domain in discovery.domains {
            if self.remaining_work == 0
                || self.output_stop.is_some()
                || self.records.len() >= self.options.max_assessment_ranges.saturating_sub(2)
            {
                break;
            }
            let old = super::block_extent(self.sides[0], &domain.old_span)?;
            let new = super::block_extent(self.sides[1], &domain.new_span)?;
            let count = old.len().saturating_add(new.len());
            if !charge_work(&mut self.remaining_work, count) {
                break;
            }
            let mut source_tokens = 0_usize;
            let mut literal = true;
            for (side, extent) in self.sides.into_iter().zip([old.clone(), new.clone()]) {
                for index in extent {
                    let tokens = &side.canonical[index];
                    if !charge_work(&mut self.remaining_work, tokens.len()) {
                        return Ok(());
                    }
                    literal &= tokens
                        .iter()
                        .all(|token| matches!(token, super::ComparableToken::Scalar(_)));
                    source_tokens = source_tokens.saturating_add(tokens.len());
                }
            }
            if !literal {
                continue;
            }
            let mut bytes = source_tokens.saturating_add(count).saturating_mul(512);
            // A growing table temporarily retains its old allocation. Charge
            // moving existing entries before the unchanged proof path reserves.
            if self.domains.len() == self.domains.capacity() {
                bytes = bytes.saturating_add(
                    self.domains
                        .capacity()
                        .max(4)
                        .saturating_mul(4)
                        .saturating_mul(std::mem::size_of::<(DomainKey, super::DomainProof)>()),
                );
            }
            if self.records.len() == self.records.capacity() {
                bytes = bytes.saturating_add(
                    self.records
                        .capacity()
                        .max(4)
                        .saturating_mul(3)
                        .saturating_mul(std::mem::size_of::<super::RelationAssessment>()),
                );
            }

            if bytes > 64 * 1024 * 1024 || !self.charge(bytes) {
                continue;
            }
            let key = DomainKey {
                old,
                new,
                old_separator: domain
                    .old_span
                    .separator
                    .unwrap_or(super::BlockSeparator::Space),
                new_separator: domain
                    .new_span
                    .separator
                    .unwrap_or(super::BlockSeparator::Space),
                local: Some((domain.old_span.clone(), domain.new_span.clone())),
            };
            self.local_domains
                .try_reserve(1)
                .map_err(|_| super::allocation_error("deferred local domains"))?;
            self.local_domains.push(domain);
            if matches!(self.prove_domain(&key)?, DomainState::Stopped(_)) {
                break;
            }
        }
        Ok(())
    }
}
