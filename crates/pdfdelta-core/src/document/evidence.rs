use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    model::{DecodedText, Document, Glyph, GlyphId, PageId, Rect, Vec2, VectorLineId},
    pdf::ObjectRef,
};

/// Independently selectable comparison obligations, not extraction backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Text,
    Visual,
    Forms,
    Relations,
    Presentation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonContract {
    pub version: u32,
    pub channels: BTreeSet<Channel>,
}

impl Default for ComparisonContract {
    fn default() -> Self {
        Self {
            version: 1,
            // Benign reflow is observable presentation, not a default content change.
            channels: [
                Channel::Text,
                Channel::Visual,
                Channel::Forms,
                Channel::Relations,
            ]
            .into(),
        }
    }
}

/// An immutable execution identity. Model and rendering choices affect evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendIdentity {
    pub kind: BackendKind,
    pub name: String,
    pub version: String,
    pub profile: String,
    pub model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    NativeParser,
    Renderer,
    Ocr,
    StructureModel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageEvidence {
    pub page: PageId,
    /// Missing geometry is retained rather than replaced with an invented page.
    pub bounds: Option<Rect>,
}

/// References are disjoint by origin: recognized text cannot impersonate a glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum SourceRef {
    Native { glyph: GlyphId },
    NativeVector { line: VectorLineId },
    Rendered { region: u64 },
    Structured { element: u64 },
}

/// A raster in its declared rendering profile, with tightly packed RGB samples.
/// Pixel differences are visual facts; they do not prove recognized characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderedEvidence {
    pub id: u64,
    pub page: PageId,
    pub polygon: Vec<Vec2>,
    pub backend: usize,
    pub raster: Raster,
    /// An image/object region or the composited page. The latter includes overlap.
    pub composited_page: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FieldValue {
    Text(String),
    Name(Vec<u8>),
    Selected(bool),
    Choices(Vec<String>),
    Empty,
    Unresolved {
        raw_bytes: Option<Vec<u8>>,
        reason: String,
    },
}

/// A widget's declared appearance selection, not a claim about rendered pixels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonAppearanceState {
    pub widget: Option<ObjectRef>,
    /// Exact PDF name bytes. Unknown or invalid states never become `Off`.
    pub name: Option<Vec<u8>>,
}

