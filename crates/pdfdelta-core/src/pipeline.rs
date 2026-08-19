use crate::{
    Error, Result,
    alignment::{
        AlignmentOptions, InvertedIndexCandidateGenerator, align_ordered, build_block_features,
        estimate_ngram_token_elements, validate_ngram_size,
    },
    diff::{
        Comparison, DiffOptions, compare_aligned, enforce_diff_raw_token_budget,
        enforce_diff_token_budget, validate_diff_token_budget,
    },
    layout::{BlockOptions, LineOptions, reconstruct_blocks, reconstruct_lines},
    model::{Document, Glyph, TextRenderMode},
    normalize::{BlockText, normalize_blocks},
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

pub fn compare_glyph_documents(
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

fn enforce_ngram_token_element_budget(
    old: &[BlockText],
    new: &[BlockText],
    options: PipelineOptions,
) -> Result<()> {
    validate_ngram_size(options.ngram_size)?;
    if options.max_ngram_token_elements == 0 {
        return Err(Error::InvalidConfiguration(
            "pipeline max_ngram_token_elements must be greater than zero".to_owned(),
        ));
    }
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

fn enforce_pre_layout_token_budget(
    old: &Document<Glyph>,
    new: &Document<Glyph>,
    options: DiffOptions,
) -> Result<()> {
    validate_diff_token_budget(options)?;
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
