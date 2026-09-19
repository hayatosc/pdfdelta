//! Exact relative-displacement resolution for one closed line whose edit
//! location is ambiguous.
//!
//! The pure functions here are ready for the assessment to call; no production
//! path calls them yet. They pre-check every token of one line, enumerate the
//! maximum equal-token matchings (unit insert/delete), take the pairs common to
//! every matching as the anchor, and keep only the matchings whose matched
//! tokens have exactly equal cumulative displacement from that anchor. A single
//! survivor becomes a candidate resolution; zero or several survivors, a
//! missing or disagreeing record, an unsupported construct and every arithmetic
//! or enumeration limit hold the whole line. All work is charged to the shared
//! assessment budget.

use crate::model::GlyphDisplacement;
use crate::normalize::ComparableToken;

/// Exact rational `num / den` with `den > 0` and a bounded bit width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rational {
    num: i128,
    den: i128,
}

const MAX_BITS: u32 = 120;
const MAX_LINE_TOKENS: usize = 512;
const MAX_MATCHINGS: usize = 4096;

impl Rational {
    fn zero() -> Self {
        Self { num: 0, den: 1 }
    }

    fn from_f64(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        if value == 0.0 {
            return Some(Self::zero());
        }
        let bits = value.to_bits();
        let negative = bits >> 63 == 1;
        let exponent = ((bits >> 52) & 0x7ff) as i64;
        let mantissa = bits & ((1u64 << 52) - 1);
        let (significand, exponent) = if exponent == 0 {
            (mantissa, -1074i64)
        } else {
            (mantissa | (1u64 << 52), exponent - 1075)
        };
        let mut num = i128::from(significand);
        if negative {
            num = -num;
        }
        let mut den = 1i128;
        let mut shift = exponent;
        while shift > 0 {
            num = num.checked_mul(2)?;
            shift -= 1;
        }
        while shift < 0 {
            den = den.checked_mul(2)?;
            shift += 1;
        }
        Self { num, den }.reduced()
    }

    fn reduced(self) -> Option<Self> {
        if self.den == 0 {
            return None;
        }
        let (num, den) = if self.den < 0 {
            (-self.num, -self.den)
        } else {
            (self.num, self.den)
        };
        let gcd = gcd(num.unsigned_abs(), den.unsigned_abs());
        let num = num / i128::try_from(gcd).ok()?;
        let den = den / i128::try_from(gcd).ok()?;
        let limit = 128u32.saturating_sub(MAX_BITS);
        if num.unsigned_abs().leading_zeros() < limit || den.unsigned_abs().leading_zeros() < limit
        {
            return None;
        }
        Some(Self { num, den })
    }

    fn add(self, other: Self) -> Option<Self> {
        let num = self
            .num
            .checked_mul(other.den)?
            .checked_add(other.num.checked_mul(self.den)?)?;
        let den = self.den.checked_mul(other.den)?;
        Self { num, den }.reduced()
    }

    fn mul(self, other: Self) -> Option<Self> {
        let num = self.num.checked_mul(other.num)?;
        let den = self.den.checked_mul(other.den)?;
        Self { num, den }.reduced()
    }

    fn div_int(self, value: i128) -> Option<Self> {
        let den = self.den.checked_mul(value)?;
        Self { num: self.num, den }.reduced()
    }

    fn cmp_exact(self, other: Self) -> Option<std::cmp::Ordering> {
        let left = self.num.checked_mul(other.den)?;
        let right = other.num.checked_mul(self.den)?;
        Some(left.cmp(&right))
    }
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

/// Why the exact-displacement rule cannot resolve the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HoldReason {
    /// A token has no record, or the records disagree with the line.
    NoEvidence,
    /// A construct the first version does not support: a non-finite value, an
    /// arithmetic overflow, a horizontal or coordinate anomaly.
    Unsupported,
    /// The line is longer than the token cap.
    TokenCap,
    /// The maximum matchings share no anchor pair.
    NoAnchor,
    /// The shared work budget ended.
    Budget,
    /// No maximum matching is position-consistent.
    NoneConsistent,
    /// Several maximum matchings are position-consistent.
    Multiple,
}

/// The rule outcome for one closed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Resolution {
    /// Exactly one maximum matching keeps every matched pair at an equal exact
    /// displacement from the shared anchor.
    Unique(Vec<(usize, usize)>),
    /// The line stays unresolved for the given reason.
    Hold(HoldReason),
}

