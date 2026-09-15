//! Bounded, non-owning suggestions for native paragraph merges and splits.
//! Literal word pairs rank views only; neither tokenization nor greedy adjacency
//! establishes correspondence, interval closure, or reading order.

use std::collections::{BTreeMap, BTreeSet};

use crate::Result;

use super::{
    DocumentComparisonLimits, DocumentView, EdgeKind, GraphNode, InterpretationStatus,
    LocalViewComparison, NodeContent, NodeId, SourceRef, TextScopeReview, TypedOperation,
    ViewBasis, spend,
};
use crate::document::{
    CorrespondenceScope, DocumentViewComparison, NodeKind,
    groups::overlaps,
    matching::source_children,
    operations::{native_count_change, private_use_scalar},
};

type Grams = BTreeMap<(String, String), usize>;

struct Paragraph<'a> {
    node: &'a GraphNode,
    text: String,
    grams: Grams,
    count: usize,
}

struct Population<'a> {
    paragraphs: BTreeMap<NodeId, Paragraph<'a>>,
    next: BTreeMap<NodeId, NodeId>,
    previous: BTreeMap<NodeId, NodeId>,
}

fn grams(text: &str) -> Grams {
    let mut words = text.split_whitespace();
    let mut result = Grams::new();
    let Some(mut previous) = words.next() else {
        return result;
    };
    for word in words {
        *result.entry((previous.into(), word.into())).or_default() += 1;
        previous = word;
    }
    result
}

// This supplier is deliberately limited to sentence-shaped prose. Incomplete
// line fragments and TOC titles can otherwise rank above their actual paragraph
// counterparts. This is a display heuristic, never a sentence-boundary proof.
fn prose(text: &str) -> bool {
    let text = text.trim();
    if !text.ends_with('.') {
        return false;
    }
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if first.chars().next().is_some_and(char::is_uppercase) {
        return true;
    }
    let label = first.strip_suffix('.').unwrap_or(first);
    let enumerator = label.chars().count() == 1 && label.chars().all(char::is_alphabetic)
        || label.chars().all(|c| c.is_ascii_digit() || c == '.');
    enumerator
        && words
            .next()
            .and_then(|word| word.chars().next())
            .is_some_and(char::is_uppercase)
}

fn starts_after_fragment(population: &Population<'_>, first: NodeId) -> bool {
    population.previous.get(&first).is_some_and(|id| {
        let previous = population.paragraphs[id].text.trim();
        !previous.is_empty() && !previous.ends_with(['.', '?', '!', ':'])
    })
}

// Local edge overlap declines attractive groups with unrelated leading/trailing
// text. In particular, a merged native node can contain a neighboring definition
// that cannot be removed while retaining that node's original text.
fn supported_edges(old: &str, new: &str) -> bool {
    let edge_grams = |text: &str, end: bool| {
        let words: Vec<_> = text.split_whitespace().collect();
        let edge = if end {
            &words[words.len().saturating_sub(8)..]
        } else {
            &words[..8.min(words.len())]
        };
        grams(&edge.join(" "))
    };
    [false, true].into_iter().all(|end| {
        let a = edge_grams(old, end);
        let b = edge_grams(new, end);
        a.keys()
            .filter(|gram| b.contains_key(*gram))
            .take(2)
            .count()
            == 2
    })
}

fn population<'a>(
    view: DocumentView<'a>,
    root: NodeId,
    limits: DocumentComparisonLimits,
    remaining: &mut usize,
) -> Result<Option<Population<'a>>> {
    if spend(
        remaining,
        view.graph
            .nodes
            .len()
            .saturating_add(view.graph.edges.len()),
    )
    .is_none()
    {
        return Ok(None);
    }
    let mut paragraphs = BTreeMap::new();
    for node in source_children(view.graph, root, limits.matching.channels)? {
        if node.kind != NodeKind::Paragraph
            || node.basis != ViewBasis::NativeLayout
            || node.identity.is_some()
            || node.pages.len() != 1
            || node.sources.is_empty()
        {
            continue;
        }
        let NodeContent::Text { view } = &node.content else {
            continue;
        };
        if spend(
            remaining,
            view.tokens
                .len()
                .saturating_mul(4)
                .saturating_add(node.sources.len()),
        )
        .is_none()
        {
            return Ok(None);
        }
        if !view.has_validated_normalization()
            || view
                .tokens
                .iter()
                .any(|token| token.as_scalar().is_none_or(private_use_scalar))
            || node
                .sources
                .iter()
                .any(|source| !matches!(source, SourceRef::Native { .. }))
        {
            continue;
        }
        let Some(text) = view.display_text() else {
            continue;
        };
        let grams = grams(&text);
        let count = grams.values().sum();
        paragraphs.insert(
            node.id,
            Paragraph {
                node,
                text,
                grams,
                count,
            },
        );
    }
    // A branch, missing member, or page transition terminates this display path.
    // Even a unique layout edge supplies only an inferred order.
    let mut outgoing = BTreeMap::<_, BTreeSet<_>>::new();
    let mut incoming = BTreeMap::<_, BTreeSet<_>>::new();
    for edge in &view.graph.edges {
        if edge.kind == EdgeKind::Precedes {
            outgoing.entry(edge.from).or_default().insert(edge.to);
            incoming.entry(edge.to).or_default().insert(edge.from);
        }
    }
    let mut next = BTreeMap::new();
    let mut previous = BTreeMap::new();
    for (from, successors) in outgoing {
        if successors.len() != 1 {
            continue;
        }
        let to = *successors.first().expect("one successor");
        if incoming[&to].len() != 1 {
            continue;
        }
        if let (Some(a), Some(b)) = (paragraphs.get(&from), paragraphs.get(&to))
            && a.node.pages == b.node.pages
        {
            next.insert(from, to);
            previous.insert(to, from);
        }
    }
    Ok(Some(Population {
        paragraphs,
        next,
        previous,
    }))
}

