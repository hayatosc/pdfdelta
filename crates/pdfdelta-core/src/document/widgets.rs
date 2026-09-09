use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{FormWidget, RenderedEvidence, evidence::invalid};
use crate::{
    Error, Result,
    model::{PageId, Rect, Vec2},
    pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, PageCoordinateFrame},
};

pub(super) struct WidgetContext {
    pages: HashMap<ObjectRef, PageId>,
    attached: HashMap<ObjectRef, Option<PageId>>,
    frames: Vec<Result<PageCoordinateFrame>>,
}

impl WidgetContext {
    pub(super) fn new(pdf: &dyn ParsedPdf, max_nodes: usize) -> Result<Self> {
        let pages = pdf.pages()?;
        if pages.len() > max_nodes {
            return Err(limit(max_nodes));
        }
        let mut attached = HashMap::new();
        let mut visited = 0usize;
        for (index, page) in pages.iter().enumerate() {
            let page_id = PageId(index as u32);
            let dict = pdf.page_dict(*page)?;
            let Some(annotations) = dict.get(b"Annots".as_slice()) else {
                continue;
            };
            let (PdfObject::Array(annotations), _) =
                super::forms::resolve(pdf, annotations.clone())?
            else {
                return Err(invalid("page annotation inventory is not an array"));
            };
            visited = visited.saturating_add(annotations.len());
            if visited > max_nodes {
                return Err(limit(max_nodes));
            }
            for annotation in annotations {
                let (dict, reference) = super::forms::dictionary(pdf, annotation)?;
                if matches!(dict.get(b"Subtype".as_slice()), Some(PdfObject::Name(name)) if name == b"Widget")
                    && let Some(reference) = reference
                {
                    attached
                        .entry(reference)
                        .and_modify(|page| *page = None)
                        .or_insert(Some(page_id));
                }
            }
        }
        let frames =
            ContentStreamGlyphExtractor.page_frames(pdf, ExtractionLimits::default(), max_nodes)?;
        Ok(Self {
            pages: pages
                .into_iter()
                .enumerate()
                .map(|(index, page)| (page.0, PageId(index as u32)))
                .collect(),
            attached,
            frames,
        })
    }

    pub(super) fn widget(
        &self,
        pdf: &dyn ParsedPdf,
        dict: &PdfDict,
        object: Option<ObjectRef>,
    ) -> FormWidget {
        let mut widget = FormWidget {
            object,
            page: None,
            bounds: None,
            normal_appearance: None,
            crop: None,
            unresolved: None,
        };
        let result: Result<()> = (|| {
            let attached = object.and_then(|object| self.attached.get(&object).copied());
            if attached == Some(None) {
                return Err(invalid("widget is attached more than once"));
            }
            let declared = match dict.get(b"P".as_slice()) {
                Some(PdfObject::Reference(reference)) => Some(
                    *self
                        .pages
                        .get(reference)
                        .ok_or_else(|| invalid("widget page reference is absent"))?,
                ),
                Some(_) => return Err(invalid("widget page is not an indirect reference")),
                None => None,
            };
            if let (Some(declared), Some(Some(attached))) = (declared, attached)
                && declared != attached
            {
                return Err(invalid("widget page and annotation membership disagree"));
            }
            let page = attached.flatten().ok_or_else(|| {
                invalid("widget is not uniquely attached to a page annotation inventory")
            })?;
            widget.page = Some(page);
            let raw = dict
                .get(b"Rect".as_slice())
                .ok_or_else(|| invalid("widget rectangle is missing"))?;
            let (PdfObject::Array(values), _) = super::forms::resolve(pdf, raw.clone())? else {
                return Err(invalid("widget rectangle is not an array"));
            };
            if values.len() != 4 {
                return Err(invalid("widget rectangle must have four coordinates"));
            }
            let mut coordinates = [0.0; 4];
            for (target, value) in coordinates.iter_mut().zip(values) {
                *target = match value {
                    PdfObject::Integer(value) => value as f64,
                    PdfObject::Real(value) => value,
                    _ => return Err(invalid("widget coordinate is not numeric")),
                };
            }
            let frame = self
                .frames
                .get(page.0 as usize)
                .ok_or_else(|| invalid("widget page frame is missing"))?
                .as_ref()
                .map_err(|error| invalid(&error.to_string()))?;
            widget.bounds = Some(frame.map_box(Rect {
                min: Vec2 {
                    x: coordinates[0],
                    y: coordinates[1],
                },
                max: Vec2 {
                    x: coordinates[2],
                    y: coordinates[3],
                },
            })?);
            if let Some(flags) = dict.get(b"F".as_slice()) {
                let PdfObject::Integer(flags) = flags else {
                    return Err(invalid("widget flags are not an integer"));
                };
                if *flags < 0 || flags & (1 | 2 | 8 | 16 | 32 | 256) != 0 {
                    return Err(invalid(
                        "widget visibility flags require separate interpretation",
                    ));
                }
            }
            let ap = dict
                .get(b"AP".as_slice())
                .ok_or_else(|| invalid("widget normal appearance is missing"))?;
            let (ap, _) = super::forms::dictionary(pdf, ap.clone())?;
            let normal = ap
                .get(b"N".as_slice())
                .ok_or_else(|| invalid("widget normal appearance is missing"))?;
            let (normal, reference) = super::forms::resolve(pdf, normal.clone())?;
            if !matches!(normal, PdfObject::Stream(_)) {
                return Err(invalid("state-selected widget rendering is not supported"));
            }
            widget.normal_appearance = Some(
                reference.ok_or_else(|| invalid("widget appearance has no stream reference"))?,
            );
            Ok(())
        })();
        if let Err(error) = result {
            widget.unresolved = Some(error.to_string());
        }
        widget
    }
}

