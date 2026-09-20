use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    model::{GlyphId, PageId},
    normalize::ComparableToken,
};

use super::{
    BackendKind, Channel, EvidenceLimits, EvidenceStore, FieldValue, SourceRef,
    evidence::{bounded, invalid},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Document,
    Page,
    Section,
    Paragraph,
    Header,
    Footer,
    List,
    ListItem,
    Table,
    Row,
    Column,
    Cell,
    Form,
    Field,
    Figure,
    Caption,
    Code,
    Formula,
    Annotation,
    Unknown,
}

/// A source-backed interpretation is still a view, not a mutation of evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewBasis {
    SourceStructure,
    NativeLayout,
    /// Source geometry supports a structural hypothesis, not semantic identity.
    ReconstructedStructure,
    RenderedRegion,
    Recognition {
        backend: usize,
    },
    Model {
        backend: usize,
    },
}

impl ViewBasis {
    #[must_use]
    pub fn is_inferred(self) -> bool {
        matches!(
            self,
            Self::ReconstructedStructure | Self::Recognition { .. } | Self::Model { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextNormalization {
    Exact,
    /// Positions may be retained or removed independently, never selected by diff cost.
    Alternatives {
        optional_positions: Vec<usize>,
        /// In-process source validation, bound to the complete token projection.
        /// Serialized reports cannot convey this trust; reloaded alternatives
        /// must be rebuilt from source evidence before local comparison.
        #[serde(skip)]
        certificate: Option<super::NormalizationCertificate>,
    },
    Unresolved {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextView {
    pub tokens: Vec<ComparableToken>,
    /// One source set per token. Synthetic boundaries retain their neighbors.
    pub origins: Vec<Vec<SourceRef>>,
    /// False for synthetic layout separators; they do not inflate source counts.
    pub source_backed: Vec<bool>,
    pub normalization: TextNormalization,
}

impl TextView {
    pub fn display_text(&self) -> Option<String> {
        self.tokens.iter().map(ComparableToken::as_scalar).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeContent {
    Container,
    Text { view: TextView },
    Value { value: FieldValue },
    Visual { region: u64 },
    Unknown,
}

impl NodeContent {
    #[must_use]
    pub fn channel(&self) -> Option<Channel> {
        match self {
            Self::Text { .. } => Some(Channel::Text),
            Self::Value { .. } => Some(Channel::Forms),
            Self::Visual { .. } => Some(Channel::Visual),
            Self::Container | Self::Unknown => None,
        }
    }
}

/// Identity is scoped by typed containment; it is never matched by value alone.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IdentityKey {
    pub namespace: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub pages: Vec<PageId>,
    pub sources: Vec<SourceRef>,
    pub identity: Option<IdentityKey>,
    pub basis: ViewBasis,
    pub content: NodeContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    Precedes,
    RowMember,
    ColumnMember,
    LabelFor,
    CaptionFor,
    RefersTo,
    AppearanceFor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub sources: Vec<SourceRef>,
    pub basis: ViewBasis,
}

/// Overlapping physical material must not produce independent content events.
/// The relationship records a conflict, not unproved equivalence of readings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceConflict {
    pub sources: Vec<SourceRef>,
    pub reason: String,
}

/// Alternative partitions share a parent view. Each partition must preserve
/// exactly its sources; the solver may select at most one partition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlternativeViews {
    pub parent: NodeId,
    pub partitions: Vec<Vec<NodeId>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub alternatives: Vec<AlternativeViews>,
    pub source_conflicts: Vec<SourceConflict>,
    /// Explicitly observed relationships versus unexamined/inferred structure.
    pub relations_complete: bool,
    /// Sources marked uncertain by the declaration pass in `from_evidence`.
    /// Projection must not turn a node holding one of these sources from
    /// `Unresolved` back into an exact view. Empty without declarations.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub declaration_affected: BTreeSet<SourceRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphLimits {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_tokens: usize,
    pub max_references: usize,
    pub max_label_bytes: usize,
    pub max_normalization_work: usize,
}

impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            max_nodes: 500_000,
            max_edges: 1_000_000,
            max_tokens: 5_000_000,
            max_references: 10_000_000,
            max_label_bytes: 16 * 1024 * 1024,
            max_normalization_work: 1_000_000,
        }
    }
}

impl EvidenceStore {
    pub(super) fn source_pages(&self) -> impl Iterator<Item = (SourceRef, Option<PageId>)> + '_ {
        self.native
            .items()
            .iter()
            .map(|glyph| (SourceRef::Native { glyph: glyph.id }, Some(glyph.page)))
            .chain(
                self.native
                    .vector_lines()
                    .iter()
                    .map(|line| (SourceRef::NativeVector { line: line.id }, Some(line.page))),
            )
            .chain(
                self.rendered
                    .iter()
                    .map(|region| (SourceRef::Rendered { region: region.id }, Some(region.page))),
            )
            .chain(self.structured.iter().map(|element| {
                (
                    SourceRef::Structured {
                        element: element.id,
                    },
                    element.page,
                )
            }))
    }
}

impl DocumentGraph {
    /// Checks source accounting and graph invariants without assuming one global order.
    ///
    /// # Errors
    /// Rejects malformed views, dangling/cyclic relations, invalid source origins,
    /// and incomplete or overlapping alternative partitions; enforces aggregate limits.
    pub fn validate(
        &self,
        store: &EvidenceStore,
        evidence: EvidenceLimits,
        limits: GraphLimits,
    ) -> Result<()> {
        self.validate_indexed(store, evidence, limits).map(|_| ())
    }

