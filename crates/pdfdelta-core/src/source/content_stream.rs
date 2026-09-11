use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use crate::{
    Error, Result,
    model::{
        DecodedText, Document, FontId, FontProgramHash, Glyph, GlyphCropStatus, GlyphId,
        GlyphPathClipStatus, GlyphProvenance, MarkedContent, PageId, Rect, TextRenderMode, Vec2,
        VectorLine, VectorLineId,
    },
    pdf::{
        ObjectRef, PageRef, ParsedPdf, PdfDict, PdfObject,
        content::{
            ContentBudget, ContentLimits, ContentParser, Matrix, Operand, OperandBudget, Operation,
            OperatorBudget,
        },
        font::cmap::CMapLimits,
        font::{
            DecodedGlyph as FontGlyph, FontDecoder, FontDecoderLimits, FontIdentitySource,
            UnicodeMapping, WritingMode, load_font_identity,
        },
    },
};

use super::{
    ExternalFontIdentities, ExtractionIssue, ExtractionLimits, ExtractionOutcome, ExtractionScope,
    GlyphExtractor,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct ContentStreamGlyphExtractor;

impl ContentStreamGlyphExtractor {
    /// Returns each page's bounds in the same cropped, rotated coordinate system
    /// as native glyphs. Individual geometry failures do not discard other pages.
    ///
    /// # Errors
    /// The outer result reports page-tree or page-count limits. Inner results
    /// retain each page's contextual backend, unsupported, or unresolved error.
    pub fn page_bounds(
        &self,
        pdf: &dyn ParsedPdf,
        limits: ExtractionLimits,
        max_pages: usize,
    ) -> Result<Vec<Result<Rect>>> {
        Ok(self
            .page_frames(pdf, limits, max_pages)?
            .into_iter()
            .map(|frame| frame.map(|frame| frame.canonical_bounds()))
            .collect())
    }

    /// Returns source-to-canonical page frames using the native extraction
    /// transform, including inherited crop geometry and page rotation.
    ///
    /// # Errors
    /// Preserves individual page failures and enforces the requested page limit.
    pub fn page_frames(
        &self,
        pdf: &dyn ParsedPdf,
        limits: ExtractionLimits,
        max_pages: usize,
    ) -> Result<Vec<Result<PageCoordinateFrame>>> {
        let pages = pdf.pages()?;
        if pages.len() > max_pages {
            return Err(Error::LimitExceeded {
                resource: "evidence pages",
                limit: max_pages,
            });
        }
        let extraction = Extraction::new(pdf, limits);
        Ok(pages
            .into_iter()
            .map(|page| {
                let snapshot = pdf.page_snapshot(page)?;
                extraction
                    .page_geometry(&snapshot.dictionary)
                    .map(|geometry| PageCoordinateFrame { geometry })
            })
            .collect())
    }

    fn extract_outcome_inner(
        &self,
        pdf: &dyn ParsedPdf,
        limits: ExtractionLimits,
        external_font_identities: &ExternalFontIdentities,
    ) -> Result<ExtractionOutcome> {
        let pages = match pdf.pages() {
            Ok(pages) => pages,
            Err(error) => return ExtractionOutcome::from_error(ExtractionScope::Document, error),
        };
        for issue in pdf.issues() {
            if let crate::pdf::PdfIssueLocation::PageTreeGap { retained_before } = issue.location()
                && retained_before > pages.len()
            {
                return Err(Error::InvalidConfiguration(format!(
                    "PDF issue page-tree gap boundary {retained_before} exceeds the recovered page count {}",
                    pages.len()
                )));
            }
        }
        let mut extraction =
            Extraction::new_with_external_font_identities(pdf, limits, external_font_identities);
        let mut issues = pdf
            .issues()
            .iter()
            .map(ExtractionIssue::from_pdf_issue)
            .collect::<Result<Vec<_>>>()?;
        for (index, page) in pages.into_iter().enumerate() {
            let page_id = u32::try_from(index)
                .map(PageId)
                .map_err(|_| Error::LimitExceeded {
                    resource: "PDF page count address space",
                    limit: u32::MAX as usize,
                })?;
            let glyph_start = extraction.glyphs.len();
            let vector_line_start = extraction.vector_lines.len();
            let marked_start = extraction.marked_content.len();
            let issue_start = extraction.issues.len();
            if let Err(error) = extraction.extract_page(page, page_id) {
                let issue = ExtractionIssue::from_error(ExtractionScope::Page(page_id), error)?;
                extraction.glyphs.truncate(glyph_start);
                extraction.vector_lines.truncate(vector_line_start);
                extraction.marked_content.truncate(marked_start);
                extraction.issues.truncate(issue_start);
                extraction.active_forms.clear();
                issues.push(issue);
            }
        }
        issues.extend(extraction.issues);
        ExtractionOutcome::new(
            Document::with_vector_lines(extraction.glyphs, extraction.vector_lines)
                .with_marked_content(extraction.marked_content)
                .with_last_non_text_paint(extraction.last_non_text_paint),
            issues,
        )
    }

    pub fn extract_outcome_with_external_font_identities(
        &self,
        pdf: &dyn ParsedPdf,
        limits: ExtractionLimits,
        external_font_identities: &ExternalFontIdentities,
    ) -> Result<ExtractionOutcome> {
        self.extract_outcome_inner(pdf, limits, external_font_identities)
    }
}

impl GlyphExtractor for ContentStreamGlyphExtractor {
    fn extract(&self, pdf: &dyn ParsedPdf, limits: ExtractionLimits) -> Result<Document<Glyph>> {
        self.extract_outcome_inner(pdf, limits, &ExternalFontIdentities::default())?
            .into_complete()
    }

    fn extract_outcome(
        &self,
        pdf: &dyn ParsedPdf,
        limits: ExtractionLimits,
    ) -> Result<ExtractionOutcome> {
        self.extract_outcome_inner(pdf, limits, &ExternalFontIdentities::default())
    }
}

struct Extraction<'a> {
    pdf: &'a dyn ParsedPdf,
    external_font_identities: Option<&'a ExternalFontIdentities>,
    limits: ExtractionLimits,
    glyphs: Vec<Glyph>,
    vector_lines: Vec<VectorLine>,
    marked_content: Vec<MarkedContent>,
    last_non_text_paint: std::collections::BTreeMap<PageId, u32>,
    issues: Vec<ExtractionIssue>,
    consumed_glyphs: usize,
    font_cache: HashMap<FontCacheKey, CachedFont>,
    bound_fonts: HashMap<BoundFontKey, Arc<BoundFont>>,
    ext_gstate_fonts: HashMap<ExtGStateKey, Option<(Arc<BoundFont>, f64)>>,
    page_resource_cache: HashMap<usize, (Arc<PdfObject>, Resources)>,
    resource_cache: HashMap<ObjectRef, Resources>,
    resource_map_cache: HashMap<ObjectRef, Arc<ScopedResourceMap>>,
    xobject_cache: HashMap<ObjectRef, Arc<CachedXObject>>,
    decoded_stream_cache: HashMap<ObjectRef, Arc<Vec<u8>>>,
    content_stream_cache: HashMap<ObjectRef, Arc<CachedContentNode>>,
    active_forms: HashSet<ObjectRef>,
    decoded_bytes: usize,
    stream_invocations: usize,
    operator_budget: OperatorBudget,
    operand_budget: OperandBudget,
    cmap_entries: usize,
    cid_width_entries: usize,
    next_font_id: u32,
    next_scope_id: u64,
    render_order: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PageGeometry {
    transform: Matrix,
    crop_bounds: Rect,
    rotation: u16,
}

/// A native page transform constructed only from validated PDF page geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageCoordinateFrame {
    geometry: PageGeometry,
}

impl PageCoordinateFrame {
    pub fn canonical_bounds(self) -> Rect {
        self.geometry.crop_bounds
    }

    pub fn rotation(self) -> u16 {
        self.geometry.rotation
    }

