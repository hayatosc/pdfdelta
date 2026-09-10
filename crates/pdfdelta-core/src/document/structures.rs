use std::collections::{HashMap, HashSet};

use crate::{
    Error, Result,
    model::{Document, Glyph, GlyphId, PageId},
    pdf::{ObjectRef, ParsedPdf, PdfObject},
};

use super::{
    Channel, ChannelInventory, EvidenceIssue, SourceRef, StructuredEvidence, StructuredValue,
    forms::{classify, dictionary},
};

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
            let mut bound = true;
            let mut structural_children = Vec::new();
            let mut binding_errors = Vec::new();
            for (order, entry) in entries.into_iter().enumerate() {
                let membership = match entry {
                    PdfObject::Integer(mcid) => bind_mcid(
                        native,
                        &marks,
                        page,
                        None,
                        mcid,
                        &mut glyph_references,
                        limits,
                    ),
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
                            }
                            Some(PdfObject::Name(kind)) if kind == b"OBJR" => {
                                Err(Error::Unsupported(
                                    "structure object-reference binding is not implemented".into(),
                                ))
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
                                continue;
                            }
                        }
                    }
                };
                match membership {
                    Ok(members) => glyphs.extend(members),
                    Err(error @ Error::LimitExceeded { .. }) => return Err(error),
                    Err(error) => {
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
                    glyphs,
                    parent: item.parent,
                    order: Some(item.order),
                },
            });
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
    issue(
        &mut result,
        None,
        unresolved(
            "native tags do not establish complete semantic relationships; role maps, table axes, and object-reference bindings remain unexamined",
        ),
    );
    Ok(result)
}

fn bind_mcid(
    native: &Document<Glyph>,
    marks: &HashMap<MarkKey, Vec<usize>>,
    page: Option<PageId>,
    form: Option<ObjectRef>,
    mcid: i64,
    used: &mut usize,
    limits: StructureLimits,
) -> Result<Vec<GlyphId>> {
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
    if glyphs.is_empty() || glyphs.iter().any(|glyph| glyph.page != page) {
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
    Ok(glyphs.iter().map(|glyph| glyph.id).collect())
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
