use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    model::{Document, Glyph, GlyphId, PageId},
    pdf::{ObjectRef, ParsedPdf, PdfObject},
};

use super::{
    Channel, ChannelInventory, DeclaredStructureText, DeclaredTextStatus, EvidenceIssue,
    NativeStructureKid, SourceRef, StructuredEvidence, StructuredValue,
    forms::{classify, dictionary},
};

/// A declared replacement captured during the walk; validation is finalized
/// only after the `ParentTree` bindings are known.
struct PendingDeclaration {
    element: u64,
    /// The narrow validated candidate: a single direct integer kid with
    /// non-empty complete membership.
    candidate: Option<(usize, Vec<GlyphId>)>,
    /// Why no candidate was established.
    reason: Option<String>,
}

mod parents;

#[derive(Clone, Copy, Debug)]
pub struct StructureLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_glyph_references: usize,
    pub max_label_bytes: usize,
}

impl Default for StructureLimits {
    fn default() -> Self {
        Self {
            max_nodes: 100_000,
            max_depth: 64,
            max_glyph_references: 1_000_000,
            max_label_bytes: 1_048_576,
        }
    }
}

pub struct StructureEvidence {
    pub elements: Vec<StructuredEvidence>,
    pub issues: Vec<EvidenceIssue>,
    pub inventory: ChannelInventory,
    pub key_inventory: super::KeyInventory,
    pub native_inventory: super::NativeStructureInventory,
}

struct Pending {
    object: PdfObject,
    parent: Option<u64>,
    parent_object: Option<ObjectRef>,
    page: Option<PageId>,
    order: u32,
    depth: usize,
}

type MarkKey = (PageId, Option<ObjectRef>, u32);

