//! Reproduce bounded structure and alignment claims without reading PDFs.

use std::{
    collections::HashSet,
    fmt::{self, Display},
    io::{self, Write},
    mem::size_of,
    ops::Range,
};

use pdfdelta_core::{
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
    pipeline::{PipelineOptions, compare_glyph_documents},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_LCS_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const EXPECTED_FIPS: &str =
    include_str!("../../../benchmark/realworld/expected/nist-fips-186-4-to-5.json");
const EXPECTED_CSF: &str =
    include_str!("../../../benchmark/realworld/expected/nist-csf-v1-1-to-v2-0.json");
const EXPECTED_ATTENTION: &str =
    include_str!("../../../benchmark/realworld/expected/arxiv-attention-v6-to-v7.json");
#[cfg(test)]
const EXPECTED_IRS: &str =
    include_str!("../../../benchmark/realworld/expected/irs-form-1040-2024-to-2025.json");
const HISTORY_FIPS: &str = include_str!(
    "../../../benchmark/realworld/results/issue20-order-experiment/order-controls-source-fixed/nist-fips-186-4-to-5.json"
);
const HISTORY_CSF: &str = include_str!(
    "../../../benchmark/realworld/results/issue20-order-experiment/order-controls-source-fixed/nist-csf-v1-1-to-v2-0.json"
);
const HISTORY_ATTENTION: &str = include_str!(
    "../../../benchmark/realworld/results/issue20-order-experiment/order-controls-source-fixed/arxiv-attention-v6-to-v7.json"
);
const PROBE_SOURCE: &[u8] = include_bytes!("structure_claim_probe.rs");

const DSA_OLD_INTRO: &[u8] = include_bytes!(
    "../../../benchmark/realworld/results/structure-claim-probe/inputs/dsa-old-introduction.txt"
);
const DSA_NEW_INTRO: &[u8] = include_bytes!(
    "../../../benchmark/realworld/results/structure-claim-probe/inputs/dsa-new-introduction.txt"
);
const DSA_OLD_PARAGRAPH: &[u8] = include_bytes!(
    "../../../benchmark/realworld/results/structure-claim-probe/inputs/dsa-old-paragraph.txt"
);
const DSA_NEW_PARAGRAPH: &[u8] = include_bytes!(
    "../../../benchmark/realworld/results/structure-claim-probe/inputs/dsa-new-paragraph.txt"
);

type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct ProbeError(String);

impl Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProbeError {}

fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(ProbeError(message.into()))
}

#[derive(Clone, Debug, Deserialize)]
struct ExpectedRevision {
    pair: String,
    annotation: String,
    changes: Vec<ExpectedChange>,
}

#[derive(Clone, Debug, Deserialize)]
struct ExpectedChange {
    id: String,
    kind: String,
    scope: Option<String>,
    old_quote: Option<String>,
    new_quote: Option<String>,
    old_changed_ranges: Option<Vec<ExpectedRange>>,
    new_changed_ranges: Option<Vec<ExpectedRange>>,
}

#[derive(Clone, Debug, Deserialize)]
struct ExpectedRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, Serialize)]
struct RangeValue {
    start: usize,
    end: usize,
}

fn range_value(range: &Range<usize>) -> RangeValue {
    RangeValue {
        start: range.start,
        end: range.end,
    }
}

fn parse_expected(text: &str, pair: &str) -> ProbeResult<ExpectedRevision> {
    let revision: ExpectedRevision = serde_json::from_str(text)?;
    if revision.pair != pair {
        return Err(invalid(format!(
            "expected manifest pair mismatch: wanted {pair}, found {}",
            revision.pair
        )));
    }
    if revision.changes.is_empty() {
        return Err(invalid(format!("expected manifest {pair} has no changes")));
    }
    Ok(revision)
}

fn expected_change<'a>(
    revision: &'a ExpectedRevision,
    id: &str,
) -> ProbeResult<&'a ExpectedChange> {
    revision
        .changes
        .iter()
        .find(|change| change.id == id)
        .ok_or_else(|| {
            invalid(format!(
                "expected manifest {} has no change {id}",
                revision.pair
            ))
        })
}

fn required_quote<'a>(quote: &'a Option<String>, label: &str) -> ProbeResult<&'a str> {
    quote
        .as_deref()
        .ok_or_else(|| invalid(format!("{label} is required for this probe")))
}

fn expected_ranges<'a>(
    ranges: &'a Option<Vec<ExpectedRange>>,
    label: &str,
) -> ProbeResult<&'a [ExpectedRange]> {
    ranges
        .as_deref()
        .ok_or_else(|| invalid(format!("{label} are required for this probe")))
}

#[derive(Clone, Debug)]
struct LoadedSource {
    descriptor: SourceInput,
    text: String,
}

#[derive(Clone, Debug, Serialize)]
struct SourceInput {
    id: &'static str,
    path: &'static str,
    normalized_byte_length: usize,
    scalar_length: usize,
    stripped_one_trailing_newline: bool,
    sha256: String,
}

fn load_source(
    id: &'static str,
    path: &'static str,
    bytes: &'static [u8],
    expected_length: usize,
    expected_sha256: &str,
) -> ProbeResult<LoadedSource> {
    let normalized = bytes
        .strip_suffix(b"\n")
        .ok_or_else(|| invalid(format!("source {id} must end with one repository newline")))?;
    let text = std::str::from_utf8(normalized)?.to_owned();
    let sha256 = hex_digest(Sha256::digest(normalized).as_slice());
    if normalized.len() != expected_length || sha256 != expected_sha256 {
        return Err(invalid(format!(
            "source {id} hash or length mismatch: {} bytes, {sha256}",
            normalized.len()
        )));
    }
    Ok(LoadedSource {
        descriptor: SourceInput {
            id,
            path,
            normalized_byte_length: normalized.len(),
            scalar_length: text.chars().count(),
            stripped_one_trailing_newline: true,
            sha256,
        },
        text,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Copy, Debug, Default)]
struct CountCell {
    lcs: u32,
    min_matched: u32,
    max_matched: u32,
}

fn combine(left: CountCell, right: CountCell) -> CountCell {
    match left.lcs.cmp(&right.lcs) {
        std::cmp::Ordering::Greater => left,
        std::cmp::Ordering::Less => right,
        std::cmp::Ordering::Equal => CountCell {
            lcs: left.lcs,
            min_matched: left.min_matched.min(right.min_matched),
            max_matched: left.max_matched.max(right.max_matched),
        },
    }
}

fn allocate<T: Default + Clone>(length: usize, label: &str) -> ProbeResult<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|error| invalid(format!("could not allocate {label}: {error}")))?;
    values.resize(length, T::default());
    Ok(values)
}

struct LcsTables {
    columns: usize,
    forward: Vec<u32>,
    reverse: Vec<u32>,
    lcs: u32,
}

impl LcsTables {
    fn new(old: &[char], new: &[char]) -> ProbeResult<Self> {
        if old.len() > u32::MAX as usize || new.len() > u32::MAX as usize {
            return Err(invalid("LCS input length exceeds u32 evidence coordinates"));
        }
        let rows = old
            .len()
            .checked_add(1)
            .ok_or_else(|| invalid("LCS row count overflow"))?;
        let columns = new
            .len()
            .checked_add(1)
            .ok_or_else(|| invalid("LCS column count overflow"))?;
        let cells = rows
            .checked_mul(columns)
            .ok_or_else(|| invalid("LCS cell count overflow"))?;
        let bytes = cells
            .checked_mul(size_of::<u32>())
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or_else(|| invalid("LCS memory calculation overflow"))?;
        if bytes > MAX_LCS_MEMORY_BYTES {
            return Err(invalid(format!(
                "LCS tables require {bytes} bytes, exceeding the 64 MiB probe guard"
            )));
        }
        let mut forward: Vec<u32> = allocate(cells, "LCS forward table")?;
        let mut reverse: Vec<u32> = allocate(cells, "LCS reverse table")?;

        for old_count in 1..rows {
            let row_start = old_count * columns;
            let previous_start = (old_count - 1) * columns;
            for new_count in 1..columns {
                let value = if old[old_count - 1] == new[new_count - 1] {
                    forward[previous_start + new_count - 1]
                        .checked_add(1)
                        .ok_or_else(|| invalid("LCS length overflow"))?
                } else {
                    forward[previous_start + new_count].max(forward[row_start + new_count - 1])
                };
                forward[row_start + new_count] = if old[old_count - 1] == new[new_count - 1] {
                    value
                        .max(forward[previous_start + new_count])
                        .max(forward[row_start + new_count - 1])
                } else {
                    value
                };
            }
        }

        for old_index in (0..old.len()).rev() {
            for new_index in (0..new.len()).rev() {
                let value = if old[old_index] == new[new_index] {
                    reverse[(old_index + 1) * columns + new_index + 1]
                        .checked_add(1)
                        .ok_or_else(|| invalid("reverse LCS length overflow"))?
                } else {
                    reverse[(old_index + 1) * columns + new_index]
                        .max(reverse[old_index * columns + new_index + 1])
                };
                reverse[old_index * columns + new_index] = if old[old_index] == new[new_index] {
                    value
                        .max(reverse[(old_index + 1) * columns + new_index])
                        .max(reverse[old_index * columns + new_index + 1])
                } else {
                    value
                };
            }
        }

        let lcs = forward[old.len() * columns + new.len()];
        Ok(Self {
            columns,
            forward,
            reverse,
            lcs,
        })
    }

    fn mandatory_changed(&self, old: &[char], new: &[char]) -> (Vec<bool>, Vec<bool>) {
        let mut old_changed = vec![true; old.len()];
        let mut new_changed = vec![true; new.len()];
        for (old_index, old_char) in old.iter().enumerate() {
            for (new_index, new_char) in new.iter().enumerate() {
                if old_char != new_char {
                    continue;
                }
                let through = self.forward[old_index * self.columns + new_index]
                    + 1
                    + self.reverse[(old_index + 1) * self.columns + new_index + 1];
                if through == self.lcs {
                    old_changed[old_index] = false;
                    new_changed[new_index] = false;
                }
            }
        }
        (old_changed, new_changed)
    }

    fn new_position_state(
        &self,
        old: &[char],
        new: &[char],
        new_index: usize,
    ) -> ProbeResult<PositionState> {
        if new_index >= new.len() {
            return Err(invalid("LCS position is outside the new side"));
        }
        let can_match = old.iter().enumerate().any(|(old_index, old_char)| {
            old_char == &new[new_index]
                && self.forward[old_index * self.columns + new_index]
                    + 1
                    + self.reverse[(old_index + 1) * self.columns + new_index + 1]
                    == self.lcs
        });
        if !can_match {
            return Ok(PositionState::CertainChanged);
        }
        let can_avoid = (0..=old.len()).any(|old_split| {
            self.forward[old_split * self.columns + new_index]
                + self.reverse[old_split * self.columns + new_index + 1]
                == self.lcs
        });
        Ok(if can_avoid {
            PositionState::Ambiguous
        } else {
            PositionState::CertainSame
        })
    }

