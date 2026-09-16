//! One-sided terminal-line views; matching and acceptance belong to the common solver.

use std::collections::BTreeMap;

use crate::{Result, alignment::BlockSeparator, layout::BlockRole, normalize::BlockText};

pub(crate) struct FooterCandidate {
    pub page: u32,
    pub role: BlockRole,
    pub catalog: String,
    pub form: String,
    pub prefix: String,
    pub members: Vec<usize>,
}

pub(crate) fn identity(
    role: BlockRole,
    catalog: &str,
    form: &str,
    prefix: &str,
) -> super::IdentityKey {
    super::IdentityKey {
        namespace: format!("terminal-catalog-form-v1/{role:?}"),
        value: format!(
            "{}:{}{}:{}{}:{}",
            catalog.len(),
            catalog,
            form.len(),
            form,
            prefix.len(),
            prefix
        ),
    }
}

pub(crate) struct CandidateSearch {
    pub complete: bool,
    pub work_limited: bool,
}

fn charge_work(remaining: &mut usize, count: usize) -> bool {
    if let Some(next) = remaining.checked_sub(count) {
        *remaining = next;
        true
    } else {
        *remaining = 0;
        false
    }
}

pub(crate) fn candidates(
    blocks: &[BlockText],
    min_prefix_tokens: usize,
    remaining: &mut usize,
    limit: usize,
    discovery: &mut CandidateSearch,
) -> Result<Vec<FooterCandidate>> {
    let mut pages = BTreeMap::<u32, Vec<usize>>::new();
    for (index, block) in blocks.iter().enumerate() {
        if !charge_work(remaining, block.pages.len().saturating_add(1)) {
            discovery.complete = false;
            discovery.work_limited = true;
            return Ok(Vec::new());
        }
        for page in &block.pages {
            pages.entry(*page).or_default().push(index);
        }
    }
    let mut result = Vec::new();
    for (page, indices) in pages {
        let work = indices.iter().fold(0usize, |work, &index| {
            work.saturating_add(
                blocks[index]
                    .canonical
                    .text
                    .len()
                    .saturating_add(blocks[index].canonical.unmapped.len()),
            )
            .saturating_add(1)
        });
        if !charge_work(remaining, work.saturating_mul(4)) {
            discovery.complete = false;
            discovery.work_limited = true;
            return Ok(Vec::new());
        }
        // Missing page-local geometry can hide competing material below the
        // proposed footer. Multi-page blocks and missing line-boundary
        // evidence need a finer source view first.
        if indices.iter().any(|&index| {
            let block = &blocks[index];
            block.pages != [page]
                || block.position_signatures.is_none()
                || block.font_size_signatures.is_none()
                || block.line_breaks.is_none()
        }) {
            discovery.complete = false;
            continue;
        }
        let lowest = indices
            .iter()
            .flat_map(|&index| {
                blocks[index]
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
                blocks[index]
                    .font_size_signatures
                    .as_ref()
                    .expect("page geometry was checked above")
                    .iter()
                    .flat_map(super::super::normalize::FontSizeSignature::values)
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
            let block = &blocks[index];
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
            discovery.complete = false;
            discovery.work_limited = true;
            return Ok(Vec::new());
        }
        members.sort_unstable_by(|&a, &b| {
            blocks[a]
                .position_signatures
                .as_ref()
                .expect("page geometry was checked above")[0]
                .baseline()
                .x
                .total_cmp(
                    &blocks[b]
                        .position_signatures
                        .as_ref()
                        .expect("page geometry was checked above")[0]
                        .baseline()
                        .x,
                )
        });
        if members.windows(2).any(|pair| {
            let a = &blocks[pair[0]];
            let b = &blocks[pair[1]];
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
        let mut tokens = Vec::new();
        for &index in &members {
            let next = blocks[index].canonical.comparable_tokens()?;
            if tokens.is_empty() {
                tokens.extend(next);
            } else {
                BlockSeparator::Space.append(&mut tokens, &next);
            }
        }
        let Some(text) = tokens
            .iter()
            .map(super::super::normalize::ComparableToken::as_scalar)
            .collect::<Option<String>>()
        else {
            continue;
        };
        let words = text.split_whitespace().collect::<Vec<_>>();
        let identities = identities(&words);
        let [(key, catalog, form)] = identities.as_slice() else {
            continue;
        };
        let prefix = words[..*key].join(" ");
        if prefix.chars().count() < min_prefix_tokens || !prefix.chars().any(char::is_alphabetic) {
            continue;
        }
        let page_words = indices
            .iter()
            .flat_map(|&index| blocks[index].canonical.text.split_whitespace())
            .collect::<Vec<_>>();
        if catalog_count(&page_words, catalog) != 1 {
            continue;
        }
        if result.len() == limit {
            discovery.complete = false;
            discovery.work_limited = true;
            break;
        }
        result.push(FooterCandidate {
            page,
            role: blocks[members[0]].role,
            catalog: (*catalog).to_owned(),
            form: (*form).to_owned(),
            prefix,
            members,
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

impl super::DocumentGraph {
    pub(super) fn add_native_footer_views(
        &mut self,
        blocks: &[BlockText],
        block_start: usize,
        options: crate::pipeline::PipelineOptions,
        limits: super::GraphLimits,
        tokens_used: &mut usize,
        references_used: &mut usize,
    ) -> Result<()> {
        use super::{
            EdgeKind, GraphEdge, GraphNode, NodeContent, NodeId, NodeKind, TextNormalization,
            TextView, ViewBasis,
        };
        use crate::{model::PageId, normalize::ComparableToken};
        if !blocks
            .iter()
            .any(|block| block.canonical.text.contains("Cat."))
        {
            return Ok(());
        }
        let mut search = CandidateSearch {
            complete: true,
            work_limited: false,
        };
        let mut remaining = options.diff.max_assessment_work;
        let candidates = candidates(
            blocks,
            options.alignment.anchor_min_tokens,
            &mut remaining,
            limits.max_nodes,
            &mut search,
        )?;
        if search.work_limited {
            return Err(crate::Error::LimitExceeded {
                resource: "native footer candidate work",
                limit: options.diff.max_assessment_work,
            });
        }
        if !search.complete {
            self.relations_complete = false;
            return Ok(());
        }
        for candidate in candidates {
            let identity = identity(
                candidate.role,
                &candidate.catalog,
                &candidate.form,
                &candidate.prefix,
            );
            if candidate.members.len() == 1 {
                let node = &mut self.nodes[block_start + candidate.members[0]];
                node.kind = NodeKind::Footer;
                node.identity = Some(identity);
                continue;
            }
            let mut view = TextView {
                tokens: Vec::new(),
                origins: Vec::new(),
                source_backed: Vec::new(),
                normalization: TextNormalization::Exact,
            };
            let mut sources = std::collections::BTreeSet::new();
            for index in candidate.members {
                let node = &self.nodes[block_start + index];
                let NodeContent::Text { view: next } = &node.content else {
                    return Err(super::evidence::invalid("native footer member is not text"));
                };
                if next.normalization != TextNormalization::Exact {
                    view.normalization = next.normalization.clone();
                }
                let separator = !view.tokens.is_empty()
                    && view.tokens.last() != Some(&ComparableToken::Scalar(' '))
                    && next.tokens.first() != Some(&ComparableToken::Scalar(' '));
                super::graph::charge(
                    tokens_used,
                    next.tokens.len().saturating_add(usize::from(separator)),
                    limits.max_tokens,
                    "graph text tokens",
                )?;
                if separator {
                    let origins = view
                        .origins
                        .last()
                        .into_iter()
                        .flatten()
                        .chain(next.origins.first().into_iter().flatten())
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>();
                    super::graph::charge(
                        references_used,
                        origins.len(),
                        limits.max_references,
                        "graph references",
                    )?;
                    view.tokens.push(ComparableToken::Scalar(' '));
                    view.origins.push(origins.into_iter().collect());
                    view.source_backed.push(false);
                }
                for origins in &next.origins {
                    super::graph::charge(
                        references_used,
                        origins.len(),
                        limits.max_references,
                        "graph references",
                    )?;
                }
                view.tokens.extend_from_slice(&next.tokens);
                view.origins.extend_from_slice(&next.origins);
                view.source_backed.extend_from_slice(&next.source_backed);
                sources.extend(node.sources.iter().copied());
            }
            super::evidence::bounded(
                self.nodes.len().saturating_add(1),
                limits.max_nodes,
                "graph nodes",
            )?;
            super::evidence::bounded(
                self.edges.len().saturating_add(1),
                limits.max_edges,
                "graph edges",
            )?;
            let id = NodeId(self.nodes.len() as u64);
            self.nodes.push(GraphNode {
                id,
                kind: NodeKind::Footer,
                pages: vec![PageId(candidate.page)],
                sources: sources.into_iter().collect(),
                identity: Some(identity),
                basis: ViewBasis::NativeLayout,
                content: NodeContent::Text { view },
            });
            self.edges.push(GraphEdge {
                from: NodeId(0),
                to: id,
                kind: EdgeKind::Contains,
                sources: Vec::new(),
                basis: ViewBasis::NativeLayout,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::BlockId,
        model::Vec2,
        normalize::{ComparableToken, FontSizeSignature, MappedText, PositionSignature},
    };

    fn terminal_block(line_breaks: Option<Vec<usize>>) -> BlockText {
        let text = "Cat. No. 1234";
        let tokens = text
            .chars()
            .map(ComparableToken::Scalar)
            .collect::<Vec<_>>();
        let sizes = tokens
            .iter()
            .map(|_| FontSizeSignature::new(&[10.0]).expect("single size"))
            .collect();
        let positions = tokens
            .iter()
            .enumerate()
            .map(|(index, _)| {
                PositionSignature::new(
                    Vec2 {
                        x: 10.0 + index as f64,
                        y: 10.0,
                    },
                    Vec2 { x: 1.0, y: 0.0 },
                )
                .expect("horizontal position")
            })
            .collect();
        BlockText {
            block: BlockId(1),
            role: BlockRole::RepeatedFooter,
            raw: MappedText {
                text: text.to_owned(),
                source_map: Vec::new(),
                unmapped: Vec::new(),
            },
            canonical: MappedText {
                text: text.to_owned(),
                source_map: Vec::new(),
                unmapped: Vec::new(),
            },
            matching: text.to_owned(),
            matching_tokens: tokens,
            numeric_mask_applied: false,
            normalization_events: Vec::new(),
            issues: Vec::new(),
            pages: vec![0],
            font_size_signatures: Some(sizes),
            position_signatures: Some(positions),
            line_breaks,
            page_breaks: Some(Vec::new()),
        }
    }

    #[test]
    fn missing_line_break_evidence_keeps_footer_discovery_incomplete() {
        let mut discovery = CandidateSearch {
            complete: true,
            work_limited: false,
        };
        let mut remaining = 1_000_000;
        let found = candidates(
            std::slice::from_ref(&terminal_block(None)),
            1,
            &mut remaining,
            64,
            &mut discovery,
        )
        .expect("valid terminal block");
        assert!(found.is_empty());
        assert!(!discovery.complete);
    }

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