/// Imports native structure roles and their exact marked-content memberships.
/// Tags remain alternative views over the same glyphs as geometric layout.
/// Missing/repeated MCIDs, cycles, and unsupported object references never become
/// invented text. Reading order is the declared tag order, not author intent.
///
/// # Errors
/// Rejects an unreadable catalog/root or invalid source metadata. Descendant
/// failures retain partial structure and explicit unresolved dependencies.
pub fn extract_structure_evidence(
    pdf: &dyn ParsedPdf,
    native: &Document<Glyph>,
    backend: usize,
    first_id: u64,
    limits: StructureLimits,
) -> Result<StructureEvidence> {
    let mut result = StructureEvidence {
        native_inventory: super::NativeStructureInventory {
            backend,
            root: None,
            roots: Vec::new(),
            complete: false,
            parents: None,
        },
        key_inventory: super::KeyInventory {
            domain: super::KeyDomain::PdfStructureId,
            backend,
            complete: true,
        },
        elements: Vec::new(),
        issues: Vec::new(),
        inventory: ChannelInventory {
            page: None,
            channel: Channel::Relations,
            backend,
            sources: Vec::new(),
            complete: false,
        },
    };
    let trailer = pdf.trailer()?;
    let (catalog, _) = dictionary(
        pdf,
        trailer
            .get(b"Root".as_slice())
            .cloned()
            .ok_or_else(|| unresolved("missing PDF catalog"))?,
    )?;
    let Some(root) = catalog.get(b"StructTreeRoot".as_slice()) else {
        return Ok(result);
    };
    let (root, root_object) = dictionary(pdf, root.clone())?;
    result.native_inventory.root = root_object;
    let pages: HashMap<_, _> = pdf
        .pages()?
        .into_iter()
        .enumerate()
        .map(|(index, page)| (page.0, PageId(index as u32)))
        .collect();
    if native.marked_content().len() > limits.max_nodes {
        return Err(limit("marked-content sequences", limits.max_nodes));
    }
    let mut marks: HashMap<MarkKey, Vec<usize>> = HashMap::new();
    let native_glyphs = crate::model::index_glyphs(native)?;
    for (index, sequence) in native.marked_content().iter().enumerate() {
        if native.items().get(sequence.glyph_range.clone()).is_none() {
            return Err(unresolved("invalid marked-content glyph range"));
        }
        marks
            .entry((sequence.page, sequence.form, sequence.mcid))
            .or_default()
            .push(index);
    }
    let roots = children(pdf, root.get(b"K".as_slice()).cloned())?;
    let root_count = roots.len();
    if roots.len() > limits.max_nodes {
        return Err(limit("structure nodes", limits.max_nodes));
    }
    let mut pending: Vec<_> = roots
        .into_iter()
        .enumerate()
        .rev()
        .map(|(order, object)| Pending {
            object,
            parent: None,
            parent_object: root_object,
            page: None,
            order: order as u32,
            depth: 0,
        })
        .collect();
    let mut visited = HashSet::new();
    let mut nodes = 0usize;
    let mut glyph_references = 0usize;
    let mut label_bytes = 0usize;
    let mut pending_declarations: Vec<PendingDeclaration> = Vec::new();
    while let Some(item) = pending.pop() {
        let imported = (|| {
            nodes += 1;
            if nodes > limits.max_nodes {
                return Err(limit("structure traversal", limits.max_nodes));
            }
            if item.depth > limits.max_depth {
                return Err(limit("structure nesting depth", limits.max_depth));
            }
            let (element, object) = dictionary(pdf, item.object)?;
            if let Some(object) = object
                && !visited.insert(object)
            {
                return Err(unresolved("repeated or cyclic structure element"));
            }
            if let Some(parent) = item.parent_object {
                let actual = match element.get(b"P".as_slice()) {
                    Some(PdfObject::Reference(reference)) => pdf.terminal_reference(*reference)?,
                    _ => return Err(unresolved("structure parent reference is missing")),
                };
                if actual != parent {
                    return Err(unresolved(
                        "structure parent disagrees with child inventory",
                    ));
                }
            }
            let role = match element.get(b"S".as_slice()) {
                Some(PdfObject::Name(role)) => String::from_utf8(role.clone())
                    .map_err(|_| unresolved("structure role is not UTF-8"))?,
                _ => return Err(unresolved("structure role is missing")),
            };
            label_bytes = label_bytes.saturating_add(role.len());
            let identifier = match element.get(b"ID".as_slice()) {
                Some(PdfObject::String(bytes)) => Some(bytes),
                None => None,
                _ => return Err(unresolved("structure ID is not a byte string")),
            };
            label_bytes = label_bytes.saturating_add(identifier.map_or(0, Vec::len));
            if label_bytes > limits.max_label_bytes {
                return Err(limit("structure label bytes", limits.max_label_bytes));
            }
            // `ActualText` is the only replacement declaration; `Alt` is a
            // description and is never read here. Invalid encodings stay
            // explicit: the raw bytes are retained with an unresolved reason.
            let declaration = match element.get(b"ActualText".as_slice()) {
                None => None,
                Some(PdfObject::String(bytes)) => {
                    if bytes.len() > limits.max_label_bytes {
                        return Err(limit("declared replacement bytes", limits.max_label_bytes));
                    }
                    label_bytes = label_bytes.saturating_add(bytes.len());
                    if label_bytes > limits.max_label_bytes {
                        return Err(limit("structure label bytes", limits.max_label_bytes));
                    }
                    match crate::pdf::decode_text_string(bytes, limits.max_label_bytes) {
                        Ok(text) => {
                            label_bytes = label_bytes.saturating_add(text.len());
                            if label_bytes > limits.max_label_bytes {
                                return Err(limit("structure label bytes", limits.max_label_bytes));
                            }
                            Some((bytes.clone(), text, None))
                        }
                        Err(error) => Some((bytes.clone(), String::new(), Some(error.to_string()))),
                    }
                }
                Some(_) => Some((
                    Vec::new(),
                    String::new(),
                    Some("ActualText is not a string".into()),
                )),
            };
            let page = page_reference(pdf, element.get(b"Pg".as_slice()), item.page, &pages)?;
            let id = first_id
                .checked_add(result.elements.len() as u64)
                .ok_or_else(|| unresolved("structure identity overflow"))?;
            let source = SourceRef::Structured { element: id };
            let entries = children(pdf, element.get(b"K".as_slice()).cloned())?;
            if nodes
                .saturating_add(pending.len())
                .saturating_add(entries.len())
                > limits.max_nodes
            {
                return Err(limit("structure child inventory", limits.max_nodes));
            }
            nodes = nodes.saturating_add(entries.len());
            let mut glyphs = Vec::new();
            let mut content = vec![NativeStructureKid::Unresolved; entries.len()];
            let mut bound = true;
            let mut structural_children = Vec::new();
            let mut binding_errors = Vec::new();
            let single_integer_kid =
                entries.len() == 1 && matches!(entries.first(), Some(PdfObject::Integer(_)));
            let mut declaration_candidate: Option<(usize, Vec<GlyphId>)> = None;
            let mut declaration_reason: Option<String> = None;
            for (order, entry) in entries.into_iter().enumerate() {
                let membership = (|| match entry {
                    PdfObject::Integer(mcid) => bind_mcid(
                        native,
                        &marks,
                        page,
                        None,
                        mcid,
                        &mut glyph_references,
                        limits,
                    )
                    .map(Some),
                    other => {
                        let (child, _) = dictionary(pdf, other.clone())?;
                        match child.get(b"Type".as_slice()) {
                            Some(PdfObject::Name(kind)) if kind == b"MCR" => {
                                let child_page =
                                    page_reference(pdf, child.get(b"Pg".as_slice()), page, &pages)?;
                                let form = match child.get(b"Stm".as_slice()) {
                                    None => None,
                                    Some(PdfObject::Reference(reference)) => {
                                        Some(pdf.terminal_reference(*reference)?)
                                    }
                                    _ => {
                                        return Err(unresolved(
                                            "invalid marked-content stream reference",
                                        ));
                                    }
                                };
                                let mcid = match child.get(b"MCID".as_slice()) {
                                    Some(PdfObject::Integer(mcid)) => *mcid,
                                    _ => {
                                        return Err(unresolved(
                                            "missing marked-content identifier",
                                        ));
                                    }
                                };
                                bind_mcid(
                                    native,
                                    &marks,
                                    child_page,
                                    form,
                                    mcid,
                                    &mut glyph_references,
                                    limits,
                                )
                                .map(Some)
                            }
                            Some(PdfObject::Name(kind)) if kind == b"OBJR" => {
                                let (object, page) =
                                    parents::annotation(pdf, &child, page, &pages)?;
                                content[order] = NativeStructureKid::Annotation { object, page };
                                bound = false;
                                binding_errors.push(Error::Unsupported(
                                    "structure object-reference binding is not implemented".into(),
                                ));
                                Ok(None)
                            }
                            _ => {
                                structural_children.push(Pending {
                                    object: other,
                                    parent: Some(id),
                                    parent_object: object,
                                    page,
                                    order: order as u32,
                                    depth: item.depth + 1,
                                });
                                Ok(None)
                            }
                        }
                    }
                })();
                match membership {
                    Ok(Some((sequence, members))) => {
                        content[order] = NativeStructureKid::MarkedContent { sequence };
                        if single_integer_kid && declaration.is_some() {
                            if members.is_empty() {
                                declaration_reason =
                                    Some("marked-content membership is empty".into());
                            } else {
                                // The candidate duplicates the element membership
                                // for the declaration only; charge it before cloning.
                                glyph_references = glyph_references.saturating_add(members.len());
                                if glyph_references > limits.max_glyph_references {
                                    return Err(limit(
                                        "structure glyph references",
                                        limits.max_glyph_references,
                                    ));
                                }
                                declaration_candidate = Some((sequence, members.clone()));
                            }
                        }
                        if members.is_empty() {
                            bound = false;
                            binding_errors.push(unresolved(
                                "marked content has no complete native glyph membership",
                            ));
                        }
                        glyphs.extend(members);
                    }
                    Ok(None) => {
                        if single_integer_kid {
                            declaration_reason =
                                Some("structure kid is not a marked-content reference".into());
                        }
                    }
                    Err(error @ Error::LimitExceeded { .. }) => return Err(error),
                    Err(error) => {
                        if single_integer_kid {
                            declaration_reason = Some(error.to_string());
                        }
                        bound = false;
                        binding_errors.push(error);
                    }
                }
            }
            let unique: HashSet<_> = glyphs.iter().copied().collect();
            if unique.len() != glyphs.len()
                || (!glyphs.is_empty() && !structural_children.is_empty())
            {
                bound = false;
                binding_errors.push(unresolved(
                    "overlapping or mixed structure memberships require additional views",
                ));
            }
            if !bound {
                glyphs.clear();
            }
            // Marked content may explicitly refer to several pages; retain page
            // identity per glyph instead of assigning them to an invented page.
            let page = page.filter(|p| {
                glyphs
                    .iter()
                    .all(|id| native_glyphs.get(*id).is_some_and(|glyph| glyph.page == *p))
            });
            let declared_text = declaration.map(|(raw, text, decode_error)| {
                // An undecodable or empty declaration never validates: it stays
                // explicit with its raw bytes and an unresolved reason.
                let valid_text = decode_error.is_none() && !text.is_empty();
                let candidate = valid_text.then_some(declaration_candidate).flatten();
                let reason = decode_error
                    .clone()
                    .or_else(|| declaration_reason.clone())
                    .or_else(|| (!valid_text).then(|| "declared replacement text is empty".into()))
                    .or_else(|| {
                        candidate
                            .is_none()
                            .then(|| "replacement binding is not validated".into())
                    });
                pending_declarations.push(PendingDeclaration {
                    element: id,
                    candidate,
                    reason: reason.clone(),
                });
                DeclaredStructureText {
                    raw,
                    text,
                    status: DeclaredTextStatus::Unresolved,
                    glyphs: Vec::new(),
                    reason,
                }
            });
            result.elements.push(StructuredEvidence {
                id,
                page,
                bounds: None,
                object,
                backend,
                value: StructuredValue::StructureElement {
                    role,
                    identifier: identifier.cloned(),
                    text: None,
                    declared_text,
                    glyphs,
                    content: Some(content),
                    parent: item.parent,
                    order: Some(item.order),
                },
            });
            if let Some(parent) = item.parent {
                // The parent slot exists before descendant acquisition. A
                // failed child therefore leaves an explicit unresolved entry.
                let parent = &mut result.elements[(parent - first_id) as usize];
                if let StructuredValue::StructureElement {
                    content: Some(content),
                    ..
                } = &mut parent.value
                {
                    content[item.order as usize] = NativeStructureKid::Element { element: id };
                }
            } else {
                result.native_inventory.roots.push(id);
            }
            result.inventory.sources.push(source);
            for error in binding_errors {
                issue(&mut result, Some(source), error);
            }
            pending.extend(structural_children.into_iter().rev());
            Ok(())
        })();
        if let Err(error) = imported {
            result.key_inventory.complete = false;
            let stop = matches!(error, Error::LimitExceeded { .. });
            issue(
                &mut result,
                item.parent.map(|element| SourceRef::Structured { element }),
                error,
            );
            if stop {
                break;
            }
        }
    }
    result.native_inventory.complete = root_object.is_some()
        && result.native_inventory.roots.len() == root_count
        && result.elements.iter().all(|element| {
            matches!(&element.value,
            StructuredValue::StructureElement { content: Some(content), .. }
            if !content.contains(&NativeStructureKid::Unresolved))
        });
    match parents::extract(
        pdf,
        native,
        &pages,
        root.get(b"ParentTree".as_slice()),
        &mut nodes,
        limits,
    ) {
        Ok(bindings) => result.native_inventory.parents = Some(bindings),
        Err(error) => issue(&mut result, None, error),
    }
    finalize_declarations(&mut result, first_id, pending_declarations, native, limits);
    issue(
        &mut result,
        None,
        unresolved(
            "native tags do not establish complete semantic relationships; role maps, table axes, and object-reference bindings remain unexamined",
        ),
    );
    Ok(result)
}

