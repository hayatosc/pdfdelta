//! Page-local catalog/form correspondence with independently bounded source lines.

use std::collections::BTreeMap;

use crate::{Result, alignment::BlockSeparator, layout::BlockRole, normalize::BlockText};

use super::{SentenceRecoveryInput, Side, charge_work, span_has_source_issues, views::LocalDomain};

struct Footer {
    page: u32,
    role: BlockRole,
    catalog: String,
    form: String,
    prefix: String,
    span: super::TextSpan,
}

/// Pairs a terminal horizontal line only when its catalog/form identity is
/// unique on the page and its preceding source context is unchanged.
pub(super) fn discover(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    remaining: &mut usize,
    limit: usize,
) -> Result<Vec<LocalDomain>> {
    let old = candidates(sides[0], recovery, remaining, limit)?;
    let new = candidates(sides[1], recovery, remaining, limit)?;
    if *remaining == 0 {
        return Ok(Vec::new());
    }
    let mut domains = Vec::new();
    for before in old {
        for after in &new {
            if !charge_work(
                remaining,
                before
                    .prefix
                    .len()
                    .saturating_add(after.prefix.len())
                    .saturating_add(1),
            ) {
                return Ok(domains);
            }
            if before.page != after.page
                || !before.role.is_alignment_compatible(after.role)
                || before.catalog != after.catalog
                || before.form != after.form
                || before.prefix != after.prefix
            {
                continue;
            }
            if domains.len() == limit {
                return Ok(domains);
            }
            domains.push(LocalDomain {
                old_span: before.span.clone(),
                new_span: after.span.clone(),
            });
        }
    }
    Ok(domains)
}

fn candidates(
    side: &Side<'_>,
    recovery: SentenceRecoveryInput<'_>,
    remaining: &mut usize,
    limit: usize,
) -> Result<Vec<Footer>> {
    let mut pages = BTreeMap::<u32, Vec<usize>>::new();
    for (index, block) in side.blocks.iter().enumerate() {
        if !charge_work(remaining, block.pages.len().saturating_add(1)) {
            return Ok(Vec::new());
        }
        for page in &block.pages {
            pages.entry(*page).or_default().push(index);
        }
    }
    let mut result = Vec::new();
    for (page, indices) in pages {
        let work = indices.iter().fold(0usize, |work, &index| {
            work.saturating_add(side.canonical[index].len())
                .saturating_add(1)
        });
        if !charge_work(remaining, work.saturating_mul(4)) {
            return Ok(Vec::new());
        }
        // Missing page-local geometry can hide competing material below the
        // proposed footer. Multi-page blocks need a finer source view first.
        if indices.iter().any(|&index| {
            let block = &side.blocks[index];
            block.pages != [page]
                || block.position_signatures.is_none()
                || block.font_size_signatures.is_none()
        }) {
            continue;
        }
        let lowest = indices
            .iter()
            .flat_map(|&index| {
                side.blocks[index]
                    .position_signatures
                    .as_ref()
                    .expect("page geometry was checked above")
                    .iter()
                    .map(|p| p.baseline().y)
            })
            .min_by(f64::total_cmp);
        let Some(lowest) = lowest else {
            continue;
        };
        let size = indices
            .iter()
            .flat_map(|&index| {
                side.blocks[index]
                    .font_size_signatures
                    .as_ref()
                    .expect("page geometry was checked above")
                    .iter()
                    .flat_map(|s| s.values())
            })
            .min_by(f64::total_cmp);
        let Some(size) = size else {
            continue;
        };
        // Source baselines of different fonts in one line can differ. Half
        // the smallest page font size admits the measured IRS footer while
        // keeping the adjacent form row outside this terminal band.
        let band = size * 0.5;
        let mut members = Vec::new();
        let mut valid = true;
        for &index in &indices {
            let block = &side.blocks[index];
            let positions = block
                .position_signatures
                .as_ref()
                .expect("page geometry was checked above");
            if !positions.iter().any(|p| p.baseline().y - lowest <= band) {
                continue;
            }
            if !single_horizontal_band(block, lowest, band) {
                valid = false;
                break;
            }
            members.push(index);
        }
        if !valid || members.is_empty() {
            continue;
        }
        // This local order is certified by complete, horizontal source glyph
        // geometry. It does not promote the page's global reading order.
        let sort_work = members
            .len()
            .saturating_mul(members.len().ilog2() as usize + 1);
        if !charge_work(remaining, sort_work) {
            return Ok(Vec::new());
        }
        members.sort_unstable_by(|&a, &b| {
            side.blocks[a]
                .position_signatures
                .as_ref()
                .expect("page geometry was checked above")[0]
                .baseline()
                .x
                .total_cmp(
                    &side.blocks[b]
                        .position_signatures
                        .as_ref()
                        .expect("page geometry was checked above")[0]
                        .baseline()
                        .x,
                )
        });
        if members.windows(2).any(|pair| {
            let a = &side.blocks[pair[0]];
            let b = &side.blocks[pair[1]];
            !a.role.is_alignment_compatible(b.role)
                || a.position_signatures
                    .as_ref()
                    .expect("page geometry was checked above")
                    .last()
                    .expect("footer members have nonempty horizontal positions")
                    .baseline()
                    .x
                    > b.position_signatures
                        .as_ref()
                        .expect("page geometry was checked above")
                        .first()
                        .expect("footer members have nonempty horizontal positions")
                        .baseline()
                        .x
        }) {
            continue;
        }
        let blocks = members
            .iter()
            .map(|&index| side.blocks[index].block)
            .collect::<Vec<_>>();
        let group =
            side.canonical_group(&blocks, (blocks.len() > 1).then_some(BlockSeparator::Space));
        let text = group
            .tokens
            .iter()
            .map(|token| token.as_scalar())
            .collect::<Option<String>>();
        let Some(text) = text else {
            continue;
        };
        let words = text.split_whitespace().collect::<Vec<_>>();
        let identities = identities(&words);
        let [(key, catalog, form)] = identities.as_slice() else {
            continue;
        };
        let prefix = words[..*key].join(" ");
        if prefix.chars().count() < recovery.min_tokens || !prefix.chars().any(char::is_alphabetic)
        {
            continue;
        }
        let page_words = indices
            .iter()
            .flat_map(|&index| side.blocks[index].canonical.text.split_whitespace())
            .collect::<Vec<_>>();
        if catalog_count(&page_words, catalog) != 1 {
            continue;
        }
        let span = group.full_span();
        if span_has_source_issues(side, &span, remaining)? {
            continue;
        }
        if result.len() == limit {
            break;
        }
        result.push(Footer {
            page,
            role: side.blocks[members[0]].role,
            catalog: (*catalog).to_owned(),
            form: (*form).to_owned(),
            prefix,
            span,
        });
    }
    Ok(result)
}

