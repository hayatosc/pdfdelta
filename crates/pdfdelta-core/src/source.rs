use std::sync::Arc;

use crate::{
    Result,
    model::{Document, Glyph},
    pdf::{ParseLimits, ParsedPdf, PdfParser},
};

mod content_stream;

pub use content_stream::ContentStreamGlyphExtractor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_glyphs: usize,
    pub max_form_depth: usize,
    pub max_nesting_depth: usize,
    pub max_operators: usize,
    pub max_stream_invocations: usize,
    pub max_total_decoded_bytes: usize,
    pub max_operand_stack: usize,
    pub max_operand_nodes: usize,
    pub max_fonts: usize,
    pub max_cmap_entries: usize,
    pub max_string_bytes: usize,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_glyphs: 5_000_000,
            max_form_depth: 32,
            max_nesting_depth: 64,
            max_operators: 5_000_000,
            max_stream_invocations: 5_000_000,
            max_total_decoded_bytes: 512 * 1024 * 1024,
            max_operand_stack: 4_096,
            max_operand_nodes: 1_000_000,
            max_fonts: 100_000,
            max_cmap_entries: 1_000_000,
            max_string_bytes: 64 * 1024 * 1024,
        }
    }
}

pub trait GlyphExtractor: Send + Sync {
    fn extract(&self, pdf: &dyn ParsedPdf, limits: ExtractionLimits) -> Result<Document<Glyph>>;
}

pub struct ParserBackedGlyphSource<P, E> {
    parser: P,
    extractor: E,
}

impl<P, E> ParserBackedGlyphSource<P, E>
where
    P: PdfParser,
    E: GlyphExtractor,
{
    pub fn new(parser: P, extractor: E) -> Self {
        Self { parser, extractor }
    }

    pub fn extract(
        &self,
        pdf: Arc<[u8]>,
        parse_limits: ParseLimits,
        extraction_limits: ExtractionLimits,
    ) -> Result<Document<Glyph>> {
        let parsed = self.parser.parse(pdf, parse_limits)?;
        self.extractor.extract(parsed.as_ref(), extraction_limits)
    }
}