/// Finalizes declared replacement bindings after `ParentTree` acquisition.
///
/// A declaration is validated only for a single complete marked-content
/// sequence with non-empty native membership, a reciprocal `ParentTree` owner,
/// no competing or nested declaration and no retained non-text paint on its
/// page. Everything else stays explicitly unresolved with the native view
/// untouched.
fn finalize_declarations(
    result: &mut StructureEvidence,
    first_id: u64,
    pending: Vec<PendingDeclaration>,
    native: &Document<Glyph>,
    limits: StructureLimits,
) {
    if pending.is_empty() {
        return;
    }
    let parents_acquired = result.native_inventory.parents.is_some();
    let owners: HashMap<usize, ObjectRef> = result
        .native_inventory
        .parents
        .as_ref()
        .map(|bindings| {
            bindings
                .iter()
                .map(|binding| (binding.sequence, binding.owner))
                .collect()
        })
        .unwrap_or_default();
    let declaring: HashSet<u64> = pending
        .iter()
        .map(|declaration| declaration.element)
        .collect();
    let parent_of: HashMap<u64, u64> = result
        .elements
        .iter()
        .filter_map(|element| match &element.value {
            StructuredValue::StructureElement {
                parent: Some(parent),
                ..
            } => Some((element.id, *parent)),
            _ => None,
        })
        .collect();
    let mut blocked: HashMap<u64, &'static str> = HashMap::new();
    // Nested replacement owners: an ancestor or descendant declaration may
    // cover the same content, so neither side may validate in isolation. The
    // walk has its own explicit budget and never consumes the traversal budget.
    let mut ancestry_work = 0usize;
    for declaration in &pending {
        let mut current = parent_of.get(&declaration.element).copied();
        let mut depth = 0usize;
        while let Some(ancestor) = current {
            if depth >= limits.max_depth {
                blocked.insert(declaration.element, "replacement ancestry depth limit");
                break;
            }
            depth += 1;
            ancestry_work = ancestry_work.saturating_add(1);
            if ancestry_work > limits.max_nodes {
                blocked.insert(declaration.element, "replacement ancestry node limit");
                break;
            }
            if declaring.contains(&ancestor) {
                blocked.insert(declaration.element, "nested replacement owners");
                blocked.insert(ancestor, "nested replacement owners");
            }
            current = parent_of.get(&ancestor).copied();
        }
    }
    // Overlapping memberships: two declarations replacing intersecting glyph
    // sets (including distinct nested MCIDs over the same glyphs) conflict.
    let mut glyph_owner: HashMap<GlyphId, u64> = HashMap::new();
    let mut overlap_work = 0usize;
    for declaration in &pending {
        let index = declaration
            .element
            .checked_sub(first_id)
            .and_then(|index| usize::try_from(index).ok());
        let glyphs = index
            .and_then(|index| result.elements.get(index))
            .and_then(|element| match &element.value {
                StructuredValue::StructureElement { glyphs, .. } => Some(glyphs),
                _ => None,
            });
        let Some(glyphs) = glyphs else {
            continue;
        };
        overlap_work = overlap_work.saturating_add(glyphs.len());
        if overlap_work > limits.max_glyph_references {
            blocked.insert(declaration.element, "replacement overlap work limit");
            continue;
        }
        for glyph in glyphs {
            if let Some(previous) = glyph_owner.insert(*glyph, declaration.element)
                && previous != declaration.element
            {
                blocked.insert(declaration.element, "overlapping replacement memberships");
                blocked.insert(previous, "overlapping replacement memberships");
            }
        }
    }
    // One page-level paint index: a scope is text-only only when its whole page
    // has no retained non-text paint. Form-internal and sibling-stream paints
    // are recorded on the invoking page, so a stream-local check could accept a
    // paint-bearing scope. This over-rejects pages with unrelated paint.
    let paint_pages: Option<HashSet<PageId>> = native
        .non_text_paint_bounds()
        .map(|paints| paints.iter().map(|paint| paint.page).collect());
    for declaration in pending {
        let Some(index) = declaration
            .element
            .checked_sub(first_id)
            .and_then(|index| usize::try_from(index).ok())
        else {
            continue;
        };
        let object = result
            .elements
            .get(index)
            .and_then(|element| element.object);
        let (status, glyphs, reason) = match declaration.candidate {
            Some((sequence, members)) => {
                let owner = owners.get(&sequence);
                let paint_free = paint_free_scope(native, sequence, paint_pages.as_ref());
                if !parents_acquired {
                    (
                        DeclaredTextStatus::Unresolved,
                        Vec::new(),
                        Some("ParentTree owner binding was not acquired".to_owned()),
                    )
                } else if owner.is_none() {
                    (
                        DeclaredTextStatus::Unresolved,
                        Vec::new(),
                        Some("marked-content sequence has no ParentTree owner".to_owned()),
                    )
                } else if owner != object.as_ref() {
                    (
                        DeclaredTextStatus::Unresolved,
                        Vec::new(),
                        Some("ParentTree owner disagrees with the declaring element".to_owned()),
                    )
                } else if let Some(conflict) = blocked.get(&declaration.element) {
                    (
                        DeclaredTextStatus::Unresolved,
                        Vec::new(),
                        Some((*conflict).to_owned()),
                    )
                } else {
                    match paint_free {
                        Ok(true) => (DeclaredTextStatus::Validated, members, None),
                        Ok(false) => (
                            DeclaredTextStatus::Unresolved,
                            Vec::new(),
                            Some("page retains non-text paint".to_owned()),
                        ),
                        Err(reason) => (
                            DeclaredTextStatus::Unresolved,
                            Vec::new(),
                            Some(reason.to_owned()),
                        ),
                    }
                }
            }
            None => (
                DeclaredTextStatus::Unresolved,
                Vec::new(),
                Some(
                    declaration
                        .reason
                        .clone()
                        .unwrap_or_else(|| "replacement binding is not validated".to_owned()),
                ),
            ),
        };
        if let Some(StructuredValue::StructureElement {
            declared_text: Some(declared),
            ..
        }) = result
            .elements
            .get_mut(index)
            .map(|element| &mut element.value)
        {
            declared.status = status;
            declared.glyphs = glyphs;
            declared.reason = reason;
        }
    }
}