/// A native widget location and an optional crop of its composited page.
/// A crop retains surrounding/overlaid pixels; it does not isolate an operator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormWidget {
    pub object: Option<ObjectRef>,
    pub page: Option<PageId>,
    pub bounds: Option<Rect>,
    /// A direct normal appearance stream supported by the current renderer.
    pub normal_appearance: Option<ObjectRef>,
    pub crop: Option<WidgetCrop>,
    pub unresolved: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetCrop {
    pub page_region: u64,
    pub region: u64,
    pub pixel_bounds: [u32; 4],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StructuredValue {
    RecognizedText {
        /// The recognizer's complete region text, including its separators.
        text: String,
        region: u64,
        /// Left, top, right, bottom in the referenced raster's pixel frame.
        pixel_bounds: [u32; 4],
        words: Vec<RecognizedWord>,
    },
    FormField {
        name: String,
        /// The inherited PDF field type; unknown types remain distinct from text.
        #[serde(default)]
        field_type: Option<Vec<u8>>,
        value: FieldValue,
        /// Every widget is retained independently, including unresolved locations.
        #[serde(default)]
        widgets: Vec<FormWidget>,
        #[serde(default)]
        button_states: Vec<ButtonAppearanceState>,
    },
    StructureElement {
        role: String,
        /// The PDF structure ID is a byte string, not normalized display text.
        #[serde(default)]
        identifier: Option<Vec<u8>>,
        text: Option<String>,
        /// Native memberships are alternatives to layout views, not copied text.
        #[serde(default)]
        glyphs: Vec<GlyphId>,
        parent: Option<u64>,
        order: Option<u32>,
    },
    Annotation {
        category: String,
        text: Option<String>,
        target: Option<String>,
    },
}

/// A recognition candidate, not a native character or an exact source reading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecognizedWord {
    pub text: String,
    /// Left, top, right, bottom in the referenced raster's pixel frame.
    pub pixel_bounds: [u32; 4],
    /// Backend score in [0, 100], or None when the backend supplies no score.
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructuredEvidence {
    pub id: u64,
    /// None for document-level fields or structure without a widget/page.
    pub page: Option<PageId>,
    pub bounds: Option<Rect>,
    pub object: Option<ObjectRef>,
    pub backend: usize,
    pub value: StructuredValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceFailure {
    Unsupported,
    Unresolved,
    ResourceLimit,
    BackendFailure,
}

/// A gap in retained extraction order. Neighbors locate the boundary; they are
/// neither missing glyphs nor an estimate of the missing content's geometry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceBoundary {
    GlyphGap {
        retained_before: usize,
        before: Option<GlyphId>,
        after: Option<GlyphId>,
    },
    /// The extractor identified the failed invocation's page independently of
    /// retained neighbors, which may lie on different pages or be absent.
    PageGlyphGap {
        page: PageId,
        retained_before: usize,
        before: Option<GlyphId>,
        after: Option<GlyphId>,
    },
    PageGap {
        retained_before: usize,
        before: Option<PageId>,
        after: Option<PageId>,
    },
}

impl EvidenceBoundary {
    pub(super) fn page_scope(&self, native: &Document<Glyph>) -> Option<PageId> {
        if let Self::PageGlyphGap { page, .. } = self {
            return Some(*page);
        }
        let Self::GlyphGap {
            retained_before, ..
        } = self
        else {
            return None;
        };
        let before = retained_before
            .checked_sub(1)
            .and_then(|index| native.items().get(index))?;
        let after = native.items().get(*retained_before)?;
        (before.page == after.page).then_some(before.page)
    }

    pub(super) fn glyph_gap(native: &Document<Glyph>, retained_before: usize) -> Result<Self> {
        if retained_before > native.items().len() {
            return Err(invalid("glyph gap exceeds retained evidence"));
        }
        Ok(Self::GlyphGap {
            retained_before,
            before: retained_before
                .checked_sub(1)
                .and_then(|index| native.items().get(index))
                .map(|glyph| glyph.id),
            after: native.items().get(retained_before).map(|glyph| glyph.id),
        })
    }

    pub(super) fn page_gap(pages: &[PageEvidence], retained_before: usize) -> Result<Self> {
        if retained_before > pages.len() {
            return Err(invalid("page gap exceeds retained evidence"));
        }
        Ok(Self::PageGap {
            retained_before,
            before: retained_before
                .checked_sub(1)
                .and_then(|index| pages.get(index))
                .map(|page| page.page),
            after: pages.get(retained_before).map(|page| page.page),
        })
    }

    pub(super) fn page_glyph_gap(
        native: &Document<Glyph>,
        page: PageId,
        retained_before: usize,
    ) -> Result<Self> {
        let Self::GlyphGap { before, after, .. } = Self::glyph_gap(native, retained_before)? else {
            unreachable!()
        };
        Ok(Self::PageGlyphGap {
            page,
            retained_before,
            before,
            after,
        })
    }
}

/// A failure's explicit dependency scope. No page means document-wide uncertainty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceIssue {
    pub page: Option<PageId>,
    pub channel: Channel,
    pub sources: Vec<SourceRef>,
    /// Local boundary evidence does not close the inventory of undiscovered rivals.
    #[serde(default)]
    pub boundary: Option<EvidenceBoundary>,
    pub kind: EvidenceFailure,
    pub reason: String,
}

/// Inventory completeness concerns discovery, independently of successful matching.
/// A complete empty inventory requires an actual inspection by the named backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelInventory {
    /// None denotes an explicitly inspected document-wide inventory.
    pub page: Option<PageId>,
    pub channel: Channel,
    pub backend: usize,
    pub sources: Vec<SourceRef>,
    pub complete: bool,
}

/// Raw identity domains have different enumeration contracts from content channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyDomain {
    PdfFieldName,
    PdfStructureId,
}

/// Document-wide discovery of every structured element in this native domain.
/// Members are the store's structured evidence from this backend and domain,
/// including members without keys. Completeness does not establish text coverage,
/// unique keys, semantic identity across renames, or complete relationships.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInventory {
    pub domain: KeyDomain,
    pub backend: usize,
    pub complete: bool,
}