    pub(super) fn validate_indexed<'a>(
        &self,
        store: &'a EvidenceStore,
        evidence: EvidenceLimits,
        limits: GraphLimits,
    ) -> Result<super::evidence::NativeIndex<'a>> {
        let mut native = store.validate_indexed(evidence)?;
        bounded(self.nodes.len(), limits.max_nodes, "graph nodes")?;
        bounded(self.edges.len(), limits.max_edges, "graph edges")?;
        bounded(
            self.alternatives.len(),
            limits.max_nodes,
            "alternative view groups",
        )?;
        bounded(
            self.source_conflicts.len(),
            limits.max_nodes,
            "source conflicts",
        )?;
        let sources = store.source_pages().collect::<BTreeMap<_, _>>();
        // Validated declared replacements cover their native membership at the
        // membership level. The exemption is typed by the evidence and applies
        // only to the exact verified replacement view: correct owner and basis,
        // full declared tokens, Exact normalization and the complete native
        // membership. Every other node must satisfy source conservation.
        let declared_memberships: BTreeMap<u64, (&str, &[GlyphId])> = store
            .structured
            .iter()
            .filter_map(|element| {
                let super::StructuredValue::StructureElement {
                    declared_text: Some(declared),
                    glyphs,
                    ..
                } = &element.value
                else {
                    return None;
                };
                if declared.status != super::DeclaredTextStatus::Validated
                    || declared.glyphs.is_empty()
                    || declared.glyphs != *glyphs
                {
                    return None;
                }
                Some((element.id, (declared.text.as_str(), glyphs.as_slice())))
            })
            .collect();
        let recognized_sources: BTreeSet<_> = store
            .structured
            .iter()
            .filter(|element| {
                matches!(element.value, super::StructuredValue::RecognizedText { .. })
            })
            .map(|element| SourceRef::Structured {
                element: element.id,
            })
            .collect();
        let pages = store
            .pages
            .iter()
            .map(|page| page.page)
            .collect::<BTreeSet<_>>();
        let mut nodes = BTreeMap::new();
        let mut references = 0usize;
        let mut tokens = 0usize;
        let mut label_bytes = 0usize;
        // The declaration-affected set is public graph input, so it is charged
        // against the same aggregate reference budget as node sources and every
        // member must exist in the evidence store before it is copied into the
        // index. The charge precedes the copy.
        charge(
            &mut references,
            self.declaration_affected.len(),
            limits.max_references,
            "graph references",
        )?;
        if self
            .declaration_affected
            .iter()
            .any(|source| !sources.contains_key(source))
        {
            return Err(invalid(
                "declaration-affected source is not present in the evidence store",
            ));
        }
        for node in &self.nodes {
            if nodes.insert(node.id, node).is_some() {
                return Err(invalid("duplicate graph node identity"));
            }
            charge(
                &mut references,
                node.pages.len(),
                limits.max_references,
                "graph references",
            )?;
            let node_pages = node.pages.iter().copied().collect::<BTreeSet<_>>();
            if node_pages.len() != node.pages.len()
                || node_pages.iter().any(|page| !pages.contains(page))
            {
                return Err(invalid("graph node references an unknown page"));
            }
            validate_basis(node.basis, store)?;
            let source_set = check_sources(&node.sources, &sources, &mut references, limits)?;
            if !node.basis.is_inferred() && !source_set.is_disjoint(&recognized_sources) {
                return Err(invalid(
                    "recognized text cannot claim a direct-source view basis",
                ));
            }
            if let Some(source) = node
                .sources
                .iter()
                .find(|source| sources[*source].is_some_and(|page| !node_pages.contains(&page)))
            {
                return Err(invalid(&format!(
                    "graph node {:?} omits a contributing source page {:?} from {:?}",
                    node.id, sources[source], source,
                )));
            }
            if let Some(key) = &node.identity {
                if key.namespace.is_empty() || key.value.is_empty() {
                    return Err(invalid("empty graph identity key"));
                }
                charge(
                    &mut label_bytes,
                    key.namespace.len(),
                    limits.max_label_bytes,
                    "graph label bytes",
                )?;
                charge(
                    &mut label_bytes,
                    key.value.len(),
                    limits.max_label_bytes,
                    "graph label bytes",
                )?;
            }
            if !matches!(node.content, NodeContent::Container) && source_set.is_empty() {
                return Err(invalid("content view has no original evidence"));
            }
            match &node.content {
                NodeContent::Text { view } => {
                    charge(
                        &mut tokens,
                        view.tokens.len(),
                        limits.max_tokens,
                        "graph text tokens",
                    )?;
                    if view.tokens.len() != view.origins.len()
                        || view.tokens.len() != view.source_backed.len()
                    {
                        return Err(invalid(
                            "text view tokens and source projection differ in length",
                        ));
                    }
                    for token in &view.tokens {
                        if let ComparableToken::Unmapped { font_hash, .. } = token {
                            charge(
                                &mut label_bytes,
                                font_hash.0.len(),
                                limits.max_label_bytes,
                                "graph label bytes",
                            )?;
                        }
                    }
                    let mut used = BTreeSet::new();
                    for origins in &view.origins {
                        let token_sources =
                            check_sources(origins, &sources, &mut references, limits)?;
                        if token_sources.is_empty() || !token_sources.is_subset(&source_set) {
                            return Err(invalid(
                                "text token lies outside its view source evidence",
                            ));
                        }
                        used.extend(token_sources);
                    }
                    // A node presenting a validated declaration's exact text
                    // must carry the complete native membership it replaces.
                    for source in &node.sources {
                        if let SourceRef::Structured { element } = source
                            && let Some((declared_text, members)) =
                                declared_memberships.get(element)
                            && declares_tokens(view, declared_text)
                            && !members.iter().all(|glyph| {
                                source_set.contains(&SourceRef::Native { glyph: *glyph })
                            })
                        {
                            return Err(invalid(
                                "declared replacement view omits its native membership",
                            ));
                        }
                    }
                    if used != source_set {
                        for source in &node.sources {
                            if let SourceRef::Structured { element } = source
                                && let Some((declared_text, members)) =
                                    declared_memberships.get(element)
                                && is_exact_declared_replacement_view(
                                    node,
                                    view,
                                    declared_text,
                                    members,
                                    &source_set,
                                )
                            {
                                used.extend(
                                    members
                                        .iter()
                                        .filter_map(|glyph| {
                                            source_set
                                                .get(&SourceRef::Native { glyph: *glyph })
                                                .copied()
                                        })
                                        .collect::<Vec<_>>(),
                                );
                            }
                        }
                    }
                    if used != source_set {
                        return Err(invalid("text view silently omits source evidence"));
                    }
                    match &view.normalization {
                        TextNormalization::Alternatives {
                            optional_positions, ..
                        } => {
                            charge(
                                &mut references,
                                optional_positions.len(),
                                limits.max_references,
                                "graph references",
                            )?;
                            if optional_positions.windows(2).any(|pair| pair[0] >= pair[1])
                                || optional_positions.iter().any(|index| {
                                    !matches!(
                                        view.tokens.get(*index),
                                        Some(ComparableToken::Scalar('-' | '\u{2010}'))
                                    )
                                })
                            {
                                return Err(invalid(
                                    "normalization alternatives require distinct retained hyphen positions",
                                ));
                            }
                        }
                        TextNormalization::Unresolved { reason } => {
                            if reason.is_empty() {
                                return Err(invalid("unresolved normalization has no reason"));
                            }
                            charge(
                                &mut label_bytes,
                                reason.len(),
                                limits.max_label_bytes,
                                "graph label bytes",
                            )?;
                        }
                        TextNormalization::Exact => {}
                    }
                }
                NodeContent::Value { value } => {
                    let values = match value {
                        FieldValue::Text(value) => std::slice::from_ref(value),
                        FieldValue::Choices(values) => values,
                        FieldValue::Name(bytes) => {
                            charge(
                                &mut label_bytes,
                                bytes.len(),
                                limits.max_label_bytes,
                                "field name value bytes",
                            )?;
                            &[]
                        }
                        FieldValue::Unresolved { raw_bytes, reason } => {
                            charge(
                                &mut label_bytes,
                                raw_bytes.as_ref().map_or(0, Vec::len),
                                limits.max_label_bytes,
                                "unresolved field bytes",
                            )?;
                            std::slice::from_ref(reason)
                        }
                        FieldValue::Selected(_) | FieldValue::Empty => &[],
                    };
                    charge(
                        &mut references,
                        values.len(),
                        limits.max_references,
                        "graph references",
                    )?;
                    for value in values {
                        charge(
                            &mut label_bytes,
                            value.len(),
                            limits.max_label_bytes,
                            "graph label bytes",
                        )?;
                    }
                }
                NodeContent::Visual { region } => {
                    if !source_set.contains(&SourceRef::Rendered { region: *region }) {
                        return Err(invalid("visual view has no matching rendered source"));
                    }
                }
                NodeContent::Container | NodeContent::Unknown => {}
            }
        }
        let mut parents = BTreeMap::new();
        let mut adjacency = BTreeMap::<NodeId, Vec<NodeId>>::new();
        let mut incoming = BTreeMap::<NodeId, usize>::new();
        let mut unique_edges = BTreeSet::new();
        for edge in &self.edges {
            if !nodes.contains_key(&edge.from)
                || !nodes.contains_key(&edge.to)
                || edge.from == edge.to
                || !unique_edges.insert((edge.kind, edge.from, edge.to))
            {
                return Err(invalid("dangling, reflexive, or duplicate graph edge"));
            }
            validate_basis(edge.basis, store)?;
            check_sources(&edge.sources, &sources, &mut references, limits)?;
            if !edge.basis.is_inferred()
                && edge
                    .sources
                    .iter()
                    .any(|source| recognized_sources.contains(source))
            {
                return Err(invalid(
                    "recognition-backed relations must retain an inferred basis",
                ));
            }
            if edge.kind == EdgeKind::Contains {
                if parents.insert(edge.to, edge.from).is_some() {
                    return Err(invalid("multiple containment parents"));
                }
                adjacency.entry(edge.from).or_default().push(edge.to);
                *incoming.entry(edge.to).or_default() += 1;
            }
        }
        let mut queue = nodes
            .keys()
            .filter(|id| !incoming.contains_key(id))
            .copied()
            .collect::<VecDeque<_>>();
        let mut visited = 0usize;
        while let Some(node) = queue.pop_front() {
            visited += 1;
            for child in adjacency.get(&node).into_iter().flatten() {
                let count = incoming
                    .get_mut(child)
                    .expect("hierarchy child has an incoming edge");
                *count -= 1;
                if *count == 0 {
                    queue.push_back(*child);
                }
            }
        }
        if visited != nodes.len() {
            return Err(invalid("cyclic graph containment"));
        }
        for alternative in &self.alternatives {
            let parent = nodes
                .get(&alternative.parent)
                .ok_or_else(|| invalid("missing alternative parent"))?;
            charge(
                &mut references,
                parent.sources.len(),
                limits.max_references,
                "graph references",
            )?;
            let parent_sources = parent.sources.iter().copied().collect::<BTreeSet<_>>();
            charge(
                &mut references,
                alternative.partitions.len(),
                limits.max_references,
                "graph references",
            )?;
            if alternative.partitions.is_empty() {
                return Err(invalid("empty alternative view group"));
            }
            for partition in &alternative.partitions {
                charge(
                    &mut references,
                    partition.len(),
                    limits.max_references,
                    "graph references",
                )?;
                let mut represented = BTreeSet::new();
                for child in partition {
                    let child = nodes
                        .get(child)
                        .ok_or_else(|| invalid("missing alternative child"))?;
                    charge(
                        &mut references,
                        child.sources.len(),
                        limits.max_references,
                        "graph references",
                    )?;
                    if child.id == parent.id
                        || child
                            .sources
                            .iter()
                            .any(|source| !represented.insert(*source))
                    {
                        return Err(invalid("alternative partition overlaps its evidence"));
                    }
                }
                if represented != parent_sources {
                    return Err(invalid("alternative partition loses or adds evidence"));
                }
            }
        }
        for conflict in &self.source_conflicts {
            if conflict.reason.is_empty() || conflict.sources.len() < 2 {
                return Err(invalid("invalid source conflict"));
            }
            charge(
                &mut label_bytes,
                conflict.reason.len(),
                limits.max_label_bytes,
                "graph label bytes",
            )?;
            check_sources(&conflict.sources, &sources, &mut references, limits)?;
        }
        native
            .declaration_affected
            .clone_from(&self.declaration_affected);
        Ok(native)
    }
}

