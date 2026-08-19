use pdfdelta_core::diff::ChangeKind;

use crate::{
    BenchError, Result,
    canonical::{CanonicalDocument, Paragraph, validate_paragraph_id, validate_text},
};

pub const MIN_LINE_GAP: u16 = 8;
pub const MAX_LINE_GAP: u16 = 72;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderPlan {
    pages: Vec<Vec<String>>,
    line_gap: u16,
}

impl RenderPlan {
    pub fn new(pages: Vec<Vec<String>>, line_gap: u16) -> Result<Self> {
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
        Ok(Self { pages, line_gap })
    }

    pub fn pages(&self) -> &[Vec<String>] {
        &self.pages
    }

    pub const fn line_gap(&self) -> u16 {
        self.line_gap
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    LineWrap {
        paragraph_id: String,
        after_word: usize,
    },
    PageBreak {
        before_paragraph: usize,
    },
    TextReplace {
        paragraph_id: String,
        new_text: String,
    },
    ParagraphInsert {
        index: usize,
        paragraph: Paragraph,
    },
    ParagraphDelete {
        paragraph_id: String,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationPlan {
    old: RenderPlan,
    new: RenderPlan,
    expectation: ExpectedManifest,
}

impl MutationPlan {
    pub fn old(&self) -> &RenderPlan {
        &self.old
    }

    pub fn new_plan(&self) -> &RenderPlan {
        &self.new
    }

    pub const fn expectation(&self) -> &ExpectedManifest {
        &self.expectation
    }
}

impl Mutation {
    pub fn apply(&self, document: &CanonicalDocument, line_gap: u16) -> Result<MutationPlan> {
        match self {
            Self::LineWrap {
                paragraph_id,
                after_word,
            } => apply_line_wrap(document, paragraph_id, *after_word, line_gap),
            Self::PageBreak { before_paragraph } => {
                apply_page_break(document, *before_paragraph, line_gap)
            }
            Self::TextReplace {
                paragraph_id,
                new_text,
            } => apply_text_replace(document, paragraph_id, new_text, line_gap),
            Self::ParagraphInsert { index, paragraph } => {
                apply_paragraph_insert(document, *index, paragraph, line_gap)
            }
            Self::ParagraphDelete { paragraph_id } => {
                apply_paragraph_delete(document, paragraph_id, line_gap)
            }
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
    Ok(MutationPlan {
        old,
        new: RenderPlan::new(vec![lines], line_gap)?,
        expectation: ExpectedManifest::none(),
    })
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
    Ok(MutationPlan {
        old: RenderPlan::new(vec![lines.clone()], line_gap)?,
        new: RenderPlan::new(
            vec![
                lines[..before_paragraph].to_vec(),
                lines[before_paragraph..].to_vec(),
            ],
            line_gap,
        )?,
        expectation: ExpectedManifest::none(),
    })
}

fn apply_text_replace(
    document: &CanonicalDocument,
    paragraph_id: &str,
    new_text: &str,
    line_gap: u16,
) -> Result<MutationPlan> {
    validate_paragraph_id(paragraph_id)?;
    let index = paragraph_index(document, paragraph_id)?;
    let replacement = Paragraph::new(paragraph_id, new_text)?;
    let old_text = document.paragraphs()[index].text();
    if old_text == new_text {
        return Err(BenchError::InvalidInput(format!(
            "text replacement for paragraph {paragraph_id:?} must change its text"
        )));
    }
    let mut paragraphs = document.paragraphs().to_vec();
    paragraphs[index] = replacement;
    let new_document = CanonicalDocument::new(paragraphs)?;
    let old_start = paragraph_global_start(document, index)?;
    let new_start = paragraph_global_start(&new_document, index)?;
    Ok(MutationPlan {
        old: one_page_plan(document, line_gap)?,
        new: one_page_plan(&new_document, line_gap)?,
        expectation: text_change_manifest(old_text, new_text, old_start, new_start)?,
    })
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
    Ok(MutationPlan {
        old: one_page_plan(document, line_gap)?,
        new: one_page_plan(&new_document, line_gap)?,
        expectation: ExpectedManifest::one(ExpectedSemanticChange::new(
            ChangeKind::Insertion,
            Vec::new(),
            paragraph_span_variants(&new_document, index)?,
        )?),
    })
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
    Ok(MutationPlan {
        old: one_page_plan(document, line_gap)?,
        new: one_page_plan(&new_document, line_gap)?,
        expectation: ExpectedManifest::one(ExpectedSemanticChange::new(
            ChangeKind::Deletion,
            old_spans,
            Vec::new(),
        )?),
    })
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
    let preceding = document.paragraphs().get(..index).ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "paragraph index {index} exceeds document length {}",
            document.paragraphs().len()
        ))
    })?;
    preceding
        .iter()
        .try_fold(0_usize, |offset, paragraph| {
            offset
                .checked_add(paragraph.text().chars().count())
                .and_then(|offset| offset.checked_add(1))
        })
        .ok_or_else(|| BenchError::InvalidInput("canonical document span overflowed".to_owned()))
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}