/// Text-only scope evidence: the retained non-text paint inventory must be
/// available and must contain no paint on the sequence's page. Form-internal
/// and sibling-stream paints are recorded on the invoking page, so a page-level
/// check is the sound minimum; it over-rejects pages with unrelated paint.
fn paint_free_scope(
    native: &Document<Glyph>,
    sequence_index: usize,
    paint_pages: Option<&HashSet<PageId>>,
) -> std::result::Result<bool, &'static str> {
    let Some(paint_pages) = paint_pages else {
        return Err("non-text paint inventory is unavailable");
    };
    let Some(sequence) = native.marked_content().get(sequence_index) else {
        return Err("marked-content sequence is missing");
    };
    Ok(!paint_pages.contains(&sequence.page))
}

fn bind_mcid(
    native: &Document<Glyph>,
    marks: &HashMap<MarkKey, Vec<usize>>,
    page: Option<PageId>,
    form: Option<ObjectRef>,
    mcid: i64,
    used: &mut usize,
    limits: StructureLimits,
) -> Result<(usize, Vec<GlyphId>)> {
    let page = page.ok_or_else(|| unresolved("marked-content page is missing"))?;
    let mcid = u32::try_from(mcid).map_err(|_| unresolved("invalid MCID"))?;
    let matches = marks
        .get(&(page, form, mcid))
        .ok_or_else(|| unresolved("marked content was not extracted"))?;
    let [index] = matches.as_slice() else {
        return Err(unresolved(
            "MCID identifies multiple marked-content invocations",
        ));
    };
    let sequence = &native.marked_content()[*index];
    if !sequence.complete {
        return Err(unresolved("marked-content sequence is incomplete"));
    }
    let glyphs = &native.items()[sequence.glyph_range.clone()];
    if glyphs.iter().any(|glyph| glyph.page != page) {
        return Err(unresolved(
            "marked content has no complete native glyph membership",
        ));
    }
    *used = used.saturating_add(glyphs.len());
    if *used > limits.max_glyph_references {
        return Err(limit(
            "structure glyph references",
            limits.max_glyph_references,
        ));
    }
    Ok((*index, glyphs.iter().map(|glyph| glyph.id).collect()))
}