fn charge(remaining: &mut usize, work: usize) -> Option<()> {
    if let Some(left) = remaining.checked_sub(work) {
        *remaining = left;
        Some(())
    } else {
        *remaining = 0;
        None
    }
}

/// One validated token column: its exact advance and its raw coordinates.
struct Column {
    advance: Rational,
    run: u32,
}

/// Validates every token of one line before any matching runs.
///
/// All tokens, including the first, the last and any unmatched one, must have a
/// record with finite raw values, one run, one page, a horizontal advance, a
/// constant text state and an identical linear text matrix, CTM and page
/// transform. The raw `Tz` percent is divided by 100 exactly and the applied
/// word spacing is used only for space glyphs. The glyph ids must be
/// consecutive, so the source correspondence is continuous.
fn validate_columns(
    tokens: &[ComparableToken],
    evidence: &[Option<&GlyphDisplacement>],
    remaining: &mut usize,
) -> Result<Vec<Column>, HoldReason> {
    if tokens.len() != evidence.len() {
        return Err(HoldReason::NoEvidence);
    }
    if tokens.len() > MAX_LINE_TOKENS {
        return Err(HoldReason::TokenCap);
    }
    charge(remaining, tokens.len().saturating_mul(64)).ok_or(HoldReason::Budget)?;
    let mut columns = Vec::with_capacity(tokens.len());
    let mut reference: Option<&GlyphDisplacement> = None;
    let mut previous_glyph: Option<u64> = None;
    for (index, record) in evidence.iter().enumerate() {
        let Some(record) = record else {
            return Err(HoldReason::NoEvidence);
        };
        if index == 0 {
            reference = Some(record);
        }
        let Some(reference) = reference else {
            return Err(HoldReason::NoEvidence);
        };
        if !record.horizontal_scale.is_finite()
            || !record.horizontal_scale_percent.is_finite()
            || !record.width_1000_em.is_finite()
            || !record.font_size.is_finite()
            || !record.character_spacing.is_finite()
            || !record.word_spacing_applied.is_finite()
            || !record.rise.is_finite()
            || !record.text_matrix.iter().all(|value| value.is_finite())
            || !record.ctm.iter().all(|value| value.is_finite())
            || !record.page_transform.iter().all(|value| value.is_finite())
        {
            return Err(HoldReason::Unsupported);
        }
        // Only the linear text matrix is constant inside one run; the
        // translation advances with every glyph, so it is checked for
        // finiteness above and never compared across tokens.
        if !record.horizontal
            || record.page != reference.page
            || record.run != reference.run
            || !same_linear(&record.text_matrix[..4], &reference.text_matrix[..4])
            || !same_linear(&record.ctm, &reference.ctm)
            || !same_linear(&record.page_transform, &reference.page_transform)
            || record.font_size.to_bits() != reference.font_size.to_bits()
            || record.character_spacing.to_bits() != reference.character_spacing.to_bits()
            || record.horizontal_scale.to_bits() != reference.horizontal_scale.to_bits()
            || record.horizontal_scale_percent.to_bits()
                != reference.horizontal_scale_percent.to_bits()
            || record.rise.to_bits() != reference.rise.to_bits()
        {
            return Err(HoldReason::Unsupported);
        }
        if let Some(previous) = previous_glyph {
            let Some(expected) = previous.checked_add(1) else {
                return Err(HoldReason::Unsupported);
            };
            if record.glyph.0 != expected {
                return Err(HoldReason::Unsupported);
            }
        }
        previous_glyph = Some(record.glyph.0);
        let is_space = record.raw_code.as_slice() == b" ";
        if !is_space && record.word_spacing_applied != 0.0 {
            return Err(HoldReason::Unsupported);
        }
        let advance = advance(record, is_space).ok_or(HoldReason::Unsupported)?;
        columns.push(Column {
            advance,
            run: record.run,
        });
    }
    Ok(columns)
}

