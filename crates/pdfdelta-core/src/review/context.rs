//! Structural context for a case: the headings, neighbours, table labels, and
//! repeated occurrences a reviewer needs before deciding.
//!
//! Context is gathered at plan time but kept apart from the case itself, so a
//! caller that only needs the question and its text never pays for material it
//! did not ask for. Nothing here is a comparison result: an item is quoted
//! document evidence with its own location, and a heading that merely surrounds
//! a case establishes no correspondence for it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    CaseId, EvidenceRef, PlannerLimits, ReviewText, Side, SideLocation, StructureLabel,
    planner::evidence_ref,
};
use crate::document::{DocumentView, EdgeKind, GraphNode, NodeContent, NodeId, NodeKind};

/// Why one context item was gathered.
///
/// The kind states the structural relationship that produced the item. It does
/// not claim that the item explains the case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    /// A heading or section this material sits inside.
    EnclosingHeading,
    /// The material immediately before it in the retained order.
    PrecedingNeighbour,
    /// The material immediately after it in the retained order.
    FollowingNeighbour,
    /// The row this cell belongs to.
    TableRow,
    /// The column this cell belongs to.
    TableColumn,
    /// An element declared as this material's label.
    Label,
    /// A caption declared for this material.
    Caption,
    /// The same text occurring elsewhere in the same document.
    ///
    /// Equal text is not the same element: a repeated occurrence has its own
    /// location, its own sources, and its own correspondence.
    OtherOccurrence,
}

/// One piece of surrounding evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItem {
    pub kind: ContextKind,
    pub side: Side,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<StructureLabel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<ReviewText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SideLocation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
}

/// The context gathered for one case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseContext {
    pub case_id: CaseId,
    pub items: Vec<ContextItem>,
    /// False when gathering stopped at a limit, so the surrounding structure is
    /// reported as partial rather than as all there is.
    pub complete: bool,
}

/// Gathers bounded context for one case's member views.
pub(super) fn gather(
    case: &CaseId,
    old: DocumentView<'_>,
    new: DocumentView<'_>,
    members: (&[NodeId], &[NodeId]),
    limits: PlannerLimits,
) -> CaseContext {
    let mut items = Vec::new();
    let mut complete = true;
    for (side, view, nodes) in [(Side::Old, old, members.0), (Side::New, new, members.1)] {
        let gathered = side_context(side, view, nodes, limits);
        complete &= gathered.1;
        items.extend(gathered.0);
    }
    items.sort_by(|left, right| {
        (
            left.side,
            left.kind,
            left.text.as_ref().map(|text| text.text.clone()),
        )
            .cmp(&(
                right.side,
                right.kind,
                right.text.as_ref().map(|text| text.text.clone()),
            ))
    });
    items.dedup();
    CaseContext {
        case_id: case.clone(),
        items,
        complete,
    }
}

fn side_context(
    side: Side,
    view: DocumentView<'_>,
    members: &[NodeId],
    limits: PlannerLimits,
) -> (Vec<ContextItem>, bool) {
    let nodes: BTreeMap<NodeId, &GraphNode> = view
        .graph
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect();
    let selected: BTreeSet<NodeId> = members.iter().copied().collect();
    let mut items = Vec::new();
    let mut complete = true;
    let mut budget = limits.max_labels_per_case;

    let push = |items: &mut Vec<ContextItem>,
                budget: &mut usize,
                complete: &mut bool,
                kind: ContextKind,
                node: &GraphNode| {
        if *budget == 0 {
            *complete = false;
            return;
        }
        *budget -= 1;
        items.push(item(side, kind, node, limits));
    };

    for member in members {
        let Some(node) = nodes.get(member) else {
            continue;
        };
        // Enclosing containers, nearest first.
        let mut current = *member;
        let mut depth = 0;
        while depth < limits.max_context_depth {
            let Some(parent) = view
                .graph
                .edges
                .iter()
                .find(|edge| edge.kind == EdgeKind::Contains && edge.to == current)
                .map(|edge| edge.from)
            else {
                break;
            };
            depth += 1;
            current = parent;
            let Some(parent) = nodes.get(&parent) else {
                break;
            };
            if matches!(
                parent.kind,
                NodeKind::Section | NodeKind::Header | NodeKind::Table | NodeKind::Row
            ) {
                push(
                    &mut items,
                    &mut budget,
                    &mut complete,
                    match parent.kind {
                        NodeKind::Row => ContextKind::TableRow,
                        _ => ContextKind::EnclosingHeading,
                    },
                    parent,
                );
            }
        }

        for edge in &view.graph.edges {
            let (kind, other) = match (edge.kind, edge.from == *member, edge.to == *member) {
                (EdgeKind::Precedes, true, _) => (ContextKind::FollowingNeighbour, edge.to),
                (EdgeKind::Precedes, _, true) => (ContextKind::PrecedingNeighbour, edge.from),
                (EdgeKind::RowMember, _, true) => (ContextKind::TableRow, edge.from),
                (EdgeKind::ColumnMember, _, true) => (ContextKind::TableColumn, edge.from),
                (EdgeKind::LabelFor, _, true) => (ContextKind::Label, edge.from),
                (EdgeKind::CaptionFor, _, true) => (ContextKind::Caption, edge.from),
                _ => continue,
            };
            if selected.contains(&other) {
                continue;
            }
            let Some(other) = nodes.get(&other) else {
                continue;
            };
            push(&mut items, &mut budget, &mut complete, kind, other);
        }

        // Repeated text elsewhere in the same document.
        if let NodeContent::Text { view: text } = &node.content
            && let Some(display) = text.display_text()
            && !display.trim().is_empty()
        {
            for other in view.graph.nodes.iter().filter(|candidate| {
                candidate.id != node.id
                    && !selected.contains(&candidate.id)
                    && matches!(&candidate.content, NodeContent::Text { view }
                        if view.display_text().as_deref() == Some(display.as_str()))
            }) {
                push(
                    &mut items,
                    &mut budget,
                    &mut complete,
                    ContextKind::OtherOccurrence,
                    other,
                );
            }
        }
    }
    (items, complete)
}

fn item(side: Side, kind: ContextKind, node: &GraphNode, limits: PlannerLimits) -> ContextItem {
    let text = match &node.content {
        NodeContent::Text { view } => Some(super::planner::review_text(
            side, node.basis, view, None, limits,
        )),
        _ => None,
    };
    ContextItem {
        kind,
        side,
        label: text.as_ref().map(|text| StructureLabel {
            kind: node.kind,
            text: text.text.clone(),
            sources: Vec::new(),
        }),
        text,
        location: Some(
            SideLocation::unknown(side)
                .with_page(node.pages.first().copied())
                .view(node.kind),
        ),
        evidence: node
            .sources
            .iter()
            .take(limits.max_sources_per_case)
            .map(|source| evidence_ref(side, *source))
            .collect(),
    }
}
