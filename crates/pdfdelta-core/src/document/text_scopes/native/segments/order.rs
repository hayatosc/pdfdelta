//! Closed forward declarations and inverse MCID ownership support local runs.
//! Unknown bindings, non-glyph items and repeated sources separate those runs.

use super::{
    BTreeMap, BTreeSet, BackendKind, Cow, DocumentView, GlyphId, Membership, SourceRef,
    StructuredValue, spend,
};
use crate::document::NativeStructureKid;

enum Frame {
    Element(u64),
    Kid(u64, usize),
}

fn finish(result: &mut Vec<Membership<'_>>, root: u64, start: usize, glyphs: &mut Vec<GlyphId>) {
    if !glyphs.is_empty() {
        result.push(Membership {
            structure: SourceRef::Structured { element: root },
            glyphs: Cow::Owned(std::mem::take(glyphs)),
            offset: start,
            convention: "native-k-parent-bound-page-regions-v1",
        });
    }
}

pub(super) fn acquire<'a>(
    view: DocumentView<'a>,
    remaining: &mut usize,
) -> Option<Vec<Membership<'a>>> {
    let [inventory] = view.evidence.native_structures.as_slice() else {
        return None;
    };
    if !inventory.complete
        || inventory.root.is_none()
        || view.evidence.backends.get(inventory.backend)?.kind != BackendKind::NativeParser
    {
        return None;
    }
    let bindings = inventory.parents.as_ref()?;
    let native = &view.evidence.native;
    let marks = native.marked_content();
    spend(
        remaining,
        native
            .items()
            .len()
            .saturating_add(marks.len())
            .saturating_add(bindings.len()),
    )?;
    let mut counts = vec![0u8; native.items().len()];
    let mut parents = vec![None; marks.len()];
    for binding in bindings {
        let parent = parents.get_mut(binding.sequence)?;
        if parent.replace(binding.owner).is_some() {
            return None;
        }
    }
    let mut elements = BTreeMap::new();
    spend(
        remaining,
        view.evidence
            .structured
            .len()
            .saturating_mul(view.evidence.structured.len().saturating_add(1).ilog2() as usize + 1),
    )?;
    for element in &view.evidence.structured {
        let StructuredValue::StructureElement { content, .. } = &element.value else {
            continue;
        };
        if view.evidence.backends.get(element.backend)?.kind != BackendKind::NativeParser {
            continue;
        }
        if element.backend != inventory.backend || elements.insert(element.id, element).is_some() {
            return None;
        }
        for kid in content.as_ref()? {
            spend(remaining, 1)?;
            match kid {
                NativeStructureKid::MarkedContent { sequence } => {
                    let mark = marks.get(*sequence)?;
                    if !mark.complete {
                        return None;
                    }
                    spend(remaining, mark.glyph_range.len())?;
                    for count in counts.get_mut(mark.glyph_range.clone())? {
                        *count = count.saturating_add(1);
                    }
                }
                NativeStructureKid::Unresolved => return None,
                _ => {}
            }
        }
    }
    let mut result = Vec::new();
    let mut visited = BTreeSet::new();
    for (root_order, root) in inventory.roots.iter().copied().enumerate() {
        let StructuredValue::StructureElement {
            parent: None,
            order,
            ..
        } = elements.get(&root)?.value
        else {
            return None;
        };
        if order.map(|order| order as usize) != Some(root_order) {
            return None;
        }
        let mut pending = vec![Frame::Element(root)];
        let (mut position, mut start) = (0usize, 0usize);
        let mut glyphs = Vec::new();
        while let Some(frame) = pending.pop() {
            spend(
                remaining,
                (elements.len().saturating_add(1).ilog2() as usize + 1) * 3,
            )?;
            match frame {
                Frame::Element(id) => {
                    if !visited.insert(id) {
                        return None;
                    }
                    let StructuredValue::StructureElement {
                        content: Some(content),
                        ..
                    } = &elements.get(&id)?.value
                    else {
                        return None;
                    };
                    spend(remaining, content.len())?;
                    pending.extend((0..content.len()).rev().map(|index| Frame::Kid(id, index)));
                }
                Frame::Kid(owner, index) => {
                    let owner = elements.get(&owner)?;
                    let StructuredValue::StructureElement {
                        content: Some(content),
                        ..
                    } = &owner.value
                    else {
                        return None;
                    };
                    match content.get(index)? {
                        NativeStructureKid::Element { element } => {
                            let StructuredValue::StructureElement { parent, order, .. } =
                                elements.get(element)?.value
                            else {
                                return None;
                            };
                            if parent != Some(owner.id)
                                || order.map(|order| order as usize) != Some(index)
                            {
                                return None;
                            }
                            pending.push(Frame::Element(*element));
                        }
                        NativeStructureKid::MarkedContent { sequence } => {
                            let mark = marks.get(*sequence)?;
                            spend(remaining, mark.glyph_range.len().saturating_add(1))?;
                            let raw = native.items().get(mark.glyph_range.clone())?;
                            // Even a complete zero-glyph mark can contain paint.
                            // It is a barrier rather than an invented empty string.
                            if raw.is_empty()
                                || owner.object.is_none()
                                || parents[*sequence] != owner.object
                            {
                                finish(&mut result, root, start, &mut glyphs);
                                position = position.checked_add(raw.len())?;
                                continue;
                            }
                            for (offset, glyph) in raw.iter().enumerate() {
                                if counts[mark.glyph_range.start + offset] == 1 {
                                    if glyphs.is_empty() {
                                        start = position;
                                    }
                                    glyphs.push(glyph.id);
                                } else {
                                    finish(&mut result, root, start, &mut glyphs);
                                }
                                position = position.checked_add(1)?;
                            }
                        }
                        NativeStructureKid::Annotation { .. } => {
                            finish(&mut result, root, start, &mut glyphs);
                        }
                        NativeStructureKid::Unresolved => return None,
                    }
                }
            }
        }
        finish(&mut result, root, start, &mut glyphs);
    }
    (visited.len() == elements.len()).then_some(result)
}