/// Bit-exact equality of finite linear matrix components.
fn same_linear(left: &[f64], right: &[f64]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

/// Cross-side coordinate compatibility: same page, linear text matrix, CTM,
/// page transform and numeric text state. The absolute origin translation may
/// differ between the two sides.
fn same_coordinate_system(old: &GlyphDisplacement, new: &GlyphDisplacement) -> bool {
    old.page == new.page
        && same_linear(&old.text_matrix[..4], &new.text_matrix[..4])
        && same_linear(&old.ctm, &new.ctm)
        && same_linear(&old.page_transform, &new.page_transform)
        && old.font_size.to_bits() == new.font_size.to_bits()
        && old.character_spacing.to_bits() == new.character_spacing.to_bits()
        && old.horizontal_scale.to_bits() == new.horizontal_scale.to_bits()
        && old.horizontal_scale_percent.to_bits() == new.horizontal_scale_percent.to_bits()
        && old.rise.to_bits() == new.rise.to_bits()
        && old.horizontal
        && new.horizontal
}

/// Exact text-space advance from the raw operands.
///
/// `(width/1000 * font_size + character_spacing + word_spacing) * percent/100`
/// is computed as exact rationals over the finite f64 inputs; the computed
/// `horizontal_scale` field is not used.
fn advance(record: &GlyphDisplacement, is_space: bool) -> Option<Rational> {
    let width = Rational::from_f64(record.width_1000_em)?;
    let font_size = Rational::from_f64(record.font_size)?;
    let character_spacing = Rational::from_f64(record.character_spacing)?;
    let word_spacing = if is_space {
        Rational::from_f64(record.word_spacing_applied)?
    } else {
        Rational::zero()
    };
    let percent = Rational::from_f64(record.horizontal_scale_percent)?;
    width
        .div_int(1000)?
        .mul(font_size)?
        .add(character_spacing)?
        .add(word_spacing)?
        .mul(percent)?
        .div_int(100)
}

/// Bounded search state for the maximum-matching enumeration.
struct MatchingSearch<'a, T> {
    old: &'a [T],
    new: &'a [T],
    suffix: Vec<usize>,
    columns: usize,
}

impl<T: Eq> MatchingSearch<'_, T> {
    fn run(&self, remaining: &mut usize) -> Option<Vec<Vec<(usize, usize)>>> {
        let mut matchings = Vec::new();
        let mut path = Vec::new();
        self.walk(0, 0, &mut path, &mut matchings, remaining)?;
        Some(matchings)
    }

    fn walk(
        &self,
        old_index: usize,
        new_index: usize,
        path: &mut Vec<(usize, usize)>,
        matchings: &mut Vec<Vec<(usize, usize)>>,
        remaining: &mut usize,
    ) -> Option<()> {
        charge(remaining, 1)?;
        if matchings.len() > MAX_MATCHINGS {
            return None;
        }
        let remaining_length = self.suffix[old_index * self.columns + new_index];
        if remaining_length == 0 {
            charge(remaining, path.len())?;
            matchings.push(path.clone());
            return Some(());
        }
        for candidate_old in old_index..self.old.len() {
            for candidate_new in new_index..self.new.len() {
                charge(remaining, 1)?;
                if self.old[candidate_old] == self.new[candidate_new]
                    && remaining_length
                        == 1 + self.suffix[(candidate_old + 1) * self.columns + candidate_new + 1]
                {
                    path.push((candidate_old, candidate_new));
                    self.walk(
                        candidate_old + 1,
                        candidate_new + 1,
                        path,
                        matchings,
                        remaining,
                    )?;
                    path.pop();
                }
            }
        }
        Some(())
    }
}

/// Enumerates every maximum equal-token matching, or `None` past a limit.
fn maximum_matchings<T: Eq>(
    old: &[T],
    new: &[T],
    remaining: &mut usize,
) -> Option<Vec<Vec<(usize, usize)>>> {
    let cells = old.len().checked_mul(new.len())?;
    charge(remaining, cells.saturating_mul(2))?;
    let columns = new.len().checked_add(1)?;
    let mut suffix = vec![0usize; old.len().checked_add(1)?.checked_mul(columns)?];
    for old_index in (0..old.len()).rev() {
        for new_index in (0..new.len()).rev() {
            suffix[old_index * columns + new_index] = if old[old_index] == new[new_index] {
                suffix[(old_index + 1) * columns + new_index + 1] + 1
            } else {
                suffix[(old_index + 1) * columns + new_index]
                    .max(suffix[old_index * columns + new_index + 1])
            };
        }
    }
    MatchingSearch {
        old,
        new,
        suffix,
        columns,
    }
    .run(remaining)
}