fn score(a: &Grams, b: &Grams) -> u32 {
    let common: usize = a
        .iter()
        .map(|(gram, count)| (*count).min(*b.get(gram).unwrap_or(&0)))
        .sum();
    let total: usize = a.values().chain(b.values()).sum();
    if total == 0 {
        return 0;
    }
    ((common as u128 * 2_000_000) / total as u128) as u32
}

struct Suggestion {
    old: Vec<NodeId>,
    new: Vec<NodeId>,
    score: u32,
}

fn discover(
    whole: &Population<'_>,
    parts: &Population<'_>,
    reverse: bool,
    limits: DocumentComparisonLimits,
    remaining: &mut usize,
    output: &mut Vec<Suggestion>,
) {
    let mut index = BTreeMap::<_, Vec<_>>::new();
    let mut entries = 0;
    for (&id, paragraph) in &parts.paragraphs {
        for (gram, &count) in &paragraph.grams {
            if entries == limits.text.max_feature_entries || spend(remaining, 1).is_none() {
                return;
            }
            entries += 1;
            index.entry(gram).or_default().push((id, count));
        }
    }
    for paragraph in whole.paragraphs.values() {
        if !prose(&paragraph.text) || starts_after_fragment(whole, paragraph.node.id) {
            continue;
        }
        let mut overlap = BTreeMap::<NodeId, usize>::new();
        for (gram, &count) in &paragraph.grams {
            if let Some(postings) = index.get(gram) {
                if spend(remaining, postings.len()).is_none() {
                    return;
                }
                for &(id, other) in postings {
                    *overlap.entry(id).or_default() += count.min(other);
                }
            }
        }
        let mut seeds: Vec<_> = overlap
            .into_iter()
            .map(|(id, common)| {
                let total = paragraph.count + parts.paragraphs[&id].count;
                (((common as u128 * 2_000_000) / total as u128) as u32, id)
            })
            .collect();
        seeds.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let Some(&(initial, seed)) = seeds.first() else {
            continue;
        };
        // Tied seeds have no preferred starting correspondence. Identical singles
        // already explain the text without an inferred split/merge suggestion.
        if initial == 1_000_000 || seeds.get(1).is_some_and(|other| other.0 == initial) {
            continue;
        }
        let mut group = vec![seed];
        let mut best = initial;
        while group.len() < limits.matching.max_group_nodes {
            let mut extension = None;
            for (id, prepend) in [
                (parts.previous.get(&group[0]), true),
                (parts.next.get(group.last().expect("nonempty group")), false),
            ] {
                let Some(&id) = id else { continue };
                if group.contains(&id) {
                    continue;
                }
                let mut candidate = group.clone();
                if prepend {
                    candidate.insert(0, id);
                } else {
                    candidate.push(id);
                }
                let length = candidate.iter().fold(0usize, |sum, id| {
                    sum.saturating_add(parts.paragraphs[id].text.len())
                });
                if length > limits.local.max_tokens
                    || spend(remaining, length.saturating_mul(4)).is_none()
                {
                    return;
                }
                let text: String = candidate
                    .iter()
                    .map(|id| parts.paragraphs[id].text.as_str())
                    .collect();
                let value = score(&paragraph.grams, &grams(&text));
                if value > best
                    && extension
                        .as_ref()
                        .is_none_or(|(_, previous)| value > *previous)
                {
                    extension = Some((candidate, value));
                }
            }
            let Some((candidate, value)) = extension else {
                break;
            };
            group = candidate;
            best = value;
        }
        // Display-ranking cutoffs require substantial overlap and improvement
        // over the best single. Neither score supplies a correspondence proof.
        if group.len() < 2 || best < 700_000 || best.saturating_sub(initial) < 50_000 {
            continue;
        }
        let text: String = group
            .iter()
            .map(|id| parts.paragraphs[id].text.as_str())
            .collect();
        if !prose(&text)
            || starts_after_fragment(parts, group[0])
            || !supported_edges(&text, &paragraph.text)
        {
            continue;
        }
        if output.len() == limits.matching.max_proposals {
            return;
        }
        let (old, new) = if reverse {
            (group, vec![paragraph.node.id])
        } else {
            (vec![paragraph.node.id], group)
        };
        output.push(Suggestion {
            old,
            new,
            score: best,
        });
    }
}

