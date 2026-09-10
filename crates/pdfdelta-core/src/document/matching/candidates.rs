use std::hash::{DefaultHasher, Hash, Hasher};

use super::{GraphNode, MatchingLimits, ScopeProposals};
use crate::document::{IdentityKey, NodeContent, NodeKind, TextNormalization, tables::CellKeys};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Key<'a> {
    Scoped(NodeKind, &'a IdentityKey),
    Cell(&'a IdentityKey, &'a IdentityKey),
    Literal(NodeKind, u64),
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
        charge(result, view.tokens.len(), limits)?;
        let mut hasher = DefaultHasher::new();
        view.tokens.hash(&mut hasher);
        keys.push(Key::Literal(node.kind, hasher.finish()));
    }
    Some(keys)
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