/// Pairs present in every maximum matching.
fn universal_anchor(
    matchings: &[Vec<(usize, usize)>],
    remaining: &mut usize,
) -> Option<Vec<(usize, usize)>> {
    let first = matchings.first()?;
    // The walk emits every matching in increasing pair order, so the linear
    // two-pointer intersection needs no sort.
    let mut anchor = first.clone();
    for matching in &matchings[1..] {
        charge(
            remaining,
            anchor
                .len()
                .saturating_add(matching.len())
                .saturating_add(1),
        )?;
        let mut next = Vec::with_capacity(anchor.len());
        let (mut left, mut right) = (0usize, 0usize);
        while left < anchor.len() && right < matching.len() {
            charge(remaining, 1)?;
            match anchor[left].cmp(&matching[right]) {
                std::cmp::Ordering::Less => left += 1,
                std::cmp::Ordering::Greater => right += 1,
                std::cmp::Ordering::Equal => {
                    next.push(anchor[left]);
                    left += 1;
                    right += 1;
                }
            }
        }
        anchor = next;
        if anchor.is_empty() {
            break;
        }
    }
    Some(anchor)
}

/// Exact displacement from `anchor` to `index`; every token in the interval,
/// including the endpoint, must share the anchor's run.
fn displacement(
    columns: &[Column],
    anchor: usize,
    index: usize,
    remaining: &mut usize,
) -> Option<Rational> {
    let anchor_run = columns.get(anchor)?.run;
    let (start, end) = if anchor <= index {
        (anchor, index)
    } else {
        (index, anchor)
    };
    charge(
        remaining,
        (end - start).saturating_mul(32).saturating_add(32),
    )?;
    let mut total = Rational::zero();
    for position in start..end {
        let column = columns.get(position)?;
        if column.run != anchor_run {
            return None;
        }
        total = total.add(column.advance)?;
    }
    if anchor <= index {
        Some(total)
    } else {
        Some(Rational {
            num: -total.num,
            den: total.den,
        })
    }
}

/// Applies the exact-displacement rule to one closed line.
///
/// The anchor is the first pair common to every maximum matching, so it is
/// chosen before any position is evaluated and never derived from the
/// position-selected path. The maximum matchings are the unit insert/delete
/// equal-token matchings only; a non-maximum submatching never confirms.
pub(super) fn resolve(
    old_tokens: &[ComparableToken],
    new_tokens: &[ComparableToken],
    old_evidence: &[Option<&GlyphDisplacement>],
    new_evidence: &[Option<&GlyphDisplacement>],
    remaining_work: &mut usize,
) -> Resolution {
    let old_columns = match validate_columns(old_tokens, old_evidence, remaining_work) {
        Ok(columns) => columns,
        Err(reason) => return Resolution::Hold(reason),
    };
    let new_columns = match validate_columns(new_tokens, new_evidence, remaining_work) {
        Ok(columns) => columns,
        Err(reason) => return Resolution::Hold(reason),
    };
    let (Some(old_first), Some(new_first)) = (old_evidence.first(), new_evidence.first()) else {
        return Resolution::Hold(HoldReason::NoEvidence);
    };
    let (Some(old_first), Some(new_first)) = (old_first, new_first) else {
        return Resolution::Hold(HoldReason::NoEvidence);
    };
    if !same_coordinate_system(old_first, new_first) {
        return Resolution::Hold(HoldReason::Unsupported);
    }
    let Some(matchings) = maximum_matchings(old_tokens, new_tokens, remaining_work) else {
        return Resolution::Hold(HoldReason::Budget);
    };
    let Some(anchor) = universal_anchor(&matchings, remaining_work) else {
        return Resolution::Hold(HoldReason::Budget);
    };
    let Some(&(anchor_old, anchor_new)) = anchor.first() else {
        return Resolution::Hold(HoldReason::NoAnchor);
    };
    let (Some(old_anchor_record), Some(new_anchor_record)) =
        (old_evidence[anchor_old], new_evidence[anchor_new])
    else {
        return Resolution::Hold(HoldReason::NoEvidence);
    };
    if !same_coordinate_system(old_anchor_record, new_anchor_record) {
        return Resolution::Hold(HoldReason::Unsupported);
    }
    let mut survivors = Vec::new();
    for matching in &matchings {
        let mut consistent = true;
        for &(old_index, new_index) in matching {
            let Some(old_offset) =
                displacement(&old_columns, anchor_old, old_index, remaining_work)
            else {
                return Resolution::Hold(HoldReason::Budget);
            };
            let Some(new_offset) =
                displacement(&new_columns, anchor_new, new_index, remaining_work)
            else {
                return Resolution::Hold(HoldReason::Budget);
            };
            match old_offset.cmp_exact(new_offset) {
                Some(std::cmp::Ordering::Equal) => {}
                Some(_) => {
                    consistent = false;
                    break;
                }
                // A cross-multiplication overflow is not evidence that this
                // candidate differs; the whole line is unsupported.
                None => return Resolution::Hold(HoldReason::Unsupported),
            }
        }
        if consistent {
            if charge(remaining_work, matching.len().saturating_add(1)).is_none() {
                return Resolution::Hold(HoldReason::Budget);
            }
            survivors.push(matching.clone());
        }
    }
    match survivors.len() {
        1 => Resolution::Unique(survivors.remove(0)),
        0 => Resolution::Hold(HoldReason::NoneConsistent),
        _ => Resolution::Hold(HoldReason::Multiple),
    }
}