pub(in crate::document) fn append(
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    root: CorrespondenceScope,
    document: &mut DocumentViewComparison,
    limits: DocumentComparisonLimits,
) -> Result<()> {
    if !limits.matching.channels.text || limits.matching.max_group_nodes < 2 {
        return Ok(());
    }
    let mut remaining = limits.text.max_token_visits;
    let (Some(left), Some(right)) = (
        population(old, root.old, limits, &mut remaining)?,
        population(new, root.new, limits, &mut remaining)?,
    ) else {
        return Ok(());
    };
    let mut suggestions = Vec::new();
    // Each direction has a bounded share; a large old document cannot starve
    // merge discovery on the first new page (or vice versa).
    let mut forward = remaining / 2;
    let mut reverse = remaining - forward;
    discover(&left, &right, false, limits, &mut forward, &mut suggestions);
    discover(&right, &left, true, limits, &mut reverse, &mut suggestions);
    suggestions.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.old.cmp(&b.old))
            .then(a.new.cmp(&b.new))
    });
    let mut used_old = BTreeSet::new();
    let mut used_new = BTreeSet::new();
    for scope in &document.scopes {
        for review in &scope.result.text_scope_reviews {
            used_old.extend(&review.old_sources);
            used_new.extend(&review.new_sources);
        }
    }
    let mut proof_work = limits.local.proof_work;
    let mut ownership_checks = 0;
    let old_nodes = left
        .paragraphs
        .iter()
        .map(|(&id, paragraph)| (id, paragraph.node))
        .collect();
    let new_nodes = right
        .paragraphs
        .iter()
        .map(|(&id, paragraph)| (id, paragraph.node))
        .collect();
    for suggestion in suggestions {
        if overlaps(
            &suggestion.old,
            &old_nodes,
            old.graph,
            &mut ownership_checks,
            limits.matching.max_ownership_visits,
        ) != Some(false)
            || overlaps(
                &suggestion.new,
                &new_nodes,
                new.graph,
                &mut ownership_checks,
                limits.matching.max_ownership_visits,
            ) != Some(false)
        {
            continue;
        }
        let a: Vec<_> = suggestion
            .old
            .iter()
            .map(|id| left.paragraphs[id].node)
            .collect();
        let b: Vec<_> = suggestion
            .new
            .iter()
            .map(|id| right.paragraphs[id].node)
            .collect();
        let old_sources: Vec<_> = a
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect();
        let new_sources: Vec<_> = b
            .iter()
            .flat_map(|node| node.sources.iter().copied())
            .collect();
        let mut old_unique = BTreeSet::new();
        let mut new_unique = BTreeSet::new();
        if old_sources
            .iter()
            .any(|source| used_old.contains(source) || !old_unique.insert(*source))
            || new_sources
                .iter()
                .any(|source| used_new.contains(source) || !new_unique.insert(*source))
        {
            continue;
        }
        let proof = match native_count_change(&a, &b, limits.local, &mut proof_work) {
            Ok(Some(proof)) => proof,
            Ok(None) | Err(crate::Error::LimitExceeded { .. } | crate::Error::Unresolved(_)) => {
                continue;
            }
            Err(error) => return Err(error),
        };
        used_old.extend(old_unique);
        used_new.extend(new_unique);
        document.scopes[0].result.text_scope_reviews.push(TextScopeReview {
            convention: "inferred-native-paragraph-group-v1".into(),
            boundaries: Vec::new(), source_cuts: None, presence: None, native_regions: None,
            old_sources, new_sources,
            old_boundaries: Default::default(), new_boundaries: Default::default(),
            candidate_search_exhaustive: Some(false), spacing: None,
            comparison: LocalViewComparison {
                operation: Some(TypedOperation::TextChanged {
                    old: Some(suggestion.old.iter().map(|id| left.paragraphs[id].text.as_str()).collect()),
                    new: Some(suggestion.new.iter().map(|id| right.paragraphs[id].text.as_str()).collect()),
                }),
                old: suggestion.old, new: suggestion.new,
                interpretation: InterpretationStatus::Inferred,
                text_mask: None, text_change_proof: Some(proof), pixel_mask: None,
                unresolved: vec![format!(
                    "Literal word-pair Dice score {}/1000000 ranks this greedy adjacent group; correspondence, boundaries, and reading order are unproved. Search is not exhaustive. Original view text and all member sources are retained; source counts prove a non-whitespace difference only within this suggested group.", suggestion.score)],
                compared: true,
            },
        });
    }
    Ok(())
}
