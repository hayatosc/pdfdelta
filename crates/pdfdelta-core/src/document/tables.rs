//! Scoped cell identity comes from row/column membership, never equal values.

use std::collections::{BTreeMap, BTreeSet};

use super::{DocumentGraph, EdgeKind, IdentityKey, NodeId, NodeKind};

pub(super) struct CellKey<'a> {
    pub row: NodeId,
    pub column: NodeId,
    pub identity: (&'a IdentityKey, &'a IdentityKey),
    pub source_backed: bool,
}

impl CellKey<'_> {
    pub fn bytes(&self) -> usize {
        [self.identity.0, self.identity.1]
            .iter()
            .fold(0usize, |total, key| {
                total
                    .saturating_add(key.namespace.len())
                    .saturating_add(key.value.len())
            })
    }
}

pub(super) struct CellKeys<'a> {
    pub keys: BTreeMap<NodeId, CellKey<'a>>,
    pub exhaustive: bool,
}

fn spend(budget: &mut usize, work: usize) -> Option<()> {
    if let Some(left) = budget.checked_sub(work) {
        *budget = left;
        Some(())
    } else {
        *budget = 0;
        None
    }
}

/// `None` means work exhaustion; missing or duplicate axes never manufacture a key.
pub(super) fn cell_keys<'a>(
    graph: &'a DocumentGraph,
    scope: NodeId,
    budget: &mut usize,
) -> Option<CellKeys<'a>> {
    let mut result = CellKeys {
        keys: BTreeMap::new(),
        exhaustive: true,
    };
    spend(budget, graph.nodes.len())?;
    let root = graph.nodes.iter().find(|node| node.id == scope)?;
    if !matches!(root.kind, NodeKind::Table | NodeKind::Row) {
        return Some(result);
    }
    let work = graph
        .nodes
        .len()
        .saturating_add(graph.edges.len().saturating_mul(4));
    spend(budget, work)?;
    let nodes: BTreeMap<_, _> = graph.nodes.iter().map(|node| (node.id, node)).collect();
    let table = if root.kind == NodeKind::Table {
        scope
    } else {
        let parents = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Contains && edge.to == scope)
            .filter_map(|edge| {
                nodes
                    .get(&edge.from)
                    .filter(|node| node.kind == NodeKind::Table)
                    .map(|node| node.id)
            })
            .collect::<BTreeSet<_>>();
        if parents.len() != 1 {
            return Some(result);
        }
        *parents.first().expect("one containing table")
    };
    let children: BTreeMap<_, _> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains && edge.from == table)
        .fold(BTreeMap::new(), |mut children, edge| {
            children
                .entry(edge.to)
                .and_modify(|backed| *backed &= !edge.basis.is_inferred())
                .or_insert(!edge.basis.is_inferred());
            children
        });
    let mut rows = BTreeMap::new();
    let mut columns = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for (&id, &backed) in &children {
        let Some(node) = nodes.get(&id) else {
            continue;
        };
        let axis = match node.kind {
            NodeKind::Row => &mut rows,
            NodeKind::Column => &mut columns,
            _ => continue,
        };
        let Some(key) = node.identity.as_ref() else {
            result.exhaustive = false;
            continue;
        };
        spend(budget, key.namespace.len().saturating_add(key.value.len()))?;
        *counts.entry((node.kind, key)).or_insert(0usize) += 1;
        axis.insert(id, (key, backed && !node.basis.is_inferred()));
    }
    let selected: BTreeSet<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains && edge.from == scope)
        .map(|edge| edge.to)
        .collect();
    let mut row_members = BTreeMap::<NodeId, BTreeMap<NodeId, bool>>::new();
    let mut column_members = BTreeMap::<NodeId, BTreeMap<NodeId, bool>>::new();
    for edge in &graph.edges {
        let members = match edge.kind {
            EdgeKind::RowMember => &mut row_members,
            EdgeKind::ColumnMember => &mut column_members,
            EdgeKind::Contains
                if nodes
                    .get(&edge.from)
                    .is_some_and(|node| node.kind == NodeKind::Row) =>
            {
                &mut row_members
            }
            _ => continue,
        };
        members
            .entry(edge.to)
            .or_default()
            .entry(edge.from)
            .and_modify(|backed| *backed &= !edge.basis.is_inferred())
            .or_insert(!edge.basis.is_inferred());
    }
    for id in selected {
        if !nodes
            .get(&id)
            .is_some_and(|node| node.kind == NodeKind::Cell && node.identity.is_none())
        {
            continue;
        }
        let (Some(row), Some(column)) = (row_members.get(&id), column_members.get(&id)) else {
            result.exhaustive = false;
            continue;
        };
        if row.len() != 1 || column.len() != 1 {
            result.exhaustive = false;
            continue;
        }
        let (&row_id, &row_backed) = row.first_key_value().expect("one row");
        let (&column_id, &column_backed) = column.first_key_value().expect("one column");
        let (Some(&(row_key, row_key_backed)), Some(&(column_key, column_key_backed))) =
            (rows.get(&row_id), columns.get(&column_id))
        else {
            result.exhaustive = false;
            continue;
        };
        if counts[&(NodeKind::Row, row_key)] != 1 || counts[&(NodeKind::Column, column_key)] != 1 {
            result.exhaustive = false;
            continue;
        }
        result.keys.insert(
            id,
            CellKey {
                row: row_id,
                column: column_id,
                identity: (row_key, column_key),
                source_backed: row_backed
                    && column_backed
                    && row_key_backed
                    && column_key_backed
                    && !root.basis.is_inferred()
                    && !nodes[&table].basis.is_inferred(),
            },
        );
    }
    Some(result)
}