/// Converts one matching into the existing edit contract.
///
/// Every hunk becomes one deletion edit when the old side is non-empty and one
/// insertion edit when the new side is non-empty; a replacement is two edits,
/// because [`crate::diff::AtomicEdit`] carries exactly one non-empty side. The edit
/// distance counts changed tokens, exactly as the Myers bound does, so a
/// matching whose hunks change more than `max_edit_distance` tokens is
/// refused. The hunk scan and the edit construction are charged to the shared
/// budget. `None` reports a refused conversion or an exhausted budget.
pub(super) fn edits_from_matching(
    matching: &[(usize, usize)],
    old_len: usize,
    new_len: usize,
    max_edit_distance: usize,
    remaining: &mut usize,
) -> Option<Vec<super::super::AtomicEdit>> {
    let mut previous = (0usize, 0usize);
    let mut changed = 0usize;
    let mut hunk_count = 0usize;
    for &(old_index, new_index) in matching {
        if previous.0 < old_index || previous.1 < new_index {
            changed = changed
                .checked_add(old_index - previous.0)?
                .checked_add(new_index - previous.1)?;
            hunk_count = hunk_count.checked_add(1)?;
        }
        previous = (old_index + 1, new_index + 1);
    }
    if previous.0 < old_len || previous.1 < new_len {
        changed = changed
            .checked_add(old_len - previous.0)?
            .checked_add(new_len - previous.1)?;
        hunk_count = hunk_count.checked_add(1)?;
    }
    charge(
        remaining,
        changed
            .saturating_add(hunk_count.saturating_mul(2))
            .saturating_add(matching.len())
            .saturating_add(1),
    )?;
    if changed > max_edit_distance {
        return None;
    }
    let mut hunks = Vec::with_capacity(hunk_count);
    let mut previous = (0usize, 0usize);
    for &(old_index, new_index) in matching {
        if previous.0 < old_index || previous.1 < new_index {
            hunks.push((previous.0..old_index, previous.1..new_index));
        }
        previous = (old_index + 1, new_index + 1);
    }
    if previous.0 < old_len || previous.1 < new_len {
        hunks.push((previous.0..old_len, previous.1..new_len));
    }
    let mut edits = Vec::new();
    for (old, new) in hunks {
        if old.is_empty() && new.is_empty() {
            return None;
        }
        if !old.is_empty() {
            edits.push(super::super::AtomicEdit {
                old: old.clone(),
                new: new.start..new.start,
            });
        }
        if !new.is_empty() {
            edits.push(super::super::AtomicEdit {
                old: old.end..old.end,
                new,
            });
        }
    }
    Some(edits)
}

#[cfg(test)]
mod tests {
    use super::{HoldReason, Resolution, edits_from_matching, resolve};
    use crate::model::{GlyphDisplacement, GlyphId, PageId};
    use crate::normalize::ComparableToken;

    const OLD_TEXT: &str = "Schedule SE (Form 1040) 2024 ";
    const NEW_TEXT: &str = "Schedule SE (Form 1040) 2025 Created 5/7/25 ";

    /// Advances x1000 of the saved SE trace, one per glyph.
    const OLD_WIDTHS: [f64; 29] = [
        649.0, 574.0, 593.0, 574.0, 611.0, 593.0, 141.0, 574.0, 278.0, 649.0, 647.0, 278.0, 296.0,
        574.0, 611.0, 391.0, 901.0, 278.0, 556.0, 556.0, 556.0, 556.0, 296.0, 278.0, 556.0, 556.0,
        556.0, 556.0, 278.0,
    ];
    const NEW_WIDTHS: [f64; 44] = [
        649.0, 574.0, 593.0, 574.0, 611.0, 593.0, 141.0, 574.0, 278.0, 649.0, 647.0, 278.0, 296.0,
        574.0, 611.0, 391.0, 901.0, 278.0, 556.0, 556.0, 556.0, 556.0, 296.0, 278.0, 556.0, 556.0,
        556.0, 556.0, 278.0, 722.0, 333.0, 537.0, 537.0, 315.0, 537.0, 593.0, 278.0, 556.0, 333.0,
        556.0, 333.0, 556.0, 556.0, 278.0,
    ];

