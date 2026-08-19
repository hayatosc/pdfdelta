use std::sync::Arc;

use crate::{
    Result,
    model::{Document, Glyph},
    pdf::{ParseLimits, ParsedPdf, PdfParser},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_glyphs: usize,
    pub max_form_depth: usize,
    pub max_nesting_depth: usize,
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