    /// Maps an unrotated PDF-space rectangle into native glyph coordinates.
    ///
    /// # Errors
    /// Rejects non-finite, empty, inverted, or overflowing geometry.
    pub fn map_box(self, bounds: Rect) -> Result<Rect> {
        if ![bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]
            .into_iter()
            .all(f64::is_finite)
            || bounds.min.x >= bounds.max.x
            || bounds.min.y >= bounds.max.y
        {
            return Err(Error::Unresolved("invalid rendered page box".into()));
        }
        transformed_rect(
            self.geometry.transform,
            bounds.min.x,
            bounds.min.y,
            bounds.max.x,
            bounds.max.y,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum ClipRegion {
    #[default]
    Unbounded,
    Rectangle(Rect),
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PathSegment {
    from: Vec2,
    to: Vec2,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CurrentPath {
    segments: Vec<PathSegment>,
    current_point: Option<Vec2>,
    subpath_start: Option<Vec2>,
    drawn_subpaths: usize,
    current_subpath_has_segment: bool,
    clip_rectangle: Option<Rect>,
    has_unsupported_segments: bool,
    clip_pending: bool,
}

impl CurrentPath {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn move_to(&mut self, point: Vec2) {
        if self.current_point.is_some() || !self.segments.is_empty() {
            self.clip_rectangle = None;
        }
        self.current_point = Some(point);
        self.subpath_start = Some(point);
        self.current_subpath_has_segment = false;
    }

    fn line_to(&mut self, point: Vec2) -> Result<()> {
        let from = self.current_point.ok_or_else(|| {
            Error::Unresolved("path line segment has no current point".to_owned())
        })?;
        self.mark_current_subpath_drawn();
        self.segments.push(PathSegment { from, to: point });
        self.current_point = Some(point);
        self.clip_rectangle = None;
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        let start = self.subpath_start.ok_or_else(|| {
            Error::Unresolved("path close operation has no current subpath".to_owned())
        })?;
        let current = self.current_point.ok_or_else(|| {
            Error::Unresolved("path close operation has no current point".to_owned())
        })?;
        if current != start {
            self.mark_current_subpath_drawn();
            self.segments.push(PathSegment {
                from: current,
                to: start,
            });
        }
        self.current_point = Some(start);
        Ok(())
    }

    fn curve_to(&mut self, point: Vec2) -> Result<()> {
        if self.current_point.is_none() {
            return Err(Error::Unresolved(
                "path curve has no current point".to_owned(),
            ));
        }
        self.mark_current_subpath_drawn();
        self.current_point = Some(point);
        self.clip_rectangle = None;
        self.has_unsupported_segments = true;
        Ok(())
    }

    fn append_rectangle(&mut self, corners: [Vec2; 4], clip_rectangle: Option<Rect>) {
        let was_empty = self.drawn_subpaths == 0 && !self.has_unsupported_segments;
        self.drawn_subpaths = self.drawn_subpaths.saturating_add(1);
        self.current_subpath_has_segment = true;
        self.segments.extend([
            PathSegment {
                from: corners[0],
                to: corners[1],
            },
            PathSegment {
                from: corners[1],
                to: corners[2],
            },
            PathSegment {
                from: corners[2],
                to: corners[3],
            },
            PathSegment {
                from: corners[3],
                to: corners[0],
            },
        ]);
        self.current_point = Some(corners[0]);
        self.subpath_start = Some(corners[0]);
        self.clip_rectangle = was_empty.then_some(clip_rectangle).flatten();
    }

    fn clipping_rectangle(&self) -> Option<Rect> {
        self.clip_rectangle
            .or_else(|| self.line_built_clipping_rectangle())
    }

    fn line_built_clipping_rectangle(&self) -> Option<Rect> {
        if self.has_unsupported_segments || self.drawn_subpaths != 1 {
            return None;
        }

        let segments = match self.segments.as_slice() {
            [first, second, third] if !points_approximately_equal(third.to, first.from) => [
                *first,
                *second,
                *third,
                PathSegment {
                    from: third.to,
                    to: first.from,
                },
            ],
            [first, second, third, fourth] => [*first, *second, *third, *fourth],
            _ => return None,
        };
        rectangle_from_segments(segments)
    }

    fn mark_current_subpath_drawn(&mut self) {
        if !self.current_subpath_has_segment {
            self.drawn_subpaths = self.drawn_subpaths.saturating_add(1);
            self.current_subpath_has_segment = true;
        }
    }
}

impl<'a> Extraction<'a> {
    fn new(pdf: &'a dyn ParsedPdf, limits: ExtractionLimits) -> Self {
        let operator_budget = ContentBudget::for_operators(limits.max_operators);
        let operand_budget = ContentBudget::for_operands(limits.max_operand_nodes);
        Self {
            pdf,
            external_font_identities: None,
            limits,
            glyphs: Vec::new(),
            vector_lines: Vec::new(),
            marked_content: Vec::new(),
            last_non_text_paint: std::collections::BTreeMap::new(),
            issues: Vec::new(),
            consumed_glyphs: 0,
            font_cache: HashMap::new(),
            bound_fonts: HashMap::new(),
            ext_gstate_fonts: HashMap::new(),
            page_resource_cache: HashMap::new(),
            resource_cache: HashMap::new(),
            resource_map_cache: HashMap::new(),
            xobject_cache: HashMap::new(),
            decoded_stream_cache: HashMap::new(),
            content_stream_cache: HashMap::new(),
            active_forms: HashSet::new(),
            decoded_bytes: 0,
            stream_invocations: 0,
            operator_budget,
            operand_budget,
            cmap_entries: 0,
            cid_width_entries: 0,
            next_font_id: 0,
            next_scope_id: 0,
            render_order: 0,
        }
    }

    fn new_with_external_font_identities(
        pdf: &'a dyn ParsedPdf,
        limits: ExtractionLimits,
        external_font_identities: &'a ExternalFontIdentities,
    ) -> Self {
        let mut extraction = Self::new(pdf, limits);
        extraction.external_font_identities = Some(external_font_identities);
        extraction
    }

    fn extract_page(&mut self, page: PageRef, page_id: PageId) -> Result<()> {
        let snapshot = self.pdf.page_snapshot(page)?;
        let dictionary = snapshot.dictionary;
        let page_geometry = self.page_geometry(&dictionary)?;
        let resources = self.page_resources(snapshot.resources)?;
        let streams = self.content_streams(dictionary.get(b"Contents".as_slice()))?;
        let mut state = InterpreterState::default();
        let mut parser = ContentParser::with_budgets(
            self.content_limits(),
            self.operator_budget.clone(),
            self.operand_budget.clone(),
        );

        for stream in streams {
            self.interpret_stream(
                stream,
                page_id,
                page_geometry,
                &resources,
                &mut state,
                &mut parser,
                0,
            )?;
        }
        parser.finish()?;

        self.finish_marked_content(&state);

        if state.in_text {
            return Err(Error::Unresolved(format!(
                "page {} has an unterminated text object",
                page.0.object_number
            )));
        }
        if !state.graphics_stack.is_empty() {
            return Err(Error::Unresolved(format!(
                "page {} has an unbalanced graphics-state stack",
                page.0.object_number
            )));
        }
        if state.compatibility_depth != 0 {
            return Err(Error::Unresolved(format!(
                "page {} has an unterminated compatibility section",
                page.0.object_number
            )));
        }
        Ok(())
    }

    fn finish_marked_content(&mut self, state: &InterpreterState) {
        for index in state.marked_stack.iter().flatten() {
            self.marked_content[*index].glyph_range.end = self.glyphs.len();
            self.marked_content[*index].complete = false;
        }
    }

    fn page_resources(&mut self, resource: Option<Arc<PdfObject>>) -> Result<Resources> {
        let Some(resource) = resource else {
            return self.resources(None, None);
        };
        let key = Arc::as_ptr(&resource) as usize;
        if let Some((_, resources)) = self.page_resource_cache.get(&key) {
            return Ok(resources.clone());
        }

        let resources = self.resources(Some(resource.as_ref()), None)?;
        self.page_resource_cache
            .insert(key, (resource, resources.clone()));
        Ok(resources)
    }

    #[allow(clippy::too_many_arguments)]
    fn interpret_stream(
        &mut self,
        stream: ObjectRef,
        page: PageId,
        page_geometry: PageGeometry,
        resources: &Resources,
        state: &mut InterpreterState,
        parser: &mut ContentParser,
        form_depth: usize,
    ) -> Result<()> {
        self.account_stream_invocation()?;
        let bytes = self.decoded_stream_bytes(stream)?;
        let operations = parser.parse_fragment(bytes.as_slice())?;

        for operation in operations {
            self.apply_operation(
                &operation,
                stream,
                page,
                page_geometry,
                resources,
                state,
                form_depth,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_operation(
        &mut self,
        operation: &Operation,
        stream: ObjectRef,
        page: PageId,
        page_geometry: PageGeometry,
        resources: &Resources,
        state: &mut InterpreterState,
        form_depth: usize,
    ) -> Result<()> {
        match operation.operator.as_slice() {
            b"BI" => {
                self.record_non_text_paint(page);
            }
            b"sh" => {
                one_name(operation)?;
                self.record_non_text_paint(page);
            }
            b"BMC" | b"BDC" => {
                if state.marked_stack.len() >= self.limits.max_nesting_depth
                    || state.marked_overflow > 0
                {
                    state.marked_overflow = state.marked_overflow.saturating_add(1);
                    for index in state.marked_stack.iter().flatten() {
                        self.marked_content[*index].complete = false;
                    }
                } else {
                    let mcid = match operation.operands.as_slice() {
                        [Operand::Name(_), Operand::Dictionary(properties)]
                            if operation.operator == b"BDC" =>
                        {
                            let mut values = properties.iter().filter(|(key, _)| key == b"MCID");
                            match (values.next(), values.next()) {
                                (Some((_, Operand::Number(value))), None)
                                    if value.is_finite()
                                        && *value >= 0.0
                                        && *value <= f64::from(u32::MAX)
                                        && value.fract() == 0.0 =>
                                {
                                    Some(*value as u32)
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    let index = mcid.map(|mcid| {
                        let index = self.marked_content.len();
                        self.marked_content.push(MarkedContent {
                            page,
                            form: state.content_form,
                            mcid,
                            glyph_range: self.glyphs.len()..self.glyphs.len(),
                            complete: true,
                        });
                        index
                    });
                    state.marked_stack.push(index);
                }
            }
            b"EMC" => {
                if state.marked_overflow > 0 {
                    state.marked_overflow -= 1;
                } else if let Some(Some(index)) = state.marked_stack.pop() {
                    self.marked_content[index].glyph_range.end = self.glyphs.len();
                    self.marked_content[index].complete &= operation.operands.is_empty();
                }
            }
            b"q" => {
                no_operands(operation)?;
                if state.graphics_stack.len() >= self.limits.max_nesting_depth {
                    return Err(Error::LimitExceeded {
                        resource: "graphics-state stack depth",
                        limit: self.limits.max_nesting_depth,
                    });
                }
                state.graphics_stack.push(state.graphics.clone());
            }
            b"Q" => {
                no_operands(operation)?;
                state.graphics = state
                    .graphics_stack
                    .pop()
                    .ok_or_else(|| operation_error(operation, "graphics-state stack underflow"))?;
            }
            b"cm" => {
                let [a, b, c, d, e, f] = number_operands(operation)?;
                state.graphics.ctm = state
                    .graphics
                    .ctm
                    .concatenate(Matrix::new(a, b, c, d, e, f)?)?;
            }
            b"w" => {
                let width = one_number(operation)?;
                if width < 0.0 {
                    return Err(operation_error(
                        operation,
                        "line width must be non-negative",
                    ));
                }
                state.graphics.line_width = width;
            }
            b"m" => {
                let [x, y] = number_operands(operation)?;
                let point = path_point(page_geometry, state.graphics.ctm, x, y)?;
                state.current_path.move_to(point);
            }
            b"l" => {
                let [x, y] = number_operands(operation)?;
                let point = path_point(page_geometry, state.graphics.ctm, x, y)?;
                self.ensure_path_segment_capacity(state, 1)?;
                state.current_path.line_to(point).map_err(|_| {
                    operation_error(operation, "path line segment has no current point")
                })?;
            }
            b"c" => {
                let [_, _, _, _, x, y] = number_operands(operation)?;
                let point = path_point(page_geometry, state.graphics.ctm, x, y)?;
                state
                    .current_path
                    .curve_to(point)
                    .map_err(|_| operation_error(operation, "path curve has no current point"))?;
            }
            b"v" | b"y" => {
                let [_, _, x, y] = number_operands(operation)?;
                let point = path_point(page_geometry, state.graphics.ctm, x, y)?;
                state
                    .current_path
                    .curve_to(point)
                    .map_err(|_| operation_error(operation, "path curve has no current point"))?;
            }
            b"h" => {
                no_operands(operation)?;
                self.ensure_path_segment_capacity(state, 1)?;
                state.current_path.close().map_err(|_| {
                    operation_error(operation, "path close operation has no current subpath")
                })?;
            }
            b"re" => {
                let [x, y, width, height] = number_operands(operation)?;
                let (corners, rectangle) = transformed_rectangle_path(
                    page_geometry,
                    state.graphics.ctm,
                    x,
                    y,
                    width,
                    height,
                )?;
                self.ensure_path_segment_capacity(state, 4)?;
                state.current_path.append_rectangle(corners, rectangle);
            }
            b"W" | b"W*" => {
                no_operands(operation)?;
                state.current_path.clip_pending = true;
            }
            b"S" => {
                no_operands(operation)?;
                self.finish_path(operation, stream, page, page_geometry, state, true, false)?;
            }
            b"s" => {
                no_operands(operation)?;
                self.finish_path(operation, stream, page, page_geometry, state, true, true)?;
            }
            b"B" | b"B*" => {
                no_operands(operation)?;
                self.finish_path(operation, stream, page, page_geometry, state, true, false)?;
            }
            b"b" | b"b*" => {
                no_operands(operation)?;
                self.finish_path(operation, stream, page, page_geometry, state, true, true)?;
            }
            b"f" | b"F" | b"f*" | b"n" => {
                no_operands(operation)?;
                self.finish_path(operation, stream, page, page_geometry, state, false, false)?;
            }
            b"BT" => {
                no_operands(operation)?;
                if state.in_text {
                    return Err(operation_error(operation, "nested text object"));
                }
                state.in_text = true;
                state.text_matrix = Matrix::IDENTITY;
                state.text_line_matrix = Matrix::IDENTITY;
            }
            b"ET" => {
                no_operands(operation)?;
                require_text_object(operation, state)?;
                state.in_text = false;
            }
            b"Tf" => {
                let (name, size) = name_and_number(operation)?;
                if size == 0.0 {
                    return Err(operation_error(operation, "font size must be non-zero"));
                }
                let font = resources.fonts.entries.get(name).ok_or_else(|| {
                    operation_error(
                        operation,
                        &format!(
                            "font resource /{} is not defined",
                            String::from_utf8_lossy(name)
                        ),
                    )
                })?;
                let key = BoundFontKey::Resource {
                    name: name.to_vec(),
                    scope_id: resources.fonts.scope_id,
                };
                state.graphics.font = Some(self.bind_font(key, Arc::clone(font))?);
                state.graphics.font_size = size;
            }
            b"Tm" => {
                require_text_object(operation, state)?;
                let [a, b, c, d, e, f] = number_operands(operation)?;
                let matrix = Matrix::new(a, b, c, d, e, f)?;
                state.text_matrix = matrix;
                state.text_line_matrix = matrix;
            }
            b"Td" => {
                require_text_object(operation, state)?;
                let [x, y] = number_operands(operation)?;
                move_text_line(state, x, y)?;
            }
            b"TD" => {
                require_text_object(operation, state)?;
                let [x, y] = number_operands(operation)?;
                state.graphics.leading = -y;
                move_text_line(state, x, y)?;
            }
            b"T*" => {
                require_text_object(operation, state)?;
                no_operands(operation)?;
                move_text_line(state, 0.0, -state.graphics.leading)?;
            }
            b"TL" => {
                state.graphics.leading = one_number(operation)?;
            }
            b"Tc" => {
                state.graphics.character_spacing = one_number(operation)?;
            }
            b"Tw" => {
                state.graphics.word_spacing = one_number(operation)?;
            }
            b"Tz" => {
                state.graphics.horizontal_scale = one_number(operation)? / 100.0;
            }
            b"Ts" => {
                state.graphics.rise = one_number(operation)?;
            }
            b"Tr" => {
                state.graphics.render_mode = render_mode(one_number(operation)?, operation)?;
            }
            b"gs" => self.apply_ext_gstate(operation, resources, state)?,
            b"Tj" => {
                require_text_object(operation, state)?;
                let bytes = one_string(operation)?;
                self.show_text(
                    bytes,
                    operation,
                    stream,
                    page,
                    page_geometry,
                    resources,
                    state,
                )?;
            }
            b"TJ" => {
                require_text_object(operation, state)?;
                self.show_text_array(operation, stream, page, page_geometry, resources, state)?;
            }
            b"'" => {
                require_text_object(operation, state)?;
                let bytes = one_string(operation)?;
                move_text_line(state, 0.0, -state.graphics.leading)?;
                self.show_text(
                    bytes,
                    operation,
                    stream,
                    page,
                    page_geometry,
                    resources,
                    state,
                )?;
            }
            b"\"" => {
                require_text_object(operation, state)?;
                let (word_spacing, character_spacing, bytes) = quote_operands(operation)?;
                state.graphics.word_spacing = word_spacing;
                state.graphics.character_spacing = character_spacing;
                move_text_line(state, 0.0, -state.graphics.leading)?;
                self.show_text(
                    bytes,
                    operation,
                    stream,
                    page,
                    page_geometry,
                    resources,
                    state,
                )?;
            }
            b"Do" => {
                let name = one_name(operation)?;
                let glyph_start = self.glyphs.len();
                let vector_line_start = self.vector_lines.len();
                let marked_start = self.marked_content.len();
                let issue_start = self.issues.len();
                let render_order = self.render_order;
                let result = self.invoke_xobject(
                    name,
                    operation,
                    page,
                    page_geometry,
                    resources,
                    state,
                    form_depth,
                );
                match result {
                    Ok(()) => {}
                    Err(error @ (Error::Unsupported(_) | Error::Unresolved(_))) => {
                        self.glyphs.truncate(glyph_start);
                        self.vector_lines.truncate(vector_line_start);
                        self.marked_content.truncate(marked_start);
                        for index in state.marked_stack.iter().flatten() {
                            self.marked_content[*index].complete = false;
                        }
                        self.issues.truncate(issue_start);
                        self.render_order = render_order;
                        self.issues.push(ExtractionIssue::from_error(
                            ExtractionScope::PageGlyphGap {
                                page,
                                retained_before: glyph_start,
                            },
                            error,
                        )?);
                    }
                    Err(error) => return Err(error),
                }
            }
            b"BX" => {
                no_operands(operation)?;
                state.compatibility_depth =
                    state
                        .compatibility_depth
                        .checked_add(1)
                        .ok_or(Error::LimitExceeded {
                            resource: "compatibility-section depth",
                            limit: self.limits.max_nesting_depth,
                        })?;
                if state.compatibility_depth > self.limits.max_nesting_depth {
                    return Err(Error::LimitExceeded {
                        resource: "compatibility-section depth",
                        limit: self.limits.max_nesting_depth,
                    });
                }
            }
            b"EX" => {
                no_operands(operation)?;
                state.compatibility_depth = state
                    .compatibility_depth
                    .checked_sub(1)
                    .ok_or_else(|| operation_error(operation, "compatibility-section underflow"))?;
            }
            operator if is_ignored_operator(operator) || state.compatibility_depth > 0 => {}
            _ => {
                return Err(Error::Unsupported(format!(
                    "content operator {} at index {}",
                    String::from_utf8_lossy(&operation.operator),
                    operation.index
                )));
            }
        }
        Ok(())
    }
}

impl Extraction<'_> {
    fn record_non_text_paint(&mut self, page: PageId) {
        self.last_non_text_paint
            .entry(page)
            .and_modify(|order| *order = (*order).max(self.render_order))
            .or_insert(self.render_order);
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_path(
        &mut self,
        operation: &Operation,
        stream: ObjectRef,
        page: PageId,
        page_geometry: PageGeometry,
        state: &mut InterpreterState,
        stroke: bool,
        close: bool,
    ) -> Result<()> {
        if close {
            self.ensure_path_segment_capacity(state, 1)?;
            state.current_path.close().map_err(|_| {
                operation_error(operation, "path close operation has no current subpath")
            })?;
        }

        if operation.operator != b"n" && state.current_path.drawn_subpaths != 0 {
            self.record_non_text_paint(page);
        }

        let clip_update = if state.current_path.clip_pending {
            if state.current_path.has_unsupported_segments {
                return Err(Error::Unsupported(format!(
                    "curved clipping path at content operator index {}",
                    operation.index
                )));
            }
            let rectangle = state.current_path.clipping_rectangle().ok_or_else(|| {
                Error::Unsupported(format!(
                    "non-rectangular clipping path at content operator index {}",
                    operation.index
                ))
            })?;
            Some(intersect_clip_region(state.graphics.clip_region, rectangle))
        } else {
            None
        };

        if stroke && !state.current_path.has_unsupported_segments {
            let width = transformed_line_width(
                page_geometry,
                state.graphics.ctm,
                state.graphics.line_width,
            )?;
            for segment in state.current_path.segments.iter().copied() {
                if segment_is_visible(segment, state.graphics.clip_region) {
                    self.emit_vector_line(segment, width, operation, stream, page)?;
                }
            }
        }

        if let Some(clip_region) = clip_update {
            state.graphics.clip_region = clip_region;
        }
        state.current_path.reset();
        Ok(())
    }

    fn emit_vector_line(
        &mut self,
        segment: PathSegment,
        width: f64,
        operation: &Operation,
        stream: ObjectRef,
        page: PageId,
    ) -> Result<()> {
        let delta_x = segment.to.x - segment.from.x;
        let delta_y = segment.to.y - segment.from.y;
        if delta_x.hypot(delta_y) <= f64::EPSILON {
            return Ok(());
        }
        if self.vector_lines.len() >= self.limits.max_vector_lines {
            return Err(Error::LimitExceeded {
                resource: "vector line count",
                limit: self.limits.max_vector_lines,
            });
        }
        let id = u64::try_from(self.vector_lines.len())
            .map(VectorLineId)
            .map_err(|_| Error::LimitExceeded {
                resource: "vector line identifier address space",
                limit: usize::MAX,
            })?;
        let render_order = self.allocate_render_order()?;
        self.vector_lines.push(VectorLine {
            id,
            page,
            from: segment.from,
            to: segment.to,
            width,
            render_order,
            provenance: GlyphProvenance {
                content_stream: stream,
                operator_index: operation.index,
            },
        });
        Ok(())
    }

    fn allocate_render_order(&mut self) -> Result<u32> {
        let render_order = self.render_order;
        self.render_order = self
            .render_order
            .checked_add(1)
            .ok_or(Error::LimitExceeded {
                resource: "render-order address space",
                limit: u32::MAX as usize,
            })?;
        Ok(render_order)
    }

    fn ensure_path_segment_capacity(
        &self,
        state: &InterpreterState,
        additional: usize,
    ) -> Result<()> {
        if state.current_path.segments.len().saturating_add(additional)
            > self.limits.max_vector_lines
        {
            return Err(Error::LimitExceeded {
                resource: "path segment count",
                limit: self.limits.max_vector_lines,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn show_text(
        &mut self,
        bytes: &[u8],
        operation: &Operation,
        stream: ObjectRef,
        page: PageId,
        page_geometry: PageGeometry,
        _resources: &Resources,
        state: &mut InterpreterState,
    ) -> Result<()> {
        let font =
            state.graphics.font.as_ref().ok_or_else(|| {
                operation_error(operation, "text is shown before selecting a font")
            })?;
        let run = self.decode_font(font, bytes)?;
        for glyph in run.glyphs {
            self.emit_glyph(
                glyph,
                run.font_id,
                run.ascent,
                run.descent,
                run.font_hash.as_ref(),
                operation,
                stream,
                page,
                page_geometry,
                state,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn show_text_array(
        &mut self,
        operation: &Operation,
        stream: ObjectRef,
        page: PageId,
        page_geometry: PageGeometry,
        resources: &Resources,
        state: &mut InterpreterState,
    ) -> Result<()> {
        let [Operand::Array(items)] = operation.operands.as_slice() else {
            return Err(operation_error(
                operation,
                "expected one text-array operand",
            ));
        };
        let font =
            state.graphics.font.clone().ok_or_else(|| {
                operation_error(operation, "text is shown before selecting a font")
            })?;
        let writing_mode = self.decode_font(&font, &[])?.writing_mode;
        for item in items {
            match item {
                Operand::String(bytes) => self.show_text(
                    bytes,
                    operation,
                    stream,
                    page,
                    page_geometry,
                    resources,
                    state,
                )?,
                Operand::Number(adjustment) if adjustment.is_finite() => {
                    let (offset_x, offset_y) = match writing_mode {
                        WritingMode::Horizontal => (
                            -(adjustment / 1000.0)
                                * state.graphics.font_size
                                * state.graphics.horizontal_scale,
                            0.0,
                        ),
                        WritingMode::Vertical => {
                            (0.0, -(adjustment / 1000.0) * state.graphics.font_size)
                        }
                    };
                    state.text_matrix = state
                        .text_matrix
                        .concatenate(Matrix::translation(offset_x, offset_y)?)?;
                }
                _ => {
                    return Err(operation_error(
                        operation,
                        "text arrays may contain only strings and numbers",
                    ));
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_glyph(
        &mut self,
        glyph: FontGlyph,
        font_id: FontId,
        ascent: f64,
        descent: f64,
        font_hash: Option<&FontProgramHash>,
        operation: &Operation,
        stream: ObjectRef,
        page: PageId,
        page_geometry: PageGeometry,
        state: &mut InterpreterState,
    ) -> Result<()> {
        let glyph_id = glyph.glyph_id;
        let text = match glyph.mapping {
            UnicodeMapping::Mapped(text) => DecodedText::Mapped(text),
            UnicodeMapping::Unmapped => DecodedText::Unmapped {
                font_hash: font_hash.cloned().ok_or_else(|| {
                    operation_error(
                        operation,
                        "font code has no Unicode mapping or stable font identity",
                    )
                })?,
                glyph_id,
            },
        };
        let scale = Matrix::new(
            state.graphics.font_size * state.graphics.horizontal_scale,
            0.0,
            0.0,
            state.graphics.font_size,
            0.0,
            state.graphics.rise,
        )?;
        let text_rendering = page_geometry
            .transform
            .concatenate(state.graphics.ctm)?
            .concatenate(state.text_matrix)?
            .concatenate(scale)?;
        let (left, bottom, right, top, direction_x, direction_y) =
            if let Some(vertical) = glyph.vertical {
                (
                    -vertical.origin_x_1000_em / 1000.0,
                    (descent - vertical.origin_y_1000_em) / 1000.0,
                    (glyph.width_1000_em - vertical.origin_x_1000_em) / 1000.0,
                    (ascent - vertical.origin_y_1000_em) / 1000.0,
                    0.0,
                    -1.0,
                )
            } else {
                (
                    0.0,
                    descent / 1000.0,
                    glyph.width_1000_em / 1000.0,
                    ascent / 1000.0,
                    1.0,
                    0.0,
                )
            };
        let bbox = transformed_rect(text_rendering, left, bottom, right, top)?;
        let (baseline_x, baseline_y) = text_rendering.transform_point(0.0, 0.0)?;
        let (direction_x, direction_y) =
            text_rendering.transform_vector(direction_x, direction_y)?;
        let direction = normalized_vector(direction_x, direction_y, operation)?;
        let (font_x, font_y) = text_rendering.transform_vector(0.0, 1.0)?;
        let effective_font_size = font_x.hypot(font_y);
        if !effective_font_size.is_finite() || effective_font_size <= f64::EPSILON {
            return Err(operation_error(
                operation,
                "text transform produces a non-positive font size",
            ));
        }
        let id =
            u64::try_from(self.glyphs.len())
                .map(GlyphId)
                .map_err(|_| Error::LimitExceeded {
                    resource: "glyph identifier address space",
                    limit: usize::MAX,
                })?;
        let render_order = self.allocate_render_order()?;
        let raw_code = glyph.raw_code;
        let is_word_space = raw_code.as_slice() == b" ";
        self.glyphs.push(Glyph {
            id,
            text,
            raw_code,
            page,
            bbox,
            baseline: Vec2 {
                x: baseline_x,
                y: baseline_y,
            },
            direction,
            font_id,
            font_size: effective_font_size,
            render_order,
            render_mode: state.graphics.render_mode,
            crop_status: glyph_crop_status(bbox, page_geometry.crop_bounds),
            path_clip_status: glyph_path_clip_status(bbox, state.graphics.clip_region),
            provenance: GlyphProvenance {
                content_stream: stream,
                operator_index: operation.index,
            },
        });

        let word_spacing = if is_word_space {
            state.graphics.word_spacing
        } else {
            0.0
        };
        let (advance_x, advance_y) = if let Some(vertical) = glyph.vertical {
            (
                0.0,
                (vertical.displacement_y_1000_em / 1000.0) * state.graphics.font_size
                    + state.graphics.character_spacing
                    + word_spacing,
            )
        } else {
            (
                ((glyph.width_1000_em / 1000.0) * state.graphics.font_size
                    + state.graphics.character_spacing
                    + word_spacing)
                    * state.graphics.horizontal_scale,
                0.0,
            )
        };
        state.text_matrix = state
            .text_matrix
            .concatenate(Matrix::translation(advance_x, advance_y)?)?;
        Ok(())
    }

    fn decode_font(&mut self, selection: &BoundFont, bytes: &[u8]) -> Result<DecodedRun> {
        let key = &selection.cache_key;

        if !self.font_cache.contains_key(key) {
            if self.font_cache.len() >= self.limits.max_fonts {
                return Err(Error::LimitExceeded {
                    resource: "loaded font count",
                    limit: self.limits.max_fonts,
                });
            }
            let remaining_bytes = self
                .limits
                .max_total_decoded_bytes
                .saturating_sub(self.decoded_bytes);
            let remaining_entries = self
                .limits
                .max_cmap_entries
                .saturating_sub(self.cmap_entries);
            let remaining_cid_width_entries = self
                .limits
                .max_cid_width_entries
                .saturating_sub(self.cid_width_entries);
            let loaded = FontDecoder::load(
                self.pdf,
                selection.object.as_ref(),
                FontDecoderLimits {
                    max_indirections: self.limits.max_nesting_depth,
                    max_simple_width_entries: 256,
                    max_cid_width_entries: remaining_cid_width_entries,
                    max_decoded_font_bytes: remaining_bytes,
                    cmap: CMapLimits {
                        max_entries: remaining_entries,
                        max_code_bytes: 4,
                        max_output_scalars: self.limits.max_string_bytes,
                    },
                },
            )
            .map_err(|error| match error {
                Error::LimitExceeded {
                    resource: "CID width entries",
                    ..
                } => Error::LimitExceeded {
                    resource: "CID width entries",
                    limit: self.limits.max_cid_width_entries,
                },
                error => error,
            })?;
            self.account_decoded_bytes(loaded.decoded_font_bytes)?;
            self.account_cid_width_entries(loaded.cid_width_entries)?;
            self.cmap_entries = self
                .cmap_entries
                .checked_add(loaded.decoder.cmap_entry_count())
                .ok_or(Error::LimitExceeded {
                    resource: "ToUnicode CMap entries",
                    limit: self.limits.max_cmap_entries,
                })?;
            if self.cmap_entries > self.limits.max_cmap_entries {
                return Err(Error::LimitExceeded {
                    resource: "ToUnicode CMap entries",
                    limit: self.limits.max_cmap_entries,
                });
            }
            let id = FontId(self.next_font_id);
            self.next_font_id = self
                .next_font_id
                .checked_add(1)
                .ok_or(Error::LimitExceeded {
                    resource: "font identifier address space",
                    limit: u32::MAX as usize,
                })?;
            let external_font_hash = loaded
                .external_base_font
                .as_deref()
                .and_then(|base_font| {
                    self.external_font_identities
                        .and_then(|identities| identities.get(base_font))
                })
                .cloned();
            self.font_cache.insert(
                key.clone(),
                CachedFont {
                    id,
                    decoder: loaded.decoder,
                    identity_source: loaded.identity_source,
                    font_hash: external_font_hash,
                },
            );
        }

        let remaining_glyphs = self.limits.max_glyphs.saturating_sub(self.consumed_glyphs);
        let remaining_mapped_text_bytes = self
            .limits
            .max_total_decoded_bytes
            .saturating_sub(self.decoded_bytes);
        let (font_id, ascent, descent, writing_mode, glyphs) = {
            let cached = self.font_cache.get(key).ok_or_else(|| {
                Error::Unresolved("loaded font disappeared from the extraction cache".into())
            })?;
            (
                cached.id,
                cached.decoder.ascent_1000_em(),
                cached.decoder.descent_1000_em(),
                cached.decoder.writing_mode(),
                cached
                    .decoder
                    .decode(bytes, remaining_glyphs, remaining_mapped_text_bytes)?,
            )
        };
        let consumed_glyphs =
            self.consumed_glyphs
                .checked_add(glyphs.len())
                .ok_or(Error::LimitExceeded {
                    resource: "extracted glyph count",
                    limit: self.limits.max_glyphs,
                })?;
        if consumed_glyphs > self.limits.max_glyphs {
            return Err(Error::LimitExceeded {
                resource: "extracted glyph count",
                limit: self.limits.max_glyphs,
            });
        }
        self.consumed_glyphs = consumed_glyphs;
        let mapped_text_bytes = glyphs.iter().try_fold(0usize, |total, glyph| {
            let bytes = match &glyph.mapping {
                UnicodeMapping::Mapped(text) => text.len(),
                UnicodeMapping::Unmapped => 0,
            };
            total.checked_add(bytes).ok_or(Error::LimitExceeded {
                resource: "decoded Unicode text bytes",
                limit: remaining_mapped_text_bytes,
            })
        })?;
        let needs_identity = glyphs
            .iter()
            .any(|glyph| matches!(glyph.mapping, UnicodeMapping::Unmapped));
        let mut loaded_identity = None;
        let font_hash = if needs_identity {
            let cached = self.font_cache.get(key).ok_or_else(|| {
                Error::Unresolved("loaded font disappeared from the extraction cache".into())
            })?;
            if let Some(hash) = &cached.font_hash {
                Some(hash.clone())
            } else if let Some(source) = &cached.identity_source {
                let identity = load_font_identity(
                    self.pdf,
                    source,
                    remaining_mapped_text_bytes.saturating_sub(mapped_text_bytes),
                )?;
                let hash = identity.hash.clone();
                loaded_identity = Some(identity);
                Some(hash)
            } else {
                None
            }
        } else {
            None
        };
        let decoded_identity_bytes = loaded_identity
            .as_ref()
            .map_or(0, |identity| identity.decoded_bytes);
        let decoded_bytes = mapped_text_bytes
            .checked_add(decoded_identity_bytes)
            .ok_or(Error::LimitExceeded {
                resource: "decoded font and Unicode text bytes",
                limit: remaining_mapped_text_bytes,
            })?;
        self.account_decoded_bytes(decoded_bytes)?;
        if let Some(identity) = loaded_identity {
            let cached = self.font_cache.get_mut(key).ok_or_else(|| {
                Error::Unresolved("loaded font disappeared from the extraction cache".into())
            })?;
            cached.font_hash = Some(identity.hash);
        }
        Ok(DecodedRun {
            font_id,
            font_hash,
            ascent,
            descent,
            writing_mode,
            glyphs,
        })
    }

    fn bind_font(&mut self, key: BoundFontKey, object: Arc<PdfObject>) -> Result<Arc<BoundFont>> {
        let (key, object) = match object.as_ref() {
            PdfObject::Reference(reference) => {
                let terminal = self.pdf.terminal_reference(*reference)?;
                (
                    BoundFontKey::Reference(terminal),
                    Arc::new(PdfObject::Reference(terminal)),
                )
            }
            _ => (key, object),
        };
        if let Some(font) = self.bound_fonts.get(&key) {
            return Ok(Arc::clone(font));
        }
        let cache_key = match &key {
            BoundFontKey::Reference(reference) => FontCacheKey::Reference(*reference),
            BoundFontKey::Resource { scope_id, name } => FontCacheKey::ScopedResource {
                scope_id: *scope_id,
                name: name.clone(),
            },
            BoundFontKey::ExtGState(key) => FontCacheKey::ScopedExtGState(key.clone()),
        };
        let font = Arc::new(BoundFont { object, cache_key });
        self.bound_fonts.insert(key, Arc::clone(&font));
        Ok(font)
    }

    fn apply_ext_gstate(
        &mut self,
        operation: &Operation,
        resources: &Resources,
        state: &mut InterpreterState,
    ) -> Result<()> {
        let name = one_name(operation)?;
        let resource = resources.ext_gstates.entries.get(name).ok_or_else(|| {
            operation_error(
                operation,
                &format!(
                    "ExtGState resource /{} is not defined",
                    String::from_utf8_lossy(name)
                ),
            )
        })?;
        let key = match resource.as_ref() {
            PdfObject::Reference(reference) => {
                ExtGStateKey::Reference(self.pdf.terminal_reference(*reference)?)
            }
            _ => ExtGStateKey::Direct {
                scope_id: resources.ext_gstates.scope_id,
                name: name.to_vec(),
            },
        };
        if let Some(selection) = self.ext_gstate_fonts.get(&key) {
            if let Some((font, size)) = selection {
                state.graphics.font = Some(Arc::clone(font));
                state.graphics.font_size = *size;
            }
            return Ok(());
        }
        let dictionary = self.dictionary_value(resource.as_ref(), "ExtGState resource")?;
        let Some(font) = dictionary.get(b"Font".as_slice()) else {
            self.ext_gstate_fonts.insert(key, None);
            return Ok(());
        };
        let font = self.resolve_value(font, "ExtGState Font")?;
        let PdfObject::Array(values) = font else {
            return Err(operation_error(operation, "ExtGState Font is not an array"));
        };
        let [font, size] = values.as_slice() else {
            return Err(operation_error(
                operation,
                "ExtGState Font must contain a font and size",
            ));
        };
        let size = self.number_value(size, "ExtGState font size")?;
        if size == 0.0 {
            return Err(operation_error(
                operation,
                "ExtGState font size must be non-zero",
            ));
        }
        let font = self.bind_font(BoundFontKey::ExtGState(key.clone()), Arc::new(font.clone()))?;
        self.ext_gstate_fonts
            .insert(key, Some((Arc::clone(&font), size)));
        state.graphics.font = Some(font);
        state.graphics.font_size = size;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_xobject(
        &mut self,
        name: &[u8],
        operation: &Operation,
        page: PageId,
        page_geometry: PageGeometry,
        resources: &Resources,
        state: &InterpreterState,
        form_depth: usize,
    ) -> Result<()> {
        let object = resources.xobjects.entries.get(name).ok_or_else(|| {
            operation_error(
                operation,
                &format!(
                    "XObject resource /{} is not defined",
                    String::from_utf8_lossy(name)
                ),
            )
        })?;
        let reference = match object.as_ref() {
            PdfObject::Reference(reference) => *reference,
            PdfObject::Stream(_) => {
                return Err(Error::Unsupported(
                    "direct Form XObject streams cannot retain object provenance".into(),
                ));
            }
            _ => {
                return Err(operation_error(
                    operation,
                    "XObject resource is not a stream reference",
                ));
            }
        };
        let xobject = self.cached_xobject(reference)?;
        if matches!(xobject.kind, CachedXObjectKind::Image) {
            self.record_non_text_paint(page);
        }
        let CachedXObjectKind::Form {
            matrix: form_matrix,
            resources: local_resources,
        } = &xobject.kind
        else {
            return Ok(());
        };
        let reference = xobject.reference;
        if form_depth >= self.limits.max_form_depth {
            return Err(Error::LimitExceeded {
                resource: "Form XObject recursion depth",
                limit: self.limits.max_form_depth,
            });
        }
        if !self.active_forms.insert(reference) {
            return Err(Error::Unresolved(format!(
                "cyclic Form XObject reference {} {}",
                reference.object_number, reference.generation
            )));
        }

        let result = (|| {
            let form_resources = local_resources.clone().unwrap_or_else(|| resources.clone());
            let mut form_state = state.clone();
            form_state.marked_stack.clear();
            form_state.marked_overflow = 0;
            form_state.content_form = Some(reference);
            form_state.graphics.ctm = form_state.graphics.ctm.concatenate(*form_matrix)?;
            form_state.graphics_stack.clear();
            form_state.current_path.reset();
            form_state.compatibility_depth = 0;
            let initial_text_state = form_state.in_text;
            let mut parser = ContentParser::with_budgets(
                self.content_limits(),
                self.operator_budget.clone(),
                self.operand_budget.clone(),
            );
            self.interpret_stream(
                reference,
                page,
                page_geometry,
                &form_resources,
                &mut form_state,
                &mut parser,
                form_depth + 1,
            )?;
            parser.finish()?;
            self.finish_marked_content(&form_state);
            // Form execution is isolated from its caller, so unmatched saves can be
            // discarded at the boundary without leaking graphics state.
            form_state.graphics_stack.clear();
            if form_state.in_text != initial_text_state {
                return Err(Error::Unresolved(format!(
                    "Form XObject {} changes the enclosing text-object state",
                    reference.object_number
                )));
            }
            if form_state.compatibility_depth != 0 {
                return Err(Error::Unresolved(format!(
                    "Form XObject {} has an unterminated compatibility section",
                    reference.object_number
                )));
            }
            Ok(())
        })();
        self.active_forms.remove(&reference);
        result
    }

    fn cached_xobject(&mut self, reference: ObjectRef) -> Result<Arc<CachedXObject>> {
        if let Some(xobject) = self.xobject_cache.get(&reference) {
            return Ok(Arc::clone(xobject));
        }
        let terminal = self.pdf.terminal_reference(reference)?;
        if let Some(xobject) = self.xobject_cache.get(&terminal).cloned() {
            self.xobject_cache.insert(reference, Arc::clone(&xobject));
            return Ok(xobject);
        }
        let PdfObject::Stream(dictionary) = self.pdf.resolve(terminal)? else {
            return Err(Error::Unresolved(
                "XObject reference does not resolve to a stream".into(),
            ));
        };
        let kind = match self
            .name_value(dictionary.get(b"Subtype".as_slice()), "XObject Subtype")?
            .as_slice()
        {
            b"Image" => CachedXObjectKind::Image,
            b"Form" => {
                let matrix = match dictionary.get(b"Matrix".as_slice()) {
                    Some(value) => self.matrix_value(value, "Form Matrix")?,
                    None => Matrix::IDENTITY,
                };
                let resources = dictionary
                    .get(b"Resources".as_slice())
                    .map(|value| self.resources(Some(value), None))
                    .transpose()?;
                CachedXObjectKind::Form { matrix, resources }
            }
            subtype => {
                return Err(Error::Unsupported(format!(
                    "XObject subtype /{}",
                    String::from_utf8_lossy(subtype)
                )));
            }
        };
        let xobject = Arc::new(CachedXObject {
            reference: terminal,
            kind,
        });
        self.xobject_cache.insert(terminal, Arc::clone(&xobject));
        self.xobject_cache.insert(reference, Arc::clone(&xobject));
        Ok(xobject)
    }

    fn decoded_stream_bytes(&mut self, reference: ObjectRef) -> Result<Arc<Vec<u8>>> {
        if let Some(bytes) = self.decoded_stream_cache.get(&reference) {
            return Ok(Arc::clone(bytes));
        }
        let terminal = self.pdf.terminal_reference(reference)?;
        if let Some(bytes) = self.decoded_stream_cache.get(&terminal).cloned() {
            self.decoded_stream_cache
                .insert(reference, Arc::clone(&bytes));
            return Ok(bytes);
        }
        let bytes = Arc::new(self.pdf.decoded_stream(terminal)?.bytes);
        // Decoded stream bytes are charged once per unique stream.
        self.account_decoded_bytes(bytes.len())?;
        self.decoded_stream_cache
            .insert(terminal, Arc::clone(&bytes));
        self.decoded_stream_cache
            .insert(reference, Arc::clone(&bytes));
        Ok(bytes)
    }

    fn account_decoded_bytes(&mut self, bytes: usize) -> Result<()> {
        self.decoded_bytes = self
            .decoded_bytes
            .checked_add(bytes)
            .ok_or(Error::LimitExceeded {
                resource: "decoded extraction bytes",
                limit: self.limits.max_total_decoded_bytes,
            })?;
        if self.decoded_bytes > self.limits.max_total_decoded_bytes {
            return Err(Error::LimitExceeded {
                resource: "decoded extraction bytes",
                limit: self.limits.max_total_decoded_bytes,
            });
        }
        Ok(())
    }

    fn account_cid_width_entries(&mut self, entries: usize) -> Result<()> {
        self.cid_width_entries =
            self.cid_width_entries
                .checked_add(entries)
                .ok_or(Error::LimitExceeded {
                    resource: "CID width entries",
                    limit: self.limits.max_cid_width_entries,
                })?;
        if self.cid_width_entries > self.limits.max_cid_width_entries {
            return Err(Error::LimitExceeded {
                resource: "CID width entries",
                limit: self.limits.max_cid_width_entries,
            });
        }
        Ok(())
    }

    fn account_stream_invocation(&mut self) -> Result<()> {
        if self.stream_invocations >= self.limits.max_stream_invocations {
            return Err(Error::LimitExceeded {
                resource: "content stream invocations",
                limit: self.limits.max_stream_invocations,
            });
        }
        self.stream_invocations += 1;
        Ok(())
    }

    fn content_limits(&self) -> ContentLimits {
        ContentLimits {
            max_operators: self.limits.max_operators,
            max_operand_stack: self.limits.max_operand_stack,
            max_array_elements: self.limits.max_array_elements,
            max_operand_nodes: self.limits.max_operand_nodes,
            max_nesting_depth: self.limits.max_nesting_depth,
            max_string_bytes: self.limits.max_string_bytes,
        }
    }

    fn content_streams(&mut self, contents: Option<&PdfObject>) -> Result<Vec<ObjectRef>> {
        let Some(contents) = contents else {
            return Ok(Vec::new());
        };
        let mut active = HashSet::new();
        let summary = self.summarize_content_object(contents, 0, &mut active)?;
        let materialization_limit = self
            .limits
            .max_stream_invocations
            .saturating_sub(self.stream_invocations);
        self.ensure_content_stream_count(summary.stream_count, materialization_limit)?;
        let mut streams = Vec::new();
        streams
            .try_reserve(summary.stream_count)
            .map_err(|_| Error::LimitExceeded {
                resource: "content stream invocations",
                limit: self.limits.max_stream_invocations,
            })?;
        if summary.stream_count != 0 {
            self.materialize_content_object(contents, &mut streams, summary.stream_count)?;
        }
        if streams.len() != summary.stream_count {
            return Err(Error::Unresolved(
                "page Contents summary changed during materialization".into(),
            ));
        }
        Ok(streams)
    }

    fn summarize_content_object(
        &mut self,
        object: &PdfObject,
        depth: usize,
        active: &mut HashSet<ObjectRef>,
    ) -> Result<ContentStreamSummary> {
        self.ensure_content_nesting_depth(depth, 0)?;
        match object {
            PdfObject::Null => Ok(ContentStreamSummary::EMPTY),
            PdfObject::Array(items) => self.summarize_content_array(items, depth, active),
            PdfObject::Reference(reference) => {
                let node = self.load_content_reference(*reference, depth, active)?;
                Ok(node.summary())
            }
            PdfObject::Stream(_) => Err(Error::Unsupported(
                "direct page content streams cannot retain object provenance".into(),
            )),
            _ => Err(Error::Unresolved(
                "page Contents is not a stream or stream array".into(),
            )),
        }
    }

    fn summarize_content_array(
        &mut self,
        items: &[PdfObject],
        depth: usize,
        active: &mut HashSet<ObjectRef>,
    ) -> Result<ContentStreamSummary> {
        self.ensure_content_nesting_depth(depth, 0)?;
        let mut summary = ContentStreamSummary::EMPTY;
        for item in items {
            let child = self.summarize_content_object(item, depth.saturating_add(1), active)?;
            summary.stream_count =
                self.checked_content_stream_count(summary.stream_count, child.stream_count)?;
            let child_depth = child
                .nesting_depth
                .checked_add(1)
                .ok_or(Error::LimitExceeded {
                    resource: "page Contents indirection depth",
                    limit: self.limits.max_nesting_depth,
                })?;
            summary.nesting_depth = summary.nesting_depth.max(child_depth);
        }
        self.ensure_content_nesting_depth(depth, summary.nesting_depth)?;
        Ok(summary)
    }

    fn load_content_reference(
        &mut self,
        reference: ObjectRef,
        depth: usize,
        active: &mut HashSet<ObjectRef>,
    ) -> Result<Arc<CachedContentNode>> {
        if let Some(node) = self.content_stream_cache.get(&reference).cloned() {
            self.ensure_content_nesting_depth(depth, node.nesting_depth)?;
            return Ok(node);
        }
        let terminal = self.pdf.terminal_reference(reference)?;
        if let Some(node) = self.content_stream_cache.get(&terminal).cloned() {
            self.ensure_content_nesting_depth(depth, node.nesting_depth)?;
            self.content_stream_cache
                .insert(reference, Arc::clone(&node));
            return Ok(node);
        }
        if !active.insert(terminal) {
            return Err(Error::Unresolved(format!(
                "cyclic page Contents reference {} {}",
                terminal.object_number, terminal.generation
            )));
        }
        let result = (|| {
            let resolved = self.pdf.resolve(terminal)?;
            let node = match resolved {
                PdfObject::Stream(_) => CachedContentNode {
                    kind: CachedContentNodeKind::Stream(terminal),
                    stream_count: 1,
                    nesting_depth: 0,
                },
                PdfObject::Null => CachedContentNode {
                    kind: CachedContentNodeKind::Null,
                    stream_count: 0,
                    nesting_depth: 1,
                },
                PdfObject::Array(items) => {
                    let summary =
                        self.summarize_content_array(&items, depth.saturating_add(1), active)?;
                    let nesting_depth =
                        summary
                            .nesting_depth
                            .checked_add(1)
                            .ok_or(Error::LimitExceeded {
                                resource: "page Contents indirection depth",
                                limit: self.limits.max_nesting_depth,
                            })?;
                    CachedContentNode {
                        kind: CachedContentNodeKind::Array(items),
                        stream_count: summary.stream_count,
                        nesting_depth,
                    }
                }
                _ => Err(Error::Unresolved(
                    "page Contents is not a stream or stream array".into(),
                ))?,
            };
            self.ensure_content_stream_count(
                node.stream_count,
                self.limits.max_stream_invocations,
            )?;
            self.ensure_content_nesting_depth(depth, node.nesting_depth)?;
            Ok(node)
        })();
        active.remove(&terminal);
        let node = Arc::new(result?);
        self.content_stream_cache
            .insert(terminal, Arc::clone(&node));
        self.content_stream_cache
            .insert(reference, Arc::clone(&node));
        Ok(node)
    }

    fn materialize_content_object(
        &self,
        object: &PdfObject,
        streams: &mut Vec<ObjectRef>,
        materialization_limit: usize,
    ) -> Result<()> {
        match object {
            PdfObject::Null => Ok(()),
            PdfObject::Array(items) => {
                for item in items {
                    self.materialize_content_object(item, streams, materialization_limit)?;
                }
                Ok(())
            }
            PdfObject::Reference(reference) => {
                let node = self.content_stream_cache.get(reference).ok_or_else(|| {
                    Error::Unresolved(
                        "page Contents cache entry disappeared during materialization".into(),
                    )
                })?;
                self.materialize_content_node(node, streams, materialization_limit)
            }
            PdfObject::Stream(_) => Err(Error::Unsupported(
                "direct page content streams cannot retain object provenance".into(),
            )),
            _ => Err(Error::Unresolved(
                "page Contents is not a stream or stream array".into(),
            )),
        }
    }

    fn materialize_content_node(
        &self,
        node: &CachedContentNode,
        streams: &mut Vec<ObjectRef>,
        materialization_limit: usize,
    ) -> Result<()> {
        let materialized_count =
            streams
                .len()
                .checked_add(node.stream_count)
                .ok_or(Error::LimitExceeded {
                    resource: "content stream invocations",
                    limit: self.limits.max_stream_invocations,
                })?;
        self.ensure_content_stream_count(materialized_count, materialization_limit)?;
        if node.stream_count == 0 {
            return Ok(());
        }
        match &node.kind {
            CachedContentNodeKind::Stream(reference) => {
                streams.push(*reference);
                Ok(())
            }
            CachedContentNodeKind::Null => Ok(()),
            CachedContentNodeKind::Array(items) => {
                for item in items {
                    self.materialize_content_object(item, streams, materialization_limit)?;
                }
                Ok(())
            }
        }
    }

    fn ensure_content_nesting_depth(&self, depth: usize, relative_depth: usize) -> Result<()> {
        if depth.saturating_add(relative_depth) > self.limits.max_nesting_depth {
            return Err(Error::LimitExceeded {
                resource: "page Contents indirection depth",
                limit: self.limits.max_nesting_depth,
            });
        }
        Ok(())
    }

    fn ensure_content_stream_count(&self, count: usize, limit: usize) -> Result<()> {
        if count > limit {
            return Err(Error::LimitExceeded {
                resource: "content stream invocations",
                limit: self.limits.max_stream_invocations,
            });
        }
        Ok(())
    }

    fn checked_content_stream_count(&self, count: usize, additional: usize) -> Result<usize> {
        let count = count.checked_add(additional).ok_or(Error::LimitExceeded {
            resource: "content stream invocations",
            limit: self.limits.max_stream_invocations,
        })?;
        self.ensure_content_stream_count(count, self.limits.max_stream_invocations)?;
        Ok(count)
    }

    fn resources(
        &mut self,
        resource: Option<&PdfObject>,
        inherited: Option<&Resources>,
    ) -> Result<Resources> {
        let Some(resource) = resource else {
            return inherited.cloned().map_or_else(
                || {
                    Ok(Resources {
                        fonts: self.empty_resource_map()?,
                        xobjects: self.empty_resource_map()?,
                        ext_gstates: self.empty_resource_map()?,
                    })
                },
                Ok,
            );
        };
        if let PdfObject::Reference(reference) = resource {
            return self.indirect_resources(*reference);
        }
        let dictionary = self.dictionary_value(resource, "Resources")?;
        self.resources_from_dictionary(&dictionary)
    }

    fn resources_from_dictionary(&mut self, dictionary: &PdfDict) -> Result<Resources> {
        let fonts =
            self.shared_resource_map(dictionary.get(b"Font".as_slice()), "Font resources")?;
        let xobjects =
            self.shared_resource_map(dictionary.get(b"XObject".as_slice()), "XObject resources")?;
        let ext_gstates = self.shared_resource_map(
            dictionary.get(b"ExtGState".as_slice()),
            "ExtGState resources",
        )?;
        Ok(Resources {
            fonts,
            xobjects,
            ext_gstates,
        })
    }

    fn empty_resource_map(&mut self) -> Result<Arc<ScopedResourceMap>> {
        self.scoped_resource_map(BTreeMap::new())
    }

    fn scoped_resource_map(&mut self, entries: ResourceMap) -> Result<Arc<ScopedResourceMap>> {
        Ok(Arc::new(ScopedResourceMap {
            scope_id: self.allocate_scope_id()?,
            entries,
        }))
    }

    fn indirect_resources(&mut self, reference: ObjectRef) -> Result<Resources> {
        if let Some(resources) = self.resource_cache.get(&reference) {
            return Ok(resources.clone());
        }
        let terminal = self.pdf.terminal_reference(reference)?;
        if let Some(resources) = self.resource_cache.get(&terminal).cloned() {
            self.resource_cache.insert(reference, resources.clone());
            return Ok(resources);
        }
        let PdfObject::Dictionary(dictionary) = self.pdf.resolve(terminal)? else {
            return Err(Error::Unresolved(
                "Resources does not resolve to a dictionary".into(),
            ));
        };
        let resources = self.resources_from_dictionary(&dictionary)?;
        self.resource_cache.insert(terminal, resources.clone());
        self.resource_cache.insert(reference, resources.clone());
        Ok(resources)
    }

    fn shared_resource_map(
        &mut self,
        value: Option<&PdfObject>,
        context: &str,
    ) -> Result<Arc<ScopedResourceMap>> {
        let Some(value) = value else {
            return self.empty_resource_map();
        };
        if let PdfObject::Reference(reference) = value {
            return self.indirect_resource_map(*reference, context);
        }
        self.scoped_resource_map(
            self.dictionary_value(value, context)?
                .into_iter()
                .map(|(name, object)| (name, Arc::new(object)))
                .collect(),
        )
    }

    fn indirect_resource_map(
        &mut self,
        reference: ObjectRef,
        context: &str,
    ) -> Result<Arc<ScopedResourceMap>> {
        if let Some(resources) = self.resource_map_cache.get(&reference) {
            return Ok(Arc::clone(resources));
        }
        let terminal = self.pdf.terminal_reference(reference)?;
        if let Some(resources) = self.resource_map_cache.get(&terminal).cloned() {
            self.resource_map_cache
                .insert(reference, Arc::clone(&resources));
            return Ok(resources);
        }
        let PdfObject::Dictionary(dictionary) = self.pdf.resolve(terminal)? else {
            return Err(Error::Unresolved(format!(
                "{context} does not resolve to a dictionary"
            )));
        };
        let resources = self.scoped_resource_map(
            dictionary
                .into_iter()
                .map(|(name, object)| (name, Arc::new(object)))
                .collect(),
        )?;
        self.resource_map_cache
            .insert(terminal, Arc::clone(&resources));
        self.resource_map_cache
            .insert(reference, Arc::clone(&resources));
        Ok(resources)
    }

    fn allocate_scope_id(&mut self) -> Result<u64> {
        let id = self.next_scope_id;
        self.next_scope_id = self
            .next_scope_id
            .checked_add(1)
            .ok_or(Error::LimitExceeded {
                resource: "resource-scope identifier address space",
                limit: usize::MAX,
            })?;
        Ok(id)
    }

    fn page_geometry(&self, dictionary: &PdfDict) -> Result<PageGeometry> {
        let bounds = dictionary
            .get(b"CropBox".as_slice())
            .or_else(|| dictionary.get(b"MediaBox".as_slice()))
            .ok_or_else(|| Error::Unresolved("page has no CropBox or MediaBox".into()))?;
        let [x0, y0, x1, y1] = self.rectangle_value(bounds, "page box")?;
        let min_x = x0.min(x1);
        let min_y = y0.min(y1);
        let max_x = x0.max(x1);
        let max_y = y0.max(y1);
        let width = max_x - min_x;
        let height = max_y - min_y;
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(Error::Unresolved(
                "page box must have finite positive dimensions".into(),
            ));
        }
        let rotation = match dictionary.get(b"Rotate".as_slice()) {
            Some(value) => self.integer_value(value, "page Rotate")?,
            None => 0,
        };
        let normalized = rotation.rem_euclid(360);
        if normalized % 90 != 0 {
            return Err(Error::Unsupported(format!(
                "page rotation {rotation} is not a multiple of 90 degrees"
            )));
        }
        let crop = Matrix::translation(-min_x, -min_y)?;
        let rotation = match normalized {
            0 => Matrix::IDENTITY,
            90 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, width)?,
            180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, width, height)?,
            270 => Matrix::new(0.0, 1.0, -1.0, 0.0, height, 0.0)?,
            _ => {
                return Err(Error::Unresolved(
                    "normalized page rotation is outside the supported set".into(),
                ));
            }
        };
        let transform = rotation.concatenate(crop)?;
        let (canonical_width, canonical_height) = if matches!(normalized, 90 | 270) {
            (height, width)
        } else {
            (width, height)
        };
        Ok(PageGeometry {
            transform,
            rotation: normalized as u16,
            crop_bounds: Rect {
                min: Vec2 { x: 0.0, y: 0.0 },
                max: Vec2 {
                    x: canonical_width,
                    y: canonical_height,
                },
            },
        })
    }

    fn matrix_value(&self, value: &PdfObject, context: &str) -> Result<Matrix> {
        let object = self.resolve_value(value, context)?;
        let PdfObject::Array(values) = object else {
            return Err(Error::Unresolved(format!("{context} is not an array")));
        };
        if values.len() != 6 {
            return Err(Error::Unresolved(format!(
                "{context} must contain six numbers"
            )));
        }
        let mut numbers = [0.0; 6];
        for (number, value) in numbers.iter_mut().zip(&values) {
            *number = self.number_value(value, context)?;
        }
        Matrix::new(
            numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5],
        )
    }

    fn rectangle_value(&self, value: &PdfObject, context: &str) -> Result<[f64; 4]> {
        let object = self.resolve_value(value, context)?;
        let PdfObject::Array(values) = object else {
            return Err(Error::Unresolved(format!("{context} is not an array")));
        };
        if values.len() != 4 {
            return Err(Error::Unresolved(format!(
                "{context} must contain four numbers"
            )));
        }
        let mut numbers = [0.0; 4];
        for (number, value) in numbers.iter_mut().zip(&values) {
            *number = self.number_value(value, context)?;
        }
        Ok(numbers)
    }

    fn dictionary_value(&self, value: &PdfObject, context: &str) -> Result<PdfDict> {
        match self.resolve_value(value, context)? {
            PdfObject::Dictionary(dictionary) => Ok(dictionary),
            _ => Err(Error::Unresolved(format!(
                "{context} does not resolve to a dictionary"
            ))),
        }
    }

    fn name_value(&self, value: Option<&PdfObject>, context: &str) -> Result<Vec<u8>> {
        let value = value.ok_or_else(|| Error::Unresolved(format!("{context} is missing")))?;
        match self.resolve_value(value, context)? {
            PdfObject::Name(name) => Ok(name),
            _ => Err(Error::Unresolved(format!(
                "{context} does not resolve to a name"
            ))),
        }
    }

    fn integer_value(&self, value: &PdfObject, context: &str) -> Result<i64> {
        match self.resolve_value(value, context)? {
            PdfObject::Integer(number) => Ok(number),
            _ => Err(Error::Unresolved(format!(
                "{context} does not resolve to an integer"
            ))),
        }
    }

    fn number_value(&self, value: &PdfObject, context: &str) -> Result<f64> {
        let number = match self.resolve_value(value, context)? {
            PdfObject::Integer(number) => number as f64,
            PdfObject::Real(number) => number,
            _ => {
                return Err(Error::Unresolved(format!(
                    "{context} contains a non-numeric value"
                )));
            }
        };
        if number.is_finite() {
            Ok(number)
        } else {
            Err(Error::Unresolved(format!(
                "{context} contains a non-finite number"
            )))
        }
    }

    fn resolve_value(&self, value: &PdfObject, context: &str) -> Result<PdfObject> {
        let mut value = value.clone();
        let mut active = HashSet::new();
        let mut depth = 0;
        while let PdfObject::Reference(reference) = value {
            if depth >= self.limits.max_nesting_depth {
                return Err(Error::LimitExceeded {
                    resource: "PDF extraction indirection depth",
                    limit: self.limits.max_nesting_depth,
                });
            }
            if !active.insert(reference) {
                return Err(Error::Unresolved(format!(
                    "{context} contains a cyclic indirect reference"
                )));
            }
            value = self.pdf.resolve(reference)?;
            depth += 1;
        }
        Ok(value)
    }
}

#[derive(Clone)]
struct GraphicsState {
    ctm: Matrix,
    line_width: f64,
    clip_region: ClipRegion,
    character_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    font: Option<Arc<BoundFont>>,
    font_size: f64,
    rise: f64,
    render_mode: TextRenderMode,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            line_width: 1.0,
            clip_region: ClipRegion::Unbounded,
            character_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            font: None,
            font_size: 0.0,
            rise: 0.0,
            render_mode: TextRenderMode::Fill,
        }
    }
}

#[derive(Clone)]
struct InterpreterState {
    marked_stack: Vec<Option<usize>>,
    marked_overflow: usize,
    content_form: Option<ObjectRef>,
    graphics: GraphicsState,
    graphics_stack: Vec<GraphicsState>,
    current_path: CurrentPath,
    text_matrix: Matrix,
    text_line_matrix: Matrix,
    in_text: bool,
    compatibility_depth: usize,
}

impl Default for InterpreterState {
    fn default() -> Self {
        Self {
            marked_stack: Vec::new(),
            marked_overflow: 0,
            content_form: None,
            graphics: GraphicsState::default(),
            graphics_stack: Vec::new(),
            current_path: CurrentPath::default(),
            text_matrix: Matrix::IDENTITY,
            text_line_matrix: Matrix::IDENTITY,
            in_text: false,
            compatibility_depth: 0,
        }
    }
}

#[derive(Clone)]
struct Resources {
    fonts: Arc<ScopedResourceMap>,
    xobjects: Arc<ScopedResourceMap>,
    ext_gstates: Arc<ScopedResourceMap>,
}

type ResourceMap = BTreeMap<Vec<u8>, Arc<PdfObject>>;

struct ScopedResourceMap {
    scope_id: u64,
    entries: ResourceMap,
}

struct CachedXObject {
    reference: ObjectRef,
    kind: CachedXObjectKind,
}

#[derive(Clone, Copy, Debug)]
struct ContentStreamSummary {
    stream_count: usize,
    nesting_depth: usize,
}

impl ContentStreamSummary {
    const EMPTY: Self = Self {
        stream_count: 0,
        nesting_depth: 0,
    };
}

struct CachedContentNode {
    kind: CachedContentNodeKind,
    stream_count: usize,
    nesting_depth: usize,
}

impl CachedContentNode {
    fn summary(&self) -> ContentStreamSummary {
        ContentStreamSummary {
            stream_count: self.stream_count,
            nesting_depth: self.nesting_depth,
        }
    }
}

enum CachedContentNodeKind {
    Stream(ObjectRef),
    Null,
    Array(Vec<PdfObject>),
}

enum CachedXObjectKind {
    Image,
    Form {
        matrix: Matrix,
        resources: Option<Resources>,
    },
}

struct BoundFont {
    object: Arc<PdfObject>,
    cache_key: FontCacheKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum BoundFontKey {
    Reference(ObjectRef),
    Resource { scope_id: u64, name: Vec<u8> },
    ExtGState(ExtGStateKey),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ExtGStateKey {
    Reference(ObjectRef),
    Direct { scope_id: u64, name: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum FontCacheKey {
    Reference(ObjectRef),
    ScopedResource { scope_id: u64, name: Vec<u8> },
    ScopedExtGState(ExtGStateKey),
}

struct CachedFont {
    id: FontId,
    decoder: FontDecoder,
    identity_source: Option<FontIdentitySource>,
    font_hash: Option<FontProgramHash>,
}

struct DecodedRun {
    font_id: FontId,
    font_hash: Option<FontProgramHash>,
    ascent: f64,
    descent: f64,
    writing_mode: WritingMode,
    glyphs: Vec<FontGlyph>,
}

fn move_text_line(state: &mut InterpreterState, x: f64, y: f64) -> Result<()> {
    state.text_line_matrix = state
        .text_line_matrix
        .concatenate(Matrix::translation(x, y)?)?;
    state.text_matrix = state.text_line_matrix;
    Ok(())
}

fn require_text_object(operation: &Operation, state: &InterpreterState) -> Result<()> {
    if state.in_text {
        Ok(())
    } else {
        Err(operation_error(operation, "text operator outside BT/ET"))
    }
}

fn no_operands(operation: &Operation) -> Result<()> {
    if operation.operands.is_empty() {
        Ok(())
    } else {
        Err(operation_error(operation, "expected no operands"))
    }
}

fn number_operands<const N: usize>(operation: &Operation) -> Result<[f64; N]> {
    if operation.operands.len() != N {
        return Err(operation_error(
            operation,
            &format!("expected {N} numeric operands"),
        ));
    }
    let mut values = [0.0; N];
    for (value, operand) in values.iter_mut().zip(&operation.operands) {
        *value = match operand {
            Operand::Number(number) if number.is_finite() => *number,
            _ => return Err(operation_error(operation, "expected numeric operands")),
        };
    }
    Ok(values)
}

fn one_number(operation: &Operation) -> Result<f64> {
    Ok(number_operands::<1>(operation)?[0])
}

fn one_name(operation: &Operation) -> Result<&[u8]> {
    match operation.operands.as_slice() {
        [Operand::Name(name)] => Ok(name),
        _ => Err(operation_error(operation, "expected one name operand")),
    }
}

fn one_string(operation: &Operation) -> Result<&[u8]> {
    match operation.operands.as_slice() {
        [Operand::String(bytes)] => Ok(bytes),
        _ => Err(operation_error(operation, "expected one string operand")),
    }
}

fn name_and_number(operation: &Operation) -> Result<(&[u8], f64)> {
    match operation.operands.as_slice() {
        [Operand::Name(name), Operand::Number(number)] if number.is_finite() => Ok((name, *number)),
        _ => Err(operation_error(
            operation,
            "expected a font name and numeric size",
        )),
    }
}

fn quote_operands(operation: &Operation) -> Result<(f64, f64, &[u8])> {
    match operation.operands.as_slice() {
        [
            Operand::Number(word),
            Operand::Number(character),
            Operand::String(bytes),
        ] if word.is_finite() && character.is_finite() => Ok((*word, *character, bytes)),
        _ => Err(operation_error(
            operation,
            "expected word spacing, character spacing, and a string",
        )),
    }
}

fn render_mode(value: f64, operation: &Operation) -> Result<TextRenderMode> {
    match value {
        0.0 => Ok(TextRenderMode::Fill),
        1.0 => Ok(TextRenderMode::Stroke),
        2.0 => Ok(TextRenderMode::FillAndStroke),
        3.0 => Ok(TextRenderMode::Invisible),
        4.0 => Ok(TextRenderMode::FillAndClip),
        5.0 => Ok(TextRenderMode::StrokeAndClip),
        6.0 => Ok(TextRenderMode::FillStrokeAndClip),
        7.0 => Ok(TextRenderMode::Clip),
        _ => Err(operation_error(operation, "invalid text render mode")),
    }
}

fn operation_error(operation: &Operation, message: &str) -> Error {
    Error::Unresolved(format!(
        "content operator {} at index {}: {message}",
        String::from_utf8_lossy(&operation.operator),
        operation.index
    ))
}

fn is_ignored_operator(operator: &[u8]) -> bool {
    matches!(
        operator,
        b"J" | b"j"
            | b"M"
            | b"d"
            | b"ri"
            | b"i"
            | b"CS"
            | b"cs"
            | b"SC"
            | b"SCN"
            | b"sc"
            | b"scn"
            | b"G"
            | b"g"
            | b"RG"
            | b"rg"
            | b"K"
            | b"k"
            | b"MP"
            | b"DP"
    )
}

fn path_point(page_geometry: PageGeometry, ctm: Matrix, x: f64, y: f64) -> Result<Vec2> {
    let (x, y) = page_geometry
        .transform
        .concatenate(ctm)?
        .transform_point(x, y)?;
    Ok(Vec2 { x, y })
}

fn transformed_rectangle_path(
    page_geometry: PageGeometry,
    ctm: Matrix,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<([Vec2; 4], Option<Rect>)> {
    let max_x = x + width;
    let max_y = y + height;
    if !max_x.is_finite() || !max_y.is_finite() {
        return Err(Error::Unresolved(
            "path rectangle coordinates are not finite".to_owned(),
        ));
    }
    let corners = [
        path_point(page_geometry, ctm, x, y)?,
        path_point(page_geometry, ctm, max_x, y)?,
        path_point(page_geometry, ctm, max_x, max_y)?,
        path_point(page_geometry, ctm, x, max_y)?,
    ];
    let rectangle = bounding_rect(corners);
    let axis_aligned = corners.iter().all(|point| {
        (approximately_equal(point.x, rectangle.min.x)
            || approximately_equal(point.x, rectangle.max.x))
            && (approximately_equal(point.y, rectangle.min.y)
                || approximately_equal(point.y, rectangle.max.y))
    });
    Ok((corners, axis_aligned.then_some(rectangle)))
}

fn bounding_rect(points: [Vec2; 4]) -> Rect {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for point in points {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    Rect {
        min: Vec2 { x: min_x, y: min_y },
        max: Vec2 { x: max_x, y: max_y },
    }
}

fn rectangle_from_segments(segments: [PathSegment; 4]) -> Option<Rect> {
    if !segments
        .windows(2)
        .all(|pair| points_approximately_equal(pair[0].to, pair[1].from))
        || !points_approximately_equal(segments[3].to, segments[0].from)
    {
        return None;
    }

    let corners = segments.map(|segment| segment.from);
    let rectangle = bounding_rect(corners);
    if approximately_equal(rectangle.min.x, rectangle.max.x)
        || approximately_equal(rectangle.min.y, rectangle.max.y)
    {
        return None;
    }

    let mut corner_mask = 0_u8;
    for segment in segments {
        let same_x = approximately_equal(segment.from.x, segment.to.x);
        let same_y = approximately_equal(segment.from.y, segment.to.y);
        if same_x == same_y {
            return None;
        }
        corner_mask |= rectangle_corner_bit(segment.from, rectangle)?;
    }
    (corner_mask == 0b1111).then_some(rectangle)
}

fn rectangle_corner_bit(point: Vec2, rectangle: Rect) -> Option<u8> {
    let x = if approximately_equal(point.x, rectangle.min.x) {
        0
    } else if approximately_equal(point.x, rectangle.max.x) {
        1
    } else {
        return None;
    };
    let y = if approximately_equal(point.y, rectangle.min.y) {
        0
    } else if approximately_equal(point.y, rectangle.max.y) {
        1
    } else {
        return None;
    };
    Some(1 << (x + 2 * y))
}

fn points_approximately_equal(left: Vec2, right: Vec2) -> bool {
    approximately_equal(left.x, right.x) && approximately_equal(left.y, right.y)
}

fn approximately_equal(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= 64.0 * f64::EPSILON * scale
}

fn transformed_line_width(page_geometry: PageGeometry, ctm: Matrix, width: f64) -> Result<f64> {
    let matrix = page_geometry.transform.concatenate(ctm)?;
    let determinant = matrix.a.mul_add(matrix.d, -(matrix.b * matrix.c));
    let width = width * determinant.abs().sqrt();
    if width.is_finite() {
        Ok(width)
    } else {
        Err(Error::Unresolved(
            "line transform produces a non-finite width".to_owned(),
        ))
    }
}

fn intersect_clip_region(current: ClipRegion, rectangle: Rect) -> ClipRegion {
    if rectangle.max.x <= rectangle.min.x || rectangle.max.y <= rectangle.min.y {
        return ClipRegion::Empty;
    }
    match current {
        ClipRegion::Unbounded => ClipRegion::Rectangle(rectangle),
        ClipRegion::Rectangle(current) => {
            let intersection = Rect {
                min: Vec2 {
                    x: current.min.x.max(rectangle.min.x),
                    y: current.min.y.max(rectangle.min.y),
                },
                max: Vec2 {
                    x: current.max.x.min(rectangle.max.x),
                    y: current.max.y.min(rectangle.max.y),
                },
            };
            if intersection.max.x <= intersection.min.x || intersection.max.y <= intersection.min.y
            {
                ClipRegion::Empty
            } else {
                ClipRegion::Rectangle(intersection)
            }
        }
        ClipRegion::Empty => ClipRegion::Empty,
    }
}

fn segment_is_visible(segment: PathSegment, clip_region: ClipRegion) -> bool {
    match clip_region {
        ClipRegion::Unbounded => true,
        ClipRegion::Rectangle(rectangle) => {
            point_is_inside(segment.from, rectangle) && point_is_inside(segment.to, rectangle)
        }
        ClipRegion::Empty => false,
    }
}

fn point_is_inside(point: Vec2, rectangle: Rect) -> bool {
    point.x >= rectangle.min.x
        && point.x <= rectangle.max.x
        && point.y >= rectangle.min.y
        && point.y <= rectangle.max.y
}

fn transformed_rect(matrix: Matrix, x0: f64, y0: f64, x1: f64, y1: f64) -> Result<Rect> {
    let corners = [
        matrix.transform_point(x0, y0)?,
        matrix.transform_point(x0, y1)?,
        matrix.transform_point(x1, y0)?,
        matrix.transform_point(x1, y1)?,
    ];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in corners {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    Ok(Rect {
        min: Vec2 { x: min_x, y: min_y },
        max: Vec2 { x: max_x, y: max_y },
    })
}

fn glyph_crop_status(glyph: Rect, crop: Rect) -> GlyphCropStatus {
    if glyph.max.x <= crop.min.x
        || glyph.min.x >= crop.max.x
        || glyph.max.y <= crop.min.y
        || glyph.min.y >= crop.max.y
    {
        GlyphCropStatus::Outside
    } else if glyph.min.x < crop.min.x
        || glyph.max.x > crop.max.x
        || glyph.min.y < crop.min.y
        || glyph.max.y > crop.max.y
    {
        GlyphCropStatus::PartiallyOutside
    } else {
        GlyphCropStatus::Inside
    }
}

fn glyph_path_clip_status(glyph: Rect, clip_region: ClipRegion) -> GlyphPathClipStatus {
    let ClipRegion::Rectangle(clip) = clip_region else {
        return match clip_region {
            ClipRegion::Unbounded => GlyphPathClipStatus::Unclipped,
            ClipRegion::Empty => GlyphPathClipStatus::Outside,
            ClipRegion::Rectangle(_) => unreachable!(),
        };
    };
    if glyph.max.x <= clip.min.x
        || glyph.min.x >= clip.max.x
        || glyph.max.y <= clip.min.y
        || glyph.min.y >= clip.max.y
    {
        GlyphPathClipStatus::Outside
    } else if glyph.min.x < clip.min.x
        || glyph.max.x > clip.max.x
        || glyph.min.y < clip.min.y
        || glyph.max.y > clip.max.y
    {
        GlyphPathClipStatus::PartiallyOutside
    } else {
        GlyphPathClipStatus::Inside
    }
}

fn normalized_vector(x: f64, y: f64, operation: &Operation) -> Result<Vec2> {
    let length = x.hypot(y);
    if !length.is_finite() || length <= f64::EPSILON {
        return Err(operation_error(
            operation,
            "text transform produces a zero writing direction",
        ));
    }
    Ok(Vec2 {
        x: x / length,
        y: y / length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::pdf::{DecodedStream, PdfVersion, RawStream};

    fn empty_map(scope_id: u64) -> Arc<ScopedResourceMap> {
        Arc::new(ScopedResourceMap {
            scope_id,
            entries: BTreeMap::new(),
        })
    }

    struct CountingPdf {
        objects: HashMap<ObjectRef, PdfObject>,
        terminals: HashMap<ObjectRef, ObjectRef>,
        bytes: Vec<u8>,
        resolve_calls: AtomicUsize,
        terminal_calls: AtomicUsize,
        decoded_calls: AtomicUsize,
    }

    impl ParsedPdf for CountingPdf {
        fn version(&self) -> PdfVersion {
            PdfVersion { major: 1, minor: 7 }
        }

        fn trailer(&self) -> Result<PdfDict> {
            Ok(PdfDict::new())
        }

        fn resolve(&self, reference: ObjectRef) -> Result<PdfObject> {
            self.resolve_calls.fetch_add(1, Ordering::Relaxed);
            let reference = self.terminals.get(&reference).copied().unwrap_or(reference);
            self.objects
                .get(&reference)
                .cloned()
                .ok_or_else(|| Error::Unresolved("fixture object is missing".into()))
        }

        fn terminal_reference(&self, reference: ObjectRef) -> Result<ObjectRef> {
            self.terminal_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.terminals.get(&reference).copied().unwrap_or(reference))
        }

        fn pages(&self) -> Result<Vec<PageRef>> {
            Ok(Vec::new())
        }

        fn page_dict(&self, _page: PageRef) -> Result<PdfDict> {
            Err(Error::Unresolved("fixture has no pages".into()))
        }

        fn raw_stream(&self, _reference: ObjectRef) -> Result<RawStream> {
            Err(Error::Unresolved("fixture has no raw stream".into()))
        }

        fn decoded_stream(&self, _reference: ObjectRef) -> Result<DecodedStream> {
            self.decoded_calls.fetch_add(1, Ordering::Relaxed);
            Ok(DecodedStream {
                dictionary: PdfDict::new(),
                bytes: self.bytes.clone(),
            })
        }
    }

    #[test]
    fn clones_graphics_state_and_resources_by_shared_handle() {
        let object = Arc::new(PdfObject::Dictionary(BTreeMap::from([(
            b"LargePayload".to_vec(),
            PdfObject::String(vec![0; 1024]),
        )])));
        let font = Arc::new(BoundFont {
            object: Arc::clone(&object),
            cache_key: FontCacheKey::ScopedResource {
                scope_id: 1,
                name: b"F1".to_vec(),
            },
        });
        let graphics = GraphicsState {
            font: Some(Arc::clone(&font)),
            ..GraphicsState::default()
        };
        let resources = Resources {
            fonts: Arc::new(ScopedResourceMap {
                scope_id: 1,
                entries: BTreeMap::from([(b"F1".to_vec(), object)]),
            }),
            xobjects: empty_map(2),
            ext_gstates: empty_map(3),
        };

        let cloned_graphics = graphics.clone();
        let cloned_resources = resources.clone();

        assert!(Arc::ptr_eq(
            graphics.font.as_ref().expect("font should be selected"),
            cloned_graphics
                .font
                .as_ref()
                .expect("cloned font should remain selected")
        ));
        assert!(Arc::ptr_eq(&resources.fonts, &cloned_resources.fonts));
    }

    #[test]
    fn rejects_non_finite_page_box_coordinates() {
        let pdf = CountingPdf {
            objects: HashMap::new(),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let extraction = Extraction::new(&pdf, ExtractionLimits::default());
        let page = PdfDict::from([(
            b"CropBox".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Real(f64::NAN),
                PdfObject::Integer(0),
                PdfObject::Integer(100),
                PdfObject::Integer(100),
            ]),
        )]);

        assert!(matches!(
            extraction.page_geometry(&page),
            Err(Error::Unresolved(message)) if message.contains("non-finite")
        ));
    }

    #[test]
    fn reuses_shared_page_resource_snapshots() -> Result<()> {
        let pdf = CountingPdf {
            objects: HashMap::new(),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());
        let snapshot = Arc::new(PdfObject::Dictionary(BTreeMap::from([(
            b"Font".to_vec(),
            PdfObject::Dictionary(BTreeMap::new()),
        )])));

        let first = extraction.page_resources(Some(Arc::clone(&snapshot)))?;
        let second = extraction.page_resources(Some(Arc::clone(&snapshot)))?;

        assert!(Arc::ptr_eq(&first.fonts, &second.fonts));
        assert!(Arc::ptr_eq(&first.xobjects, &second.xobjects));
        assert!(Arc::ptr_eq(&first.ext_gstates, &second.ext_gstates));
        assert_eq!(extraction.page_resource_cache.len(), 1);
        Ok(())
    }

    #[test]
    fn caches_xobject_metadata_and_decoded_bytes_by_object_reference() -> Result<()> {
        let alias = ObjectRef {
            object_number: 7,
            generation: 0,
        };
        let second_alias = ObjectRef {
            object_number: 8,
            generation: 0,
        };
        let terminal = ObjectRef {
            object_number: 9,
            generation: 0,
        };
        let pdf = CountingPdf {
            objects: HashMap::from([(
                terminal,
                PdfObject::Stream(BTreeMap::from([
                    (b"Subtype".to_vec(), PdfObject::Name(b"Form".to_vec())),
                    (
                        b"Resources".to_vec(),
                        PdfObject::Dictionary(BTreeMap::new()),
                    ),
                ])),
            )]),
            terminals: HashMap::from([(alias, terminal), (second_alias, terminal)]),
            bytes: b"% cached form".to_vec(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());

        let first_xobject = extraction.cached_xobject(alias)?;
        let second_xobject = extraction.cached_xobject(second_alias)?;
        let first_bytes = extraction.decoded_stream_bytes(first_xobject.reference)?;
        let second_bytes = extraction.decoded_stream_bytes(second_xobject.reference)?;

        assert!(Arc::ptr_eq(&first_xobject, &second_xobject));
        assert_eq!(first_xobject.reference, terminal);
        assert!(Arc::ptr_eq(&first_bytes, &second_bytes));
        assert_eq!(pdf.resolve_calls.load(Ordering::Relaxed), 1);
        assert_eq!(pdf.terminal_calls.load(Ordering::Relaxed), 3);
        assert_eq!(pdf.decoded_calls.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[test]
    fn shares_indirect_resources_and_category_maps_across_aliases() -> Result<()> {
        let form_one = ObjectRef {
            object_number: 10,
            generation: 0,
        };
        let form_two = ObjectRef {
            object_number: 11,
            generation: 0,
        };
        let resources_alias = ObjectRef {
            object_number: 20,
            generation: 0,
        };
        let resources_terminal = ObjectRef {
            object_number: 21,
            generation: 0,
        };
        let font_alias_one = ObjectRef {
            object_number: 30,
            generation: 0,
        };
        let font_alias_two = ObjectRef {
            object_number: 31,
            generation: 0,
        };
        let font_terminal = ObjectRef {
            object_number: 32,
            generation: 0,
        };
        let form = || {
            PdfObject::Stream(BTreeMap::from([
                (b"Subtype".to_vec(), PdfObject::Name(b"Form".to_vec())),
                (b"Resources".to_vec(), PdfObject::Reference(resources_alias)),
            ]))
        };
        let pdf = CountingPdf {
            objects: HashMap::from([
                (form_one, form()),
                (form_two, form()),
                (
                    resources_terminal,
                    PdfObject::Dictionary(BTreeMap::from([(
                        b"Font".to_vec(),
                        PdfObject::Reference(font_alias_one),
                    )])),
                ),
                (
                    font_terminal,
                    PdfObject::Dictionary(BTreeMap::from([(
                        b"F1".to_vec(),
                        PdfObject::Dictionary(BTreeMap::new()),
                    )])),
                ),
            ]),
            terminals: HashMap::from([
                (resources_alias, resources_terminal),
                (font_alias_one, font_terminal),
                (font_alias_two, font_terminal),
            ]),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());

        let first = extraction.cached_xobject(form_one)?;
        let second = extraction.cached_xobject(form_two)?;
        let (
            CachedXObjectKind::Form {
                resources: Some(first_resources),
                ..
            },
            CachedXObjectKind::Form {
                resources: Some(second_resources),
                ..
            },
        ) = (&first.kind, &second.kind)
        else {
            return Err(Error::Unresolved("fixtures should be Forms".into()));
        };
        assert_eq!(
            first_resources.fonts.scope_id,
            second_resources.fonts.scope_id
        );
        assert!(Arc::ptr_eq(&first_resources.fonts, &second_resources.fonts));

        let direct_one = PdfObject::Dictionary(BTreeMap::from([(
            b"Font".to_vec(),
            PdfObject::Reference(font_alias_one),
        )]));
        let direct_two = PdfObject::Dictionary(BTreeMap::from([(
            b"Font".to_vec(),
            PdfObject::Reference(font_alias_two),
        )]));
        let first_direct = extraction.resources(Some(&direct_one), None)?;
        let second_direct = extraction.resources(Some(&direct_two), None)?;
        assert!(Arc::ptr_eq(&first_direct.fonts, &second_direct.fonts));
        assert_eq!(pdf.resolve_calls.load(Ordering::Relaxed), 4);
        Ok(())
    }

    #[test]
    fn caches_an_ext_gstate_without_a_font() -> Result<()> {
        let pdf = CountingPdf {
            objects: HashMap::new(),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());
        let ext_gstates = Arc::new(ScopedResourceMap {
            scope_id: 4,
            entries: BTreeMap::from([(
                b"GS".to_vec(),
                Arc::new(PdfObject::Dictionary(BTreeMap::from([(
                    b"LargePayload".to_vec(),
                    PdfObject::String(vec![0; 1024]),
                )]))),
            )]),
        });
        let first_resources = Resources {
            fonts: empty_map(5),
            xobjects: empty_map(6),
            ext_gstates: Arc::clone(&ext_gstates),
        };
        let second_resources = Resources {
            fonts: empty_map(7),
            xobjects: empty_map(8),
            ext_gstates,
        };
        let operation = Operation {
            operands: vec![Operand::Name(b"GS".to_vec())],
            operator: b"gs".to_vec(),
            index: 0,
        };
        let mut state = InterpreterState::default();

        extraction.apply_ext_gstate(&operation, &first_resources, &mut state)?;
        extraction.apply_ext_gstate(&operation, &second_resources, &mut state)?;

        assert_eq!(extraction.ext_gstate_fonts.len(), 1);
        assert!(extraction.ext_gstate_fonts.values().all(Option::is_none));
        Ok(())
    }

    #[test]
    fn shares_ext_gstate_positive_and_negative_caches_across_entry_aliases() -> Result<()> {
        let negative_aliases = [
            ObjectRef {
                object_number: 40,
                generation: 0,
            },
            ObjectRef {
                object_number: 41,
                generation: 0,
            },
        ];
        let negative_terminal = ObjectRef {
            object_number: 42,
            generation: 0,
        };
        let positive_aliases = [
            ObjectRef {
                object_number: 50,
                generation: 0,
            },
            ObjectRef {
                object_number: 51,
                generation: 0,
            },
        ];
        let positive_terminal = ObjectRef {
            object_number: 52,
            generation: 0,
        };
        let pdf = CountingPdf {
            objects: HashMap::from([
                (negative_terminal, PdfObject::Dictionary(BTreeMap::new())),
                (
                    positive_terminal,
                    PdfObject::Dictionary(BTreeMap::from([(
                        b"Font".to_vec(),
                        PdfObject::Array(vec![
                            PdfObject::Dictionary(BTreeMap::new()),
                            PdfObject::Integer(10),
                        ]),
                    )])),
                ),
            ]),
            terminals: HashMap::from([
                (negative_aliases[0], negative_terminal),
                (negative_aliases[1], negative_terminal),
                (positive_aliases[0], positive_terminal),
                (positive_aliases[1], positive_terminal),
            ]),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());
        let resources = |scope_id, negative, positive| Resources {
            fonts: empty_map(scope_id),
            xobjects: empty_map(scope_id + 1),
            ext_gstates: Arc::new(ScopedResourceMap {
                scope_id: scope_id + 2,
                entries: BTreeMap::from([
                    (
                        b"Negative".to_vec(),
                        Arc::new(PdfObject::Reference(negative)),
                    ),
                    (
                        b"Positive".to_vec(),
                        Arc::new(PdfObject::Reference(positive)),
                    ),
                ]),
            }),
        };
        let first_resources = resources(60, negative_aliases[0], positive_aliases[0]);
        let second_resources = resources(70, negative_aliases[1], positive_aliases[1]);
        let operation = |name: &[u8]| Operation {
            operands: vec![Operand::Name(name.to_vec())],
            operator: b"gs".to_vec(),
            index: 0,
        };
        let mut first_state = InterpreterState::default();
        let mut second_state = InterpreterState::default();

        extraction.apply_ext_gstate(&operation(b"Negative"), &first_resources, &mut first_state)?;
        extraction.apply_ext_gstate(
            &operation(b"Negative"),
            &second_resources,
            &mut second_state,
        )?;
        extraction.apply_ext_gstate(&operation(b"Positive"), &first_resources, &mut first_state)?;
        extraction.apply_ext_gstate(
            &operation(b"Positive"),
            &second_resources,
            &mut second_state,
        )?;

        assert_eq!(extraction.ext_gstate_fonts.len(), 2);
        assert_eq!(extraction.bound_fonts.len(), 1);
        assert!(Arc::ptr_eq(
            first_state
                .graphics
                .font
                .as_ref()
                .expect("positive font should be selected"),
            second_state
                .graphics
                .font
                .as_ref()
                .expect("positive font should be shared")
        ));
        assert_eq!(pdf.resolve_calls.load(Ordering::Relaxed), 2);
        Ok(())
    }

    #[test]
    fn caches_shared_indirect_contents_without_losing_duplicates() -> Result<()> {
        let aliases = [
            ObjectRef {
                object_number: 80,
                generation: 0,
            },
            ObjectRef {
                object_number: 81,
                generation: 0,
            },
        ];
        let array = ObjectRef {
            object_number: 82,
            generation: 0,
        };
        let stream = ObjectRef {
            object_number: 83,
            generation: 0,
        };
        let pdf = CountingPdf {
            objects: HashMap::from([
                (array, PdfObject::Array(vec![PdfObject::Reference(stream)])),
                (stream, PdfObject::Stream(BTreeMap::new())),
            ]),
            terminals: HashMap::from([(aliases[0], array), (aliases[1], array)]),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());
        let contents = PdfObject::Array(vec![
            PdfObject::Reference(aliases[0]),
            PdfObject::Reference(aliases[1]),
        ]);

        assert_eq!(
            extraction.content_streams(Some(&contents))?,
            [stream, stream]
        );
        assert_eq!(pdf.resolve_calls.load(Ordering::Relaxed), 2);
        assert!(Arc::ptr_eq(
            &extraction.content_stream_cache[&aliases[0]],
            &extraction.content_stream_cache[&aliases[1]],
        ));
        Ok(())
    }

    #[test]
    fn bounds_content_dag_summaries_by_stream_invocations() {
        let reference = |object_number| ObjectRef {
            object_number,
            generation: 0,
        };
        let stream = reference(90);
        let array_0 = reference(91);
        let array_1 = reference(92);
        let array_2 = reference(93);
        let array_3 = reference(94);
        let pdf = CountingPdf {
            objects: HashMap::from([
                (stream, PdfObject::Stream(BTreeMap::new())),
                (
                    array_0,
                    PdfObject::Array(vec![PdfObject::Reference(stream)]),
                ),
                (
                    array_1,
                    PdfObject::Array(vec![
                        PdfObject::Reference(array_0),
                        PdfObject::Reference(array_0),
                    ]),
                ),
                (
                    array_2,
                    PdfObject::Array(vec![
                        PdfObject::Reference(array_1),
                        PdfObject::Reference(array_1),
                    ]),
                ),
                (
                    array_3,
                    PdfObject::Array(vec![
                        PdfObject::Reference(array_2),
                        PdfObject::Reference(array_2),
                    ]),
                ),
            ]),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(
            &pdf,
            ExtractionLimits {
                max_stream_invocations: 4,
                ..ExtractionLimits::default()
            },
        );
        extraction.stream_invocations = 1;

        assert_eq!(
            extraction
                .content_streams(Some(&PdfObject::Reference(array_2)))
                .expect_err("the expanded DAG must exceed the remaining invocation budget"),
            Error::LimitExceeded {
                resource: "content stream invocations",
                limit: 4,
            }
        );
        assert!(extraction.content_stream_cache.contains_key(&array_1));
        assert_eq!(extraction.content_stream_cache[&array_2].stream_count, 4);

        assert_eq!(
            extraction
                .content_streams(Some(&PdfObject::Reference(array_3)))
                .expect_err("the larger DAG must exceed the global invocation budget"),
            Error::LimitExceeded {
                resource: "content stream invocations",
                limit: 4,
            }
        );
        assert!(!extraction.content_stream_cache.contains_key(&array_3));
    }

    #[test]
    fn caches_content_dag_structure_without_flattened_vectors() -> Result<()> {
        const EXPANDED_STREAMS: usize = 1 << 22;
        const DOUBLING_LEVELS: usize = 21;
        const UNARY_WRAPPERS: usize = 64;

        let reference = |object_number| ObjectRef {
            object_number,
            generation: 0,
        };
        let first_stream = reference(200);
        let second_stream = reference(201);
        let mut objects = HashMap::from([
            (first_stream, PdfObject::Stream(BTreeMap::new())),
            (second_stream, PdfObject::Stream(BTreeMap::new())),
        ]);
        let mut next_object_number = 202;
        let mut root = reference(next_object_number);
        objects.insert(
            root,
            PdfObject::Array(vec![
                PdfObject::Reference(first_stream),
                PdfObject::Reference(second_stream),
            ]),
        );
        let mut retained_items = 2;

        for _ in 0..DOUBLING_LEVELS {
            next_object_number += 1;
            let parent = reference(next_object_number);
            objects.insert(
                parent,
                PdfObject::Array(vec![PdfObject::Reference(root), PdfObject::Reference(root)]),
            );
            root = parent;
            retained_items += 2;
        }
        for _ in 0..UNARY_WRAPPERS {
            next_object_number += 1;
            let parent = reference(next_object_number);
            objects.insert(parent, PdfObject::Array(vec![PdfObject::Reference(root)]));
            root = parent;
            retained_items += 1;
        }

        let pdf = CountingPdf {
            objects,
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(
            &pdf,
            ExtractionLimits {
                max_nesting_depth: 256,
                max_stream_invocations: EXPANDED_STREAMS,
                ..ExtractionLimits::default()
            },
        );

        let streams = extraction.content_streams(Some(&PdfObject::Reference(root)))?;

        assert_eq!(streams.len(), EXPANDED_STREAMS);
        assert!(
            streams
                .as_chunks::<2>()
                .0
                .iter()
                .all(|pair| *pair == [first_stream, second_stream])
        );
        let cached_items = extraction
            .content_stream_cache
            .values()
            .map(|node| match &node.kind {
                CachedContentNodeKind::Array(items) => items.len(),
                CachedContentNodeKind::Stream(_) | CachedContentNodeKind::Null => 0,
            })
            .sum::<usize>();
        assert_eq!(cached_items, retained_items);
        assert_eq!(
            extraction.content_stream_cache[&root].stream_count,
            EXPANDED_STREAMS
        );
        Ok(())
    }

    #[test]
    fn counts_alternating_content_references_and_arrays() -> Result<()> {
        let reference = |object_number| ObjectRef {
            object_number,
            generation: 0,
        };
        let stream = reference(100);
        let inner_array = reference(101);
        let outer_array = reference(102);
        let pdf = CountingPdf {
            objects: HashMap::from([
                (stream, PdfObject::Stream(BTreeMap::new())),
                (
                    inner_array,
                    PdfObject::Array(vec![PdfObject::Reference(stream)]),
                ),
                (
                    outer_array,
                    PdfObject::Array(vec![PdfObject::Reference(inner_array)]),
                ),
            ]),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let contents = PdfObject::Reference(outer_array);
        let mut shallow = Extraction::new(
            &pdf,
            ExtractionLimits {
                max_nesting_depth: 3,
                ..ExtractionLimits::default()
            },
        );

        assert_eq!(
            shallow
                .content_streams(Some(&contents))
                .expect_err("the alternating chain has relative depth four"),
            Error::LimitExceeded {
                resource: "page Contents indirection depth",
                limit: 3,
            }
        );

        let mut exact = Extraction::new(
            &pdf,
            ExtractionLimits {
                max_nesting_depth: 4,
                ..ExtractionLimits::default()
            },
        );
        assert_eq!(exact.content_streams(Some(&contents))?, [stream]);
        Ok(())
    }

    #[test]
    fn records_reference_relative_content_depth() -> Result<()> {
        let reference = |object_number| ObjectRef {
            object_number,
            generation: 0,
        };
        let null = reference(105);
        let empty_array = reference(106);
        let stream = reference(107);
        let stream_array = reference(108);
        let pdf = CountingPdf {
            objects: HashMap::from([
                (null, PdfObject::Null),
                (empty_array, PdfObject::Array(Vec::new())),
                (stream, PdfObject::Stream(BTreeMap::new())),
                (
                    stream_array,
                    PdfObject::Array(vec![PdfObject::Reference(stream)]),
                ),
            ]),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());

        assert!(
            extraction
                .content_streams(Some(&PdfObject::Reference(null)))?
                .is_empty()
        );
        assert!(
            extraction
                .content_streams(Some(&PdfObject::Reference(empty_array)))?
                .is_empty()
        );
        assert_eq!(
            extraction.content_streams(Some(&PdfObject::Reference(stream_array)))?,
            [stream]
        );
        assert_eq!(extraction.content_stream_cache[&null].nesting_depth, 1);
        assert_eq!(
            extraction.content_stream_cache[&empty_array].nesting_depth,
            1
        );
        assert_eq!(
            extraction.content_stream_cache[&stream_array].nesting_depth,
            2
        );
        Ok(())
    }

    #[test]
    fn enforces_cached_content_depth_at_the_call_site() -> Result<()> {
        let reference = |object_number| ObjectRef {
            object_number,
            generation: 0,
        };
        let stream = reference(110);
        let array = reference(111);
        let pdf = CountingPdf {
            objects: HashMap::from([
                (stream, PdfObject::Stream(BTreeMap::new())),
                (array, PdfObject::Array(vec![PdfObject::Reference(stream)])),
            ]),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let limits = ExtractionLimits {
            max_nesting_depth: 2,
            ..ExtractionLimits::default()
        };
        let mut extraction = Extraction::new(&pdf, limits);
        let contents = PdfObject::Reference(array);

        assert_eq!(extraction.content_streams(Some(&contents))?, [stream]);
        assert_eq!(
            extraction
                .content_stream_cache
                .get(&array)
                .expect("the indirect array should be cached")
                .nesting_depth,
            2
        );
        let resolved_before_cache_hit = pdf.resolve_calls.load(Ordering::Relaxed);
        let mut active = HashSet::new();
        assert_eq!(
            extraction
                .summarize_content_object(&contents, 1, &mut active)
                .expect_err("the cached relative depth must be applied to the new call depth"),
            Error::LimitExceeded {
                resource: "page Contents indirection depth",
                limit: 2,
            }
        );
        assert_eq!(
            pdf.resolve_calls.load(Ordering::Relaxed),
            resolved_before_cache_hit
        );
        Ok(())
    }

    #[test]
    fn does_not_cache_cyclic_content_nodes() {
        let first = ObjectRef {
            object_number: 120,
            generation: 0,
        };
        let second = ObjectRef {
            object_number: 121,
            generation: 0,
        };
        let pdf = CountingPdf {
            objects: HashMap::from([
                (first, PdfObject::Array(vec![PdfObject::Reference(second)])),
                (second, PdfObject::Array(vec![PdfObject::Reference(first)])),
            ]),
            terminals: HashMap::new(),
            bytes: Vec::new(),
            resolve_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            decoded_calls: AtomicUsize::new(0),
        };
        let mut extraction = Extraction::new(&pdf, ExtractionLimits::default());

        assert!(matches!(
            extraction.content_streams(Some(&PdfObject::Reference(first))),
            Err(Error::Unresolved(message)) if message.starts_with("cyclic page Contents reference")
        ));
        assert!(!extraction.content_stream_cache.contains_key(&first));
        assert!(!extraction.content_stream_cache.contains_key(&second));
    }
}