    fn fixture(widths: &[f64], text: &str) -> Vec<GlyphDisplacement> {
        let mut origin = 0.0f64;
        widths
            .iter()
            .zip(text.chars())
            .enumerate()
            .map(|(index, (&width, character))| {
                let text_matrix = [7.0, 0.0, 0.0, 7.0, origin, 0.0];
                origin += 7.0 * width / 1000.0;
                let _ = index;
                GlyphDisplacement {
                    glyph: GlyphId(index as u64),
                    page: PageId(0),
                    raw_code: vec![character as u8],
                    width_1000_em: width,
                    font_size: 1.0,
                    character_spacing: 0.0,
                    word_spacing_applied: 0.0,
                    horizontal_scale_percent: 100.0,
                    horizontal_scale: 1.0,
                    rise: 0.0,
                    horizontal: true,
                    run: 0,
                    text_matrix,
                    ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    page_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                }
            })
            .collect()
    }

    fn tokens(text: &str) -> Vec<ComparableToken> {
        text.chars().map(ComparableToken::Scalar).collect()
    }

    fn borrowed(records: &[GlyphDisplacement]) -> Vec<Option<&GlyphDisplacement>> {
        records.iter().map(Some).collect()
    }

    #[test]
    fn zero_advance_duplicate_holds_with_two_consistent_ways() {
        // old "axb" -> new "axxb" with x at zero advance: the old x can pair
        // with either new x, and both maximum matchings displace every pair
        // identically, so no single matching proves the edit.
        let old_records = fixture(&[600.0, 0.0, 600.0], "axb");
        let new_records = fixture(&[600.0, 0.0, 0.0, 600.0], "axxb");
        let mut budget = 1_000_000;
        let resolution = resolve(
            &tokens("axb"),
            &tokens("axxb"),
            &borrowed(&old_records),
            &borrowed(&new_records),
            &mut budget,
        );
        assert!(
            matches!(resolution, Resolution::Hold(_)),
            "two equal-displacement ways cannot prove one edit"
        );
    }

    #[test]
    fn se_line_resolves_to_the_intended_matching() {
        let old = tokens(OLD_TEXT);
        let new = tokens(NEW_TEXT);
        let old_records = fixture(&OLD_WIDTHS, OLD_TEXT);
        let new_records = fixture(&NEW_WIDTHS, NEW_TEXT);
        let mut budget = 1_000_000;
        let resolution = resolve(
            &old,
            &new,
            &borrowed(&old_records),
            &borrowed(&new_records),
            &mut budget,
        );
        let Resolution::Unique(matching) = resolution else {
            panic!("M0 must be the only position-consistent matching: {resolution:?}");
        };
        assert_eq!(matching.len(), 28);
        assert_eq!(matching[0], (0, 0));
        assert!(matching.contains(&(28, 28)));
        assert!(!matching.contains(&(28, 36)));
        assert!(!matching.contains(&(28, 43)));
        assert!(!matching.contains(&(26, 41)));
        let edits = edits_from_matching(&matching, old.len(), new.len(), 17, &mut 1_000_000)
            .expect("the hunks fit the edit distance");
        assert_eq!(
            edits.len(),
            3,
            "one replacement and one insertion: {edits:?}"
        );
        assert_eq!(edits[0].old, 27..28);
        assert_eq!(edits[0].new, 27..27);
        assert_eq!(edits[1].old, 28..28);
        assert_eq!(edits[1].new, 27..28);
        assert_eq!(edits[2].old, 29..29);
        assert_eq!(edits[2].new, 29..44);
    }

    #[test]
    fn side_swap_keeps_the_same_decision() {
        let old = tokens(OLD_TEXT);
        let new = tokens(NEW_TEXT);
        let old_records = fixture(&OLD_WIDTHS, OLD_TEXT);
        let new_records = fixture(&NEW_WIDTHS, NEW_TEXT);
        let forward = resolve(
            &old,
            &new,
            &borrowed(&old_records),
            &borrowed(&new_records),
            &mut 1_000_000,
        );
        let reverse = resolve(
            &new,
            &old,
            &borrowed(&new_records),
            &borrowed(&old_records),
            &mut 1_000_000,
        );
        let (Resolution::Unique(forward), Resolution::Unique(reverse)) = (forward, reverse) else {
            panic!("both directions must resolve");
        };
        let mirrored = reverse
            .into_iter()
            .map(|(new_index, old_index)| (old_index, new_index))
            .collect::<Vec<_>>();
        assert_eq!(forward, mirrored);
    }

