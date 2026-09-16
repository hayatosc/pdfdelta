use pdfdelta_core::{
    document::{
        Channel, ChannelInventory, EvidenceFailure, EvidenceIssue, EvidenceLimits, EvidenceStore,
        Raster, RenderedEvidence, SourceRef, StructuredValue, WidgetCrop,
    },
    model::Vec2,
};
use std::collections::BTreeMap;

/// Retains source-checked crops without treating rendered pixels as field values.
pub fn collect(store: &mut EvidenceStore) {
    let pages: BTreeMap<_, _> = store
        .rendered
        .iter()
        .enumerate()
        .filter(|(_, region)| region.composited_page)
        .map(|(index, region)| (region.page, index))
        .collect();
    let mut bytes: usize = store
        .rendered
        .iter()
        .map(|region| region.raster.rgb.len())
        .sum();
    let mut inventories: BTreeMap<_, Vec<SourceRef>> = BTreeMap::new();
    for field in &mut store.structured {
        let StructuredValue::FormField { widgets, .. } = &mut field.value else {
            continue;
        };
        for widget in widgets {
            let result = (|| {
                if let Some(reason) = &widget.unresolved {
                    return Err((EvidenceFailure::Unresolved, reason.clone()));
                }
                if widget.normal_appearance.is_none() {
                    return Err((
                        EvidenceFailure::Unresolved,
                        "widget normal appearance is unavailable".into(),
                    ));
                }
                let page = widget
                    .page
                    .and_then(|page| pages.get(&page))
                    .map(|index| &store.rendered[*index])
                    .ok_or_else(|| {
                        (
                            EvidenceFailure::Unresolved,
                            "widget page raster is unavailable".into(),
                        )
                    })?;
                let bounds = widget.bounds.ok_or_else(|| {
                    (
                        EvidenceFailure::Unresolved,
                        "widget location is unavailable".into(),
                    )
                })?;
                let pixels = page
                    .pixel_bounds_covering(bounds)
                    .map_err(|error| (EvidenceFailure::Unresolved, error.to_string()))?;
                let [left, top, right, bottom] = pixels;
                let width = right - left;
                let height = bottom - top;
                let size = width as usize * height as usize * 3;
                if size
                    > EvidenceLimits::default()
                        .max_raster_bytes
                        .saturating_sub(bytes)
                {
                    return Err((
                        EvidenceFailure::ResourceLimit,
                        "widget raster budget exhausted".into(),
                    ));
                }
                let mut rgb = Vec::with_capacity(size);
                for row in top..bottom {
                    let start = (row as usize * page.raster.width as usize + left as usize) * 3;
                    rgb.extend_from_slice(&page.raster.rgb[start..start + width as usize * 3]);
                }
                let id = store.rendered.len() as u64;
                let bounds = page
                    .pixel_bounds_in_page(pixels)
                    .map_err(|error| (EvidenceFailure::Unresolved, error.to_string()))?;
                Ok((
                    RenderedEvidence {
                        id,
                        page: page.page,
                        polygon: vec![
                            Vec2 {
                                x: bounds.min.x,
                                y: bounds.max.y,
                            },
                            bounds.max,
                            Vec2 {
                                x: bounds.max.x,
                                y: bounds.min.y,
                            },
                            bounds.min,
                        ],
                        backend: page.backend,
                        raster: Raster { width, height, rgb },
                        composited_page: false,
                    },
                    WidgetCrop {
                        page_region: page.id,
                        region: id,
                        pixel_bounds: pixels,
                    },
                ))
            })();
            match result {
                Ok((region, crop)) => {
                    bytes += region.raster.rgb.len();
                    inventories
                        .entry((region.page, region.backend))
                        .or_default()
                        .push(SourceRef::Rendered { region: region.id });
                    widget.crop = Some(crop);
                    store.rendered.push(region);
                }
                Err((kind, reason)) => store.issues.push(EvidenceIssue {
                    boundary: None,
                    page: field.page,
                    channel: Channel::Forms,
                    sources: vec![SourceRef::Structured { element: field.id }],
                    kind,
                    reason,
                }),
            }
        }
    }
    store
        .inventories
        .extend(
            inventories
                .into_iter()
                .map(|((page, backend), sources)| ChannelInventory {
                    page: Some(page),
                    channel: Channel::Forms,
                    backend,
                    sources,
                    complete: false,
                }),
        );
}
