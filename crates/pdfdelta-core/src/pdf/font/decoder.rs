use crate::{
    Result,
    pdf::{ParsedPdf, PdfObject},
};

use super::{
    UnicodeMapping,
    cmap::CMapLimits,
    common::{FontIdentitySource, resolve_object},
    composite::{CompositeFontDecoder, LoadedCompositeFont},
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DecodedGlyph {
    pub(crate) raw_code: Vec<u8>,
    pub(crate) mapping: UnicodeMapping,
    pub(crate) glyph_id: u16,
    pub(crate) width_1000_em: f64,
}

#[derive(Clone, Debug)]
pub(crate) enum FontDecoder {
    Composite(CompositeFontDecoder),
    Simple(SimpleFontDecoder),
}

pub(crate) struct LoadedFont {
    pub(crate) decoder: FontDecoder,
    pub(crate) identity_source: Option<FontIdentitySource>,
    pub(crate) decoded_font_bytes: usize,
    pub(crate) cid_width_entries: usize,
}

impl FontDecoder {
    pub(crate) fn load(
        pdf: &dyn ParsedPdf,
        font: &PdfObject,
        limits: FontDecoderLimits,
    ) -> Result<LoadedFont> {
        let resolved = resolve_object(pdf, font.clone(), limits.max_indirections)?;
        if let PdfObject::Dictionary(dictionary) = &resolved
            && matches!(dictionary.get(b"Subtype".as_slice()), Some(PdfObject::Name(name)) if name.as_slice() == b"Type0")
        {
            let LoadedCompositeFont {
                decoder,
                identity_source,
                decoded_font_bytes,
                cid_width_entries,
            } = CompositeFontDecoder::load(pdf, dictionary, limits)?;
            return Ok(LoadedFont {
                decoder: Self::Composite(decoder),
                identity_source,
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
            decoder: Self::Simple(decoder),
            identity_source,
            decoded_font_bytes,
            cid_width_entries: 0,
        })
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
