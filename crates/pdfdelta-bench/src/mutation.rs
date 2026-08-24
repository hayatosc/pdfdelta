use pdfdelta_core::diff::ChangeKind;

use crate::{
    BenchError, Result,
    canonical::{CanonicalDocument, Paragraph, validate_paragraph_id, validate_text},
};

pub const MIN_LINE_GAP: u16 = 8;
pub const MAX_LINE_GAP: u16 = 72;

pub const DEFAULT_PAGE_WIDTH: u16 = 612;
/// Matches the MediaBox height both renderers emitted before plans carried a page size.
pub const DEFAULT_PAGE_HEIGHT: u16 = 792;
pub const MIN_MARGIN: u16 = 0;
pub const MAX_MARGIN: u16 = DEFAULT_PAGE_WIDTH - 1;
/// Matches the horizontal origin both renderers used before plans carried a margin.
pub const DEFAULT_MARGIN: u16 = 36;

pub const MIN_FONT_SIZE: u16 = 1;
/// Matches the `/F1 10 Tf` size both renderers emitted before plans carried a font size.
pub const DEFAULT_FONT_SIZE: u16 = 10;

/// Vertical text origin both renderers use for the first line of every page.
pub(crate) const PAGE_TOP: i64 = 740;
pub(crate) const PAGE_BOTTOM: i64 = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderPlan {
    pages: Vec<Vec<String>>,
    line_gap: u16,
    margin: u16,
    font_size: u16,
    page_width: u16,
    page_height: u16,
}

impl RenderPlan {
    pub fn new(pages: Vec<Vec<String>>, line_gap: u16) -> Result<Self> {
        Self::with_margin(pages, line_gap, DEFAULT_MARGIN)
    }

    pub fn with_margin(pages: Vec<Vec<String>>, line_gap: u16, margin: u16) -> Result<Self> {
        Self::with_font_size(pages, line_gap, margin, DEFAULT_FONT_SIZE)
    }

    pub fn with_font_size(
        pages: Vec<Vec<String>>,
        line_gap: u16,
        margin: u16,
        font_size: u16,
    ) -> Result<Self> {
        Self::build(
            pages,
            line_gap,
            margin,
            font_size,
            DEFAULT_PAGE_WIDTH,
            DEFAULT_PAGE_HEIGHT,
        )
    }

    /// Renders with explicit page dimensions while keeping the default margin
    /// and font size, so callers only name what they actually change.
    pub fn with_page_size(
        pages: Vec<Vec<String>>,
        line_gap: u16,
        page_width: u16,
        page_height: u16,
    ) -> Result<Self> {
        Self::build(
            pages,
            line_gap,
            DEFAULT_MARGIN,
            DEFAULT_FONT_SIZE,
            page_width,
            page_height,
        )
    }

    fn build(
        pages: Vec<Vec<String>>,
        line_gap: u16,
        margin: u16,
        font_size: u16,
        page_width: u16,
        page_height: u16,
    ) -> Result<Self> {
        if pages.is_empty() {
            return Err(BenchError::InvalidInput(
                "render plans require at least one page".to_owned(),
            ));
        }
        if !(MIN_LINE_GAP..=MAX_LINE_GAP).contains(&line_gap) {
            return Err(BenchError::InvalidInput(format!(
                "line_gap must be between {MIN_LINE_GAP} and {MAX_LINE_GAP}"
            )));
        }
        if !(MIN_MARGIN..=MAX_MARGIN).contains(&margin) {
            return Err(BenchError::InvalidInput(format!(
                "margin must be between {MIN_MARGIN} and {MAX_MARGIN}"
            )));
        }
        if font_size < MIN_FONT_SIZE {
            return Err(BenchError::InvalidInput(
                "font_size must be greater than zero".to_owned(),
            ));
        }
        if page_width == 0 || page_height == 0 {
            return Err(BenchError::InvalidInput(
                "page dimensions must be greater than zero".to_owned(),
            ));
        }
        if margin >= page_width || PAGE_TOP >= i64::from(page_height) {
            return Err(BenchError::InvalidInput(format!(
                "text origin ({margin}, {PAGE_TOP}) must lie inside the {page_width}x{page_height} page"
            )));
        }
        // deliberate: only the text origin is constrained to the MediaBox;
        // add font-metric width validation when fixtures exercise clipping.
        for (page_index, lines) in pages.iter().enumerate() {
            if lines.is_empty() {
                return Err(BenchError::InvalidInput(format!(
                    "render plan page {page_index} must contain at least one line"
                )));
            }
            for line in lines {
                validate_text(line)?;
            }
        }
        Ok(Self {
            pages,
            line_gap,
            margin,
            font_size,
            page_width,
            page_height,
        })
    }