pub(super) fn validate_content(store: &super::EvidenceStore) -> Result<()> {
    let elements: HashMap<_, _> = store
        .structured
        .iter()
        .map(|element| (element.id, element))
        .collect();
    for parent in &store.structured {
        if let StructuredValue::StructureElement {
            parent: Some(owner),
            order,
            ..
        } = parent.value
            && let Some(owner) = elements.get(&owner)
            && let StructuredValue::StructureElement {
                content: Some(content),
                ..
            } = &owner.value
            && order.and_then(|order| content.get(order as usize))
                != Some(&NativeStructureKid::Element { element: parent.id })
        {
            return Err(super::invalid(
                "native structure parent omits its child slot",
            ));
        }
        let StructuredValue::StructureElement {
            content: Some(content),
            ..
        } = &parent.value
        else {
            continue;
        };
        for (order, kid) in content.iter().enumerate() {
            let NativeStructureKid::Element { element } = kid else {
                continue;
            };
            let child = elements
                .get(element)
                .ok_or_else(|| super::invalid("missing native structure child"))?;
            let StructuredValue::StructureElement {
                parent: owner,
                order: position,
                ..
            } = child.value
            else {
                return Err(super::invalid(
                    "native structure child is not a structure element",
                ));
            };
            if child.id == parent.id
                || child.backend != parent.backend
                || owner != Some(parent.id)
                || position.map(|position| position as usize) != Some(order)
            {
                return Err(super::invalid(
                    "native structure child disagrees with parent or order",
                ));
            }
        }
    }
    for inventory in &store.native_structures {
        let mut visited = HashSet::new();
        let mut pending = Vec::new();
        for (order, root) in inventory.roots.iter().enumerate() {
            let root = elements
                .get(root)
                .ok_or_else(|| super::invalid("missing native structure root"))?;
            let StructuredValue::StructureElement {
                parent: None,
                order: declared_order,
                ..
            } = root.value
            else {
                return Err(super::invalid("native structure root has a parent"));
            };
            if root.backend != inventory.backend
                || (inventory.complete && declared_order.map(|value| value as usize) != Some(order))
            {
                return Err(super::invalid(
                    "native structure root acquisition disagrees with its declaration",
                ));
            }
            pending.push(root.id);
        }
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                return Err(super::invalid("repeated native structure forest element"));
            }
            let element = elements[&id];
            let StructuredValue::StructureElement {
                content: Some(content),
                ..
            } = &element.value
            else {
                if inventory.complete {
                    return Err(super::invalid(
                        "complete native structure forest has missing content",
                    ));
                }
                continue;
            };
            for kid in content {
                match kid {
                    NativeStructureKid::Element { element } => pending.push(*element),
                    NativeStructureKid::Unresolved if inventory.complete => {
                        return Err(super::invalid(
                            "complete native structure forest has unresolved content",
                        ));
                    }
                    _ => {}
                }
            }
        }
        if inventory.complete
            && store.structured.iter().any(|element| {
                element.backend == inventory.backend
                    && matches!(element.value, StructuredValue::StructureElement { .. })
                    && !visited.contains(&element.id)
            })
        {
            return Err(super::invalid(
                "complete native structure forest omits an acquired element",
            ));
        }
    }
    Ok(())
}