    fn region_insert_bounds(
        &self,
        old: &[char],
        new: &[char],
        query: Range<usize>,
    ) -> ProbeResult<(usize, usize)> {
        if query.start > query.end || query.end > new.len() {
            return Err(invalid("LCS query range is outside the new side"));
        }
        let query_length = u32::try_from(query.end - query.start)
            .map_err(|_| invalid("LCS query length exceeds u32"))?;
        let mut row = allocate(self.columns, "LCS count row")?;
        for old_char in old {
            let mut diagonal = CountCell::default();
            for new_count in 1..self.columns {
                let up = row[new_count];
                let left = row[new_count - 1];
                let mut best = combine(up, left);
                let new_index = new_count - 1;
                if old_char == &new[new_index] {
                    let selected = u32::from(query.contains(&new_index));
                    let matched = CountCell {
                        lcs: diagonal
                            .lcs
                            .checked_add(1)
                            .ok_or_else(|| invalid("LCS count overflow"))?,
                        min_matched: diagonal
                            .min_matched
                            .checked_add(selected)
                            .ok_or_else(|| invalid("LCS minimum count overflow"))?,
                        max_matched: diagonal
                            .max_matched
                            .checked_add(selected)
                            .ok_or_else(|| invalid("LCS maximum count overflow"))?,
                    };
                    best = combine(best, matched);
                }
                row[new_count] = best;
                diagonal = up;
            }
        }
        let result = row[self.columns - 1];
        if result.lcs != self.lcs {
            return Err(invalid("LCS count row disagrees with the forward table"));
        }
        let lower = query_length
            .checked_sub(result.max_matched)
            .ok_or_else(|| invalid("LCS lower insertion bound underflow"))?;
        let upper = query_length
            .checked_sub(result.min_matched)
            .ok_or_else(|| invalid("LCS upper insertion bound underflow"))?;
        Ok((lower as usize, upper as usize))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PositionState {
    CertainChanged,
    Ambiguous,
    CertainSame,
}

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn text_slice(text: &str, range: &Range<usize>) -> ProbeResult<String> {
    let values = chars(text);
    if range.start > range.end || range.end > values.len() {
        return Err(invalid("source range is outside the scalar text"));
    }
    Ok(values[range.clone()].iter().collect())
}

fn range_mask(ranges: &[ExpectedRange], length: usize, label: &str) -> ProbeResult<Vec<bool>> {
    let mut mask = vec![false; length];
    let mut previous_end = 0;
    for range in ranges {
        if range.start > range.end || range.end > length || range.start < previous_end {
            return Err(invalid(format!(
                "{label} are not sorted, disjoint, valid ranges"
            )));
        }
        mask[range.start..range.end].fill(true);
        previous_end = range.end;
    }
    Ok(mask)
}

fn kept_text(values: &[char], changed: &[bool]) -> String {
    values
        .iter()
        .zip(changed)
        .filter_map(|(value, changed)| (!changed).then_some(*value))
        .collect()
}

#[derive(Clone, Debug, Serialize)]
struct MaskAudit {
    old_length: usize,
    new_length: usize,
    scalar_lcs: usize,
    scalar_optimal_cost: usize,
    old_changed_count: usize,
    new_changed_count: usize,
    expected_mask_cost: usize,
    expected_mask_is_valid_edit_witness: bool,
    expected_mask_in_policy_solution_set: bool,
    expected_kept_pairs: usize,
    gold_minus_optimal: usize,
}

struct MaskFacts {
    audit: MaskAudit,
}

fn mask_facts(
    old_text: &str,
    new_text: &str,
    old_ranges: &[ExpectedRange],
    new_ranges: &[ExpectedRange],
) -> ProbeResult<MaskFacts> {
    let old = chars(old_text);
    let new = chars(new_text);
    let tables = LcsTables::new(&old, &new)?;
    let old_changed = range_mask(old_ranges, old.len(), "old changed ranges")?;
    let new_changed = range_mask(new_ranges, new.len(), "new changed ranges")?;
    let kept_old = kept_text(&old, &old_changed);
    let kept_new = kept_text(&new, &new_changed);
    let valid = kept_old == kept_new;
    let scalar_lcs = usize::try_from(tables.lcs)
        .map_err(|_| invalid("LCS length cannot be represented as usize"))?;
    let expected_mask_cost = old_changed.iter().filter(|changed| **changed).count()
        + new_changed.iter().filter(|changed| **changed).count();
    let scalar_optimal_cost = optimal_edit_cost(old.len(), new.len(), scalar_lcs)?;
    let expected_kept_pairs = kept_old.chars().count();
    Ok(MaskFacts {
        audit: MaskAudit {
            old_length: old.len(),
            new_length: new.len(),
            scalar_lcs,
            scalar_optimal_cost,
            old_changed_count: old_changed.iter().filter(|changed| **changed).count(),
            new_changed_count: new_changed.iter().filter(|changed| **changed).count(),
            expected_mask_cost,
            expected_mask_is_valid_edit_witness: valid,
            expected_mask_in_policy_solution_set: valid && expected_kept_pairs == scalar_lcs,
            expected_kept_pairs,
            gold_minus_optimal: expected_mask_cost.saturating_sub(scalar_optimal_cost),
        },
    })
}

#[derive(Clone, Debug, Serialize)]
struct AnnotationAudit {
    id: String,
    pair: String,
    scope: Option<String>,
    #[serde(flatten)]
    facts: MaskAudit,
}

fn annotation_audit(revision: &ExpectedRevision, id: &str) -> ProbeResult<AnnotationAudit> {
    let change = expected_change(revision, id)?;
    let old = required_quote(&change.old_quote, "old quote")?;
    let new = required_quote(&change.new_quote, "new quote")?;
    let old_ranges = expected_ranges(&change.old_changed_ranges, "old changed ranges")?;
    let new_ranges = expected_ranges(&change.new_changed_ranges, "new changed ranges")?;
    let facts = mask_facts(old, new, old_ranges, new_ranges)?.audit;
    Ok(AnnotationAudit {
        id: id.to_owned(),
        pair: revision.pair.clone(),
        scope: change.scope.clone(),
        facts,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditFacts {
    changed_count: usize,
    optimal_edit_cost: usize,
    complete_edit_witness: bool,
    minimal_complete_edit_witness: bool,
    changed_unlocalized: bool,
}

#[derive(Clone, Debug)]
struct DerivedMask {
    old_changed: Vec<bool>,
    new_changed: Vec<bool>,
}

fn optimal_edit_cost(old_length: usize, new_length: usize, lcs: usize) -> ProbeResult<usize> {
    old_length
        .checked_add(new_length)
        .and_then(|length| {
            lcs.checked_mul(2)
                .and_then(|matched| length.checked_sub(matched))
        })
        .ok_or_else(|| invalid("optimal edit cost underflow"))
}

fn edit_facts(
    old: &[char],
    new: &[char],
    mask: &DerivedMask,
    optimal_edit_cost: usize,
) -> ProbeResult<EditFacts> {
    if mask.old_changed.len() != old.len() || mask.new_changed.len() != new.len() {
        return Err(invalid("edit facts mask lengths do not match source text"));
    }
    let old_changed_count = changed_count(&mask.old_changed);
    let new_changed_count = changed_count(&mask.new_changed);
    let changed_count = old_changed_count
        .checked_add(new_changed_count)
        .ok_or_else(|| invalid("mandatory changed count overflow"))?;
    let complete_edit_witness =
        kept_text(old, &mask.old_changed) == kept_text(new, &mask.new_changed);
    let minimal_complete_edit_witness = complete_edit_witness && changed_count == optimal_edit_cost;
    Ok(EditFacts {
        changed_count,
        optimal_edit_cost,
        complete_edit_witness,
        minimal_complete_edit_witness,
        changed_unlocalized: optimal_edit_cost > 0 && !complete_edit_witness,
    })
}

fn literal_mandatory_evidence(
    old_text: &str,
    new_text: &str,
) -> ProbeResult<(DerivedMask, EditFacts)> {
    let old = chars(old_text);
    let new = chars(new_text);
    let tables = LcsTables::new(&old, &new)?;
    let (old_changed, new_changed) = tables.mandatory_changed(&old, &new);
    let mask = DerivedMask {
        old_changed,
        new_changed,
    };
    let scalar_lcs = usize::try_from(tables.lcs)
        .map_err(|_| invalid("LCS length cannot be represented as usize"))?;
    let optimal_edit_cost = optimal_edit_cost(old.len(), new.len(), scalar_lcs)?;
    let facts = edit_facts(&old, &new, &mask, optimal_edit_cost)?;
    Ok((mask, facts))
}

type AnchorRanges = (Option<Range<usize>>, Option<Range<usize>>);

fn refine_segment(
    old: &[char],
    new: &[char],
    old_range: Range<usize>,
    new_range: Range<usize>,
    old_changed: &mut [bool],
    new_changed: &mut [bool],
) -> ProbeResult<()> {
    if old_range.start > old_range.end
        || old_range.end > old.len()
        || new_range.start > new_range.end
        || new_range.end > new.len()
    {
        return Err(invalid("role refinement range is outside its source text"));
    }
    let (local_old, local_new) = LcsTables::new(&old[old_range.clone()], &new[new_range.clone()])?
        .mandatory_changed(&old[old_range.clone()], &new[new_range.clone()]);
    for (index, changed) in local_old.into_iter().enumerate() {
        old_changed[old_range.start + index] = changed;
    }
    for (index, changed) in local_new.into_iter().enumerate() {
        new_changed[new_range.start + index] = changed;
    }
    Ok(())
}

fn mark_changed(mask: &mut [bool], range: Range<usize>) -> ProbeResult<()> {
    if range.start > range.end || range.end > mask.len() {
        return Err(invalid("role change range is outside its source text"));
    }
    mask[range].fill(true);
    Ok(())
}

fn derived_mask_for_anchors(
    old: &[char],
    new: &[char],
    anchors: &[AnchorRanges],
) -> ProbeResult<DerivedMask> {
    let mut old_changed = vec![false; old.len()];
    let mut new_changed = vec![false; new.len()];
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    for (old_anchor, new_anchor) in anchors {
        let old_start = old_anchor.as_ref().map_or(old_cursor, |range| range.start);
        let new_start = new_anchor.as_ref().map_or(new_cursor, |range| range.start);
        refine_segment(
            old,
            new,
            old_cursor..old_start,
            new_cursor..new_start,
            &mut old_changed,
            &mut new_changed,
        )?;
        match (old_anchor, new_anchor) {
            (Some(old_range), Some(new_range)) => {
                refine_segment(
                    old,
                    new,
                    old_range.clone(),
                    new_range.clone(),
                    &mut old_changed,
                    &mut new_changed,
                )?;
                old_cursor = old_range.end;
                new_cursor = new_range.end;
            }
            (Some(old_range), None) => {
                mark_changed(&mut old_changed, old_range.clone())?;
                old_cursor = old_range.end;
            }
            (None, Some(new_range)) => {
                mark_changed(&mut new_changed, new_range.clone())?;
                new_cursor = new_range.end;
            }
            (None, None) => {
                return Err(invalid("role anchor must name at least one source range"));
            }
        }
    }
    refine_segment(
        old,
        new,
        old_cursor..old.len(),
        new_cursor..new.len(),
        &mut old_changed,
        &mut new_changed,
    )?;
    Ok(DerivedMask {
        old_changed,
        new_changed,
    })
}

fn mask_ranges(mask: &[bool]) -> Vec<RangeValue> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (index, changed) in mask.iter().copied().enumerate() {
        match (start, changed) {
            (None, true) => start = Some(index),
            (Some(begin), false) => {
                ranges.push(RangeValue {
                    start: begin,
                    end: index,
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        ranges.push(RangeValue {
            start: begin,
            end: mask.len(),
        });
    }
    ranges
}

#[derive(Clone, Debug, Serialize)]
struct MaskScore {
    true_positive: usize,
    false_positive: usize,
    false_negative: usize,
}

fn mask_gap(score: &MaskScore) -> usize {
    score.false_positive.saturating_add(score.false_negative)
}

fn combined_mask_gap(old: &MaskScore, new: &MaskScore) -> usize {
    mask_gap(old).saturating_add(mask_gap(new))
}

fn mask_score(derived: &[bool], expected: &[bool]) -> MaskScore {
    let mut score = MaskScore {
        true_positive: 0,
        false_positive: 0,
        false_negative: 0,
    };
    for (derived, expected) in derived.iter().zip(expected) {
        match (*derived, *expected) {
            (true, true) => score.true_positive += 1,
            (true, false) => score.false_positive += 1,
            (false, true) => score.false_negative += 1,
            (false, false) => {}
        }
    }
    score
}

#[derive(Clone, Debug, Serialize)]
struct RoleComparison {
    baseline_old_changed_ranges: Vec<RangeValue>,
    baseline_new_changed_ranges: Vec<RangeValue>,
    baseline_old_changed_count: usize,
    baseline_new_changed_count: usize,
    baseline_mandatory_changed_count: usize,
    baseline_optimal_edit_cost: usize,
    baseline_complete_edit_witness: bool,
    baseline_minimal_complete_edit_witness: bool,
    baseline_changed_unlocalized: bool,
    baseline_old_mask: MaskScore,
    baseline_new_mask: MaskScore,
    baseline_mask_gap: usize,
    derived_old_changed_ranges: Vec<RangeValue>,
    derived_new_changed_ranges: Vec<RangeValue>,
    derived_old_changed_count: usize,
    derived_new_changed_count: usize,
    derived_changed_count: usize,
    derived_optimal_edit_cost: usize,
    derived_complete_edit_witness: bool,
    derived_minimal_complete_edit_witness: bool,
    derived_changed_unlocalized: bool,
    old_mask: MaskScore,
    new_mask: MaskScore,
    derived_mask_gap: usize,
    mask_improvement: bool,
    ungrouped_event_count: usize,
    expected_event_count: usize,
    derived_event_count: usize,
    expected_event_kind: String,
    derived_event_kind: &'static str,
    grouping_improvement: bool,
    grouping_match: bool,
}

fn changed_count(mask: &[bool]) -> usize {
    mask.iter().filter(|changed| **changed).count()
}

fn event_kind(
    old_changed: &[bool],
    new_changed: &[bool],
    old_length: usize,
    new_length: usize,
    optimal_edit_cost: usize,
) -> &'static str {
    match (changed_count(old_changed), changed_count(new_changed)) {
        (0, 0) if optimal_edit_cost == 0 => "unchanged",
        (0, 0) if old_length < new_length => "insertion",
        (0, 0) if old_length > new_length => "deletion",
        (0, 0) => "replacement",
        (0, _) => "insertion",
        (_, 0) => "deletion",
        _ => "replacement",
    }
}

fn presence_event_kind(old_present: bool, new_present: bool) -> &'static str {
    match (old_present, new_present) {
        (false, false) => "unchanged",
        (false, true) => "insertion",
        (true, false) => "deletion",
        (true, true) => "replacement",
    }
}

fn unique_group_count(groups: &[&str]) -> usize {
    let mut unique = Vec::new();
    for group in groups {
        if !unique.contains(group) {
            unique.push(*group);
        }
    }
    unique.len()
}

fn compare_role_masks(
    derived: &DerivedMask,
    old_text: &str,
    new_text: &str,
    expected_ranges: (&[ExpectedRange], &[ExpectedRange]),
    expected_event_count: usize,
    expected_event_kind: &str,
    role_groups: &[&str],
) -> ProbeResult<RoleComparison> {
    let old_expected = range_mask(
        expected_ranges.0,
        derived.old_changed.len(),
        "expected old changed ranges",
    )?;
    let new_expected = range_mask(
        expected_ranges.1,
        derived.new_changed.len(),
        "expected new changed ranges",
    )?;
    let (baseline, baseline_facts) = literal_mandatory_evidence(old_text, new_text)?;
    if baseline.old_changed.len() != derived.old_changed.len()
        || baseline.new_changed.len() != derived.new_changed.len()
    {
        return Err(invalid(
            "literal baseline and derived role masks have different lengths",
        ));
    }
    let baseline_old_count = changed_count(&baseline.old_changed);
    let baseline_new_count = changed_count(&baseline.new_changed);
    let baseline_old_mask = mask_score(&baseline.old_changed, &old_expected);
    let baseline_new_mask = mask_score(&baseline.new_changed, &new_expected);
    let old_mask = mask_score(&derived.old_changed, &old_expected);
    let new_mask = mask_score(&derived.new_changed, &new_expected);
    let baseline_mask_gap = combined_mask_gap(&baseline_old_mask, &baseline_new_mask);
    let derived_mask_gap = combined_mask_gap(&old_mask, &new_mask);
    let old_count = changed_count(&derived.old_changed);
    let new_count = changed_count(&derived.new_changed);
    let old = chars(old_text);
    let new = chars(new_text);
    let derived_facts = edit_facts(&old, &new, derived, baseline_facts.optimal_edit_cost)?;
    let derived_changed_count = derived_facts.changed_count;
    let derived_event_count = unique_group_count(role_groups);
    let derived_event_kind = event_kind(
        &derived.old_changed,
        &derived.new_changed,
        old.len(),
        new.len(),
        derived_facts.optimal_edit_cost,
    );
    Ok(RoleComparison {
        baseline_old_changed_ranges: mask_ranges(&baseline.old_changed),
        baseline_new_changed_ranges: mask_ranges(&baseline.new_changed),
        baseline_old_changed_count: baseline_old_count,
        baseline_new_changed_count: baseline_new_count,
        baseline_mandatory_changed_count: baseline_facts.changed_count,
        baseline_optimal_edit_cost: baseline_facts.optimal_edit_cost,
        baseline_complete_edit_witness: baseline_facts.complete_edit_witness,
        baseline_minimal_complete_edit_witness: baseline_facts.minimal_complete_edit_witness,
        baseline_changed_unlocalized: baseline_facts.changed_unlocalized,
        baseline_old_mask,
        baseline_new_mask,
        baseline_mask_gap,
        derived_old_changed_ranges: mask_ranges(&derived.old_changed),
        derived_new_changed_ranges: mask_ranges(&derived.new_changed),
        derived_old_changed_count: old_count,
        derived_new_changed_count: new_count,
        derived_changed_count,
        derived_optimal_edit_cost: derived_facts.optimal_edit_cost,
        derived_complete_edit_witness: derived_facts.complete_edit_witness,
        derived_minimal_complete_edit_witness: derived_facts.minimal_complete_edit_witness,
        derived_changed_unlocalized: derived_facts.changed_unlocalized,
        old_mask,
        new_mask,
        derived_mask_gap,
        mask_improvement: derived_mask_gap < baseline_mask_gap,
        ungrouped_event_count: role_groups.len(),
        expected_event_count,
        derived_event_count,
        expected_event_kind: expected_event_kind.to_owned(),
        derived_event_kind,
        grouping_improvement: derived_event_count < role_groups.len(),
        grouping_match: expected_event_count == derived_event_count
            && expected_event_kind == derived_event_kind,
    })
}

#[derive(Clone, Debug, Serialize)]
struct DomainProof {
    old_scalar_count: usize,
    new_scalar_count: usize,
    query_new: RangeValue,
    query_length: usize,
    scalar_lcs: usize,
    scalar_optimal_cost: usize,
    certain_changed_positions: usize,
    ambiguous_positions: usize,
    certain_same_positions: usize,
    min_inserted: usize,
    max_inserted: usize,
}

fn domain_proof(old_text: &str, new_text: &str, query: Range<usize>) -> ProbeResult<DomainProof> {
    let old = chars(old_text);
    let new = chars(new_text);
    let tables = LcsTables::new(&old, &new)?;
    let (min_inserted, max_inserted) = tables.region_insert_bounds(&old, &new, query.clone())?;
    let query_length = query.end - query.start;
    let mut certain_changed_positions = 0;
    let mut ambiguous_positions = 0;
    let mut certain_same_positions = 0;
    for new_index in query.clone() {
        match tables.new_position_state(&old, &new, new_index)? {
            PositionState::CertainChanged => certain_changed_positions += 1,
            PositionState::Ambiguous => ambiguous_positions += 1,
            PositionState::CertainSame => certain_same_positions += 1,
        }
    }
    let scalar_lcs = usize::try_from(tables.lcs)
        .map_err(|_| invalid("LCS length cannot be represented as usize"))?;
    let scalar_optimal_cost = old
        .len()
        .checked_add(new.len())
        .and_then(|length| length.checked_sub(2 * scalar_lcs))
        .ok_or_else(|| invalid("optimal edit cost underflow"))?;
    Ok(DomainProof {
        old_scalar_count: old.len(),
        new_scalar_count: new.len(),
        query_new: range_value(&query),
        query_length,
        scalar_lcs,
        scalar_optimal_cost,
        certain_changed_positions,
        ambiguous_positions,
        certain_same_positions,
        min_inserted,
        max_inserted,
    })
}

#[derive(Clone, Debug, Serialize)]
struct DisjointField {
    name: &'static str,
    old_range: RangeValue,
    new_range: RangeValue,
}

#[derive(Clone, Debug, Serialize)]
struct RolePolicyResult {
    id: &'static str,
    role: &'static str,
    policy: &'static str,
    relation: &'static str,
    caller_supplied_domain: bool,
    hypothesis_only: bool,
    diagnostic_only: bool,
    event_group: &'static str,
    event_count: usize,
    event_kind: &'static str,
    outside_literal_objective: Option<bool>,
    fields: Vec<DisjointField>,
    source_ranges: Value,
    premises: Vec<SourcePremise>,
    literal_objective_audit: Option<MaskAudit>,
    domain_proof: Option<DomainProof>,
    derived_comparison: Option<RoleComparison>,
}

#[derive(Clone, Debug, Serialize)]
struct SourcePremise {
    name: String,
    identity: Option<String>,
    old_range: Option<RangeValue>,
    new_range: Option<RangeValue>,
    old_text: String,
    new_text: String,
    basis: &'static str,
}

#[derive(Clone, Debug)]
struct StampFieldInput {
    name: &'static str,
    old_range: Range<usize>,
    new_range: Range<usize>,
    group: &'static str,
}

#[derive(Clone, Debug)]
struct StampRoleInput {
    old_text: String,
    new_text: String,
    fields: Vec<StampFieldInput>,
}

#[derive(Clone, Copy, Debug)]
struct FunctionMemberSpec {
    identity: &'static str,
    label: &'static str,
}

const OLD_CORE_FUNCTIONS: &[FunctionMemberSpec] = &[
    FunctionMemberSpec {
        identity: "identify",
        label: "Identify",
    },
    FunctionMemberSpec {
        identity: "protect",
        label: "Protect",
    },
    FunctionMemberSpec {
        identity: "detect",
        label: "Detect",
    },
    FunctionMemberSpec {
        identity: "respond",
        label: "Respond",
    },
    FunctionMemberSpec {
        identity: "recover",
        label: "Recover",
    },
];

const NEW_CORE_FUNCTIONS: &[FunctionMemberSpec] = &[
    FunctionMemberSpec {
        identity: "govern",
        label: "GOVERN",
    },
    FunctionMemberSpec {
        identity: "identify",
        label: "IDENTIFY",
    },
    FunctionMemberSpec {
        identity: "protect",
        label: "PROTECT",
    },
    FunctionMemberSpec {
        identity: "detect",
        label: "DETECT",
    },
    FunctionMemberSpec {
        identity: "respond",
        label: "RESPOND",
    },
    FunctionMemberSpec {
        identity: "recover",
        label: "RECOVER",
    },
];

#[derive(Clone, Debug)]
struct FunctionAnchor {
    identity: &'static str,
    group: &'static str,
    old_range: Option<Range<usize>>,
    new_range: Option<Range<usize>>,
}

#[derive(Clone, Debug)]
struct FunctionRoleInput {
    old_text: String,
    new_text: String,
    anchors: Vec<FunctionAnchor>,
}

#[derive(Clone, Debug, Serialize)]
struct StampCandidate {
    start: usize,
    end: usize,
    identifier: String,
    version: String,
    subject: String,
    date: String,
    version_range: Range<usize>,
    date_range: Range<usize>,
}

fn parse_stamp_candidate_at(source: &[char], start: usize) -> Option<StampCandidate> {
    let prefix = ['a', 'r', 'X', 'i', 'v', ':'];
    let prefix_end = start.checked_add(prefix.len())?;
    if source
        .get(start..prefix_end)
        .is_none_or(|value| value != prefix)
    {
        return None;
    }
    let mut cursor = start + prefix.len();
    let identifier_start = cursor;
    while source.get(cursor).is_some_and(|character| {
        *character != 'v'
            && (character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
    }) {
        cursor += 1;
    }
    if cursor == identifier_start || source.get(cursor) != Some(&'v') {
        return None;
    }
    let version_start = cursor + 1;
    cursor = version_start;
    while source.get(cursor).is_some_and(char::is_ascii_digit) {
        cursor += 1;
    }
    if cursor == version_start || !source.get(cursor).is_some_and(char::is_ascii_whitespace) {
        return None;
    }
    let version_range = version_start..cursor;
    while source.get(cursor).is_some_and(char::is_ascii_whitespace) {
        cursor += 1;
    }
    if source.get(cursor) != Some(&'[') {
        return None;
    }
    let subject_start = cursor + 1;
    cursor = subject_start;
    while source
        .get(cursor)
        .is_some_and(|character| *character != ']')
    {
        if source
            .get(cursor)
            .is_none_or(|character| !character.is_ascii_graphic() && *character != ' ')
        {
            return None;
        }
        cursor += 1;
    }
    let subject_end = cursor;
    if subject_end == subject_start {
        return None;
    }
    if source.get(cursor) != Some(&']') {
        return None;
    }
    cursor += 1;
    while source.get(cursor).is_some_and(char::is_ascii_whitespace) {
        cursor += 1;
    }
    let date_start = cursor;
    let day_start = cursor;
    while source.get(cursor).is_some_and(char::is_ascii_digit) {
        cursor += 1;
    }
    let day_length = cursor.saturating_sub(day_start);
    if !(1..=2).contains(&day_length) || !source.get(cursor).is_some_and(char::is_ascii_whitespace)
    {
        return None;
    }
    while source.get(cursor).is_some_and(char::is_ascii_whitespace) {
        cursor += 1;
    }
    for _ in 0..3 {
        if !source.get(cursor).is_some_and(char::is_ascii_alphabetic) {
            return None;
        }
        cursor += 1;
    }
    if !source.get(cursor).is_some_and(char::is_ascii_whitespace) {
        return None;
    }
    while source.get(cursor).is_some_and(char::is_ascii_whitespace) {
        cursor += 1;
    }
    for _ in 0..4 {
        if !source.get(cursor).is_some_and(char::is_ascii_digit) {
            return None;
        }
        cursor += 1;
    }
    let date_range = date_start..cursor;
    Some(StampCandidate {
        start,
        end: cursor,
        identifier: source
            [identifier_start..identifier_start + (version_start - 1 - identifier_start)]
            .iter()
            .collect(),
        version: source[version_range.clone()].iter().collect(),
        subject: source[subject_start..subject_end].iter().collect(),
        date: source[date_range.clone()].iter().collect(),
        version_range,
        date_range,
    })
}

fn discover_stamp_candidates_from_chars(source: &[char]) -> Vec<StampCandidate> {
    (0..source.len())
        .filter_map(|start| parse_stamp_candidate_at(source, start))
        .collect()
}

fn find_unique_case_folded(text: &[char], needle: &str) -> ProbeResult<Range<usize>> {
    let needle = needle
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if needle.is_empty() || needle.len() > text.len() {
        return Err(invalid(
            "role identity must be nonempty and fit its source text",
        ));
    }
    let matches = (0..=text.len() - needle.len())
        .filter_map(|start| {
            text[start..start + needle.len()]
                .iter()
                .map(char::to_ascii_lowercase)
                .eq(needle.iter().copied())
                .then_some(start..start + needle.len())
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [range] => Ok(range.clone()),
        [] => Err(invalid(format!("role identity {needle:?} was not found"))),
        _ => Err(invalid(format!(
            "role identity {needle:?} is ambiguous in its source text"
        ))),
    }
}

fn locate_function_members(
    text: &[char],
    specs: &[FunctionMemberSpec],
) -> ProbeResult<Vec<(FunctionMemberSpec, Range<usize>)>> {
    let mut members = Vec::new();
    let mut previous_end = 0;
    for spec in specs {
        let range = find_unique_case_folded(text, spec.label)?;
        if range.start < previous_end {
            return Err(invalid("function role identities are out of source order"));
        }
        previous_end = range.end;
        members.push((*spec, range));
    }
    Ok(members)
}

fn merge_function_members(
    old_members: &[(FunctionMemberSpec, Range<usize>)],
    new_members: &[(FunctionMemberSpec, Range<usize>)],
) -> ProbeResult<Vec<FunctionAnchor>> {
    let mut anchors = Vec::new();
    let mut old_index = 0;
    let mut new_index = 0;
    while old_index < old_members.len() || new_index < new_members.len() {
        let old_member = old_members.get(old_index);
        let new_member = new_members.get(new_index);
        match (old_member, new_member) {
            (Some(old), Some(new)) if old.0.identity == new.0.identity => {
                anchors.push(FunctionAnchor {
                    identity: old.0.identity,
                    group: "core-functions-summary",
                    old_range: Some(old.1.clone()),
                    new_range: Some(new.1.clone()),
                });
                old_index += 1;
                new_index += 1;
            }
            (Some(old), Some(_))
                if !new_members[new_index..]
                    .iter()
                    .any(|member| member.0.identity == old.0.identity) =>
            {
                anchors.push(FunctionAnchor {
                    identity: old.0.identity,
                    group: "core-functions-summary",
                    old_range: Some(old.1.clone()),
                    new_range: None,
                });
                old_index += 1;
            }
            (Some(_), Some(new)) => {
                anchors.push(FunctionAnchor {
                    identity: new.0.identity,
                    group: "core-functions-summary",
                    old_range: None,
                    new_range: Some(new.1.clone()),
                });
                new_index += 1;
            }
            (Some(old), None) => {
                anchors.push(FunctionAnchor {
                    identity: old.0.identity,
                    group: "core-functions-summary",
                    old_range: Some(old.1.clone()),
                    new_range: None,
                });
                old_index += 1;
            }
            (None, Some(new)) => {
                anchors.push(FunctionAnchor {
                    identity: new.0.identity,
                    group: "core-functions-summary",
                    old_range: None,
                    new_range: Some(new.1.clone()),
                });
                new_index += 1;
            }
            (None, None) => unreachable!("member indices are bounded by their slices"),
        }
    }
    Ok(anchors)
}

fn stamp_input_from_candidates(
    old_text: String,
    new_text: String,
    old_candidate: &StampCandidate,
    new_candidate: &StampCandidate,
) -> StampRoleInput {
    StampRoleInput {
        old_text,
        new_text,
        fields: vec![
            StampFieldInput {
                name: "version",
                old_range: old_candidate.version_range.clone(),
                new_range: new_candidate.version_range.clone(),
                group: "arxiv-version-date-stamp",
            },
            StampFieldInput {
                name: "date",
                old_range: old_candidate.date_range.clone(),
                new_range: new_candidate.date_range.clone(),
                group: "arxiv-version-date-stamp",
            },
        ],
    }
}

fn stamp_role_input(change: &ExpectedChange) -> ProbeResult<StampRoleInput> {
    let old_text = required_quote(&change.old_quote, "stamp old quote")?.to_owned();
    let new_text = required_quote(&change.new_quote, "stamp new quote")?.to_owned();
    let old_candidates = discover_stamp_candidates_from_chars(&chars(&old_text));
    let new_candidates = discover_stamp_candidates_from_chars(&chars(&new_text));
    let old_candidate = old_candidates
        .as_slice()
        .first()
        .filter(|_| old_candidates.len() == 1)
        .ok_or_else(|| invalid("stamp role requires one unambiguous old stamp"))?;
    let new_candidate = new_candidates
        .as_slice()
        .first()
        .filter(|_| new_candidates.len() == 1)
        .ok_or_else(|| invalid("stamp role requires one unambiguous new stamp"))?;
    Ok(stamp_input_from_candidates(
        old_text,
        new_text,
        old_candidate,
        new_candidate,
    ))
}

fn function_role_input(change: &ExpectedChange) -> ProbeResult<FunctionRoleInput> {
    let old_text = required_quote(&change.old_quote, "function-list old quote")?.to_owned();
    let new_text = required_quote(&change.new_quote, "function-list new quote")?.to_owned();
    let old_chars = chars(&old_text);
    let new_chars = chars(&new_text);
    let old_members = locate_function_members(&old_chars, OLD_CORE_FUNCTIONS)?;
    let new_members = locate_function_members(&new_chars, NEW_CORE_FUNCTIONS)?;
    let anchors = merge_function_members(&old_members, &new_members)?;
    if anchors.is_empty() {
        return Err(invalid("function-list role has no identity anchors"));
    }
    Ok(FunctionRoleInput {
        old_text,
        new_text,
        anchors,
    })
}

fn stamp_premises(input: &StampRoleInput) -> ProbeResult<Vec<SourcePremise>> {
    input
        .fields
        .iter()
        .map(|field| {
            Ok(SourcePremise {
                name: field.name.to_owned(),
                identity: None,
                old_range: Some(range_value(&field.old_range)),
                new_range: Some(range_value(&field.new_range)),
                old_text: text_slice(&input.old_text, &field.old_range)?,
                new_text: text_slice(&input.new_text, &field.new_range)?,
                basis: "supplied field role with local exact refinement",
            })
        })
        .collect()
}

fn function_premises(input: &FunctionRoleInput) -> ProbeResult<Vec<SourcePremise>> {
    input
        .anchors
        .iter()
        .map(|anchor| {
            let old_text = anchor
                .old_range
                .as_ref()
                .map(|range| text_slice(&input.old_text, range))
                .transpose()?
                .unwrap_or_default();
            let new_text = anchor
                .new_range
                .as_ref()
                .map(|range| text_slice(&input.new_text, range))
                .transpose()?
                .unwrap_or_default();
            Ok(SourcePremise {
                name: anchor.identity.to_owned(),
                identity: Some(anchor.identity.to_owned()),
                old_range: anchor.old_range.as_ref().map(range_value),
                new_range: anchor.new_range.as_ref().map(range_value),
                old_text,
                new_text,
                basis: "supplied function identity with local exact refinement",
            })
        })
        .collect()
}

fn derive_stamp_mask(input: &StampRoleInput) -> ProbeResult<DerivedMask> {
    let old = chars(&input.old_text);
    let new = chars(&input.new_text);
    let anchors = input
        .fields
        .iter()
        .map(|field| (Some(field.old_range.clone()), Some(field.new_range.clone())))
        .collect::<Vec<_>>();
    derived_mask_for_anchors(&old, &new, &anchors)
}

fn derive_function_mask(input: &FunctionRoleInput) -> ProbeResult<DerivedMask> {
    let old = chars(&input.old_text);
    let new = chars(&input.new_text);
    let anchors = input
        .anchors
        .iter()
        .map(|anchor| (anchor.old_range.clone(), anchor.new_range.clone()))
        .collect::<Vec<_>>();
    derived_mask_for_anchors(&old, &new, &anchors)
}

fn premise_for_range(
    name: impl Into<String>,
    identity: Option<String>,
    old_text: &str,
    new_text: &str,
    old_range: Option<&Range<usize>>,
    new_range: Option<&Range<usize>>,
    basis: &'static str,
) -> ProbeResult<SourcePremise> {
    Ok(SourcePremise {
        name: name.into(),
        identity,
        old_range: old_range.map(range_value),
        new_range: new_range.map(range_value),
        old_text: old_range
            .map(|range| text_slice(old_text, range))
            .transpose()?
            .unwrap_or_default(),
        new_text: new_range
            .map(|range| text_slice(new_text, range))
            .transpose()?
            .unwrap_or_default(),
        basis,
    })
}

struct AnnotationAudits<'a> {
    attention: &'a AnnotationAudit,
    csf: &'a AnnotationAudit,
}

fn role_policy_results(
    attention: &ExpectedRevision,
    csf: &ExpectedRevision,
    old_intro: &LoadedSource,
    new_intro: &LoadedSource,
    old_paragraph: &LoadedSource,
    new_paragraph: &LoadedSource,
    audits: AnnotationAudits<'_>,
) -> ProbeResult<Vec<RolePolicyResult>> {
    let attention_change = expected_change(attention, "arxiv-version-date-stamp")?;
    let attention_old = required_quote(&attention_change.old_quote, "attention old quote")?;
    let attention_new = required_quote(&attention_change.new_quote, "attention new quote")?;
    let stamp_input = stamp_role_input(attention_change)?;
    if stamp_input.old_text != attention_old || stamp_input.new_text != attention_new {
        return Err(invalid(
            "stamp role source text changed while building the role input",
        ));
    }
    let stamp_derived = derive_stamp_mask(&stamp_input)?;
    let stamp_groups = stamp_input
        .fields
        .iter()
        .map(|field| field.group)
        .collect::<Vec<_>>();
    let stamp_comparison = compare_role_masks(
        &stamp_derived,
        attention_old,
        attention_new,
        (
            expected_ranges(&attention_change.old_changed_ranges, "attention old ranges")?,
            expected_ranges(&attention_change.new_changed_ranges, "attention new ranges")?,
        ),
        attention
            .changes
            .iter()
            .filter(|change| change.id == attention_change.id)
            .count(),
        &attention_change.kind,
        &stamp_groups,
    )?;
    let attention_fields = stamp_input
        .fields
        .iter()
        .map(|field| DisjointField {
            name: field.name,
            old_range: range_value(&field.old_range),
            new_range: range_value(&field.new_range),
        })
        .collect::<Vec<_>>();
    let attention_premises = stamp_premises(&stamp_input)?;

    let csf_change = expected_change(csf, "core-expanded-from-five-to-six-functions")?;
    let csf_old = required_quote(&csf_change.old_quote, "CSF old quote")?;
    let csf_new = required_quote(&csf_change.new_quote, "CSF new quote")?;
    let function_input = function_role_input(csf_change)?;
    if function_input.old_text != csf_old || function_input.new_text != csf_new {
        return Err(invalid(
            "function-list role source text changed while building the role input",
        ));
    }
    let function_derived = derive_function_mask(&function_input)?;
    let function_groups = function_input
        .anchors
        .iter()
        .map(|anchor| anchor.group)
        .collect::<Vec<_>>();
    let function_comparison = compare_role_masks(
        &function_derived,
        csf_old,
        csf_new,
        (
            expected_ranges(&csf_change.old_changed_ranges, "CSF old ranges")?,
            expected_ranges(&csf_change.new_changed_ranges, "CSF new ranges")?,
        ),
        csf.changes
            .iter()
            .filter(|change| change.id == csf_change.id)
            .count(),
        &csf_change.kind,
        &function_groups,
    )?;
    let function_premises = function_premises(&function_input)?;

    let list_old = 357..1157;
    let list_new = 303..1570;
    let item_old = 388..460;
    let item_new = 303..303;
    let old_item_text = text_slice(&old_intro.text, &item_old)?;
    if old_item_text != "(1) The Digital Signature Algorithm (DSA) is specified in this Standard." {
        return Err(invalid(
            "DSA old list-item range no longer names the DSA item",
        ));
    }
    if !text_slice(&new_intro.text, &item_new)?.is_empty() {
        return Err(invalid("DSA new list-item deletion boundary is not empty"));
    }

    let full_intro_query = 1611..1718;
    let paragraph_query = 41..148;
    let full_intro_proof =
        domain_proof(&old_intro.text, &new_intro.text, full_intro_query.clone())?;
    if full_intro_proof.certain_changed_positions != 74
        || full_intro_proof.ambiguous_positions != 33
        || full_intro_proof.min_inserted != 103
        || full_intro_proof.max_inserted != 107
    {
        return Err(invalid(
            "DSA full-introduction proof changed from the recorded result",
        ));
    }
    let paragraph_proof = domain_proof(
        &old_paragraph.text,
        &new_paragraph.text,
        paragraph_query.clone(),
    )?;
    if paragraph_proof.certain_changed_positions != 22
        || paragraph_proof.ambiguous_positions != 8
        || paragraph_proof.min_inserted != 26
        || paragraph_proof.max_inserted != 27
    {
        return Err(invalid(format!(
            "DSA paired-paragraph proof changed from the recorded result: changed={} ambiguous={} bounds={}..={}",
            paragraph_proof.certain_changed_positions,
            paragraph_proof.ambiguous_positions,
            paragraph_proof.min_inserted,
            paragraph_proof.max_inserted,
        )));
    }

    Ok(vec![
        RolePolicyResult {
            id: "attention-distribution-stamp",
            role: "distribution_stamp",
            policy: "structure_first",
            relation: "supplied version and date fields are one review event",
            caller_supplied_domain: true,
            hypothesis_only: true,
            diagnostic_only: true,
            event_group: "arxiv-version-date-stamp",
            event_count: unique_group_count(&stamp_groups),
            event_kind: stamp_comparison.derived_event_kind,
            outside_literal_objective: Some(true),
            fields: attention_fields,
            source_ranges: json!({
                "old_domain": {"start": 0, "end": attention_old.chars().count()},
                "new_domain": {"start": 0, "end": attention_new.chars().count()},
            }),
            premises: attention_premises,
            literal_objective_audit: Some(audits.attention.facts.clone()),
            domain_proof: None,
            derived_comparison: Some(stamp_comparison),
        },
        RolePolicyResult {
            id: "dsa-approved-algorithm-list-membership",
            role: "approved_algorithm_list_membership",
            policy: "structure_first",
            relation: "DSA list-item membership is removed",
            caller_supplied_domain: true,
            hypothesis_only: true,
            diagnostic_only: true,
            event_group: "dsa-approved-algorithm-list",
            event_count: unique_group_count(&["dsa-approved-algorithm-list"]),
            event_kind: presence_event_kind(true, false),
            outside_literal_objective: None,
            fields: Vec::new(),
            source_ranges: json!({
                "old_container": range_value(&list_old),
                "new_container": range_value(&list_new),
                "old_item": range_value(&item_old),
                "new_item_boundary": range_value(&item_new),
                "old_membership": true,
                "new_membership": false,
            }),
            premises: vec![
                premise_for_range(
                    "container",
                    Some("approved algorithm list".to_owned()),
                    &old_intro.text,
                    &new_intro.text,
                    Some(&list_old),
                    Some(&list_new),
                    "caller-supplied list container membership domain",
                )?,
                premise_for_range(
                    "DSA",
                    Some("dsa".to_owned()),
                    &old_intro.text,
                    &new_intro.text,
                    Some(&item_old),
                    Some(&item_new),
                    "caller-supplied membership fact",
                )?,
            ],
            literal_objective_audit: None,
            domain_proof: None,
            derived_comparison: None,
        },
        RolePolicyResult {
            id: "dsa-topic-note-full-introduction",
            role: "topic_note",
            policy: "structure_first",
            relation: "supplied full-introduction correspondence",
            caller_supplied_domain: true,
            hypothesis_only: true,
            diagnostic_only: true,
            event_group: "dsa-topic-note",
            event_count: unique_group_count(&["dsa-topic-note"]),
            event_kind: presence_event_kind(false, true),
            outside_literal_objective: Some(false),
            fields: Vec::new(),
            source_ranges: json!({
                "old_domain": {"start": 0, "end": old_intro.descriptor.scalar_length},
                "new_domain": {"start": 0, "end": new_intro.descriptor.scalar_length},
                "new_query": range_value(&full_intro_query),
            }),
            premises: vec![premise_for_range(
                "topic-note-query",
                Some("dsa-topic-note".to_owned()),
                &old_intro.text,
                &new_intro.text,
                None,
                Some(&full_intro_query),
                "caller-supplied full-introduction correspondence",
            )?],
            literal_objective_audit: None,
            domain_proof: Some(full_intro_proof),
            derived_comparison: None,
        },
        RolePolicyResult {
            id: "dsa-topic-note-paired-paragraph",
            role: "topic_note",
            policy: "structure_first",
            relation: "supplied paired-paragraph correspondence",
            caller_supplied_domain: true,
            hypothesis_only: true,
            diagnostic_only: true,
            event_group: "dsa-topic-note",
            event_count: unique_group_count(&["dsa-topic-note"]),
            event_kind: presence_event_kind(true, true),
            outside_literal_objective: Some(false),
            fields: Vec::new(),
            source_ranges: json!({
                "old_domain": {"start": 0, "end": old_paragraph.descriptor.scalar_length},
                "new_domain": {"start": 0, "end": new_paragraph.descriptor.scalar_length},
                "new_query": range_value(&paragraph_query),
            }),
            premises: vec![premise_for_range(
                "topic-note-query",
                Some("dsa-topic-note".to_owned()),
                &old_paragraph.text,
                &new_paragraph.text,
                None,
                Some(&paragraph_query),
                "caller-supplied paired-paragraph correspondence",
            )?],
            literal_objective_audit: None,
            domain_proof: Some(paragraph_proof),
            derived_comparison: None,
        },
        RolePolicyResult {
            id: "csf-core-functions-supplied-sentence",
            role: "supplied_sentence",
            policy: "structure_first",
            relation: "supplied sentence is retained for objective compatibility diagnosis",
            caller_supplied_domain: true,
            hypothesis_only: true,
            diagnostic_only: true,
            event_group: "core-functions-summary",
            event_count: unique_group_count(&function_groups),
            event_kind: function_comparison.derived_event_kind,
            outside_literal_objective: Some(true),
            fields: Vec::new(),
            source_ranges: json!({
                "old_domain": {"start": 0, "end": csf_old.chars().count()},
                "new_domain": {"start": 0, "end": csf_new.chars().count()},
            }),
            premises: function_premises,
            literal_objective_audit: Some(audits.csf.facts.clone()),
            domain_proof: None,
            derived_comparison: Some(function_comparison),
        },
    ])
}

#[derive(Clone, Debug, Serialize)]
struct HistoricalControlGap {
    name: String,
    status: String,
    expected_events: usize,
    accepted_events: usize,
    candidate_events: usize,
    reported_events: usize,
    accepted_event_gap: usize,
    accepted_event_overage: usize,
    recall: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
struct HistoricalGap {
    pair: String,
    source: &'static str,
    expected_events: usize,
    controls: Vec<HistoricalControlGap>,
}

#[derive(Clone, Debug, Deserialize)]
struct HistoricalResult {
    expected: HistoricalExpected,
    controls: Vec<HistoricalControl>,
}

#[derive(Clone, Debug, Deserialize)]
struct HistoricalExpected {
    pair: String,
    changes: usize,
}

#[derive(Clone, Debug, Deserialize)]
struct HistoricalControl {
    name: String,
    status: String,
    changes: usize,
    candidate_changes: usize,
    quality: HistoricalQuality,
}

#[derive(Clone, Debug, Deserialize)]
struct HistoricalQuality {
    reported_changes: usize,
    recall: Option<f64>,
}

fn historical_gap(
    source: &'static str,
    text: &str,
    expected_pair: &str,
) -> ProbeResult<HistoricalGap> {
    let result: HistoricalResult = serde_json::from_str(text)?;
    if result.expected.pair != expected_pair {
        return Err(invalid(format!(
            "historical control pair mismatch: expected {expected_pair}, found {}",
            result.expected.pair
        )));
    }
    if result.controls.is_empty() {
        return Err(invalid(format!(
            "historical control {expected_pair} has no controls"
        )));
    }
    let expected_events = result.expected.changes;
    let controls = result
        .controls
        .into_iter()
        .map(|control| HistoricalControlGap {
            name: control.name,
            status: control.status,
            expected_events,
            accepted_events: control.changes,
            candidate_events: control.candidate_changes,
            reported_events: control.quality.reported_changes,
            accepted_event_gap: expected_events.saturating_sub(control.changes),
            accepted_event_overage: control.changes.saturating_sub(expected_events),
            recall: control.quality.recall,
        })
        .collect();
    Ok(HistoricalGap {
        pair: expected_pair.to_owned(),
        source,
        expected_events,
        controls,
    })
}

#[derive(Clone, Debug, Serialize)]
struct ManifestSummary {
    pair: String,
    annotation: String,
    change_ids: Vec<String>,
}

fn manifest_summary(revision: &ExpectedRevision) -> ManifestSummary {
    ManifestSummary {
        pair: revision.pair.clone(),
        annotation: revision.annotation.clone(),
        change_ids: revision
            .changes
            .iter()
            .map(|change| change.id.clone())
            .collect(),
    }
}

#[derive(Clone, Debug, Serialize)]
struct MaskGuard {
    id: &'static str,
    old: String,
    new: String,
    old_changed_positions: Vec<usize>,
    new_changed_positions: Vec<usize>,
    space_positions: Vec<usize>,
    spaces_preserved: bool,
}

fn mask_guard(
    id: &'static str,
    old_text: &str,
    new_text: &str,
    expected_old: &[usize],
    expected_new: &[usize],
) -> ProbeResult<MaskGuard> {
    let old = chars(old_text);
    let new = chars(new_text);
    let tables = LcsTables::new(&old, &new)?;
    let (old_changed, new_changed) = tables.mandatory_changed(&old, &new);
    let old_positions = old_changed
        .iter()
        .enumerate()
        .filter_map(|(index, changed)| (*changed).then_some(index))
        .collect::<Vec<_>>();
    let new_positions = new_changed
        .iter()
        .enumerate()
        .filter_map(|(index, changed)| (*changed).then_some(index))
        .collect::<Vec<_>>();
    if old_positions != expected_old || new_positions != expected_new {
        return Err(invalid(format!("character mask guard {id} changed")));
    }
    let space_positions = old
        .iter()
        .enumerate()
        .filter_map(|(index, character)| (*character == ' ').then_some(index))
        .collect::<Vec<_>>();
    let new_space_positions = new
        .iter()
        .enumerate()
        .filter_map(|(index, character)| (*character == ' ').then_some(index))
        .collect::<Vec<_>>();
    let spaces_preserved = space_positions.iter().all(|index| !old_changed[*index])
        && new_space_positions.iter().all(|index| !new_changed[*index]);
    if !spaces_preserved {
        return Err(invalid(format!(
            "character mask guard {id} changed a space"
        )));
    }
    Ok(MaskGuard {
        id,
        old: old_text.to_owned(),
        new: new_text.to_owned(),
        old_changed_positions: old_positions,
        new_changed_positions: new_positions,
        space_positions,
        spaces_preserved,
    })
}

#[derive(Clone, Debug, Serialize)]
struct GlyphFixtureCheck {
    id: &'static str,
    old_scalars: usize,
    new_scalars: usize,
    old_glyphs: usize,
    new_glyphs: usize,
    glyphs_per_scalar: bool,
    comparison_completed: bool,
    assessment_present: bool,
    reported_change_events: usize,
    candidate_events: usize,
    unresolved_regions: usize,
    expected_old_changed_ranges: Vec<RangeValue>,
    expected_new_changed_ranges: Vec<RangeValue>,
    emitted_old_changed_ranges: Vec<RangeValue>,
    emitted_new_changed_ranges: Vec<RangeValue>,
    emitted_old_mask_exact: bool,
    emitted_new_mask_exact: bool,
    emitted_old_spaces_preserved: bool,
    emitted_new_spaces_preserved: bool,
    expected_old_source_glyph_ids: Vec<u64>,
    expected_new_source_glyph_ids: Vec<u64>,
    emitted_old_source_glyph_ids: Vec<u64>,
    emitted_new_source_glyph_ids: Vec<u64>,
    source_projection_exact: bool,
    failure: Option<String>,
}

fn scalar_document(text: &str) -> ProbeResult<Document<Glyph>> {
    let mut glyphs = Vec::new();
    glyphs
        .try_reserve_exact(text.chars().count())
        .map_err(|error| invalid(format!("could not allocate scalar glyph fixture: {error}")))?;
    for (index, character) in text.chars().enumerate() {
        let index_u64 = u64::try_from(index).map_err(|_| invalid("fixture glyph id overflow"))?;
        let index_u32 =
            u32::try_from(index).map_err(|_| invalid("fixture render order overflow"))?;
        let x = index as f64 * 6.0;
        glyphs.push(Glyph {
            id: GlyphId(index_u64 + 1),
            text: DecodedText::Mapped(character.to_string()),
            raw_code: character.to_string().into_bytes(),
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x, y: 100.0 },
                max: Vec2 {
                    x: x + 5.0,
                    y: 110.0,
                },
            },
            baseline: Vec2 { x, y: 100.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(1),
            font_size: 10.0,
            render_order: index_u32,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: index_u32,
            },
        });
    }
    Ok(Document::new(glyphs))
}

fn emitted_changed_mask(
    comparison: &pdfdelta_core::diff::Comparison,
    old_length: usize,
    new_length: usize,
) -> ProbeResult<DerivedMask> {
    let mut old_changed = vec![false; old_length];
    let mut new_changed = vec![false; new_length];
    for change in &comparison.changes {
        for occurrence in &change.occurrences {
            if let Some(span) = &occurrence.old_span {
                let range = span.canonical_range.start..span.canonical_range.end;
                if range.end > old_length || range.start > range.end {
                    return Err(invalid("production old span is outside the glyph fixture"));
                }
                old_changed[range].fill(true);
            }
            if let Some(span) = &occurrence.new_span {
                let range = span.canonical_range.start..span.canonical_range.end;
                if range.end > new_length || range.start > range.end {
                    return Err(invalid("production new span is outside the glyph fixture"));
                }
                new_changed[range].fill(true);
            }
        }
    }
    Ok(DerivedMask {
        old_changed,
        new_changed,
    })
}

fn glyph_ids_for_mask(document: &Document<Glyph>, mask: &[bool]) -> ProbeResult<Vec<u64>> {
    if document.items().len() != mask.len() {
        return Err(invalid(
            "glyph projection mask length does not match its fixture",
        ));
    }
    Ok(document
        .items()
        .iter()
        .zip(mask)
        .filter_map(|(glyph, changed)| changed.then_some(glyph.id.0))
        .collect())
}

fn spaces_preserved(text: &str, changed: &[bool]) -> ProbeResult<bool> {
    let values = chars(text);
    if values.len() != changed.len() {
        return Err(invalid(
            "space preservation mask length does not match its text",
        ));
    }
    Ok(values
        .iter()
        .zip(changed)
        .all(|(character, changed)| *character != ' ' || !changed))
}

fn glyph_fixture_check(
    id: &'static str,
    old_text: &str,
    new_text: &str,
) -> ProbeResult<GlyphFixtureCheck> {
    let old = scalar_document(old_text)?;
    let new = scalar_document(new_text)?;
    let old_scalars = old_text.chars().count();
    let new_scalars = new_text.chars().count();
    if old.items().len() != old_scalars || new.items().len() != new_scalars {
        return Err(invalid(format!(
            "glyph fixture {id} is not one glyph per scalar"
        )));
    }
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    let old_values = chars(old_text);
    let new_values = chars(new_text);
    let expected_tables = LcsTables::new(&old_values, &new_values)?;
    let (expected_old_changed, expected_new_changed) =
        expected_tables.mandatory_changed(&old_values, &new_values);
    let emitted = emitted_changed_mask(&comparison, old_scalars, new_scalars)?;
    let expected_old_glyph_ids = glyph_ids_for_mask(&old, &expected_old_changed)?;
    let expected_new_glyph_ids = glyph_ids_for_mask(&new, &expected_new_changed)?;
    let emitted_old_glyph_ids = glyph_ids_for_mask(&old, &emitted.old_changed)?;
    let emitted_new_glyph_ids = glyph_ids_for_mask(&new, &emitted.new_changed)?;
    let emitted_old_mask_exact = emitted.old_changed == expected_old_changed;
    let emitted_new_mask_exact = emitted.new_changed == expected_new_changed;
    let emitted_old_spaces_preserved = spaces_preserved(old_text, &emitted.old_changed)?;
    let emitted_new_spaces_preserved = spaces_preserved(new_text, &emitted.new_changed)?;
    let source_projection_exact = emitted_old_glyph_ids == expected_old_glyph_ids
        && emitted_new_glyph_ids == expected_new_glyph_ids;
    let failure = (!emitted_old_mask_exact
        || !emitted_new_mask_exact
        || !emitted_old_spaces_preserved
        || !emitted_new_spaces_preserved
        || !source_projection_exact)
        .then(|| {
            "production spans did not reproduce the mandatory scalar mask or exact source projection"
                .to_owned()
        });
    Ok(GlyphFixtureCheck {
        id,
        old_scalars,
        new_scalars,
        old_glyphs: old.items().len(),
        new_glyphs: new.items().len(),
        glyphs_per_scalar: true,
        comparison_completed: true,
        assessment_present: comparison.assessment.is_some(),
        reported_change_events: comparison.changes.len(),
        candidate_events: comparison.change_candidates.len(),
        unresolved_regions: comparison.unresolved_regions.len(),
        expected_old_changed_ranges: mask_ranges(&expected_old_changed),
        expected_new_changed_ranges: mask_ranges(&expected_new_changed),
        emitted_old_changed_ranges: mask_ranges(&emitted.old_changed),
        emitted_new_changed_ranges: mask_ranges(&emitted.new_changed),
        emitted_old_mask_exact,
        emitted_new_mask_exact,
        emitted_old_spaces_preserved,
        emitted_new_spaces_preserved,
        expected_old_source_glyph_ids: expected_old_glyph_ids,
        expected_new_source_glyph_ids: expected_new_glyph_ids,
        emitted_old_source_glyph_ids: emitted_old_glyph_ids,
        emitted_new_source_glyph_ids: emitted_new_glyph_ids,
        source_projection_exact,
        failure,
    })
}

#[derive(Clone, Debug, Serialize)]
struct RoleAmbiguityCheck {
    id: &'static str,
    rejected: bool,
    reason: Option<String>,
}

fn function_role_ambiguity_check() -> RoleAmbiguityCheck {
    let text = chars("Identify Identify Protect Detect Respond Recover");
    match locate_function_members(&text, OLD_CORE_FUNCTIONS) {
        Ok(_) => RoleAmbiguityCheck {
            id: "repeated-function-identity",
            rejected: false,
            reason: None,
        },
        Err(error) => RoleAmbiguityCheck {
            id: "repeated-function-identity",
            rejected: true,
            reason: Some(error.to_string()),
        },
    }
}

fn enumerate_binary(length: usize, current: &mut Vec<u8>, output: &mut Vec<Vec<u8>>) {
    if current.len() == length {
        output.push(current.clone());
        return;
    }
    for value in *b"ab" {
        current.push(value);
        enumerate_binary(length, current, output);
        current.pop();
    }
}

fn binary_strings(max_length: usize) -> Vec<Vec<u8>> {
    let mut output = Vec::new();
    for length in 0..=max_length {
        enumerate_binary(length, &mut Vec::new(), &mut output);
    }
    output
}

fn enumerate_paths(
    old: &[u8],
    new: &[u8],
    old_index: usize,
    new_index: usize,
    current: &mut Vec<(usize, usize)>,
    output: &mut HashSet<Vec<(usize, usize)>>,
) {
    if old_index == old.len() || new_index == new.len() {
        output.insert(current.clone());
        return;
    }
    enumerate_paths(old, new, old_index + 1, new_index, current, output);
    enumerate_paths(old, new, old_index, new_index + 1, current, output);
    if old[old_index] == new[new_index] {
        current.push((old_index, new_index));
        enumerate_paths(old, new, old_index + 1, new_index + 1, current, output);
        current.pop();
    }
}

fn brute_force_region_bounds(
    old: &[u8],
    new: &[u8],
    query: Range<usize>,
) -> ProbeResult<(usize, usize)> {
    let mut paths = HashSet::new();
    enumerate_paths(old, new, 0, 0, &mut Vec::new(), &mut paths);
    let best = paths
        .iter()
        .map(Vec::len)
        .max()
        .ok_or_else(|| invalid("path enumeration returned no paths"))?;
    let mut lower = query.end - query.start;
    let mut upper = 0;
    for path in paths.into_iter().filter(|path| path.len() == best) {
        let matched = path
            .iter()
            .filter(|(_, new_index)| query.contains(new_index))
            .count();
        let inserted = query.end - query.start - matched;
        lower = lower.min(inserted);
        upper = upper.max(inserted);
    }
    Ok((lower, upper))
}

#[derive(Clone, Debug, Serialize)]
struct ProofVerification {
    alphabet: &'static str,
    max_length: usize,
    pairs: usize,
    region_queries: usize,
    passed: bool,
}

fn verify_small_claims() -> ProbeResult<ProofVerification> {
    let strings = binary_strings(4);
    let mut pairs = 0;
    let mut region_queries = 0;
    for old in &strings {
        for new in &strings {
            pairs += 1;
            let old_chars = old.iter().map(|value| *value as char).collect::<Vec<_>>();
            let new_chars = new.iter().map(|value| *value as char).collect::<Vec<_>>();
            let tables = LcsTables::new(&old_chars, &new_chars)?;
            let mut paths = HashSet::new();
            enumerate_paths(old, new, 0, 0, &mut Vec::new(), &mut paths);
            let expected_lcs = paths
                .iter()
                .map(Vec::len)
                .max()
                .ok_or_else(|| invalid("path enumeration returned no paths"))?;
            if tables.lcs
                != u32::try_from(expected_lcs).map_err(|_| invalid("test LCS overflow"))?
            {
                return Err(invalid(
                    "bounded LCS table failed exhaustive LCS verification",
                ));
            }
            for start in 0..=new.len() {
                for end in start..=new.len() {
                    let actual = tables.region_insert_bounds(&old_chars, &new_chars, start..end)?;
                    let expected = brute_force_region_bounds(old, new, start..end)?;
                    if actual != expected {
                        return Err(invalid(
                            "bounded region bounds failed exhaustive optimal-path verification",
                        ));
                    }
                    region_queries += 1;
                }
            }
        }
    }
    Ok(ProofVerification {
        alphabet: "ab",
        max_length: 4,
        pairs,
        region_queries,
        passed: true,
    })
}

#[derive(Clone, Debug, Serialize)]
struct RemainderProof {
    size: usize,
    min_additionally_changed: usize,
    max_additionally_changed: usize,
    condition: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct DiscoveryDecision {
    stamp_mask_improves_over_literal_baseline: bool,
    stamp_grouping_improves: bool,
    production_fixture_masks_exact: bool,
    repeated_function_identity_rejected: bool,
    bounded_stamp_candidate_discovery_justified: bool,
    function_identity_discovery_justified: bool,
    automatic_production_matching_enabled: bool,
    reason: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct ProbeReport {
    schema_version: u32,
    experiment: &'static str,
    probe_source_sha256: String,
    diagnostic_only: bool,
    hypothesis_only: bool,
    caller_supplied_domains: bool,
    declared_alignment_policy: &'static str,
    source_inputs: Vec<SourceInput>,
    expected_manifests: Vec<ManifestSummary>,
    annotation_audits: Vec<AnnotationAudit>,
    role_policies: Vec<RolePolicyResult>,
    historical_accepted_event_gaps: Vec<HistoricalGap>,
    character_mask_guards: Vec<MaskGuard>,
    fixture_checks: Vec<GlyphFixtureCheck>,
    proof_verification: ProofVerification,
    dsa_unresolved_remainder_after_74_exact_changed_characters: RemainderProof,
    role_ambiguity_check: RoleAmbiguityCheck,
    discovery_decision: DiscoveryDecision,
    limitations: Vec<&'static str>,
}

fn build_report() -> ProbeResult<ProbeReport> {
    let fips = parse_expected(EXPECTED_FIPS, "nist-fips-186-4-to-5")?;
    let csf = parse_expected(EXPECTED_CSF, "nist-csf-v1-1-to-v2-0")?;
    let attention = parse_expected(EXPECTED_ATTENTION, "arxiv-attention-v6-to-v7")?;
    let attention_audit = annotation_audit(&attention, "arxiv-version-date-stamp")?;
    let csf_audit = annotation_audit(&csf, "core-expanded-from-five-to-six-functions")?;
    if attention_audit.facts.expected_mask_cost != 11
        || attention_audit.facts.scalar_optimal_cost != 7
        || attention_audit.facts.expected_mask_in_policy_solution_set
    {
        return Err(invalid(
            "attention objective audit changed from the recorded result",
        ));
    }
    if csf_audit.facts.expected_mask_cost != 178
        || csf_audit.facts.scalar_optimal_cost != 158
        || csf_audit.facts.expected_mask_in_policy_solution_set
    {
        return Err(invalid(
            "CSF objective audit changed from the recorded result",
        ));
    }

    let old_intro = load_source(
        "dsa-old-introduction",
        "benchmark/realworld/results/structure-claim-probe/inputs/dsa-old-introduction.txt",
        DSA_OLD_INTRO,
        1431,
        "14e567a49d96048a93a60f5c9835d8ddb009578127c0b71808fc55af26f95b55",
    )?;
    let new_intro = load_source(
        "dsa-new-introduction",
        "benchmark/realworld/results/structure-claim-probe/inputs/dsa-new-introduction.txt",
        DSA_NEW_INTRO,
        2509,
        "0a88c6bec0fb2e2408018677c703b9524c6c69ecb5c8868b5c1af9094292a35b",
    )?;
    let old_paragraph = load_source(
        "dsa-old-paragraph",
        "benchmark/realworld/results/structure-claim-probe/inputs/dsa-old-paragraph.txt",
        DSA_OLD_PARAGRAPH,
        261,
        "73944214a506ec6c5d81075574468ecd4dbed3e4162ce62c6eabd0fd14dce676",
    )?;
    let new_paragraph = load_source(
        "dsa-new-paragraph",
        "benchmark/realworld/results/structure-claim-probe/inputs/dsa-new-paragraph.txt",
        DSA_NEW_PARAGRAPH,
        150,
        "e32002b9cfc35d78144cb37373fd69a41f882c9d4702b3aca7de239d828d0764",
    )?;

    let roles = role_policy_results(
        &attention,
        &csf,
        &old_intro,
        &new_intro,
        &old_paragraph,
        &new_paragraph,
        AnnotationAudits {
            attention: &attention_audit,
            csf: &csf_audit,
        },
    )?;
    let full_intro_proof = roles
        .iter()
        .find(|role| role.id == "dsa-topic-note-full-introduction")
        .and_then(|role| role.domain_proof.as_ref())
        .ok_or_else(|| invalid("DSA full-introduction role proof was not retained"))?;
    let remainder = RemainderProof {
        size: 33,
        min_additionally_changed: full_intro_proof.min_inserted.saturating_sub(74),
        max_additionally_changed: full_intro_proof.max_inserted.saturating_sub(74),
        condition: "same supplied introduction domain and same scalar-optimal path set",
    };
    if remainder.min_additionally_changed != 29 || remainder.max_additionally_changed != 33 {
        return Err(invalid(
            "DSA unresolved remainder proof changed from the recorded result",
        ));
    }

    let verification = verify_small_claims()?;
    let character_mask_guards = vec![
        mask_guard("standard-case-fold", "Standard", "standard", &[0], &[0])?,
        mask_guard("punctuation-and-case", ". F", "; f", &[0, 2], &[0, 2])?,
    ];
    let fixture_checks = vec![
        glyph_fixture_check("standard-case-fold", "Standard", "standard")?,
        glyph_fixture_check("punctuation-and-case", ". F", "; f")?,
    ];
    let role_ambiguity_check = function_role_ambiguity_check();
    let stamp_derived = roles
        .iter()
        .find(|role| role.id == "attention-distribution-stamp")
        .and_then(|role| role.derived_comparison.as_ref())
        .ok_or_else(|| invalid("stamp derived comparison was not retained"))?;
    let function_derived = roles
        .iter()
        .find(|role| role.id == "csf-core-functions-supplied-sentence")
        .and_then(|role| role.derived_comparison.as_ref())
        .ok_or_else(|| invalid("function-list derived comparison was not retained"))?;
    let stamp_mask_improves_over_literal_baseline = stamp_derived.mask_improvement;
    let stamp_grouping_improves = stamp_derived.grouping_improvement;
    let production_fixture_masks_exact = fixture_checks.iter().all(|fixture| {
        fixture.emitted_old_mask_exact
            && fixture.emitted_new_mask_exact
            && fixture.emitted_old_spaces_preserved
            && fixture.emitted_new_spaces_preserved
            && fixture.source_projection_exact
    });
    let bounded_stamp_candidate_discovery_justified = stamp_mask_improves_over_literal_baseline;
    let function_identity_discovery_justified = function_derived.mask_improvement
        && function_derived.old_mask.false_positive == 0
        && function_derived.new_mask.false_positive == 0
        && function_derived.grouping_match
        && role_ambiguity_check.rejected;
    let discovery_decision = DiscoveryDecision {
        stamp_mask_improves_over_literal_baseline,
        stamp_grouping_improves,
        production_fixture_masks_exact,
        repeated_function_identity_rejected: role_ambiguity_check.rejected,
        bounded_stamp_candidate_discovery_justified,
        function_identity_discovery_justified,
        automatic_production_matching_enabled: false,
        reason: "The supplied stamp policy improves grouping from two fields to one event, but its derived mask equals the literal-minimal baseline (cost 7; old/new FP 0/0 and FN 2/2), so no automatic candidate-discovery extension is justified; production matching remains disabled.",
    };

    Ok(ProbeReport {
        schema_version: 1,
        experiment: "structure-claim-probe",
        probe_source_sha256: hex_digest(Sha256::digest(PROBE_SOURCE).as_slice()),
        diagnostic_only: true,
        hypothesis_only: true,
        caller_supplied_domains: true,
        declared_alignment_policy: "literal_minimal",
        source_inputs: vec![
            old_intro.descriptor,
            new_intro.descriptor,
            old_paragraph.descriptor,
            new_paragraph.descriptor,
        ],
        expected_manifests: vec![
            manifest_summary(&fips),
            manifest_summary(&csf),
            manifest_summary(&attention),
        ],
        annotation_audits: vec![attention_audit, csf_audit],
        role_policies: roles,
        historical_accepted_event_gaps: vec![
            historical_gap(
                "benchmark/realworld/results/issue20-order-experiment/order-controls-source-fixed/nist-fips-186-4-to-5.json",
                HISTORY_FIPS,
                "nist-fips-186-4-to-5",
            )?,
            historical_gap(
                "benchmark/realworld/results/issue20-order-experiment/order-controls-source-fixed/nist-csf-v1-1-to-v2-0.json",
                HISTORY_CSF,
                "nist-csf-v1-1-to-v2-0",
            )?,
            historical_gap(
                "benchmark/realworld/results/issue20-order-experiment/order-controls-source-fixed/arxiv-attention-v6-to-v7.json",
                HISTORY_ATTENTION,
                "arxiv-attention-v6-to-v7",
            )?,
        ],
        character_mask_guards,
        fixture_checks,
        proof_verification: verification,
        dsa_unresolved_remainder_after_74_exact_changed_characters: remainder,
        role_ambiguity_check,
        discovery_decision,
        limitations: vec![
            "All role domains and source correspondences are caller-supplied diagnostic hypotheses; the stamp policy improves grouping only and does not improve the literal mask.",
            "Production fixture results are derived from emitted spans and exact source projection; scalar mask guards alone do not establish production correctness.",
            "Automatic glyph/PDF role discovery is not implemented because the supplied policies did not improve fixed masks without false positives.",
            "The probe does not change production candidate selection or enable unconditional role matching.",
            "The literal-minimal objective remains separate from structure-first event grouping.",
        ],
    })
}

fn main() -> ProbeResult<()> {
    let report = build_report()?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer_pretty(&mut writer, &report)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        EXPECTED_ATTENTION, EXPECTED_CSF, EXPECTED_IRS, MaskScore, combined_mask_gap, event_kind,
        expected_change, literal_mandatory_evidence, parse_expected, required_quote,
    };

    fn literal_facts(manifest: &str, pair: &str, change_id: &str) -> super::EditFacts {
        let revision = parse_expected(manifest, pair).expect("manifest should parse");
        let change = expected_change(&revision, change_id).expect("change should exist");
        let old = required_quote(&change.old_quote, "old quote").expect("old quote required");
        let new = required_quote(&change.new_quote, "new quote").expect("new quote required");
        literal_mandatory_evidence(old, new)
            .expect("literal evidence should compute")
            .1
    }

    #[test]
    fn empty_mask_is_not_an_improvement() {
        let baseline = MaskScore {
            true_positive: 4,
            false_positive: 0,
            false_negative: 2,
        };
        let empty = MaskScore {
            true_positive: 0,
            false_positive: 0,
            false_negative: 6,
        };

        assert!(combined_mask_gap(&empty, &empty) > combined_mask_gap(&baseline, &baseline));
    }

    #[test]
    fn csf_mandatory_positions_are_an_incomplete_witness() {
        let facts = literal_facts(
            EXPECTED_CSF,
            "nist-csf-v1-1-to-v2-0",
            "core-expanded-from-five-to-six-functions",
        );

        assert_eq!(facts.changed_count, 147);
        assert_eq!(facts.optimal_edit_cost, 158);
        assert!(!facts.complete_edit_witness);
        assert!(!facts.minimal_complete_edit_witness);
        assert!(facts.changed_unlocalized);
    }

    #[test]
    fn attention_mandatory_positions_are_a_complete_seven_edit_witness() {
        let facts = literal_facts(
            EXPECTED_ATTENTION,
            "arxiv-attention-v6-to-v7",
            "arxiv-version-date-stamp",
        );

        assert_eq!(facts.changed_count, 7);
        assert_eq!(facts.optimal_edit_cost, 7);
        assert!(facts.complete_edit_witness);
        assert!(facts.minimal_complete_edit_witness);
        assert!(!facts.changed_unlocalized);
    }

    #[test]
    fn nonminimal_mask_is_complete_but_not_minimal() {
        let old = super::chars("a");
        let new = super::chars("a");
        let mask = super::DerivedMask {
            old_changed: vec![true],
            new_changed: vec![true],
        };
        let facts = super::edit_facts(&old, &new, &mask, 0).expect("edit facts should compute");

        assert_eq!(facts.changed_count, 2);
        assert_eq!(facts.optimal_edit_cost, 0);
        assert!(facts.complete_edit_witness);
        assert!(!facts.minimal_complete_edit_witness);
        assert!(!facts.changed_unlocalized);
    }

    #[test]
    fn irs_footer_is_a_complete_seventeen_edit_witness() {
        let facts = literal_facts(
            EXPECTED_IRS,
            "irs-form-1040-2024-to-2025",
            "footer-form-year-stamp",
        );

        assert_eq!(facts.changed_count, 17);
        assert_eq!(facts.optimal_edit_cost, 17);
        assert!(facts.complete_edit_witness);
        assert!(!facts.changed_unlocalized);
    }

    #[test]
    fn repeated_insertion_is_changed_but_unlocalized() {
        let (mask, facts) =
            literal_mandatory_evidence("a", "aa").expect("literal evidence should compute");

        assert_eq!(facts.changed_count, 0);
        assert_eq!(facts.optimal_edit_cost, 1);
        assert!(!facts.complete_edit_witness);
        assert!(facts.changed_unlocalized);
        assert_eq!(
            event_kind(&mask.old_changed, &mask.new_changed, 1, 2, 1),
            "insertion"
        );
    }
}