    pub fn pages(&self) -> &[Vec<String>] {
        &self.pages
    }

    pub const fn line_gap(&self) -> u16 {
        self.line_gap
    }

    pub const fn margin(&self) -> u16 {
        self.margin
    }

    pub const fn font_size(&self) -> u16 {
        self.font_size
    }

    pub const fn page_width(&self) -> u16 {
        self.page_width
    }

    pub const fn page_height(&self) -> u16 {
        self.page_height
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    LineWrap {
        paragraph_id: String,
        after_word: usize,
    },
    LineWrapTwice {
        paragraph_id: String,
        after_words: [usize; 2],
    },
    PageBreak {
        before_paragraph: usize,
    },
    /// Renders the unchanged document with a different line gap so both
    /// PDFs differ in layout while canonical text stays identical.
    LineHeightChange {
        new_line_gap: u16,
    },
    /// Renders the unchanged document with a different left margin so both
    /// PDFs differ in layout while canonical text stays identical.
    MarginChange {
        new_margin: u16,
    },
    /// Renders the unchanged document with a different font size so both
    /// PDFs differ in layout while canonical text stays identical.
    FontSizeChange {
        new_font_size: u16,
    },
    /// Renders the unchanged document on a different page size so both
    /// PDFs differ in layout while canonical text stays identical.
    PageSizeChange {
        new_page_width: u16,
        new_page_height: u16,
    },
    TextReplace {
        paragraph_id: String,
        new_text: String,
    },
    /// Inserts characters at a Unicode scalar offset inside a paragraph,
    /// exercising pure insertions that keep both paragraph neighbours intact.
    TextInsert {
        paragraph_id: String,
        at: usize,
        text: String,
    },
    /// Deletes the half-open scalar range [start, end) inside a paragraph.
    TextDelete {
        paragraph_id: String,
        start: usize,
        end: usize,
    },
    /// Replaces the first ASCII decimal run inside a paragraph so the
    /// numeric-mask matching path is exercised by an expected replacement.
    NumberReplace {
        paragraph_id: String,
        new_number: String,
    },
    ParagraphInsert {
        index: usize,
        paragraph: Paragraph,
    },
    ParagraphDelete {
        paragraph_id: String,
    },
    ParagraphMove {
        paragraph_id: String,
        to_index: usize,
    },
}

/// A half-open scalar range in paragraphs joined by one canonical space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedCanonicalSpan {
    start: usize,
    end: usize,
}

impl ExpectedCanonicalSpan {
    pub fn new(start: usize, end: usize) -> Result<Self> {
        if start >= end {
            return Err(BenchError::InvalidInput(
                "expected canonical spans must be nonempty".to_owned(),
            ));
        }
        Ok(Self { start, end })
    }

    pub const fn start(&self) -> usize {
        self.start
    }