fn validate_basis(basis: ViewBasis, store: &EvidenceStore) -> Result<()> {
    let expected = match basis {
        ViewBasis::Recognition { backend } => Some((backend, BackendKind::Ocr)),
        ViewBasis::Model { backend } => Some((backend, BackendKind::StructureModel)),
        _ => None,
    };
    if expected.is_some_and(|(backend, kind)| {
        store
            .backends
            .get(backend)
            .is_none_or(|actual| actual.kind != kind)
    }) {
        return Err(invalid(
            "view basis refers to an incompatible or missing provider",
        ));
    }
    Ok(())
}

fn check_sources(
    references: &[SourceRef],
    sources: &BTreeMap<SourceRef, Option<PageId>>,
    used: &mut usize,
    limits: GraphLimits,
) -> Result<BTreeSet<SourceRef>> {
    charge(
        used,
        references.len(),
        limits.max_references,
        "graph references",
    )?;
    let mut unique = BTreeSet::new();
    for source in references {
        if !sources.contains_key(source) || !unique.insert(*source) {
            return Err(invalid("dangling or duplicated graph source reference"));
        }
    }
    Ok(unique)
}

/// Token comparison without allocating a declared-token vector.
fn declares_tokens(view: &TextView, declared_text: &str) -> bool {
    view.tokens.len() == declared_text.chars().count()
        && view
            .tokens
            .iter()
            .zip(declared_text.chars())
            .all(|(token, character)| *token == ComparableToken::Scalar(character))
}

/// The exact verified replacement view predicate shared with provider
/// construction: only this node may cover native membership sources without
/// emitting a token per glyph.
fn is_exact_declared_replacement_view(
    node: &GraphNode,
    view: &TextView,
    declared_text: &str,
    members: &[GlyphId],
    source_set: &BTreeSet<SourceRef>,
) -> bool {
    if node.basis != ViewBasis::SourceStructure || view.normalization != TextNormalization::Exact {
        return false;
    }
    if !declares_tokens(view, declared_text) {
        return false;
    }
    members
        .iter()
        .all(|glyph| source_set.contains(&SourceRef::Native { glyph: *glyph }))
}

pub(super) fn charge(
    used: &mut usize,
    count: usize,
    limit: usize,
    resource: &'static str,
) -> Result<()> {
    *used = used
        .checked_add(count)
        .ok_or(crate::Error::LimitExceeded { resource, limit })?;
    bounded(*used, limit, resource)
}
