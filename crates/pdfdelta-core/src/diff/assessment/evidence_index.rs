//! Immutable alignment reasons in their original first-occurrence order.
use super::{Alignment, AlignmentEvidence, BlockId, Side, charge_work};

/// Stores reasons, never ownership, correspondence or a source verdict.
pub(super) struct EvidenceIndex<'a, 'd> {
    alignment: &'a Alignment,
    sides: [&'a Side<'d>; 2],
    rows: [Vec<Vec<AlignmentEvidence>>; 2],
    global: Vec<AlignmentEvidence>,
    maximum: usize,
    bytes: usize,
}

impl<'a, 'd> EvidenceIndex<'a, 'd> {
    pub(super) fn build(
        alignment: &'a Alignment,
        sides: [&'a Side<'d>; 2],
        allowance: usize,
        remaining: &mut usize,
    ) -> Option<Self> {
        let count = sides[0].blocks.len().checked_add(sides[1].blocks.len())?;
        let bytes = count
            .checked_mul(std::mem::size_of::<Vec<AlignmentEvidence>>())?
            .checked_add(std::mem::size_of::<Self>())?;
        if bytes > allowance || !charge_work(remaining, count.checked_mul(3)?.checked_add(8)?) {
            return None;
        }
        let mut result = Self {
            alignment,
            sides,
            rows: [Vec::new(), Vec::new()],
            global: Vec::new(),
            maximum: 0,
            bytes,
        };
        for (side, rows) in result.rows.iter_mut().enumerate() {
            rows.try_reserve_exact(sides[side].blocks.len()).ok()?;
            if rows.capacity() != sides[side].blocks.len() {
                return None;
            }
            rows.resize_with(sides[side].blocks.len(), Vec::new);
        }
        for aligned in &alignment.spans {
            if !charge_work(remaining, 3) {
                return None;
            }
            for reason in &aligned.evidence {
                if !charge_work(remaining, result.global.len().checked_add(4)?) {
                    return None;
                }
                if result.global.contains(reason) {
                    continue;
                }
                if result.global.len() >= usize::BITS as usize {
                    return None;
                }
                let width = std::mem::size_of::<AlignmentEvidence>();
                if result.bytes.checked_add(
                    result
                        .global
                        .capacity()
                        .checked_add(1)?
                        .checked_mul(width)?,
                )? > allowance
                    || !charge_work(remaining, result.global.len().checked_add(3)?)
                {
                    return None;
                }
                let before = result.global.capacity();
                result.global.try_reserve_exact(1).ok()?;
                result.bytes = result.bytes.checked_add(
                    result
                        .global
                        .capacity()
                        .checked_sub(before)?
                        .checked_mul(width)?,
                )?;
                if result.bytes > allowance {
                    return None;
                }
                result.global.push(*reason);
            }
            for (side, blocks) in [&aligned.old, &aligned.new].into_iter().enumerate() {
                for block in blocks {
                    if !charge_work(remaining, 3) {
                        return None;
                    }
                    let &index = sides[side].index.get(block)?;
                    let row = result.rows[side].get_mut(index)?;
                    for reason in &aligned.evidence {
                        if !charge_work(remaining, row.len().checked_add(4)?) {
                            return None;
                        }
                        if row.contains(reason) {
                            continue;
                        }
                        if row.len() >= usize::BITS as usize {
                            return None;
                        }
                        if row.len() == row.capacity() {
                            let width = std::mem::size_of::<AlignmentEvidence>();
                            let peak = result
                                .bytes
                                .checked_add(row.capacity().checked_add(1)?.checked_mul(width)?)?;
                            if peak > allowance
                                || !charge_work(remaining, row.len().checked_add(3)?)
                            {
                                return None;
                            }
                            let before = row.capacity();
                            row.try_reserve_exact(1).ok()?;
                            result.bytes = result.bytes.checked_add(
                                row.capacity().checked_sub(before)?.checked_mul(width)?,
                            )?;
                            if result.bytes > allowance {
                                return None;
                            }
                        }
                        row.push(*reason);
                        if !charge_work(remaining, 2) {
                            return None;
                        }
                        result.maximum = result.maximum.max(row.len());
                    }
                }
            }
        }
        Some(result)
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(super) fn reason_count(&self) -> usize {
        self.global.len()
    }
    pub(super) fn maximum_copy_len(&self) -> usize {
        self.maximum
    }

    pub(super) fn is_bound(&self, alignment: &Alignment, sides: [&Side<'_>; 2]) -> bool {
        std::ptr::eq(self.alignment, alignment)
            && (0..2).all(|i| std::ptr::eq(self.sides[i], sides[i]))
    }

    /// All callers still pay output copying. The immutable bindings prevent a
    /// lookup from substituting another document or alignment for this index.
    pub(super) fn lookup(
        &self,
        alignment: &Alignment,
        sides: [&Side<'_>; 2],
        side: usize,
        block: BlockId,
    ) -> Option<&[AlignmentEvidence]> {
        if !self.is_bound(alignment, sides) {
            return None;
        }
        let &index = sides.get(side)?.index.get(&block)?;
        self.rows.get(side)?.get(index).map(Vec::as_slice)
    }
}