fn limit(limit: usize) -> Error {
    Error::LimitExceeded {
        resource: "widget page/annotation inventory",
        limit,
    }
}

pub(super) fn append_crop_conflicts(
    graph: &mut super::DocumentGraph,
    store: &super::EvidenceStore,
    limits: super::GraphLimits,
) -> Result<()> {
    use super::{SourceConflict, SourceRef, StructuredValue};
    let mut earlier: BTreeMap<u64, Vec<&super::WidgetCrop>> = BTreeMap::new();
    let mut work = 0usize;
    for field in &store.structured {
        let StructuredValue::FormField { widgets, .. } = &field.value else {
            continue;
        };
        for crop in widgets.iter().filter_map(|widget| widget.crop.as_ref()) {
            let mut conflicts = vec![crop.page_region];
            let previous = earlier.entry(crop.page_region).or_default();
            for other in previous.iter() {
                work = work.saturating_add(1);
                super::evidence::bounded(
                    work,
                    limits.max_references,
                    "widget crop overlap checks",
                )?;
                let a = crop.pixel_bounds;
                let b = other.pixel_bounds;
                if a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3] {
                    conflicts.push(other.region);
                }
            }
            super::evidence::bounded(
                graph.source_conflicts.len().saturating_add(conflicts.len()),
                limits.max_nodes,
                "widget crop conflicts",
            )?;
            graph
                .source_conflicts
                .extend(conflicts.into_iter().map(|region| SourceConflict {
                    sources: vec![
                        SourceRef::Rendered { region },
                        SourceRef::Rendered {
                            region: crop.region,
                        },
                    ],
                    reason: "overlapping widget/page raster material".into(),
                }));
            previous.push(crop);
        }
    }
    Ok(())
}

impl RenderedEvidence {
    /// Returns the outward-rounded raster cells covering the visible part of a box.
    ///
    /// # Errors
    /// Rejects invalid frames/boxes and boxes outside the rendered region.
    pub fn pixel_bounds_covering(&self, bounds: Rect) -> Result<[u32; 4]> {
        let frame = self.pixel_bounds_in_page([0, 0, self.raster.width, self.raster.height])?;
        if ![bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]
            .into_iter()
            .all(f64::is_finite)
            || bounds.min.x >= bounds.max.x
            || bounds.min.y >= bounds.max.y
        {
            return Err(invalid("invalid source box for raster crop"));
        }
        let x = |value: f64| {
            ((value - frame.min.x) / (frame.max.x - frame.min.x) * f64::from(self.raster.width))
                .clamp(0.0, f64::from(self.raster.width))
        };
        let y = |value: f64| {
            ((frame.max.y - value) / (frame.max.y - frame.min.y) * f64::from(self.raster.height))
                .clamp(0.0, f64::from(self.raster.height))
        };
        let pixels = [
            x(bounds.min.x).floor() as u32,
            y(bounds.max.y).floor() as u32,
            x(bounds.max.x).ceil() as u32,
            y(bounds.min.y).ceil() as u32,
        ];
        self.pixel_bounds_in_page(pixels)?;
        Ok(pixels)
    }
}