    pub const fn end(&self) -> usize {
        self.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedSemanticChange {
    kind: ChangeKind,
    old_spans: Vec<ExpectedCanonicalSpan>,
    new_spans: Vec<ExpectedCanonicalSpan>,
}

impl ExpectedSemanticChange {
    pub fn new(
        kind: ChangeKind,
        old_spans: Vec<ExpectedCanonicalSpan>,
        new_spans: Vec<ExpectedCanonicalSpan>,
    ) -> Result<Self> {
        let valid = match kind {
            ChangeKind::Replacement | ChangeKind::Move => {
                !old_spans.is_empty() && !new_spans.is_empty()
            }
            ChangeKind::Insertion => old_spans.is_empty() && !new_spans.is_empty(),
            ChangeKind::Deletion => !old_spans.is_empty() && new_spans.is_empty(),
        };
        if !valid {
            return Err(BenchError::InvalidInput(format!(
                "expected {kind:?} span sides do not match its change kind"
            )));
        }
        Ok(Self {
            kind,
            old_spans,
            new_spans,
        })
    }

    pub const fn kind(&self) -> ChangeKind {
        self.kind
    }

    pub fn old_spans(&self) -> &[ExpectedCanonicalSpan] {
        &self.old_spans
    }

    pub fn new_spans(&self) -> &[ExpectedCanonicalSpan] {
        &self.new_spans
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedManifest {
    changes: Vec<ExpectedSemanticChange>,
}

impl ExpectedManifest {
    pub fn new(changes: Vec<ExpectedSemanticChange>) -> Result<Self> {
        if changes.len() > crate::canonical::MAX_PARAGRAPHS {
            return Err(BenchError::InvalidInput(format!(
                "expected manifest exceeds the {}-change limit",
                crate::canonical::MAX_PARAGRAPHS
            )));
        }
        Ok(Self { changes })
    }

    pub fn none() -> Self {
        Self {
            changes: Vec::new(),
        }
    }

    pub fn one(change: ExpectedSemanticChange) -> Self {
        Self {
            changes: vec![change],
        }
    }

    pub fn changes(&self) -> &[ExpectedSemanticChange] {
        &self.changes
    }

    pub fn label(&self) -> String {
        match self.changes.as_slice() {
            [] => "none".to_owned(),
            [change] => change_kind_name(change.kind).to_owned(),
            changes => format!("{}-changes", changes.len()),
        }
    }
}

/// Half-open scalar range of one canonical paragraph inside a render plan's
/// canonical source (`pages` flattened and joined by one space).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalParagraphSpan {
    paragraph_id: String,
    start: usize,
    end: usize,
}

impl CanonicalParagraphSpan {
    pub fn paragraph_id(&self) -> &str {
        &self.paragraph_id
    }

    pub const fn start(&self) -> usize {
        self.start
    }

    pub const fn end(&self) -> usize {
        self.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationPlan {
    old: RenderPlan,
    new: RenderPlan,
    expectation: ExpectedManifest,
    old_paragraphs: Vec<CanonicalParagraphSpan>,
    new_paragraphs: Vec<CanonicalParagraphSpan>,
}

impl MutationPlan {
    /// Single construction path for mutation plans. Derives canonical
    /// paragraph provenance from both documents and fails when either render
    /// plan's flattened lines stop reproducing its canonical paragraphs.
    fn build(
        old_document: &CanonicalDocument,
        new_document: &CanonicalDocument,
        old: RenderPlan,
        new: RenderPlan,
        expectation: ExpectedManifest,
    ) -> Result<Self> {
        let old_paragraphs = canonical_paragraph_spans(old_document)?;
        let new_paragraphs = canonical_paragraph_spans(new_document)?;
        validate_plan_source("old", &old, old_document)?;
        validate_plan_source("new", &new, new_document)?;
        Ok(Self {
            old,
            new,
            expectation,
            old_paragraphs,
            new_paragraphs,
        })
    }

    pub fn old(&self) -> &RenderPlan {
        &self.old
    }

    pub fn new_plan(&self) -> &RenderPlan {
        &self.new
    }

    pub const fn expectation(&self) -> &ExpectedManifest {
        &self.expectation
    }

    pub fn old_paragraphs(&self) -> &[CanonicalParagraphSpan] {
        &self.old_paragraphs
    }

    pub fn new_paragraphs(&self) -> &[CanonicalParagraphSpan] {
        &self.new_paragraphs
    }
}

impl Mutation {
    pub fn apply(&self, document: &CanonicalDocument, line_gap: u16) -> Result<MutationPlan> {
        match self {
            Self::LineWrap {
                paragraph_id,
                after_word,
            } => apply_line_wrap(document, paragraph_id, *after_word, line_gap),
            Self::LineWrapTwice {
                paragraph_id,
                after_words,
            } => apply_line_wrap_twice(document, paragraph_id, *after_words, line_gap),
            Self::PageBreak { before_paragraph } => {
                apply_page_break(document, *before_paragraph, line_gap)
            }
            Self::LineHeightChange { new_line_gap } => {
                apply_line_height_change(document, *new_line_gap, line_gap)
            }
            Self::MarginChange { new_margin } => {
                apply_margin_change(document, *new_margin, line_gap)
            }
            Self::FontSizeChange { new_font_size } => {
                apply_font_size_change(document, *new_font_size, line_gap)
            }
            Self::PageSizeChange {
                new_page_width,
                new_page_height,
            } => apply_page_size_change(document, *new_page_width, *new_page_height, line_gap),
            Self::TextReplace {
                paragraph_id,
                new_text,
            } => apply_text_replace(document, paragraph_id, new_text, line_gap),
            Self::TextInsert {
                paragraph_id,
                at,
                text,
            } => apply_text_insert(document, paragraph_id, *at, text, line_gap),
            Self::TextDelete {
                paragraph_id,
                start,
                end,
            } => apply_text_delete(document, paragraph_id, *start, *end, line_gap),
            Self::NumberReplace {
                paragraph_id,
                new_number,
            } => apply_number_replace(document, paragraph_id, new_number, line_gap),
            Self::ParagraphInsert { index, paragraph } => {
                apply_paragraph_insert(document, *index, paragraph, line_gap)
            }
            Self::ParagraphDelete { paragraph_id } => {
                apply_paragraph_delete(document, paragraph_id, line_gap)
            }
            Self::ParagraphMove {
                paragraph_id,
                to_index,
            } => apply_paragraph_move(document, paragraph_id, *to_index, line_gap),
        }
    }
}

fn apply_line_wrap(
    document: &CanonicalDocument,
    paragraph_id: &str,
    after_word: usize,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    let index = paragraph_index(document, paragraph_id)?;
    let words = document.paragraphs()[index]
        .text()
        .split(' ')
        .collect::<Vec<_>>();
    if after_word == 0 || after_word >= words.len() {
        return Err(BenchError::InvalidInput(format!(
            "line wrap after_word must split paragraph {paragraph_id:?} between words"
        )));
    }

    let old = one_page_plan(document, line_gap)?;
    let mut lines = document_lines(document);
    lines.splice(
        index..=index,
        [words[..after_word].join(" "), words[after_word..].join(" ")],
    );
    MutationPlan::build(
        document,
        document,
        old,
        RenderPlan::new(vec![lines], line_gap)?,
        ExpectedManifest::none(),
    )
}

fn apply_line_wrap_twice(
    document: &CanonicalDocument,
    paragraph_id: &str,
    after_words: [usize; 2],
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    let index = paragraph_index(document, paragraph_id)?;
    let words = document.paragraphs()[index]
        .text()
        .split(' ')
        .collect::<Vec<_>>();
    let [first, second] = after_words;
    if first == 0 || first >= second || second >= words.len() {
        return Err(BenchError::InvalidInput(format!(
            "double line wrap positions must split paragraph {paragraph_id:?} into three nonempty lines"
        )));
    }

    let old = one_page_plan(document, line_gap)?;
    let mut lines = document_lines(document);
    lines.splice(
        index..=index,
        [
            words[..first].join(" "),
            words[first..second].join(" "),
            words[second..].join(" "),
        ],
    );
    MutationPlan::build(
        document,
        document,
        old,
        RenderPlan::new(vec![lines], line_gap)?,
        ExpectedManifest::none(),
    )
}

fn apply_page_break(
    document: &CanonicalDocument,
    before_paragraph: usize,
    line_gap: u16,
) -> Result<MutationPlan> {
    let paragraphs = document.paragraphs();
    if before_paragraph == 0 || before_paragraph >= paragraphs.len() {
        return Err(BenchError::InvalidInput(format!(
            "page break index {before_paragraph} must be between existing paragraphs"
        )));
    }
    let lines = document_lines(document);
    MutationPlan::build(
        document,
        document,
        RenderPlan::new(vec![lines.clone()], line_gap)?,
        RenderPlan::new(
            vec![
                lines[..before_paragraph].to_vec(),
                lines[before_paragraph..].to_vec(),
            ],
            line_gap,
        )?,
        ExpectedManifest::none(),
    )
}

fn apply_line_height_change(
    document: &CanonicalDocument,
    new_line_gap: u16,
    line_gap: u16,
) -> Result<MutationPlan> {
    let old = one_page_plan(document, line_gap)?;
    let new = one_page_plan(document, new_line_gap)?;
    if new_line_gap == line_gap {
        return Err(BenchError::InvalidInput(
            "line height change must alter the rendered line gap".to_owned(),
        ));
    }
    MutationPlan::build(document, document, old, new, ExpectedManifest::none())
}

fn apply_margin_change(
    document: &CanonicalDocument,
    new_margin: u16,
    line_gap: u16,
) -> Result<MutationPlan> {
    if new_margin == DEFAULT_MARGIN {
        return Err(BenchError::InvalidInput(
            "margin change must alter the rendered left margin".to_owned(),
        ));
    }
    MutationPlan::build(
        document,
        document,
        one_page_plan(document, line_gap)?,
        one_page_plan_with_margin(document, line_gap, new_margin)?,
        ExpectedManifest::none(),
    )
}

fn apply_font_size_change(
    document: &CanonicalDocument,
    new_font_size: u16,
    line_gap: u16,
) -> Result<MutationPlan> {
    if new_font_size == DEFAULT_FONT_SIZE {
        return Err(BenchError::InvalidInput(
            "font size change must alter the rendered font size".to_owned(),
        ));
    }
    MutationPlan::build(
        document,
        document,
        one_page_plan(document, line_gap)?,
        one_page_plan_with_font_size(document, line_gap, new_font_size)?,
        ExpectedManifest::none(),
    )
}

fn apply_page_size_change(
    document: &CanonicalDocument,
    new_page_width: u16,
    new_page_height: u16,
    line_gap: u16,
) -> Result<MutationPlan> {
    if new_page_width == DEFAULT_PAGE_WIDTH && new_page_height == DEFAULT_PAGE_HEIGHT {
        return Err(BenchError::InvalidInput(
            "page size change must alter the rendered page dimensions".to_owned(),
        ));
    }
    MutationPlan::build(
        document,
        document,
        one_page_plan(document, line_gap)?,
        one_page_plan_with_page_size(document, line_gap, new_page_width, new_page_height)?,
        ExpectedManifest::none(),
    )
}

fn apply_text_replace(
    document: &CanonicalDocument,
    paragraph_id: &str,
    new_text: &str,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    let index = paragraph_index(document, paragraph_id)?;
    if document.paragraphs()[index].text() == new_text {
        return Err(BenchError::InvalidInput(format!(
            "text replacement for paragraph {paragraph_id:?} must change its text"
        )));
    }
    finish_paragraph_text_change(document, index, new_text.to_owned(), line_gap)
}

fn apply_text_insert(
    document: &CanonicalDocument,
    paragraph_id: &str,
    at: usize,
    text: &str,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    let index = paragraph_index(document, paragraph_id)?;
    let old_text = document.paragraphs()[index].text();
    let char_count = old_text.chars().count();
    if text.is_empty() {
        return Err(BenchError::InvalidInput(
            "text insertion must contain characters".to_owned(),
        ));
    }
    if at > char_count {
        return Err(BenchError::InvalidInput(format!(
            "text insertion offset {at} exceeds paragraph {paragraph_id:?} length {char_count}"
        )));
    }
    let mut chars = old_text.chars().collect::<Vec<_>>();
    for (offset, inserted) in text.chars().enumerate() {
        chars.insert(at + offset, inserted);
    }
    finish_paragraph_text_change(document, index, chars.into_iter().collect(), line_gap)
}

fn apply_text_delete(
    document: &CanonicalDocument,
    paragraph_id: &str,
    start: usize,
    end: usize,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    let index = paragraph_index(document, paragraph_id)?;
    let char_count = document.paragraphs()[index].text().chars().count();
    if start >= end || end > char_count {
        return Err(BenchError::InvalidInput(format!(
            "text deletion range {start}..{end} must be a nonempty in-order range within \
             paragraph {paragraph_id:?} length {char_count}"
        )));
    }
    let new_text = document.paragraphs()[index]
        .text()
        .chars()
        .enumerate()
        .filter(|(position, _)| !(start..end).contains(position))
        .map(|(_, scalar)| scalar)
        .collect();
    finish_paragraph_text_change(document, index, new_text, line_gap)
}

fn apply_number_replace(
    document: &CanonicalDocument,
    paragraph_id: &str,
    new_number: &str,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    if new_number.is_empty() || !new_number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(BenchError::InvalidInput(
            "number replacement must use nonempty ASCII digits".to_owned(),
        ));
    }
    let index = paragraph_index(document, paragraph_id)?;
    let old_text = document.paragraphs()[index].text();
    let bytes = old_text.as_bytes();
    let Some(run_start) = bytes.iter().position(u8::is_ascii_digit) else {
        return Err(BenchError::InvalidInput(format!(
            "paragraph {paragraph_id:?} contains no ASCII digits to replace"
        )));
    };
    // ASCII digits are single-byte, so byte offsets here are char-safe.
    let run_end = run_start
        + bytes[run_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    if &old_text[run_start..run_end] == new_number {
        return Err(BenchError::InvalidInput(format!(
            "number replacement for paragraph {paragraph_id:?} must change its number"
        )));
    }
    let new_text = format!(
        "{}{}{}",
        &old_text[..run_start],
        new_number,
        &old_text[run_end..]
    );
    finish_paragraph_text_change(document, index, new_text, line_gap)
}

fn finish_paragraph_text_change(
    document: &CanonicalDocument,
    index: usize,
    new_text: String,
    line_gap: u16,
) -> Result<MutationPlan> {
    let replacement = Paragraph::new(document.paragraphs()[index].id(), &new_text)?;
    let old_text = document.paragraphs()[index].text();
    if old_text == new_text {
        return Err(BenchError::InvalidInput(
            "paragraph text changes must alter canonical text".to_owned(),
        ));
    }
    let mut paragraphs = document.paragraphs().to_vec();
    paragraphs[index] = replacement;
    let new_document = CanonicalDocument::new(paragraphs)?;
    let old_start = paragraph_global_start(document, index)?;
    let new_start = paragraph_global_start(&new_document, index)?;
    MutationPlan::build(
        document,
        &new_document,
        one_page_plan(document, line_gap)?,
        one_page_plan(&new_document, line_gap)?,
        text_change_manifest(old_text, &new_text, old_start, new_start)?,
    )
}

fn apply_paragraph_insert(
    document: &CanonicalDocument,
    index: usize,
    paragraph: &Paragraph,
    line_gap: u16,
) -> Result<MutationPlan> {
    if index > document.paragraphs().len() {
        return Err(BenchError::InvalidInput(format!(
            "paragraph insertion index {index} exceeds document length {}",
            document.paragraphs().len()
        )));
    }
    let mut paragraphs = document.paragraphs().to_vec();
    paragraphs.insert(index, paragraph.clone());
    let new_document = CanonicalDocument::new(paragraphs)?;
    MutationPlan::build(
        document,
        &new_document,
        one_page_plan(document, line_gap)?,
        one_page_plan(&new_document, line_gap)?,
        ExpectedManifest::one(ExpectedSemanticChange::new(
            ChangeKind::Insertion,
            Vec::new(),
            paragraph_span_variants(&new_document, index)?,
        )?),
    )
}

fn apply_paragraph_delete(
    document: &CanonicalDocument,
    paragraph_id: &str,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    if document.paragraphs().len() == 1 {
        return Err(BenchError::InvalidInput(
            "paragraph deletion cannot produce an empty document".to_owned(),
        ));
    }
    let index = paragraph_index(document, paragraph_id)?;
    let old_spans = paragraph_span_variants(document, index)?;
    let mut paragraphs = document.paragraphs().to_vec();
    paragraphs.remove(index);
    let new_document = CanonicalDocument::new(paragraphs)?;
    MutationPlan::build(
        document,
        &new_document,
        one_page_plan(document, line_gap)?,
        one_page_plan(&new_document, line_gap)?,
        ExpectedManifest::one(ExpectedSemanticChange::new(
            ChangeKind::Deletion,
            old_spans,
            Vec::new(),
        )?),
    )
}

fn apply_paragraph_move(
    document: &CanonicalDocument,
    paragraph_id: &str,
    to_index: usize,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    let from_index = paragraph_index(document, paragraph_id)?;
    if to_index >= document.paragraphs().len() {
        return Err(BenchError::InvalidInput(format!(
            "paragraph move index {to_index} exceeds the final document index {}",
            document.paragraphs().len() - 1
        )));
    }
    if from_index == to_index {
        return Err(BenchError::InvalidInput(format!(
            "paragraph move for {paragraph_id:?} must change its index"
        )));
    }

    let old_spans = paragraph_span_variants(document, from_index)?;
    let mut paragraphs = document.paragraphs().to_vec();
    let paragraph = paragraphs.remove(from_index);
    paragraphs.insert(to_index, paragraph);
    let new_document = CanonicalDocument::new(paragraphs)?;
    MutationPlan::build(
        document,
        &new_document,
        one_page_plan(document, line_gap)?,
        one_page_plan(&new_document, line_gap)?,
        ExpectedManifest::one(ExpectedSemanticChange::new(
            ChangeKind::Move,
            old_spans,
            paragraph_span_variants(&new_document, to_index)?,
        )?),
    )
}

fn paragraph_index(document: &CanonicalDocument, id: &str) -> Result<usize> {
    document
        .paragraphs()
        .iter()
        .position(|paragraph| paragraph.id() == id)
        .ok_or_else(|| BenchError::InvalidInput(format!("unknown paragraph id {id:?}")))
}

fn one_page_plan(document: &CanonicalDocument, line_gap: u16) -> Result<RenderPlan> {
    RenderPlan::new(vec![document_lines(document)], line_gap)
}

fn one_page_plan_with_margin(
    document: &CanonicalDocument,
    line_gap: u16,
    margin: u16,
) -> Result<RenderPlan> {
    RenderPlan::with_margin(vec![document_lines(document)], line_gap, margin)
}

fn one_page_plan_with_font_size(
    document: &CanonicalDocument,
    line_gap: u16,
    font_size: u16,
) -> Result<RenderPlan> {
    RenderPlan::with_font_size(
        vec![document_lines(document)],
        line_gap,
        DEFAULT_MARGIN,
        font_size,
    )
}

fn one_page_plan_with_page_size(
    document: &CanonicalDocument,
    line_gap: u16,
    page_width: u16,
    page_height: u16,
) -> Result<RenderPlan> {
    RenderPlan::with_page_size(
        vec![document_lines(document)],
        line_gap,
        page_width,
        page_height,
    )
}

fn document_lines(document: &CanonicalDocument) -> Vec<String> {
    document
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.text().to_owned())
        .collect()
}

fn text_change_manifest(
    old: &str,
    new: &str,
    old_global_start: usize,
    new_global_start: usize,
) -> Result<ExpectedManifest> {
    let old_chars = old.chars().collect::<Vec<_>>();
    let new_chars = new.chars().collect::<Vec<_>>();
    let prefix = old_chars
        .iter()
        .zip(&new_chars)
        .take_while(|(old, new)| old == new)
        .count();
    let mut suffix = 0;
    while suffix < old_chars.len() - prefix
        && suffix < new_chars.len() - prefix
        && old_chars[old_chars.len() - suffix - 1] == new_chars[new_chars.len() - suffix - 1]
    {
        suffix += 1;
    }

    let old_end = old_chars.len() - suffix;
    let new_end = new_chars.len() - suffix;
    let old_spans = (prefix < old_end)
        .then(|| global_span(old_global_start, prefix, old_end))
        .transpose()?
        .into_iter()
        .collect::<Vec<_>>();
    let new_spans = (prefix < new_end)
        .then(|| global_span(new_global_start, prefix, new_end))
        .transpose()?
        .into_iter()
        .collect::<Vec<_>>();
    let kind = match (old_spans.is_empty(), new_spans.is_empty()) {
        (false, false) => ChangeKind::Replacement,
        (true, false) => ChangeKind::Insertion,
        (false, true) => ChangeKind::Deletion,
        (true, true) => {
            return Err(BenchError::InvalidInput(
                "text replacement must change canonical text".to_owned(),
            ));
        }
    };
    Ok(ExpectedManifest::one(ExpectedSemanticChange::new(
        kind, old_spans, new_spans,
    )?))
}

fn paragraph_span_variants(
    document: &CanonicalDocument,
    index: usize,
) -> Result<Vec<ExpectedCanonicalSpan>> {
    let paragraph = document.paragraphs().get(index).ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "paragraph index {index} exceeds document length {}",
            document.paragraphs().len()
        ))
    })?;
    let start = paragraph_global_start(document, index)?;
    let exact = global_span(start, 0, paragraph.text().chars().count())?;
    let mut spans = vec![exact];
    if index > 0 {
        spans.push(ExpectedCanonicalSpan::new(exact.start() - 1, exact.end())?);
    }
    if index + 1 < document.paragraphs().len() {
        let end = exact.end().checked_add(1).ok_or_else(|| {
            BenchError::InvalidInput("canonical separator span overflowed".to_owned())
        })?;
        spans.push(ExpectedCanonicalSpan::new(exact.start(), end)?);
    }
    Ok(spans)
}

