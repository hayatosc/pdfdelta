use crate::{
    Error, Result,
    alignment::{
        AlignmentOptions, InvertedIndexCandidateGenerator, align_ordered, build_block_features,
        estimate_ngram_token_elements, validate_alignment_options, validate_ngram_size,
    },
    diff::{
        Comparison, DiffOptions, compare_aligned, enforce_diff_raw_token_budget,
        enforce_diff_token_budget, validate_diff_options,
    },
    layout::{
        BlockOptions, LineOptions, reconstruct_blocks, reconstruct_lines, validate_block_options,
        validate_line_options,
    },
    model::{Document, Glyph, TextRenderMode},
    normalize::{BlockText, normalize_blocks},
    report::{DocumentSide, ExtractionIssueRecord, ExtractionStatus},
    source::ExtractionOutcome,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PipelineOptions {
    pub line: LineOptions,
    pub block: BlockOptions,
    pub ngram_size: usize,
    pub max_ngram_token_elements: usize,
    pub alignment: AlignmentOptions,
    pub diff: DiffOptions,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            line: LineOptions::default(),
            block: BlockOptions::default(),
            ngram_size: 3,
            max_ngram_token_elements: 4_000_000,
            alignment: AlignmentOptions::default(),
            diff: DiffOptions::default(),
        }
    }
}