pub(super) fn validate_widget(
    widget: &FormWidget,
    pages: &BTreeMap<PageId, Option<Rect>>,
    rendered: &BTreeMap<u64, &RenderedEvidence>,
    used_crops: &mut BTreeSet<u64>,
) -> Result<()> {
    if widget.page.is_some_and(|page| !pages.contains_key(&page)) {
        return Err(invalid("widget references an unknown page"));
    }
    if let Some(bounds) = widget.bounds
        && (widget.page.is_none()
            || ![bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]
                .into_iter()
                .all(f64::is_finite)
            || bounds.min.x >= bounds.max.x
            || bounds.min.y >= bounds.max.y)
    {
        return Err(invalid("invalid widget geometry"));
    }
    let Some(crop) = &widget.crop else {
        return Ok(());
    };
    if widget.normal_appearance.is_none() || widget.unresolved.is_some() {
        return Err(invalid(
            "unresolved widget cannot assert an appearance crop",
        ));
    }
    if !used_crops.insert(crop.region) {
        return Err(invalid("widget crop has multiple owners"));
    }
    let page = rendered
        .get(&crop.page_region)
        .ok_or_else(|| invalid("widget page raster is missing"))?;
    let region = rendered
        .get(&crop.region)
        .ok_or_else(|| invalid("widget crop raster is missing"))?;
    let bounds = page.pixel_bounds_in_page(crop.pixel_bounds)?;
    let [left, top, right, bottom] = crop.pixel_bounds;
    if crop.region == crop.page_region
        || !page.composited_page
        || region.composited_page
        || widget.page != Some(page.page)
        || region.page != page.page
        || region.backend != page.backend
        || widget.bounds.is_none()
        || region.raster.width != right - left
        || region.raster.height != bottom - top
        || region.pixel_bounds_in_page([0, 0, region.raster.width, region.raster.height])? != bounds
    {
        return Err(invalid("widget crop does not match its source grid"));
    }
    if page.pixel_bounds_covering(
        widget
            .bounds
            .ok_or_else(|| invalid("widget bounds are missing"))?,
    )? != crop.pixel_bounds
    {
        return Err(invalid("widget crop does not cover its declared location"));
    }
    for (row, samples) in region
        .raster
        .rgb
        .chunks_exact(region.raster.width as usize * 3)
        .enumerate()
    {
        let start = ((top as usize + row) * page.raster.width as usize + left as usize) * 3;
        if page.raster.rgb.get(start..start + samples.len()) != Some(samples) {
            return Err(invalid("widget crop pixels differ from their source"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Raster, WidgetCrop};

    #[test]
    fn widget_crop_requires_exact_pixels_frame_and_exclusive_ownership() {
        let page = RenderedEvidence {
            id: 0,
            page: PageId(0),
            backend: 0,
            polygon: vec![
                Vec2 { x: 0.0, y: 4.0 },
                Vec2 { x: 4.0, y: 4.0 },
                Vec2 { x: 4.0, y: 0.0 },
                Vec2 { x: 0.0, y: 0.0 },
            ],
            raster: Raster {
                width: 4,
                height: 4,
                rgb: (0..48).collect(),
            },
            composited_page: true,
        };
        let bounds = Rect {
            min: Vec2 { x: 1.0, y: 1.0 },
            max: Vec2 { x: 3.0, y: 3.0 },
        };
        let crop = RenderedEvidence {
            id: 1,
            page: PageId(0),
            backend: 0,
            polygon: vec![
                Vec2 { x: 1.0, y: 3.0 },
                bounds.max,
                Vec2 { x: 3.0, y: 1.0 },
                bounds.min,
            ],
            raster: Raster {
                width: 2,
                height: 2,
                rgb: [
                    page.raster.rgb[15..21].to_vec(),
                    page.raster.rgb[27..33].to_vec(),
                ]
                .concat(),
            },
            composited_page: false,
        };
        let widget = FormWidget {
            object: None,
            page: Some(PageId(0)),
            bounds: Some(bounds),
            normal_appearance: Some(ObjectRef {
                object_number: 1,
                generation: 0,
            }),
            crop: Some(WidgetCrop {
                page_region: 0,
                region: 1,
                pixel_bounds: [1, 1, 3, 3],
            }),
            unresolved: None,
        };
        let pages = BTreeMap::from([(PageId(0), None)]);
        let rendered = BTreeMap::from([(0, &page), (1, &crop)]);
        let mut used = BTreeSet::new();
        validate_widget(&widget, &pages, &rendered, &mut used).expect("source-checked crop");
        assert!(validate_widget(&widget, &pages, &rendered, &mut used).is_err());
        for mutation in 0..3 {
            let mut invalid_crop = crop.clone();
            match mutation {
                0 => invalid_crop.raster.rgb[0] ^= 1,
                1 => invalid_crop.polygon[0].x += 1.0,
                _ => invalid_crop.page = PageId(1),
            }
            assert!(
                validate_widget(
                    &widget,
                    &pages,
                    &BTreeMap::from([(0, &page), (1, &invalid_crop)]),
                    &mut BTreeSet::new()
                )
                .is_err()
            );
        }
        let outside = Rect {
            min: Vec2 { x: 5.0, y: 5.0 },
            max: Vec2 { x: 6.0, y: 6.0 },
        };
        assert!(page.pixel_bounds_covering(outside).is_err());
    }
}