/// Raw evidence survives interpretation. Graph views refer to this store without
/// deleting, rewriting, or transferring ownership of the original material.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceStore {
    pub revision: String,
    pub backends: Vec<BackendIdentity>,
    pub pages: Vec<PageEvidence>,
    pub native: Document<Glyph>,
    pub rendered: Vec<RenderedEvidence>,
    pub structured: Vec<StructuredEvidence>,
    pub inventories: Vec<ChannelInventory>,
    #[serde(default)]
    pub key_inventories: Vec<KeyInventory>,
    pub issues: Vec<EvidenceIssue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvidenceLimits {
    pub max_pages: usize,
    pub max_items: usize,
    pub max_text_bytes: usize,
    pub max_raster_bytes: usize,
    pub max_pixels_per_region: usize,
    pub max_polygon_vertices: usize,
}

impl Default for EvidenceLimits {
    fn default() -> Self {
        Self {
            max_pages: 10_000,
            max_items: 5_000_000,
            max_text_bytes: 128 * 1024 * 1024,
            max_raster_bytes: 256 * 1024 * 1024,
            max_pixels_per_region: 16_000_000,
            max_polygon_vertices: 64,
        }
    }
}

impl EvidenceStore {
    /// Validates provider output before it can affect correspondence or ownership.
    ///
    /// # Errors
    /// Returns contextual configuration errors for invalid or dangling evidence
    /// and resource-limit errors before constructing unbounded lookup indexes.
    pub fn validate(&self, limits: EvidenceLimits) -> Result<()> {
        bounded(self.pages.len(), limits.max_pages, "evidence pages")?;
        let items = [
            self.native.items().len(),
            self.native.vector_lines().len(),
            self.native.marked_content().len(),
            self.native.last_non_text_paint().len(),
            self.rendered.len(),
            self.structured.len(),
            self.inventories.len(),
            self.key_inventories.len(),
            self.issues.len(),
            self.backends.len(),
        ]
        .into_iter()
        .try_fold(0usize, |sum, count| sum.checked_add(count))
        .ok_or_else(|| invalid("evidence item count overflows"))?;
        bounded(items, limits.max_items, "evidence items")?;
        if self.revision.is_empty() {
            return Err(invalid("evidence revision identity is empty"));
        }
        let mut text_bytes = self.revision.len();
        bounded(text_bytes, limits.max_text_bytes, "evidence text bytes")?;
        for backend in &self.backends {
            if backend.name.is_empty() || backend.version.is_empty() || backend.profile.is_empty() {
                return Err(invalid("backend identity is incomplete"));
            }
            for text in [&backend.name, &backend.version, &backend.profile]
                .into_iter()
                .chain(backend.model.as_ref())
            {
                add_bytes(
                    &mut text_bytes,
                    text.len(),
                    limits.max_text_bytes,
                    "evidence text bytes",
                )?;
            }
        }
        let mut pages = BTreeMap::new();
        for page in &self.pages {
            if let Some(bounds) = page.bounds {
                valid_rect(bounds)?;
            }
            if pages.insert(page.page, page.bounds).is_some() {
                return Err(invalid("duplicate evidence page"));
            }
        }
        if self
            .native
            .last_non_text_paint()
            .keys()
            .any(|page| !pages.contains_key(page))
        {
            return Err(invalid(
                "paint acquisition marker references an unknown page",
            ));
        }
        let mut marked_memberships = 0usize;
        for sequence in self.native.marked_content() {
            let glyphs = self
                .native
                .items()
                .get(sequence.glyph_range.clone())
                .ok_or_else(|| invalid("invalid marked-content glyph range"))?;
            add_bytes(
                &mut marked_memberships,
                glyphs.len(),
                limits.max_items,
                "marked-content memberships",
            )?;
            if !pages.contains_key(&sequence.page)
                || glyphs.iter().any(|glyph| glyph.page != sequence.page)
            {
                return Err(invalid("marked-content membership crosses its page scope"));
            }
        }
        let mut sources = BTreeMap::new();
        for glyph in self.native.items() {
            valid_rect(glyph.bbox)?;
            if !pages.contains_key(&glyph.page)
                || !glyph.font_size.is_finite()
                || glyph.font_size <= 0.0
                || !finite(glyph.baseline)
                || !finite(glyph.direction)
            {
                return Err(invalid("invalid native glyph geometry or page"));
            }
            let source = SourceRef::Native { glyph: glyph.id };
            if sources.insert(source, Some(glyph.page)).is_some() {
                return Err(invalid("duplicate native glyph identity"));
            }
            let length = match &glyph.text {
                DecodedText::Mapped(text) => text.len(),
                DecodedText::Unmapped { font_hash, .. } => font_hash.0.len(),
            };
            add_bytes(
                &mut text_bytes,
                length,
                limits.max_text_bytes,
                "evidence text bytes",
            )?;
            add_bytes(
                &mut text_bytes,
                glyph.raw_code.len(),
                limits.max_text_bytes,
                "evidence text bytes",
            )?;
        }
        let mut raster_bytes = 0usize;
        for line in self.native.vector_lines() {
            if !pages.contains_key(&line.page)
                || !finite(line.from)
                || !finite(line.to)
                || !line.width.is_finite()
                || line.width < 0.0
                || sources
                    .insert(SourceRef::NativeVector { line: line.id }, Some(line.page))
                    .is_some()
            {
                return Err(invalid("invalid or duplicate native vector evidence"));
            }
        }
        for region in &self.rendered {
            if !pages.contains_key(&region.page)
                || region.backend >= self.backends.len()
                || region.polygon.len() < 3
                || region.polygon.iter().any(|point| !finite(*point))
            {
                return Err(invalid("invalid rendered region provenance or polygon"));
            }
            if self.backends[region.backend].kind != BackendKind::Renderer {
                return Err(invalid("raster provenance must identify a renderer"));
            }
            bounded(
                region.polygon.len(),
                limits.max_polygon_vertices,
                "render polygon vertices",
            )?;
            let pixels = (region.raster.width as usize)
                .checked_mul(region.raster.height as usize)
                .ok_or_else(|| invalid("raster dimensions overflow"))?;
            bounded(
                pixels,
                limits.max_pixels_per_region,
                "rendered region pixels",
            )?;
            if pixels == 0 || pixels.checked_mul(3) != Some(region.raster.rgb.len()) {
                return Err(invalid(
                    "raster does not contain tightly packed RGB samples",
                ));
            }
            add_bytes(
                &mut raster_bytes,
                region.raster.rgb.len(),
                limits.max_raster_bytes,
                "evidence raster bytes",
            )?;
            if sources
                .insert(SourceRef::Rendered { region: region.id }, Some(region.page))
                .is_some()
            {
                return Err(invalid("duplicate rendered region identity"));
            }
        }
        let rendered: BTreeMap<_, _> = self
            .rendered
            .iter()
            .map(|region| (region.id, region))
            .collect();
        let mut recognized_words = 0usize;
        let mut widget_crops = BTreeSet::new();
        let mut widget_items = items;
        for element in &self.structured {
            if element.page.is_some_and(|page| !pages.contains_key(&page))
                || element.backend >= self.backends.len()
            {
                return Err(invalid("invalid structured evidence provenance"));
            }
            if let Some(bounds) = element.bounds {
                valid_rect(bounds)?;
            }
            if sources
                .insert(
                    SourceRef::Structured {
                        element: element.id,
                    },
                    element.page,
                )
                .is_some()
            {
                return Err(invalid("duplicate structured element identity"));
            }
            // The payload is typed, so accounting traverses no arbitrary JSON tree.
            match &element.value {
                StructuredValue::RecognizedText {
                    text,
                    region,
                    words,
                    ..
                } => {
                    let image = rendered
                        .get(region)
                        .ok_or_else(|| invalid("recognized text has no raster evidence"))?;
                    super::recognition::validate_recognition(
                        element,
                        image,
                        &self.backends[element.backend],
                    )?;
                    recognized_words = recognized_words.saturating_add(words.len());
                    bounded(
                        recognized_words,
                        limits.max_items,
                        "recognized word evidence",
                    )?;
                    for value in std::iter::once(text).chain(words.iter().map(|word| &word.text)) {
                        add_bytes(
                            &mut text_bytes,
                            value.len(),
                            limits.max_text_bytes,
                            "recognition text bytes",
                        )?;
                    }
                }
                StructuredValue::FormField {
                    name,
                    field_type,
                    value,
                    widgets,
                    button_states,
                } => {
                    add_bytes(
                        &mut text_bytes,
                        field_type.as_ref().map_or(0, Vec::len),
                        limits.max_text_bytes,
                        "field type bytes",
                    )?;
                    bounded(
                        button_states.len(),
                        limits.max_items,
                        "button appearance states",
                    )?;
                    for state in button_states {
                        add_bytes(
                            &mut text_bytes,
                            state.name.as_ref().map_or(0, Vec::len),
                            limits.max_text_bytes,
                            "button appearance state bytes",
                        )?;
                    }
                    add_bytes(
                        &mut text_bytes,
                        name.len(),
                        limits.max_text_bytes,
                        "evidence text bytes",
                    )?;
                    let values: &[String] = match value {
                        FieldValue::Text(text) => std::slice::from_ref(text),
                        FieldValue::Choices(values) => values,
                        FieldValue::Name(bytes) => {
                            add_bytes(
                                &mut text_bytes,
                                bytes.len(),
                                limits.max_text_bytes,
                                "field name value bytes",
                            )?;
                            &[]
                        }
                        FieldValue::Unresolved { raw_bytes, reason } => {
                            if reason.is_empty() {
                                return Err(invalid("unresolved field has no reason"));
                            }
                            add_bytes(
                                &mut text_bytes,
                                raw_bytes.as_ref().map_or(0, Vec::len),
                                limits.max_text_bytes,
                                "unresolved field bytes",
                            )?;
                            std::slice::from_ref(reason)
                        }
                        FieldValue::Selected(_) | FieldValue::Empty => &[],
                    };
                    bounded(values.len(), limits.max_items, "form choices")?;
                    for value in values {
                        add_bytes(
                            &mut text_bytes,
                            value.len(),
                            limits.max_text_bytes,
                            "evidence text bytes",
                        )?;
                    }
                    widget_items = widget_items.saturating_add(widgets.len());
                    bounded(
                        widget_items,
                        limits.max_items,
                        "form widgets and evidence items",
                    )?;
                    for widget in widgets {
                        if let Some(reason) = &widget.unresolved {
                            add_bytes(
                                &mut text_bytes,
                                reason.len(),
                                limits.max_text_bytes,
                                "widget issue bytes",
                            )?;
                        }
                        super::widgets::validate_widget(
                            widget,
                            &pages,
                            &rendered,
                            &mut widget_crops,
                        )?;
                    }
                }
                StructuredValue::StructureElement {
                    role,
                    text,
                    glyphs,
                    identifier,
                    ..
                } => {
                    if let Some(identifier) = identifier {
                        add_bytes(
                            &mut text_bytes,
                            identifier.len(),
                            limits.max_text_bytes,
                            "structure identifier bytes",
                        )?;
                    }
                    if text.is_some() && !glyphs.is_empty() {
                        return Err(invalid(
                            "structure text and native memberships are mutually exclusive",
                        ));
                    }
                    add_bytes(
                        &mut marked_memberships,
                        glyphs.len(),
                        limits.max_items,
                        "structure glyph memberships",
                    )?;
                    let mut seen = BTreeSet::new();
                    for glyph in glyphs {
                        let page = sources
                            .get(&SourceRef::Native { glyph: *glyph })
                            .ok_or_else(|| {
                                invalid("structure references a missing native glyph")
                            })?;
                        if !seen.insert(*glyph) || element.page.is_some_and(|p| *page != Some(p)) {
                            return Err(invalid(
                                "duplicate or cross-page structure glyph membership",
                            ));
                        }
                    }
                    add_bytes(
                        &mut text_bytes,
                        role.len(),
                        limits.max_text_bytes,
                        "evidence text bytes",
                    )?;
                    if let Some(text) = text {
                        add_bytes(
                            &mut text_bytes,
                            text.len(),
                            limits.max_text_bytes,
                            "evidence text bytes",
                        )?;
                    }
                }
                StructuredValue::Annotation {
                    category,
                    text,
                    target,
                } => {
                    for value in std::iter::once(category)
                        .chain(text.as_ref())
                        .chain(target.as_ref())
                    {
                        add_bytes(
                            &mut text_bytes,
                            value.len(),
                            limits.max_text_bytes,
                            "evidence text bytes",
                        )?;
                    }
                }
            }
        }
        let mut key_domains = BTreeSet::new();
        for inventory in &self.key_inventories {
            if self
                .backends
                .get(inventory.backend)
                .map(|backend| backend.kind)
                != Some(BackendKind::NativeParser)
                || !key_domains.insert((inventory.backend, inventory.domain))
            {
                return Err(invalid(
                    "key inventory requires a unique native backend/domain",
                ));
            }
        }
        let mut inventory_keys = BTreeSet::new();
        let mut references = 0usize;
        for inventory in &self.inventories {
            if inventory
                .page
                .is_some_and(|page| !pages.contains_key(&page))
                || inventory.backend >= self.backends.len()
                || !inventory_keys.insert((inventory.page, inventory.channel, inventory.backend))
            {
                return Err(invalid("invalid or duplicated channel inventory"));
            }
            add_bytes(
                &mut references,
                inventory.sources.len(),
                limits.max_items,
                "evidence references",
            )?;
            let mut unique = BTreeSet::new();
            for source in &inventory.sources {
                if !sources.contains_key(source)
                    || inventory
                        .page
                        .is_some_and(|page| sources.get(source) != Some(&Some(page)))
                    || !unique.insert(*source)
                {
                    return Err(invalid(
                        "inventory contains a dangling, cross-page, or duplicate source",
                    ));
                }
            }
        }
        for issue in &self.issues {
            if let Some(boundary) = &issue.boundary {
                let expected = match boundary {
                    EvidenceBoundary::GlyphGap {
                        retained_before, ..
                    } => EvidenceBoundary::glyph_gap(&self.native, *retained_before)?,
                    EvidenceBoundary::PageGlyphGap {
                        page,
                        retained_before,
                        ..
                    } => EvidenceBoundary::page_glyph_gap(&self.native, *page, *retained_before)?,
                    EvidenceBoundary::PageGap {
                        retained_before, ..
                    } => EvidenceBoundary::page_gap(&self.pages, *retained_before)?,
                };
                if issue.channel != Channel::Text || *boundary != expected {
                    return Err(invalid(
                        "extraction boundary disagrees with retained evidence",
                    ));
                }
                if issue.page != boundary.page_scope(&self.native) {
                    return Err(invalid(
                        "extraction boundary has an inconsistent page scope",
                    ));
                }
            }
            if issue.reason.is_empty() || issue.page.is_some_and(|page| !pages.contains_key(&page))
            {
                return Err(invalid(
                    "evidence issue has no reason or refers to a missing page",
                ));
            }
            add_bytes(
                &mut text_bytes,
                issue.reason.len(),
                limits.max_text_bytes,
                "evidence text bytes",
            )?;
            add_bytes(
                &mut references,
                issue.sources.len(),
                limits.max_items,
                "evidence references",
            )?;
            for source in &issue.sources {
                let page = sources
                    .get(source)
                    .ok_or_else(|| invalid("dangling issue dependency"))?;
                if issue.page.is_some_and(|scope| Some(scope) != *page) {
                    return Err(invalid("issue dependency lies outside its page scope"));
                }
            }
        }
        for element in &self.structured {
            if let StructuredValue::StructureElement {
                parent: Some(parent),
                ..
            } = element.value
                && !sources.contains_key(&SourceRef::Structured { element: parent })
            {
                return Err(invalid("dangling structure parent"));
            }
        }
        Ok(())
    }