fn children(pdf: &dyn ParsedPdf, object: Option<PdfObject>) -> Result<Vec<PdfObject>> {
    let Some(object) = object else {
        return Ok(Vec::new());
    };
    let resolved = match &object {
        PdfObject::Reference(reference) => pdf.resolve(*reference)?,
        _ => object.clone(),
    };
    Ok(match resolved {
        PdfObject::Array(children) => children,
        PdfObject::Null => Vec::new(),
        _ => vec![object],
    })
}

fn page_reference(
    pdf: &dyn ParsedPdf,
    object: Option<&PdfObject>,
    inherited: Option<PageId>,
    pages: &HashMap<ObjectRef, PageId>,
) -> Result<Option<PageId>> {
    match object {
        None => Ok(inherited),
        Some(PdfObject::Reference(reference)) => pages
            .get(&pdf.terminal_reference(*reference)?)
            .copied()
            .map(Some)
            .ok_or_else(|| unresolved("structure page is absent from the retained page tree")),
        _ => Err(unresolved("structure page is not an indirect reference")),
    }
}

fn issue(result: &mut StructureEvidence, source: Option<SourceRef>, error: Error) {
    result.issues.push(EvidenceIssue {
        boundary: None,
        page: None,
        channel: Channel::Relations,
        sources: source.into_iter().collect(),
        kind: classify(&error),
        reason: error.to_string(),
    });
}

fn unresolved(reason: &str) -> Error {
    Error::Unresolved(reason.into())
}
fn limit(resource: &'static str, limit: usize) -> Error {
    Error::LimitExceeded { resource, limit }
}
