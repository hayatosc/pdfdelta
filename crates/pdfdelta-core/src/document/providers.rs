use std::collections::{BTreeMap, BTreeSet};

use crate::{Result, model::GlyphId, normalize::ComparableToken, pipeline::PipelineOptions};

use super::{
    BackendKind, DocumentGraph, EdgeKind, EvidenceLimits, EvidenceStore, GraphEdge, GraphLimits,
    GraphNode, IdentityKey, NodeContent, NodeId, NodeKind, SourceRef, StructuredValue,
    TextNormalization, TextView, ViewBasis,
    evidence::{bounded, invalid},
    graph::charge,
};

impl DocumentGraph {
    /// Preserves native, rendered, and structured views in one graph. Stored
    /// fields keep their keys and values; appearances remain separately linked
    /// visual evidence. Model/OCR text retains its inferred origin.
    ///
    /// This import does not establish equivalence between overlapping native and
    /// recognized text. Providers must supply source conflicts before comparison
    /// when two channels may describe the same physical material.
    ///
    /// # Errors
    /// Rejects invalid evidence, dangling structure parents, and graph/resource
    /// violations. Nothing is removed from the evidence store on failure.
    pub fn from_evidence(
        store: &EvidenceStore,
        options: PipelineOptions,
        evidence_limits: EvidenceLimits,
        graph_limits: GraphLimits,
    ) -> Result<Self> {
        let mut graph = Self::from_native(store, options, evidence_limits, graph_limits)?;
        bounded(
            graph
                .nodes
                .len()
                .saturating_add(store.rendered.len())
                .saturating_add(store.structured.len()),
            graph_limits.max_nodes,
            "graph nodes",
        )?;
        let mut tokens_used: usize = graph
            .nodes
            .iter()
            .map(|node| match &node.content {
                NodeContent::Text { view } => view.tokens.len(),
                _ => 0,
            })
            .sum();
        let mut rendered_nodes = BTreeMap::new();
        for region in &store.rendered {
            let id = NodeId(graph.nodes.len() as u64);
            rendered_nodes.insert(region.id, id);
            graph.nodes.push(GraphNode {
                id,
                kind: NodeKind::Figure,
                pages: vec![region.page],
                sources: vec![SourceRef::Rendered { region: region.id }],
                identity: None,
                basis: ViewBasis::RenderedRegion,
                content: NodeContent::Visual { region: region.id },
            });
            graph
                .edges
                .push(contains(NodeId(0), id, ViewBasis::RenderedRegion));
        }
        let start = graph.nodes.len() as u64;
        let structured_nodes: BTreeMap<_, _> = store
            .structured
            .iter()
            .enumerate()
            .map(|(index, element)| (element.id, NodeId(start + index as u64)))
            .collect();
        let mut structure_order: BTreeMap<_, Vec<_>> = BTreeMap::new();
        let native = crate::model::index_glyphs(&store.native)?;
        let mut widget_label_bytes = 0usize;
        let mut declaration_work = 0usize;
        for element in &store.structured {
            let id = structured_nodes[&element.id];
            let source = SourceRef::Structured {
                element: element.id,
            };
            let basis = match store.backends[element.backend].kind {
                BackendKind::Ocr => ViewBasis::Recognition {
                    backend: element.backend,
                },
                BackendKind::StructureModel => ViewBasis::Model {
                    backend: element.backend,
                },
                BackendKind::NativeParser | BackendKind::Renderer => ViewBasis::SourceStructure,
            };
            let mut parent = NodeId(0);
            let mut identity = None;
            let mut sources = vec![source];
            let (kind, content) = match &element.value {
                StructuredValue::RecognizedText { text, .. } => (
                    NodeKind::Paragraph,
                    structured_text(Some(text), source, &mut tokens_used, graph_limits)?,
                ),
                StructuredValue::FormField {
                    name,
                    value,
                    widgets,
                    ..
                } => {
                    if !name.is_empty() {
                        identity = Some(IdentityKey {
                            namespace: "pdf-field-name".into(),
                            value: name.clone(),
                        });
                    }
                    for crop in widgets.iter().filter_map(|widget| widget.crop.as_ref()) {
                        let region = &crop.region;
                        let visual = rendered_nodes
                            .get(region)
                            .ok_or_else(|| invalid("missing field appearance view"))?;
                        graph.edges.push(GraphEdge {
                            from: *visual,
                            to: id,
                            kind: EdgeKind::AppearanceFor,
                            sources: vec![source, SourceRef::Rendered { region: *region }],
                            basis,
                        });
                        let node = &mut graph.nodes[visual.0 as usize];
                        if !name.is_empty() {
                            widget_label_bytes = widget_label_bytes
                                .saturating_add(name.len())
                                .saturating_add("pdf-field-widget".len());
                            bounded(
                                widget_label_bytes,
                                graph_limits.max_label_bytes,
                                "widget identity labels",
                            )?;
                            node.identity = Some(IdentityKey {
                                namespace: "pdf-field-widget".into(),
                                value: name.clone(),
                            });
                        }
                    }
                    (
                        NodeKind::Field,
                        NodeContent::Value {
                            value: value.clone(),
                        },
                    )
                }
                StructuredValue::StructureElement {
                    role,
                    identifier,
                    text,
                    declared_text,
                    glyphs,
                    parent: owner,
                    order,
                    ..
                } => {
                    if let Some(identifier) = identifier.as_ref().filter(|id| !id.is_empty()) {
                        bounded(
                            identifier.len().saturating_mul(2),
                            graph_limits.max_label_bytes,
                            "structure identifier",
                        )?;
                        identity = Some(IdentityKey {
                            namespace: "pdf-structure-id".into(),
                            value: identifier
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect(),
                        });
                    }
                    if let Some(owner) = owner {
                        parent = *structured_nodes
                            .get(owner)
                            .ok_or_else(|| invalid("missing structure parent view"))?;
                    }
                    structure_order
                        .entry(parent)
                        .or_default()
                        .push((*order, id, source, basis));
                    sources.extend(
                        glyphs
                            .iter()
                            .map(|glyph| SourceRef::Native { glyph: *glyph }),
                    );
                    // A declared `ActualText` replacement is a separate bound
                    // interpretation. Validated declarations present their exact
                    // literal content; unresolved declarations keep the original
                    // native view but state the uncertainty explicitly through
                    // `TextNormalization::Unresolved` on every affected text
                    // view. Raw glyph evidence is untouched in either case.
                    match declared_text {
                        Some(declared)
                            if declared.status == super::DeclaredTextStatus::Validated =>
                        {
                            // The declared replacement is the verified text view;
                            // uncertainty over the same sources is indexed below.
                            (
                                role_kind(role),
                                structured_text(
                                    Some(&declared.text),
                                    source,
                                    &mut tokens_used,
                                    graph_limits,
                                )?,
                            )
                        }
                        Some(declared) => {
                            let mut content = if glyphs.is_empty() {
                                structured_text(
                                    text.as_deref(),
                                    source,
                                    &mut tokens_used,
                                    graph_limits,
                                )?
                            } else {
                                tagged_text(
                                    glyphs,
                                    source,
                                    &native,
                                    &mut tokens_used,
                                    graph_limits,
                                )?
                            };
                            if let NodeContent::Text { view } = &mut content {
                                let reason = declared
                                    .reason
                                    .as_deref()
                                    .unwrap_or("declared replacement text is not validated");
                                charge(
                                    &mut widget_label_bytes,
                                    reason.len(),
                                    graph_limits.max_label_bytes,
                                    "graph label bytes",
                                )?;
                                view.normalization = TextNormalization::Unresolved {
                                    reason: reason.to_owned(),
                                };
                            }
                            (role_kind(role), content)
                        }
                        None => (
                            role_kind(role),
                            if glyphs.is_empty() {
                                structured_text(
                                    text.as_deref(),
                                    source,
                                    &mut tokens_used,
                                    graph_limits,
                                )?
                            } else {
                                tagged_text(
                                    glyphs,
                                    source,
                                    &native,
                                    &mut tokens_used,
                                    graph_limits,
                                )?
                            },
                        ),
                    }
                }
                StructuredValue::Annotation { text, .. } => (
                    NodeKind::Annotation,
                    structured_text(text.as_deref(), source, &mut tokens_used, graph_limits)?,
                ),
            };
            // Acquisition leaves the element's page unset when its memberships
            // span pages or /Pg is absent. The retained glyphs still establish
            // every contributing page of the graph view.
            let pages = element
                .page
                .into_iter()
                .chain(sources.iter().filter_map(|source| match source {
                    SourceRef::Native { glyph } => native.get(*glyph).map(|glyph| glyph.page),
                    _ => None,
                }))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            graph.nodes.push(GraphNode {
                id,
                kind,
                pages,
                sources,
                identity,
                basis,
                content,
            });
            graph.edges.push(contains(parent, id, basis));
        }
        // Declarations make every text interpretation over their affected
        // sources uncertain: their own element, glyph membership and every
        // structural descendant. The only exempt view is the exact verified
        // replacement of a validated declaration. The whole pass is skipped
        // when no declaration exists, and every step is charged.
        let has_declarations = store.structured.iter().any(|element| {
            matches!(
                &element.value,
                StructuredValue::StructureElement {
                    declared_text: Some(_),
                    ..
                }
            )
        });
        if has_declarations {
            let mut reasons: Vec<String> = Vec::new();
            let mut declared_status: BTreeMap<u64, (bool, u32)> = BTreeMap::new();
            let mut validated_views: BTreeMap<u64, (&str, &[GlyphId])> = BTreeMap::new();
            for element in &store.structured {
                let StructuredValue::StructureElement {
                    declared_text: Some(declared),
                    glyphs,
                    ..
                } = &element.value
                else {
                    continue;
                };
                if declared.status == super::DeclaredTextStatus::Validated {
                    // Compare the declared text with the native membership text
                    // without allocating a concatenated string; charge the scan.
                    charge(
                        &mut declaration_work,
                        glyphs.len().saturating_add(declared.text.len()),
                        graph_limits.max_references,
                        "declaration text comparison work",
                    )?;
                    let mut declared_chars = declared.text.chars();
                    let mut matches = true;
                    'glyphs: for id in glyphs {
                        let Some(glyph) = native.get(*id) else {
                            matches = false;
                            break;
                        };
                        match &glyph.text {
                            crate::model::DecodedText::Mapped(text) => {
                                for character in text.chars() {
                                    if declared_chars.next() != Some(character) {
                                        matches = false;
                                        break 'glyphs;
                                    }
                                }
                            }
                            crate::model::DecodedText::Unmapped { .. } => {
                                matches = false;
                                break;
                            }
                        }
                    }
                    if matches && declared_chars.next().is_some() {
                        matches = false;
                    }
                    let reason = if matches {
                        "declared replacement text supersedes the native glyph text"
                    } else {
                        "declared replacement text differs from the native glyph text"
                    };
                    let reason_index = push_declaration_reason(
                        &mut reasons,
                        reason,
                        &mut widget_label_bytes,
                        graph_limits,
                    )?;
                    declared_status.insert(element.id, (true, reason_index));
                    validated_views.insert(element.id, (declared.text.as_str(), glyphs.as_slice()));
                } else {
                    let reason = declared
                        .reason
                        .as_deref()
                        .unwrap_or("declared replacement text is not validated");
                    let reason_index = push_declaration_reason(
                        &mut reasons,
                        reason,
                        &mut widget_label_bytes,
                        graph_limits,
                    )?;
                    declared_status.insert(element.id, (false, reason_index));
                }
            }
            let parents: BTreeMap<u64, u64> = store
                .structured
                .iter()
                .filter_map(|element| match &element.value {
                    StructuredValue::StructureElement {
                        parent: Some(parent),
                        ..
                    } => Some((element.id, *parent)),
                    _ => None,
                })
                .collect();
            let mut uncertain: BTreeMap<SourceRef, (u64, u32)> = BTreeMap::new();
            for element in &store.structured {
                let mut current = Some(element.id);
                let mut depth = 0usize;
                while let Some(owner) = current {
                    if let Some((_, reason_index)) = declared_status.get(&owner) {
                        charge(
                            &mut declaration_work,
                            1,
                            graph_limits.max_references,
                            "declaration uncertainty work",
                        )?;
                        prefer_unresolved(
                            &mut uncertain,
                            &declared_status,
                            SourceRef::Structured {
                                element: element.id,
                            },
                            owner,
                            *reason_index,
                        );
                        if let StructuredValue::StructureElement { glyphs, .. } = &element.value {
                            for glyph in glyphs {
                                charge(
                                    &mut declaration_work,
                                    1,
                                    graph_limits.max_references,
                                    "declaration uncertainty work",
                                )?;
                                prefer_unresolved(
                                    &mut uncertain,
                                    &declared_status,
                                    SourceRef::Native { glyph: *glyph },
                                    owner,
                                    *reason_index,
                                );
                            }
                        }
                    }
                    charge(
                        &mut declaration_work,
                        1,
                        graph_limits.max_references,
                        "declaration ancestry work",
                    )?;
                    depth += 1;
                    if depth > graph_limits.max_nodes {
                        return Err(invalid("declaration ancestry depth limit"));
                    }
                    current = parents.get(&owner).copied();
                }
            }
            for node in &mut graph.nodes {
                let mut exempt = false;
                let mut mark: Option<u32> = None;
                let mut node_sources: Option<BTreeSet<SourceRef>> = None;
                for source in &node.sources {
                    charge(
                        &mut declaration_work,
                        1,
                        graph_limits.max_references,
                        "declaration uncertainty work",
                    )?;
                    if let SourceRef::Structured { element } = source
                        && let Some((true, _)) = declared_status.get(element)
                        && let Some((declared_text, members)) = validated_views.get(element)
                    {
                        if node_sources.is_none() {
                            charge(
                                &mut declaration_work,
                                node.sources.len(),
                                graph_limits.max_references,
                                "declaration uncertainty work",
                            )?;
                            node_sources = Some(node.sources.iter().copied().collect());
                        }
                        if let Some(node_sources) = &node_sources
                            && is_exact_replacement_view(node, declared_text, members, node_sources)
                        {
                            exempt = true;
                            break;
                        }
                    }
                    if let Some((_, reason_index)) = uncertain.get(source) {
                        mark = Some(*reason_index);
                    }
                }
                if exempt {
                    continue;
                }
                let Some(reason_index) = mark else {
                    continue;
                };
                let reason = &reasons[reason_index as usize];
                if let NodeContent::Text { view } = &mut node.content
                    && view.normalization == TextNormalization::Exact
                {
                    charge(
                        &mut widget_label_bytes,
                        reason.len(),
                        graph_limits.max_label_bytes,
                        "graph label bytes",
                    )?;
                    view.normalization = TextNormalization::Unresolved {
                        reason: reason.clone(),
                    };
                }
            }
            // Retain the marked sources so later local projections cannot
            // clear the uncertainty. The set is bounded by the evidence items
            // already charged above.
            graph.declaration_affected = uncertain.into_keys().collect();
        }
        for mut siblings in structure_order.into_values() {
            siblings.sort_by_key(|entry| entry.0);
            if siblings.iter().any(|entry| entry.0.is_none())
                || siblings.windows(2).any(|pair| pair[0].0 == pair[1].0)
            {
                continue;
            }
            for pair in siblings.windows(2) {
                graph.edges.push(GraphEdge {
                    from: pair[0].1,
                    to: pair[1].1,
                    kind: EdgeKind::Precedes,
                    sources: vec![pair[0].2, pair[1].2],
                    basis: if pair[0].3.is_inferred() {
                        pair[0].3
                    } else {
                        pair[1].3
                    },
                });
            }
        }
        super::widgets::append_crop_conflicts(&mut graph, store, graph_limits)?;
        super::recognition::append_recognition_conflicts(&mut graph, store, graph_limits)?;
        graph.validate(store, evidence_limits, graph_limits)?;
        Ok(graph)
    }
}

