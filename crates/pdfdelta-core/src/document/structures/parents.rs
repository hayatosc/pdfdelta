//! Bounded native parent lookup and non-glyph object-reference acquisition.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{StructureLimits, dictionary, limit, page_reference, unresolved};
use crate::{
    Result,
    document::NativeStructureParent,
    model::{Document, Glyph, PageId},
    pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject},
};

pub(super) fn annotation(
    pdf: &dyn ParsedPdf,
    entry: &PdfDict,
    inherited: Option<PageId>,
    pages: &HashMap<ObjectRef, PageId>,
) -> Result<(ObjectRef, PageId)> {
    let page = page_reference(pdf, entry.get(b"Pg".as_slice()), inherited, pages)?
        .ok_or_else(|| unresolved("object-reference page is missing"))?;
    let Some(PdfObject::Reference(reference)) = entry.get(b"Obj".as_slice()) else {
        return Err(unresolved("object-reference target is missing"));
    };
    let object = pdf.terminal_reference(*reference)?;
    let (target, _) = dictionary(pdf, PdfObject::Reference(object))?;
    if !matches!(target.get(b"Type".as_slice()), Some(PdfObject::Name(kind)) if kind == b"Annot")
        || !matches!(target.get(b"Subtype".as_slice()), Some(PdfObject::Name(_)))
        || target.contains_key(b"StructParents".as_slice())
    {
        return Err(unresolved("object reference is not an acquired annotation"));
    }
    if target.contains_key(b"P".as_slice())
        && page_reference(pdf, target.get(b"P".as_slice()), None, pages)? != Some(page)
    {
        return Err(unresolved(
            "annotation page disagrees with object reference",
        ));
    }
    Ok((object, page))
}

enum ParentValue {
    Sequences(Vec<Option<ObjectRef>>),
    Object,
}

fn spend(used: &mut usize, count: usize, limits: StructureLimits) -> Result<()> {
    *used = used.saturating_add(count);
    if *used > limits.max_nodes {
        return Err(limit("structure parent lookup", limits.max_nodes));
    }
    Ok(())
}

fn value(
    pdf: &dyn ParsedPdf,
    object: PdfObject,
    used: &mut usize,
    limits: StructureLimits,
) -> Result<ParentValue> {
    let resolved = match &object {
        PdfObject::Reference(reference) => pdf.resolve(*reference)?,
        _ => object.clone(),
    };
    if let PdfObject::Array(entries) = resolved {
        spend(used, entries.len(), limits)?;
        return entries
            .into_iter()
            .map(|entry| match entry {
                PdfObject::Null => Ok(None),
                PdfObject::Reference(reference) => pdf.terminal_reference(reference).map(Some),
                _ => Err(unresolved("invalid structural parent array entry")),
            })
            .collect::<Result<Vec<_>>>()
            .map(ParentValue::Sequences);
    }
    if matches!(object, PdfObject::Reference(_)) && matches!(resolved, PdfObject::Dictionary(_)) {
        return Ok(ParentValue::Object);
    }
    Err(unresolved("invalid structural parent number-tree value"))
}

enum Frame {
    Enter(PdfObject, usize),
    Exit(Option<(i64, i64)>, usize),
}