fn global_span(
    global_start: usize,
    local_start: usize,
    local_end: usize,
) -> Result<ExpectedCanonicalSpan> {
    let start = global_start
        .checked_add(local_start)
        .ok_or_else(|| BenchError::InvalidInput("canonical span start overflowed".to_owned()))?;
    let end = global_start
        .checked_add(local_end)
        .ok_or_else(|| BenchError::InvalidInput("canonical span end overflowed".to_owned()))?;
    ExpectedCanonicalSpan::new(start, end)
}

fn paragraph_global_start(document: &CanonicalDocument, index: usize) -> Result<usize> {
    let spans = canonical_paragraph_spans(document)?;
    let span = spans.get(index).ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "paragraph index {index} exceeds document length {}",
            document.paragraphs().len()
        ))
    })?;
    Ok(span.start())
}

/// Scalar ranges of every paragraph inside the canonical source
/// `join(paragraph texts, " ")`, which every render plan must reproduce.
fn canonical_paragraph_spans(document: &CanonicalDocument) -> Result<Vec<CanonicalParagraphSpan>> {
    let mut spans = Vec::with_capacity(document.paragraphs().len());
    let mut start = 0_usize;
    for (index, paragraph) in document.paragraphs().iter().enumerate() {
        let end = start
            .checked_add(paragraph.text().chars().count())
            .ok_or_else(|| {
                BenchError::InvalidInput(format!(
                    "canonical paragraph {:?} span offsets overflowed",
                    paragraph.id()
                ))
            })?;
        spans.push(CanonicalParagraphSpan {
            paragraph_id: paragraph.id().to_owned(),
            start,
            end,
        });
        if index + 1 < document.paragraphs().len() {
            start = end.checked_add(1).ok_or_else(|| {
                BenchError::InvalidInput(format!(
                    "canonical separator after paragraph {:?} overflowed",
                    paragraph.id()
                ))
            })?;
        }
    }
    Ok(spans)
}

