use std::hash::{DefaultHasher, Hash, Hasher};

use super::{GraphNode, MatchingLimits, ScopeProposals};
use crate::document::{IdentityKey, NodeContent, NodeKind, TextNormalization, tables::CellKeys};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Key<'a> {
    Scoped(NodeKind, &'a IdentityKey),
    Cell(&'a IdentityKey, &'a IdentityKey),
    Literal(NodeKind, u64),
    PaddingBody(NodeKind, u64),
}

pub(super) fn defer_bucket(result: &mut ScopeProposals, old: &[&GraphNode], new: &[&GraphNode]) {
    result.exhaustive = false;
    let pending = result
        .incomplete_nodes
        .as_mut()
        .expect("completed key index");
    pending.old.extend(old.iter().map(|node| node.id));
    pending.new.extend(new.iter().map(|node| node.id));
}

/// Fingerprints only exclude unequal literals. Callers must verify the original
/// tokens for every retrieved pair, including hash collisions.
pub(super) fn keys<'a>(
    node: &'a GraphNode,
    cells: Option<&CellKeys<'a>>,
    result: &mut ScopeProposals,
    limits: MatchingLimits,
) -> Option<Vec<Key<'a>>> {
    if result.group_constraint_checks == limits.max_ownership_visits {
        result.exhaustive = false;
        return None;
    }
    result.group_constraint_checks += 1;
    let mut keys = Vec::new();
    if let Some(key) = &node.identity {
        charge(
            result,
            key.namespace.len().saturating_add(key.value.len()),
            limits,
        )?;
        keys.push(Key::Scoped(node.kind, key));
    }
    if node.kind == NodeKind::Cell {
        if let Some(key) = cells.and_then(|cells| cells.keys.get(&node.id)) {
            charge(result, key.bytes(), limits)?;
            keys.push(Key::Cell(key.identity.0, key.identity.1));
        }
    } else if node.identity.is_none()
        && let NodeContent::Text { view } = &node.content
        && !view.tokens.is_empty()
        && view.normalization == TextNormalization::Exact
    {
        let fingerprint = fingerprint(&view.tokens, result, limits)?;
        keys.push(Key::Literal(node.kind, fingerprint));
        if node.kind == NodeKind::Paragraph
            && node.basis == crate::document::ViewBasis::NativeLayout
        {
            let mut start = 0;
            let mut end = view.tokens.len();
            // Charge boundary scans before inspecting tokens, including all-space
            // views. Sampling must not introduce an unbounded trimming pass.
            while start < end {
                charge(result, 1, limits)?;
                if view.tokens[start] != crate::normalize::ComparableToken::Scalar(' ') {
                    break;
                }
                start += 1;
            }
            while start < end {
                charge(result, 1, limits)?;
                if view.tokens[end - 1] != crate::normalize::ComparableToken::Scalar(' ') {
                    break;
                }
                end -= 1;
            }
            if start < end {
                let body = &view.tokens[start..end];
                let body_fingerprint = if body.len() == view.tokens.len() {
                    fingerprint
                } else {
                    self::fingerprint(body, result, limits)?
                };
                keys.push(Key::PaddingBody(node.kind, body_fingerprint));
            }
        }
    }
    Some(keys)
}

// Length and at most eight edge tokens cheaply exclude many unrelated views.
// Equal keys only schedule complete literal verification; collisions retain every
// rival in the same bucket and cannot establish a correspondence on their own.
fn fingerprint(
    tokens: &[crate::normalize::ComparableToken],
    result: &mut ScopeProposals,
    limits: MatchingLimits,
) -> Option<u64> {
    charge(result, tokens.len().min(8), limits)?;
    let mut hasher = DefaultHasher::new();
    tokens.len().hash(&mut hasher);
    if tokens.len() <= 8 {
        tokens.hash(&mut hasher);
    } else {
        tokens[..4].hash(&mut hasher);
        tokens[tokens.len() - 4..].hash(&mut hasher);
    }
    Some(hasher.finish())
}

pub(super) fn charge(
    result: &mut ScopeProposals,
    work: usize,
    limits: MatchingLimits,
) -> Option<()> {
    if work > limits.max_index_work.saturating_sub(result.index_work) {
        result.exhaustive = false;
        result.incomplete_nodes = None;
        return None;
    }
    result.index_work += work;
    Some(())
}