impl PipelineOptions {
    fn validate(self) -> Result<Self> {
        validate_line_options(self.line)?;
        validate_block_options(self.block)?;
        validate_ngram_size(self.ngram_size)?;
        validate_ngram_token_element_limit(self.max_ngram_token_elements)?;
        validate_alignment_options(self.alignment)?;
        validate_diff_options(self.diff)?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComparisonOutcome {
    pub comparison: Comparison,
    pub extraction: ExtractionStatus,
}

pub fn compare_extraction_outcomes(
    old: ExtractionOutcome,
    new: ExtractionOutcome,
    options: PipelineOptions,
) -> Result<ComparisonOutcome> {
    let options = options.validate()?;
    let old_complete = old.is_complete();
    let new_complete = new.is_complete();
    let (old_document, old_issues) = old.into_parts();
    let (new_document, new_issues) = new.into_parts();
    if old_complete && new_complete {
        return Ok(ComparisonOutcome {
            comparison: compare_validated_glyph_documents(&old_document, &new_document, options)?,
            extraction: ExtractionStatus::complete(),
        });
    }

    let old_tokens = painting_raw_token_lower_bound(&old_document, options.diff.max_tokens)?;
    let new_tokens = painting_raw_token_lower_bound(&new_document, options.diff.max_tokens)?;
    enforce_diff_raw_token_budget(old_tokens, new_tokens, options.diff)?;
    let issues = old_issues
        .into_iter()
        .map(|issue| ExtractionIssueRecord::from_issue(DocumentSide::Old, issue))
        .chain(
            new_issues
                .into_iter()
                .map(|issue| ExtractionIssueRecord::from_issue(DocumentSide::New, issue)),
        )
        .collect();

    // deliberate: Any extraction gap suppresses the whole diff until region-aware alignment can
    // exclude only affected pages while proving neighboring extracted evidence safe to compare.
    Ok(ComparisonOutcome {
        comparison: Comparison {
            changes: Vec::new(),
            formatting_changes: Vec::new(),
            unresolved_regions: Vec::new(),
            old_coverage: conservative_coverage(old_tokens, old_complete),
            new_coverage: conservative_coverage(new_tokens, new_complete),
        },
        extraction: ExtractionStatus {
            old_complete,
            new_complete,
            issues,
        },
    })
}

pub fn compare_glyph_documents(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
) -> Result<Comparison> {
    let options = options.validate()?;
    compare_validated_glyph_documents(old, new, options)
}

fn compare_validated_glyph_documents(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: PipelineOptions,
) -> Result<Comparison> {
    enforce_pre_layout_token_budget(old, new, options.diff)?;
    let old = prepare(old, options)?;
    let new = prepare(new, options)?;
    enforce_diff_token_budget(&old, &new, options.diff)?;
    enforce_ngram_token_element_budget(&old, &new, options)?;
    let old_features = build_block_features(&old, options.ngram_size)?;
    let new_features = build_block_features(&new, options.ngram_size)?;
    let candidates = InvertedIndexCandidateGenerator::new(&new_features)?;
    let alignment = align_ordered(&old_features, &new_features, &candidates, options.alignment)?;
    compare_aligned(&old, &new, &alignment, options.diff)
}

fn conservative_coverage(total_tokens: usize, extraction_complete: bool) -> crate::diff::Coverage {
    crate::diff::Coverage {
        resolved_tokens: 0,
        total_tokens,
        ratio: extraction_complete.then_some(if total_tokens == 0 { 1.0 } else { 0.0 }),
    }
}

fn enforce_ngram_token_element_budget(
    old: &[BlockText],
    new: &[BlockText],
    options: PipelineOptions,
) -> Result<()> {
    validate_ngram_size(options.ngram_size)?;
    validate_ngram_token_element_limit(options.max_ngram_token_elements)?;
    let old_elements =
        estimate_ngram_token_elements(old, options.ngram_size, options.max_ngram_token_elements)?;
    let new_elements =
        estimate_ngram_token_elements(new, options.ngram_size, options.max_ngram_token_elements)?;
    let total = old_elements
        .checked_add(new_elements)
        .ok_or(Error::LimitExceeded {
            resource: "alignment n-gram token elements",
            limit: options.max_ngram_token_elements,
        })?;
    if total > options.max_ngram_token_elements {
        return Err(Error::LimitExceeded {
            resource: "alignment n-gram token elements",
            limit: options.max_ngram_token_elements,
        });
    }
    Ok(())
}

fn validate_ngram_token_element_limit(limit: usize) -> Result<()> {
    if limit == 0 {
        return Err(Error::InvalidConfiguration(
            "pipeline max_ngram_token_elements must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

fn enforce_pre_layout_token_budget(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: DiffOptions,
) -> Result<()> {
    validate_diff_options(options)?;
    let old_tokens = painting_raw_token_lower_bound(old, options.max_tokens)?;
    let new_tokens = painting_raw_token_lower_bound(new, options.max_tokens)?;
    enforce_diff_raw_token_budget(old_tokens, new_tokens, options)
}

fn painting_raw_token_lower_bound(document: &Document<Glyph>, limit: usize) -> Result<usize> {
    let mut tokens = 0_usize;
    for glyph in document
        .items()
        .iter()
        .filter(|glyph| is_painting(glyph.render_mode))
    {
        let glyph_tokens = match &glyph.text {
            crate::model::DecodedText::Mapped(text) => text.chars().count(),
            crate::model::DecodedText::Unmapped { .. } => 1,
        };
        tokens = tokens
            .checked_add(glyph_tokens)
            .ok_or(crate::Error::LimitExceeded {
                resource: "diff raw evidence tokens",
                limit,
            })?;
    }
    Ok(tokens)
}

fn prepare(document: &Document<Glyph>, options: PipelineOptions) -> Result<Vec<BlockText>> {
    let document = Document::new(
        document
            .items()
            .iter()
            .filter(|glyph| is_painting(glyph.render_mode))
            .cloned()
            .collect(),
    );
    let lines = reconstruct_lines(&document, options.line)?;
    let blocks = reconstruct_blocks(&document, &lines, options.block)?;
    normalize_blocks(&document, &lines, &blocks)
}

fn is_painting(mode: TextRenderMode) -> bool {
    matches!(
        mode,
        TextRenderMode::Fill
            | TextRenderMode::Stroke
            | TextRenderMode::FillAndStroke
            | TextRenderMode::FillAndClip
            | TextRenderMode::StrokeAndClip
            | TextRenderMode::FillStrokeAndClip
    )
}
