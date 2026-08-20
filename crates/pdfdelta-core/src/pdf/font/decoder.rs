use crate::{
    Result,
    pdf::{ParsedPdf, PdfObject},
};

use super::{
    UnicodeMapping,
    cmap::CMapLimits,
    simple::{LoadedSimpleFont, SimpleFontDecoder},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct FontDecoderLimits {
    pub(crate) max_indirections: usize,
    pub(crate) max_width_entries: usize,
    pub(crate) max_to_unicode_bytes: usize,
    pub(crate) cmap: CMapLimits,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DecodedGlyph {
    pub(crate) raw_code: Vec<u8>,
    pub(crate) mapping: UnicodeMapping,
    pub(crate) glyph_id: u16,
    pub(crate) width_1000_em: f64,
}

#[derive(Clone, Debug)]
pub(crate) enum FontDecoder {
    Simple(SimpleFontDecoder),
}

pub(crate) struct LoadedFont {
    pub(crate) decoder: FontDecoder,
    pub(crate) decoded_cmap_bytes: usize,
}

impl FontDecoder {
    pub(crate) fn load(
        pdf: &dyn ParsedPdf,
        font: &PdfObject,
        limits: FontDecoderLimits,
    ) -> Result<LoadedFont> {
        let LoadedSimpleFont {
            decoder,
            decoded_to_unicode_bytes,
        } = SimpleFontDecoder::load(pdf, font, limits)?;
        Ok(LoadedFont {
            decoder: Self::Simple(decoder),
            decoded_cmap_bytes: decoded_to_unicode_bytes,
        })
    }

    pub(crate) fn decode(
        &self,
        input: &[u8],
        max_output_glyphs: usize,
        max_mapped_text_bytes: usize,
    ) -> Result<Vec<DecodedGlyph>> {
        match self {
            Self::Simple(decoder) => {
                decoder.decode(input, max_output_glyphs, max_mapped_text_bytes)
            }
        }
    }

    pub(crate) fn cmap_entry_count(&self) -> usize {
        match self {
            Self::Simple(decoder) => decoder.cmap_entry_count(),
        }
    }

    pub(crate) fn ascent_1000_em(&self) -> f64 {
        match self {
            Self::Simple(decoder) => decoder.ascent_1000_em(),
        }
    }

    pub(crate) fn descent_1000_em(&self) -> f64 {
        match self {
            Self::Simple(decoder) => decoder.descent_1000_em(),
        }
    }
}