fn contains(from: NodeId, to: NodeId, basis: ViewBasis) -> GraphEdge {
    GraphEdge {
        from,
        to,
        kind: EdgeKind::Contains,
        sources: Vec::new(),
        basis,
    }
}

fn structured_text(
    text: Option<&str>,
    source: SourceRef,
    tokens: &mut usize,
    limits: GraphLimits,
) -> Result<NodeContent> {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Ok(NodeContent::Container);
    };
    let count = text.chars().count();
    charge(tokens, count, limits.max_tokens, "graph text tokens")?;
    Ok(NodeContent::Text {
        view: TextView {
            tokens: text.chars().map(ComparableToken::Scalar).collect(),
            origins: vec![vec![source]; count],
            source_backed: vec![true; count],
            normalization: TextNormalization::Exact,
        },
    })
}

fn tagged_text(
    glyphs: &[crate::model::GlyphId],
    membership: SourceRef,
    native: &crate::model::GlyphIndex<'_>,
    tokens_used: &mut usize,
    limits: GraphLimits,
) -> Result<NodeContent> {
    let mut view = TextView {
        tokens: Vec::new(),
        origins: Vec::new(),
        source_backed: Vec::new(),
        normalization: TextNormalization::Exact,
    };
    for id in glyphs {
        let glyph = native
            .get(*id)
            .ok_or_else(|| invalid("missing tagged glyph"))?;
        let count = match &glyph.text {
            crate::model::DecodedText::Mapped(text) => text.chars().count(),
            crate::model::DecodedText::Unmapped { .. } => 1,
        };
        charge(tokens_used, count, limits.max_tokens, "graph text tokens")?;
        match &glyph.text {
            crate::model::DecodedText::Mapped(text) => view
                .tokens
                .extend(text.chars().map(ComparableToken::Scalar)),
            crate::model::DecodedText::Unmapped {
                font_hash,
                glyph_id,
            } => view.tokens.push(ComparableToken::Unmapped {
                font_hash: font_hash.clone(),
                glyph_id: *glyph_id,
            }),
        }
        view.origins
            .extend((0..count).map(|_| vec![SourceRef::Native { glyph: *id }, membership]));
        view.source_backed.extend((0..count).map(|_| true));
    }
    Ok(NodeContent::Text { view })
}