/// The render plan's flattened lines joined by one space must exactly
/// reproduce the canonical document paragraphs joined by one space, so the
/// derived paragraph spans stay valid against what renderers actually emit.
fn validate_plan_source(side: &str, plan: &RenderPlan, document: &CanonicalDocument) -> Result<()> {
    let rendered = join_rendered_lines(plan);
    let expected = document
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.text())
        .collect::<Vec<_>>()
        .join(" ");
    if rendered != expected {
        return Err(BenchError::InvalidInput(format!(
            "{side} render-plan canonical source does not match its document (rendered {} scalars, expected {})",
            rendered.chars().count(),
            expected.chars().count()
        )));
    }
    Ok(())
}

fn join_rendered_lines(plan: &RenderPlan) -> String {
    plan.pages()
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::built_in_cases;

    fn test_document(paragraphs: &[(&str, &str)]) -> CanonicalDocument {
        let paragraphs = paragraphs
            .iter()
            .map(|(id, text)| Paragraph::new(*id, *text).expect("test paragraph is valid"))
            .collect::<Vec<_>>();
        CanonicalDocument::new(paragraphs).expect("test document is valid")
    }

    /// Spans must tile the rendered canonical source with single-space gaps,
    /// which keeps them directly comparable with evaluator global ranges.
    fn assert_spans_tile_source(plan: &RenderPlan, spans: &[CanonicalParagraphSpan]) {
        let rendered_scalar_count = join_rendered_lines(plan).chars().count();
        let mut expected_start = 0_usize;
        for span in spans {
            assert_eq!(span.start(), expected_start);
            assert!(span.start() < span.end());
            expected_start = span.end() + 1;
        }
        assert_eq!(
            spans.last().map_or(0, |span| span.end()),
            rendered_scalar_count
        );
    }

    #[test]
    fn built_in_case_plans_reproduce_their_canonical_paragraph_spans() {
        for case in built_in_cases().expect("built-in cases are valid") {
            let plan = case.plan();
            assert_spans_tile_source(plan.old(), plan.old_paragraphs());
            assert_spans_tile_source(plan.new_plan(), plan.new_paragraphs());
        }
    }

    #[test]
    fn text_replacement_shifts_later_paragraph_spans() {
        let document = test_document(&[
            ("intro", "first paragraph"),
            ("metrics", "uptime 99"),
            ("closing", "final paragraph"),
        ]);
        let plan = Mutation::TextReplace {
            paragraph_id: "metrics".to_owned(),
            new_text: "uptime 99 point 9".to_owned(),
        }
        .apply(&document, 16)
        .expect("text replacement applies");

        let old_spans = plan.old_paragraphs();
        let new_spans = plan.new_paragraphs();
        assert_eq!(
            old_spans
                .iter()
                .map(|span| span.paragraph_id())
                .collect::<Vec<_>>(),
            vec!["intro", "metrics", "closing"]
        );
        assert_eq!(
            old_spans[1].end() - old_spans[1].start(),
            "uptime 99".chars().count()
        );
        assert_eq!(
            new_spans[1].end() - new_spans[1].start(),
            "uptime 99 point 9".chars().count()
        );
        let shift = new_spans[1].end() - old_spans[1].end();
        assert_eq!(new_spans[2].start(), old_spans[2].start() + shift);
        assert_eq!(new_spans[2].paragraph_id(), "closing");
    }
}
