use crate::{
    Result,
    pdf::{ParsedPdf, PdfObject},
};

use super::{
    UnicodeMapping,
    cmap::CMapLimits,
    common::{FontIdentitySource, resolve_object},
    composite::{CompositeFontDecoder, LoadedCompositeFont, WidthTables},
    simple::{LoadedSimpleFont, SimpleFontDecoder},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct FontDecoderLimits {
    pub(crate) max_indirections: usize,
    pub(crate) max_simple_width_entries: usize,
    pub(crate) max_cid_width_entries: usize,
    pub(crate) max_decoded_font_bytes: usize,
    pub(crate) cmap: CMapLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WritingMode {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VerticalGlyphMetrics {
    pub(crate) displacement_y_1000_em: f64,
    pub(crate) origin_x_1000_em: f64,
    pub(crate) origin_y_1000_em: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DecodedGlyph {
    pub(crate) raw_code: Vec<u8>,
    pub(crate) mapping: UnicodeMapping,
    pub(crate) glyph_id: u16,
    pub(crate) width_1000_em: f64,
    pub(crate) vertical: Option<VerticalGlyphMetrics>,
}

#[derive(Clone, Debug)]
pub(crate) enum FontDecoder {
    Composite(CompositeFontDecoder),
    Simple(SimpleFontDecoder),
}

pub(crate) struct LoadedFont {
    pub(crate) decoder: FontDecoder,
    pub(crate) identity_source: Option<FontIdentitySource>,
    pub(crate) external_base_font: Option<Vec<u8>>,
    pub(crate) decoded_font_bytes: usize,
    pub(crate) cid_width_entries: usize,
}

/// Shares immutable width tables within exactly one parsed PDF. The extraction
/// owner charges each load's newly retained entries to its document-wide limit.
pub(crate) struct FontDecoderCache<'a> {
    pdf: &'a dyn ParsedPdf,
    widths: WidthTables,
}

impl<'a> FontDecoderCache<'a> {
    pub(crate) fn new(pdf: &'a dyn ParsedPdf) -> Self {
        Self {
            pdf,
            widths: WidthTables::new(),
        }
    }

    pub(crate) fn load(
        &mut self,
        font: &PdfObject,
        limits: FontDecoderLimits,
    ) -> Result<LoadedFont> {
        let pdf = self.pdf;
        let resolved = resolve_object(pdf, font.clone(), limits.max_indirections)?;
        if let PdfObject::Dictionary(dictionary) = &resolved
            && matches!(dictionary.get(b"Subtype".as_slice()), Some(PdfObject::Name(name)) if name.as_slice() == b"Type0")
        {
            let LoadedCompositeFont {
                decoder,
                identity_source,
                external_identity_allowed,
                decoded_font_bytes,
                cid_width_entries,
            } = CompositeFontDecoder::load_shared(pdf, dictionary, limits, &mut self.widths)?;
            let external_base_font = if identity_source.is_none() && external_identity_allowed {
                dictionary
                    .get(b"BaseFont".as_slice())
                    .and_then(|value| match value {
                        PdfObject::Name(name) => Some(name.clone()),
                        _ => None,
                    })
            } else {
                None
            };
            return Ok(LoadedFont {
                decoder: FontDecoder::Composite(decoder),
                identity_source,
                external_base_font,
                decoded_font_bytes,
                cid_width_entries,
            });
        }
        let LoadedSimpleFont {
            decoder,
            identity_source,
            decoded_font_bytes,
        } = SimpleFontDecoder::load(pdf, font, limits)?;
        Ok(LoadedFont {
            decoder: FontDecoder::Simple(decoder),
            identity_source,
            external_base_font: None,
            decoded_font_bytes,
            cid_width_entries: 0,
        })
    }
}

impl FontDecoder {
    #[cfg(feature = "fuzzing")]
    pub(crate) fn load(
        pdf: &dyn ParsedPdf,
        font: &PdfObject,
        limits: FontDecoderLimits,
    ) -> Result<LoadedFont> {
        FontDecoderCache::new(pdf).load(font, limits)
    }

    pub(crate) fn decode(
        &self,
        input: &[u8],
        max_output_glyphs: usize,
        max_mapped_text_bytes: usize,
    ) -> Result<Vec<DecodedGlyph>> {
        match self {
            Self::Composite(decoder) => {
                decoder.decode(input, max_output_glyphs, max_mapped_text_bytes)
            }
            Self::Simple(decoder) => {
                decoder.decode(input, max_output_glyphs, max_mapped_text_bytes)
            }
        }
    }

    pub(crate) fn cmap_entry_count(&self) -> usize {
        match self {
            Self::Composite(decoder) => decoder.cmap_entry_count(),
            Self::Simple(decoder) => decoder.cmap_entry_count(),
        }
    }

    pub(crate) fn writing_mode(&self) -> WritingMode {
        match self {
            Self::Composite(decoder) => decoder.writing_mode(),
            Self::Simple(_) => WritingMode::Horizontal,
        }
    }

    pub(crate) fn ascent_1000_em(&self) -> f64 {
        match self {
            Self::Composite(decoder) => decoder.ascent_1000_em(),
            Self::Simple(decoder) => decoder.ascent_1000_em(),
        }
    }

    pub(crate) fn descent_1000_em(&self) -> f64 {
        match self {
            Self::Composite(decoder) => decoder.descent_1000_em(),
            Self::Simple(decoder) => decoder.descent_1000_em(),
        }
    }
}