/// Keeps the unresolved owner when two declarations share one source; the
/// validated owner never overrides an existing unresolved entry.
fn prefer_unresolved(
    uncertain: &mut BTreeMap<SourceRef, (u64, u32)>,
    declared_status: &BTreeMap<u64, (bool, u32)>,
    source: SourceRef,
    owner: u64,
    reason_index: u32,
) {
    match uncertain.entry(source) {
        std::collections::btree_map::Entry::Occupied(mut existing) => {
            let existing_validated = declared_status
                .get(&existing.get().0)
                .is_some_and(|(validated, _)| *validated);
            let new_validated = declared_status
                .get(&owner)
                .is_some_and(|(validated, _)| *validated);
            if existing_validated && !new_validated {
                existing.insert((owner, reason_index));
            }
        }
        std::collections::btree_map::Entry::Vacant(vacant) => {
            vacant.insert((owner, reason_index));
        }
    }
}

/// Charges and stores one declaration reason once; sources reference its index
/// instead of cloning the string per source.
fn push_declaration_reason(
    reasons: &mut Vec<String>,
    reason: &str,
    label_bytes: &mut usize,
    limits: GraphLimits,
) -> Result<u32> {
    charge(
        label_bytes,
        reason.len(),
        limits.max_label_bytes,
        "graph label bytes",
    )?;
    let index = u32::try_from(reasons.len())
        .map_err(|_| invalid("too many declaration uncertainty reasons"))?;
    reasons.push(reason.to_owned());
    Ok(index)
}