    #[test]
    fn internal_unmatched_anomalies_hold() {
        let old = tokens(OLD_TEXT);
        let new = tokens(NEW_TEXT);
        let old_records = fixture(&OLD_WIDTHS, OLD_TEXT);
        let mut new_records = fixture(&NEW_WIDTHS, NEW_TEXT);
        // The unmatched year digit carries a different run.
        new_records[27].run = 1;
        let mut budget = 1_000_000;
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut budget
            ),
            Resolution::Hold(HoldReason::Unsupported)
        );
        let mut new_records = fixture(&NEW_WIDTHS, NEW_TEXT);
        new_records[27].ctm = [2.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::Unsupported)
        );
        let mut new_records = fixture(&NEW_WIDTHS, NEW_TEXT);
        new_records[27].font_size = 2.0;
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::Unsupported)
        );
    }

    #[test]
    fn zero_advance_duplicate_holds_as_ambiguous() {
        let old = tokens("axb");
        let new = tokens("axxb");
        let old_records = fixture(&[500.0, 0.0, 250.0], "axb");
        let new_records = fixture(&[500.0, 0.0, 0.0, 250.0], "axxb");
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::Multiple)
        );
    }

    #[test]
    fn no_common_anchor_holds() {
        let old = tokens("ab");
        let new = tokens("ba");
        let old_records = fixture(&[500.0, 250.0], "ab");
        let new_records = fixture(&[250.0, 500.0], "ba");
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::NoAnchor)
        );
    }

    #[test]
    fn width_change_holds_without_a_consistent_matching() {
        let old = tokens("ab");
        let new = tokens("ab");
        let old_records = fixture(&[500.0, 250.0], "ab");
        let new_records = fixture(&[600.0, 250.0], "ab");
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::NoneConsistent)
        );
    }

    #[test]
    fn non_finite_and_overflow_hold() {
        let old = tokens("ab");
        let new = tokens("ab");
        let mut old_records = fixture(&[500.0, 250.0], "ab");
        old_records[0].width_1000_em = f64::NAN;
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&fixture(&[500.0, 250.0], "ab")),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::Unsupported)
        );
        let mut old_records = fixture(&[500.0, 250.0], "ab");
        old_records[0].width_1000_em = f64::MAX;
        old_records[0].horizontal_scale_percent = f64::MAX;
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&fixture(&[500.0, 250.0], "ab")),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::Unsupported)
        );
    }

    #[test]
    fn budget_cut_and_token_cap_hold() {
        let old = tokens("ab");
        let new = tokens("ab");
        let old_records = fixture(&[500.0, 250.0], "ab");
        let new_records = fixture(&[500.0, 250.0], "ab");
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut 1
            ),
            Resolution::Hold(HoldReason::Budget)
        );
        let long = "a".repeat(super::MAX_LINE_TOKENS + 1);
        let old = tokens(&long);
        let new = tokens(&long);
        let old_records = fixture(&vec![500.0; old.len()], &long);
        let new_records = fixture(&vec![500.0; new.len()], &long);
        let mut budget = usize::MAX;
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut budget
            ),
            Resolution::Hold(HoldReason::TokenCap)
        );
    }

    #[test]
    fn comparison_overflow_holds_the_whole_line() {
        // Huge finite widths make the exact sums exceed the cross-multiplication
        // range; a single candidate must not be dropped as different.
        let old = tokens("ab");
        let new = tokens("ab");
        let widths = [2.0f64.powi(120), 2.0f64.powi(-60)];
        let old_records = fixture(&widths, "ab");
        let new_records = fixture(&widths, "ab");
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&new_records),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::Unsupported)
        );
    }

    #[test]
    fn cross_side_coordinate_change_holds() {
        let old = tokens("ab");
        let new = tokens("ab");
        let old_records = fixture(&[500.0, 250.0], "ab");
        for mutate in 0..3 {
            let mut new_records = fixture(&[500.0, 250.0], "ab");
            match mutate {
                0 => new_records[0].text_matrix[0] = 8.0,
                1 => new_records[0].ctm = [2.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                _ => new_records[0].page = PageId(1),
            }
            assert_eq!(
                resolve(
                    &old,
                    &new,
                    &borrowed(&old_records),
                    &borrowed(&new_records),
                    &mut 1_000_000
                ),
                Resolution::Hold(HoldReason::Unsupported),
                "cross-side change {mutate}"
            );
        }
    }

    #[test]
    fn non_finite_matrix_and_state_hold() {
        let old = tokens("ab");
        let new = tokens("ab");
        let old_records = fixture(&[500.0, 250.0], "ab");
        for mutate in 0..5 {
            let mut new_records = fixture(&[500.0, 250.0], "ab");
            match mutate {
                0 => new_records[1].text_matrix[2] = f64::NAN,
                1 => new_records[1].ctm[3] = f64::NAN,
                2 => new_records[1].page_transform[0] = f64::NAN,
                3 => new_records[1].rise = f64::NAN,
                _ => new_records[1].horizontal_scale = f64::NAN,
            }
            assert_eq!(
                resolve(
                    &old,
                    &new,
                    &borrowed(&old_records),
                    &borrowed(&new_records),
                    &mut 1_000_000
                ),
                Resolution::Hold(HoldReason::Unsupported),
                "non-finite value {mutate}"
            );
        }
    }

    #[test]
    fn glyph_continuity_overflow_holds() {
        let old = tokens("ab");
        let new = tokens("ab");
        let mut old_records = fixture(&[500.0, 250.0], "ab");
        old_records[0].glyph = GlyphId(u64::MAX);
        old_records[1].glyph = GlyphId(0);
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &borrowed(&fixture(&[500.0, 250.0], "ab")),
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::Unsupported)
        );
    }

    #[test]
    fn missing_record_holds() {
        let old = tokens("ab");
        let new = tokens("ab");
        let old_records = fixture(&[500.0, 250.0], "ab");
        let new_records = fixture(&[500.0, 250.0], "ab");
        let mut evidence = borrowed(&new_records);
        evidence[0] = None;
        assert_eq!(
            resolve(
                &old,
                &new,
                &borrowed(&old_records),
                &evidence,
                &mut 1_000_000
            ),
            Resolution::Hold(HoldReason::NoEvidence)
        );
    }

    #[test]
    fn edit_distance_limit_refuses() {
        let old = tokens(OLD_TEXT);
        let new = tokens(NEW_TEXT);
        let old_records = fixture(&OLD_WIDTHS, OLD_TEXT);
        let new_records = fixture(&NEW_WIDTHS, NEW_TEXT);
        let resolution = resolve(
            &old,
            &new,
            &borrowed(&old_records),
            &borrowed(&new_records),
            &mut 1_000_000,
        );
        let Resolution::Unique(matching) = resolution else {
            panic!("the SE line resolves");
        };
        // The Myers bound counts changed tokens: 1 deletion + 1 insertion +
        // 15 insertions = 17, so 16 refuses and 17 converts.
        assert!(edits_from_matching(&matching, old.len(), new.len(), 16, &mut 1_000_000).is_none());
        assert!(edits_from_matching(&matching, old.len(), new.len(), 17, &mut 1_000_000).is_some());
    }

    #[test]
    fn real_parser_metadata_resolves_a_line_with_advancing_origins() -> crate::Result<()> {
        use crate::pdf::{LopdfParser, ParseLimits, PdfParser};
        use crate::source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor};
        use lopdf::{Document as LopdfDocument, Object, Stream, dictionary};
        use std::sync::Arc;
        let mut document = LopdfDocument::with_version("1.7");
        let font = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => vec![Object::Integer(500); 256],
            "FontDescriptor" => dictionary! {
                "Type" => "FontDescriptor",
                "FontName" => "Helvetica",
                "Ascent" => 800,
                "Descent" => -200,
                "MissingWidth" => 500,
            },
        });
        let contents = document.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 10 20 Td (ab) Tj ET".to_vec(),
        ));
        let resources = dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font) } };
        let pages = document.new_object_id();
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "Contents" => Object::Reference(contents),
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
        });
        document.objects.insert(
            pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page)],
                "Count" => 1,
            }),
        );
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("serialize");
        let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
        let document =
            ContentStreamGlyphExtractor.extract(pdf.as_ref(), ExtractionLimits::default())?;
        let tokens = "ab"
            .chars()
            .map(ComparableToken::Scalar)
            .collect::<Vec<_>>();
        let evidence = document
            .displacements()
            .iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut budget = 1_000_000;
        let resolution = resolve(&tokens, &tokens, &evidence, &evidence, &mut budget);
        assert!(
            matches!(resolution, Resolution::Unique(_)),
            "real parser metadata must resolve the equal line: {resolution:?}"
        );
        Ok(())
    }
}