fn single_horizontal_band(block: &BlockText, bottom: f64, band: f64) -> bool {
    let Some(positions) = block.position_signatures.as_ref() else {
        return false;
    };
    !positions.is_empty()
        && block.line_breaks.as_ref().is_some_and(Vec::is_empty)
        && positions.iter().all(|p| {
            let d = p.direction();
            d.x > 0.0 && d.y == 0.0 && p.baseline().y - bottom <= band
        })
        && positions
            .windows(2)
            .all(|pair| pair[0].baseline().x <= pair[1].baseline().x)
}

fn identities<'a>(words: &[&'a str]) -> Vec<(usize, &'a str, &'a str)> {
    words
        .windows(5)
        .enumerate()
        .filter_map(|(index, words)| {
            (words[0] == "Cat."
                && words[1] == "No."
                && words[3] == "Form"
                && identifier(words[2])
                && identifier(words[4]))
            .then_some((index, words[2], words[4]))
        })
        .collect()
}

fn catalog_count(words: &[&str], catalog: &str) -> usize {
    words
        .windows(3)
        .filter(|words| words == &["Cat.", "No.", catalog])
        .count()
}

fn identifier(word: &str) -> bool {
    !word.is_empty()
        && word.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        && word.bytes().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_and_form_identity_is_generic_and_duplicates_are_visible() {
        let words = "See the separate instructions. Cat. No. 98Z76 Form 4567 (2030)"
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(identities(&words), [(4, "98Z76", "4567")]);
        assert_eq!(catalog_count(&words, "98Z76"), 1);
        let duplicate = words.iter().chain(&words).copied().collect::<Vec<_>>();
        assert_eq!(catalog_count(&duplicate, "98Z76"), 2);
        assert!(identities(&["Cat.", "No.", "unknown", "Form", "4567"]).is_empty());
    }
}