/// The exact verified replacement view: correct basis, Exact normalization,
/// full declared tokens (iterator comparison, no allocation) and the complete
/// native membership within the node's own source set.
fn is_exact_replacement_view(
    node: &GraphNode,
    declared_text: &str,
    members: &[GlyphId],
    node_sources: &BTreeSet<SourceRef>,
) -> bool {
    if node.basis != ViewBasis::SourceStructure {
        return false;
    }
    let NodeContent::Text { view } = &node.content else {
        return false;
    };
    if view.normalization != TextNormalization::Exact {
        return false;
    }
    if view.tokens.len() != declared_text.chars().count()
        || !view
            .tokens
            .iter()
            .zip(declared_text.chars())
            .all(|(token, character)| *token == ComparableToken::Scalar(character))
    {
        return false;
    }
    members
        .iter()
        .all(|glyph| node_sources.contains(&SourceRef::Native { glyph: *glyph }))
}

pub(super) fn role_kind(role: &str) -> NodeKind {
    match role.to_ascii_lowercase().as_str() {
        "document" => NodeKind::Document,
        "sect" | "section" | "h" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => NodeKind::Section,
        "p" | "paragraph" | "text" => NodeKind::Paragraph,
        "header" | "page_header" => NodeKind::Header,
        "footer" | "page_footer" => NodeKind::Footer,
        "l" | "list" => NodeKind::List,
        "li" | "list_item" => NodeKind::ListItem,
        "table" => NodeKind::Table,
        "tr" | "row" => NodeKind::Row,
        "column" => NodeKind::Column,
        "td" | "th" | "cell" => NodeKind::Cell,
        "figure" | "picture" => NodeKind::Figure,
        "caption" => NodeKind::Caption,
        "code" => NodeKind::Code,
        "formula" => NodeKind::Formula,
        _ => NodeKind::Unknown,
    }
}