    /// True only when a provider inspected this channel and no dependent issue
    /// remains. Missing inventories are never interpreted as empty documents.
    pub fn inventory_complete(&self, page: Option<PageId>, channel: Channel) -> bool {
        self.inventories.iter().any(|inventory| {
            (inventory.page.is_none() || inventory.page == page)
                && inventory.channel == channel
                && inventory.complete
        }) && !self.issues.iter().any(|issue| {
            issue.channel == channel
                && (page.is_none() || issue.page.is_none() || issue.page == page)
        })
    }
}

pub(super) fn invalid(message: &str) -> Error {
    Error::InvalidConfiguration(message.to_owned())
}

pub(super) fn bounded(value: usize, limit: usize, resource: &'static str) -> Result<()> {
    if value > limit {
        Err(Error::LimitExceeded { resource, limit })
    } else {
        Ok(())
    }
}

fn add_bytes(total: &mut usize, count: usize, limit: usize, resource: &'static str) -> Result<()> {
    *total = total
        .checked_add(count)
        .ok_or(Error::LimitExceeded { resource, limit })?;
    bounded(*total, limit, resource)
}

fn finite(point: Vec2) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

pub(super) fn valid_rect(rect: Rect) -> Result<()> {
    if !finite(rect.min) || !finite(rect.max) || rect.min.x > rect.max.x || rect.min.y > rect.max.y
    {
        Err(invalid("invalid evidence rectangle"))
    } else {
        Ok(())
    }
}
