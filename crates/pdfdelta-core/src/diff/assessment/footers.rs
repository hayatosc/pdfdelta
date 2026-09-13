//! Terminal-line candidate supply for the common correspondence solver.

use std::collections::BTreeSet;

use crate::{
    Result,
    alignment::BlockSeparator,
    document::{
        CorrespondenceScope, DocumentGraph, EdgeKind, GraphEdge, GraphNode, MatchingLimits,
        NodeContent, NodeId, NodeKind, SourceRef, ViewBasis, propose_scope_correspondences,
        solve_correspondence_scope,
    },
    layout::BlockRole,
    model::PageId,
    normalize::TextSourceAtom,
};

use super::{SentenceRecoveryInput, Side, charge, span_has_source_issues, views::LocalDomain};

struct Footer {
    page: u32,
    role: BlockRole,
    catalog: String,
    form: String,
    prefix: String,
    span: super::TextSpan,
}

pub(super) struct Discovery {
    pub domains: Vec<LocalDomain>,
    pub complete: bool,
    pub work_limited: bool,
}

/// Supplies keyed views without emitting changes. Only mandatory correspondences
/// from the common solver become local domains for the existing exact kernel.
pub(super) fn discover(
    sides: [&Side<'_>; 2],
    recovery: SentenceRecoveryInput<'_>,
    remaining: &mut usize,
    limit: usize,
) -> Result<Discovery> {
    let mut result = Discovery {
        domains: Vec::new(),
        complete: true,
        work_limited: false,
    };
    if !sides.iter().any(|side| {
        side.blocks
            .iter()
            .any(|block| block.canonical.text.contains("Cat."))
    }) {
        return Ok(result);
    }
    let old = candidates(sides[0], recovery, remaining, limit, &mut result)?;
    let new = candidates(sides[1], recovery, remaining, limit, &mut result)?;
    if !result.complete || *remaining == 0 {
        result.complete = false;
        result.work_limited |= *remaining == 0;
        return Ok(result);
    }
    let Some(old_graph) = candidate_graph(&old, sides[0], remaining)? else {
        result.complete = false;
        result.work_limited = *remaining == 0;
        return Ok(result);
    };
    let Some(new_graph) = candidate_graph(&new, sides[1], remaining)? else {
        result.complete = false;
        result.work_limited = *remaining == 0;
        return Ok(result);
    };
    let key_bytes = |graph: &DocumentGraph| {
        graph
            .nodes
            .iter()
            .filter_map(|node| node.identity.as_ref())
            .fold(0usize, |total, key| {
                total
                    .saturating_add(key.namespace.len())
                    .saturating_add(key.value.len())
            })
    };
    let pairs = old.len().saturating_mul(new.len());
    let work = key_bytes(&old_graph)
        .saturating_mul(new.len())
        .saturating_add(key_bytes(&new_graph).saturating_mul(old.len()))
        .saturating_add(pairs);
    if !charge(remaining, work) {
        result.complete = false;
        result.work_limited = true;
        return Ok(result);
    }
    let scope = CorrespondenceScope {
        old: NodeId(0),
        new: NodeId(0),
    };
    let limits = MatchingLimits {
        max_proposals: limit,
        ..MatchingLimits::default()
    };
    let proposals = propose_scope_correspondences(&old_graph, &new_graph, scope, limits)?;
    if !proposals.exhaustive {
        result.complete = false;
        result.work_limited = true;
        return Ok(result);
    }
    let phase_budget = *remaining / 3;
    let limits = MatchingLimits {
        max_pair_checks: limits.max_pair_checks.min(phase_budget),
        max_ownership_visits: limits.max_ownership_visits.min(phase_budget),
        max_states_per_component: limits
            .max_states_per_component
            .min(phase_budget / proposals.proposals.len().max(1)),
        ..limits
    };
    let matching = match solve_correspondence_scope(
        &old_graph,
        &new_graph,
        scope,
        &proposals.proposals,
        limits,
    ) {
        Ok(matching) => matching,
        Err(crate::Error::LimitExceeded { .. }) => {
            charge(remaining, phase_budget);
            result.complete = false;
            result.work_limited = true;
            return Ok(result);
        }
        Err(error) => return Err(error),
    };
    let work = matching.components.iter().fold(
        matching
            .conflict_checks
            .saturating_add(matching.ownership_visits),
        |total, component| total.saturating_add(component.explored_states),
    );
    if !charge(remaining, work) || !matching.conflict_search_complete {
        result.complete = false;
        result.work_limited = true;
        return Ok(result);
    }
    for component in matching.components {
        if !component.exhaustive {
            result.complete = false;
            result.work_limited = true;
            continue;
        }
        for index in component.mandatory {
            let proposal = &proposals.proposals[index];
            result.domains.push(LocalDomain {
                old_span: old[proposal.old[0].0 as usize - 1].span.clone(),
                new_span: new[proposal.new[0].0 as usize - 1].span.clone(),
            });
        }
    }
    Ok(result)
}

fn candidate_graph(
    candidates: &[Footer],
    side: &Side<'_>,
    remaining: &mut usize,
) -> Result<Option<DocumentGraph>> {
    let mut graph = DocumentGraph {
        nodes: vec![GraphNode {
            id: NodeId(0),
            kind: NodeKind::Document,
            pages: Vec::new(),
            sources: Vec::new(),
            identity: None,
            basis: ViewBasis::NativeLayout,
            content: NodeContent::Container,
        }],
        ..DocumentGraph::default()
    };
    for footer in candidates {
        let mut sources = BTreeSet::new();
        for block in &footer.span.blocks {
            for (_, source) in side.blocks[side.index[block]]
                .canonical
                .comparable_tokens_with_sources()?
            {
                for atom in source.atoms {
                    if !charge(remaining, 1) {
                        return Ok(None);
                    }
                    match atom {
                        TextSourceAtom::Glyph(glyph) => {
                            sources.insert(SourceRef::Native { glyph });
                        }
                        TextSourceAtom::SyntheticSpace {
                            preceding,
                            following,
                        }
                        | TextSourceAtom::LineBreak {
                            preceding,
                            following,
                        } => {
                            sources.insert(SourceRef::Native { glyph: preceding });
                            sources.insert(SourceRef::Native { glyph: following });
                        }
                    }
                }
            }
        }
        if sources.is_empty() {
            return Ok(None);
        }
        let key_bytes = footer
            .catalog
            .len()
            .saturating_add(footer.form.len())
            .saturating_add(footer.prefix.len());
        if !charge(remaining, key_bytes) {
            return Ok(None);
        }
        let id = NodeId(graph.nodes.len() as u64);
        graph.nodes.push(GraphNode {
            id,
            kind: NodeKind::Footer,
            pages: vec![PageId(footer.page)],
            sources: sources.into_iter().collect(),
            identity: Some(crate::document::footers::identity(
                footer.role,
                &footer.catalog,
                &footer.form,
                &footer.prefix,
            )),
            basis: ViewBasis::NativeLayout,
            content: NodeContent::Unknown,
        });
        graph.edges.push(GraphEdge {
            from: NodeId(0),
            to: id,
            kind: EdgeKind::Contains,
            sources: Vec::new(),
            basis: ViewBasis::NativeLayout,
        });
    }
    Ok(Some(graph))
}

fn candidates(
    side: &Side<'_>,
    recovery: SentenceRecoveryInput<'_>,
    remaining: &mut usize,
    limit: usize,
    discovery: &mut Discovery,
) -> Result<Vec<Footer>> {
    let mut search = crate::document::footers::CandidateSearch {
        complete: true,
        work_limited: false,
    };
    let candidates = crate::document::footers::candidates(
        side.blocks,
        recovery.min_tokens,
        remaining,
        limit,
        &mut search,
    )?;
    discovery.complete &= search.complete;
    discovery.work_limited |= search.work_limited;
    let mut output = Vec::new();
    for candidate in candidates {
        let blocks = candidate
            .members
            .iter()
            .map(|&index| side.blocks[index].block)
            .collect::<Vec<_>>();
        let span = side
            .canonical_group(&blocks, (blocks.len() > 1).then_some(BlockSeparator::Space))
            .full_span();
        if span_has_source_issues(side, &span, remaining)? {
            discovery.complete = false;
            discovery.work_limited |= *remaining == 0;
            continue;
        }
        output.push(Footer {
            page: candidate.page,
            role: candidate.role,
            catalog: candidate.catalog,
            form: candidate.form,
            prefix: candidate.prefix,
            span,
        });
    }
    Ok(output)
}