fn number_tree(
    pdf: &dyn ParsedPdf,
    root: PdfObject,
    used: &mut usize,
    limits: StructureLimits,
) -> Result<BTreeMap<i64, ParentValue>> {
    let mut values = BTreeMap::new();
    let mut keys = Vec::new();
    let mut visited = HashSet::new();
    let mut pending = vec![Frame::Enter(root, 0)];
    while let Some(frame) = pending.pop() {
        spend(used, 1, limits)?;
        let (object, depth) = match frame {
            Frame::Exit(bounds, start) => {
                if let Some((low, high)) = bounds
                    && (keys.get(start) != Some(&low) || keys.last() != Some(&high))
                {
                    return Err(unresolved(
                        "structural parent number-tree limits disagree with entries",
                    ));
                }
                continue;
            }
            Frame::Enter(object, depth) => (object, depth),
        };
        if depth > limits.max_depth {
            return Err(limit("structure parent nesting depth", limits.max_depth));
        }
        let (node, reference) = dictionary(pdf, object)?;
        if let Some(reference) = reference
            && !visited.insert(reference)
        {
            return Err(unresolved("repeated structural parent number-tree node"));
        }
        let bounds = match node.get(b"Limits".as_slice()) {
            None => None,
            Some(PdfObject::Array(entries)) => match entries.as_slice() {
                [PdfObject::Integer(low), PdfObject::Integer(high)] if 0 <= *low && low <= high => {
                    Some((*low, *high))
                }
                _ => return Err(unresolved("invalid structural parent number-tree limits")),
            },
            _ => return Err(unresolved("invalid structural parent number-tree limits")),
        };
        pending.push(Frame::Exit(bounds, keys.len()));
        match (node.get(b"Nums".as_slice()), node.get(b"Kids".as_slice())) {
            (Some(PdfObject::Array(entries)), None) if entries.len() % 2 == 0 => {
                spend(used, entries.len(), limits)?;
                for pair in entries.as_chunks::<2>().0 {
                    let PdfObject::Integer(key) = pair[0] else {
                        return Err(unresolved("noninteger structural parent key"));
                    };
                    if key < 0 || keys.last().is_some_and(|previous| *previous >= key) {
                        return Err(unresolved("duplicate or unordered structural parent key"));
                    }
                    let entry = value(pdf, pair[1].clone(), used, limits)?;
                    values.insert(key, entry);
                    keys.push(key);
                }
            }
            (None, Some(PdfObject::Array(children))) if !children.is_empty() => {
                spend(used, children.len(), limits)?;
                pending.extend(
                    children
                        .iter()
                        .rev()
                        .cloned()
                        .map(|child| Frame::Enter(child, depth + 1)),
                );
            }
            _ => return Err(unresolved("invalid structural parent number-tree node")),
        }
    }
    Ok(values)
}

pub(super) fn extract(
    pdf: &dyn ParsedPdf,
    native: &Document<Glyph>,
    pages: &HashMap<ObjectRef, PageId>,
    tree: Option<&PdfObject>,
    used: &mut usize,
    limits: StructureLimits,
) -> Result<Vec<NativeStructureParent>> {
    let Some(tree) = tree else {
        return Ok(Vec::new());
    };
    let values = number_tree(pdf, tree.clone(), used, limits)?;
    let page_objects: HashMap<_, _> = pages
        .iter()
        .map(|(object, page)| (*page, *object))
        .collect();
    let mut containers = HashMap::new();
    let mut keys = HashSet::new();
    let mut bindings = Vec::new();
    for (sequence, mark) in native.marked_content().iter().enumerate() {
        spend(used, 1, limits)?;
        let container = mark
            .form
            .or_else(|| page_objects.get(&mark.page).copied())
            .ok_or_else(|| unresolved("marked-content container is missing"))?;
        let key = if let Some(key) = containers.get(&container) {
            *key
        } else {
            let container_dict = match pdf.resolve(container)? {
                PdfObject::Dictionary(dictionary) if mark.form.is_none() => dictionary,
                PdfObject::Stream(dictionary)
                    if mark.form.is_some()
                        && matches!(dictionary.get(b"Subtype".as_slice()), Some(PdfObject::Name(kind)) if kind == b"Form") =>
                {
                    dictionary
                }
                _ => return Err(unresolved("invalid marked-content parent container")),
            };
            let key = match container_dict.get(b"StructParents".as_slice()) {
                None => None,
                Some(PdfObject::Integer(key))
                    if *key >= 0 && !container_dict.contains_key(b"StructParent".as_slice()) =>
                {
                    Some(*key)
                }
                _ => return Err(unresolved("invalid marked-content structural parent key")),
            };
            if let Some(key) = key
                && !keys.insert(key)
            {
                return Err(unresolved(
                    "structural parent key is shared by multiple content containers",
                ));
            }
            containers.insert(container, key);
            key
        };
        let Some(ParentValue::Sequences(parents)) = key.and_then(|key| values.get(&key)) else {
            continue;
        };
        if let Some(Some(owner)) = parents.get(mark.mcid as usize) {
            bindings.push(NativeStructureParent {
                sequence,
                owner: *owner,
            });
        }
    }
    Ok(bindings)
}
