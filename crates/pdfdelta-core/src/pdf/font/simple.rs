use std::collections::BTreeMap;

use crate::{
    Error, Result,
    pdf::{ParsedPdf, PdfDict, PdfObject},
};

use super::cmap::{ToUnicodeCMap, UnicodeMapping};
use super::common::{
    FontIdentityDomain, FontIdentitySource, apply_bbox_vertical_fallback, finite_number,
    load_descriptor_bbox, load_to_unicode, non_negative_number, optional_number,
    resolve_font_identity_source, resolve_object, type3_font_identity_source, unresolved,
};
use super::decoder::{DecodedGlyph, FontDecoderLimits};
use super::metrics;

const MAX_ENCODING_DIFFERENCE_ELEMENTS: usize = 512;

#[rustfmt::skip]
const MAC_ROMAN_UNICODE: [u16; 128] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1,
    0x00E0, 0x00E2, 0x00E4, 0x00E3, 0x00E5, 0x00E7, 0x00E9, 0x00E8,
    0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC,
    0x2020, 0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF,
    0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211,
    0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8,
    0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x0020, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153,
    0x2013, 0x2014, 0x201C, 0x201D, 0x2018, 0x2019, 0x00F7, 0x25CA,
    0x00FF, 0x0178, 0x2044, 0x00A4, 0x2039, 0x203A, 0xFB01, 0xFB02,
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1,
    0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC, 0x00D3, 0x00D4,
    0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC,
    0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];

#[derive(Clone, Debug)]
pub(crate) struct SimpleFontDecoder {
    to_unicode: Option<ToUnicodeCMap>,
    fallback: FallbackEncoding,
    differences: BTreeMap<u8, DifferenceMapping>,
    first_char: u8,
    widths: Option<Vec<f64>>,
    missing_width: f64,
    base14: Option<Base14>,
    type3_identity_glyph_ids: Option<BTreeMap<u8, u16>>,
    ascent: f64,
    descent: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedSimpleFont {
    pub(crate) decoder: SimpleFontDecoder,
    pub(crate) identity_source: Option<FontIdentitySource>,
    pub(crate) decoded_font_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FallbackEncoding {
    Standard,
    WinAnsi,
    MacRoman,
    Symbol,
    ZapfDingbats,
    Unknown,
}

struct LoadedEncoding {
    base: FallbackEncoding,
    differences: BTreeMap<u8, DifferenceMapping>,
    identity_ambiguous: bool,
    standard14_identity_encoding: Option<&'static [u8]>,
    explicit_encoding: bool,
}

struct Type3Metadata {
    char_procs: PdfDict,
    identity_resources: Type3IdentityResources,
    horizontal_scale_1000_em: f64,
    ascent: f64,
    descent: f64,
}

enum Type3IdentityResources {
    Independent,
    Dependent(PdfObject),
    Unavailable,
}

#[derive(Clone, Debug)]
struct DifferenceMapping {
    glyph_name: Vec<u8>,
    unicode: UnicodeMapping,
    metric_scalar: Option<char>,
    missing_type3_procedure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base14 {
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Symbol,
    ZapfDingbats,
}

#[derive(Clone, Copy)]
enum SimpleSubtype {
    Type1,
    MMType1,
    TrueType,
    Type3,
}

impl SimpleFontDecoder {
    pub(crate) fn load(
        pdf: &dyn ParsedPdf,
        font: &PdfObject,
        limits: FontDecoderLimits,
    ) -> Result<LoadedSimpleFont> {
        let font = resolve_object(pdf, font.clone(), limits.max_indirections)?;
        let PdfObject::Dictionary(dictionary) = font else {
            return unresolved("font resource is not a dictionary");
        };
        let subtype = validate_subtype(&dictionary)?;

        let base14 = if matches!(subtype, SimpleSubtype::Type3) {
            None
        } else {
            load_base14(pdf, &dictionary, limits.max_indirections)?
        };
        let (first_char, mut widths) = load_widths(pdf, &dictionary, limits)?;
        let type3 = if matches!(subtype, SimpleSubtype::Type3) {
            Some(validate_type3(
                pdf,
                &dictionary,
                limits,
                first_char,
                widths.as_deref(),
            )?)
        } else {
            None
        };
        let (mut ascent, mut descent, mut missing_width) =
            load_descriptor(pdf, &dictionary, limits.max_indirections, base14)?;
        if let Some(type3) = &type3 {
            ascent = Some(type3.ascent);
            descent = Some(type3.descent);
            if let Some(widths) = &mut widths {
                for width in widths {
                    *width =
                        scale_type3_metric(*width, type3.horizontal_scale_1000_em, "Type 3 width")?;
                }
            }
            missing_width = scale_type3_metric(
                missing_width,
                type3.horizontal_scale_1000_em,
                "Type 3 MissingWidth",
            )?;
        }
        if widths.is_none() && base14.is_none() {
            return unresolved("font without Widths is not a recognized Standard 14 font");
        }
        let ascent =
            ascent.ok_or_else(|| Error::Unresolved("simple font has no ascent metric".into()))?;
        let descent =
            descent.ok_or_else(|| Error::Unresolved("simple font has no descent metric".into()))?;
        let (to_unicode, decoded_to_unicode_bytes) = load_to_unicode(pdf, &dictionary, limits, 1)?;
        let encoding = load_encoding(
            pdf,
            &dictionary,
            limits,
            base14,
            type3.as_ref().map(|type3| &type3.char_procs),
        )?;
        if widths.is_none()
            && base14.is_some()
            && matches!(encoding.base, FallbackEncoding::Unknown)
        {
            return Err(Error::Unsupported(
                "simple font /SymbolSetEncoding requires explicit Widths".into(),
            ));
        }
        validate_difference_metrics(base14, widths.as_deref(), &encoding.differences)?;
        let (identity_source, type3_identity_glyph_ids) = if let Some(type3) = &type3 {
            if encoding.identity_ambiguous
                || matches!(
                    type3.identity_resources,
                    Type3IdentityResources::Unavailable
                )
            {
                (None, None)
            } else if let Some(binding) = type3_font_identity_source(
                pdf,
                &type3.char_procs,
                match &type3.identity_resources {
                    Type3IdentityResources::Dependent(resources) => Some(resources),
                    Type3IdentityResources::Independent | Type3IdentityResources::Unavailable => {
                        None
                    }
                },
                limits.max_indirections,
            )? {
                let glyph_ids = encoding
                    .differences
                    .iter()
                    .filter(|(_, mapping)| !mapping.missing_type3_procedure)
                    .map(|(code, mapping)| {
                        binding
                            .glyph_ids_by_name
                            .get(&mapping.glyph_name)
                            .copied()
                            .map(|glyph_id| (*code, glyph_id))
                            .ok_or_else(|| {
                                Error::Unresolved(
                                    "Type 3 encoding identity references a missing CharProc".into(),
                                )
                            })
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?;
                (Some(binding.source), Some(glyph_ids))
            } else {
                (None, None)
            }
        } else {
            let standard14_source = if let (Some(base14), Some(identity_encoding)) = (
                base14.filter(|_| !matches!(subtype, SimpleSubtype::MMType1)),
                encoding.standard14_identity_encoding,
            ) {
                if has_embedded_font_program(pdf, &dictionary, limits.max_indirections)? {
                    None
                } else {
                    Some(FontIdentitySource::standard14(
                        base14.identity_name(),
                        identity_encoding,
                    ))
                }
            } else {
                None
            };
            let source = if let Some(source) = standard14_source {
                Some(source)
            } else if encoding.explicit_encoding {
                // Simple fonts with an explicit /Encoding dictionary or named encoding
                // map raw byte codes to glyph selectors through that encoding, not directly to glyph IDs
                // in the embedded font program. Unless the code-to-glyph mapping is independently verified,
                // unmapped codes must not receive a stable font identity solely from (font_hash, raw_code).
                None
            } else {
                identity_domain(pdf, &dictionary, limits.max_indirections, subtype)?
                    .map(|domain| {
                        resolve_font_identity_source(
                            pdf,
                            &dictionary,
                            limits.max_indirections,
                            domain,
                        )
                    })
                    .transpose()?
                    .flatten()
            };
            (source, None)
        };

        Ok(LoadedSimpleFont {
            decoder: Self {
                to_unicode,
                fallback: encoding.base,
                differences: encoding.differences,
                first_char,
                widths,
                missing_width,
                base14,
                type3_identity_glyph_ids,
                ascent,
                descent,
            },
            identity_source,
            decoded_font_bytes: decoded_to_unicode_bytes,
        })
    }

    pub(crate) fn decode(
        &self,
        input: &[u8],
        max_output_glyphs: usize,
        max_mapped_text_bytes: usize,
    ) -> Result<Vec<DecodedGlyph>> {
        if input.len() > max_output_glyphs {
            return Err(Error::LimitExceeded {
                resource: "decoded simple-font glyphs",
                limit: max_output_glyphs,
            });
        }
        let mut glyphs = Vec::with_capacity(input.len());
        let mut mapped_text_bytes = 0usize;
        let mut push_glyph = |code, mapping: UnicodeMapping| -> Result<()> {
            if let UnicodeMapping::Mapped(text) = &mapping {
                mapped_text_bytes =
                    mapped_text_bytes
                        .checked_add(text.len())
                        .ok_or(Error::LimitExceeded {
                            resource: "decoded Unicode text bytes",
                            limit: max_mapped_text_bytes,
                        })?;
                if mapped_text_bytes > max_mapped_text_bytes {
                    return Err(Error::LimitExceeded {
                        resource: "decoded Unicode text bytes",
                        limit: max_mapped_text_bytes,
                    });
                }
            }
            glyphs.push(self.decoded_glyph(code, mapping)?);
            Ok(())
        };

        if let Some(cmap) = &self.to_unicode {
            for code in input {
                let source = [*code];
                let mapping = cmap
                    .exact_mapping_entry(&source)
                    .unwrap_or_else(|| self.fallback_mapping(*code));
                push_glyph(*code, mapping)?;
            }
        } else {
            for code in input {
                push_glyph(*code, self.fallback_mapping(*code))?;
            }
        }
        Ok(glyphs)
    }

    pub(crate) fn cmap_entry_count(&self) -> usize {
        self.to_unicode
            .as_ref()
            .map_or(0, ToUnicodeCMap::entry_count)
    }

    pub(crate) fn ascent_1000_em(&self) -> f64 {
        self.ascent
    }

    pub(crate) fn descent_1000_em(&self) -> f64 {
        self.descent
    }

    fn decoded_glyph(&self, code: u8, mapping: UnicodeMapping) -> Result<DecodedGlyph> {
        // A missing procedure affects uses of its code, not unrelated encoding
        // entries. A Unicode declaration cannot supply the absent drawing.
        if self
            .differences
            .get(&code)
            .is_some_and(|entry| entry.missing_type3_procedure)
        {
            return unresolved("Type 3 font code references a missing CharProc");
        }
        let glyph_id = if matches!(mapping, UnicodeMapping::Unmapped) {
            self.type3_identity_glyph_ids
                .as_ref()
                .map(|glyph_ids| {
                    glyph_ids.get(&code).copied().ok_or_else(|| {
                        Error::Unresolved(
                            "Type 3 font code has no unambiguous CharProc identity".into(),
                        )
                    })
                })
                .transpose()?
                .unwrap_or_else(|| u16::from(code))
        } else {
            u16::from(code)
        };
        Ok(DecodedGlyph {
            raw_code: vec![code],
            mapping,
            glyph_id,
            width_1000_em: self.width(code),
            vertical: None,
        })
    }

    fn fallback_mapping(&self, code: u8) -> UnicodeMapping {
        if let Some(difference) = self.differences.get(&code) {
            difference.unicode.clone()
        } else {
            fallback_char(self.fallback, code).map_or(UnicodeMapping::Unmapped, |character| {
                UnicodeMapping::Mapped(character.into())
            })
        }
    }

    fn width(&self, code: u8) -> f64 {
        let Some(widths) = &self.widths else {
            if let Some(difference) = self.differences.get(&code) {
                return difference
                    .metric_scalar
                    .and_then(|scalar| self.base14.and_then(|font| font.glyph_width(scalar)))
                    .unwrap_or(self.missing_width);
            }
            return self
                .base14
                .and_then(|font| font.width(self.fallback, code))
                .unwrap_or(self.missing_width);
        };
        code.checked_sub(self.first_char)
            .and_then(|index| widths.get(usize::from(index)))
            .copied()
            .unwrap_or(self.missing_width)
    }
}

fn validate_difference_metrics(
    base14: Option<Base14>,
    widths: Option<&[f64]>,
    differences: &BTreeMap<u8, DifferenceMapping>,
) -> Result<()> {
    let Some(base14 @ (Base14::Symbol | Base14::ZapfDingbats)) = base14 else {
        return Ok(());
    };
    if widths.is_some() {
        return Ok(());
    }
    if differences.values().any(|difference| {
        difference
            .metric_scalar
            .and_then(|scalar| base14.glyph_width(scalar))
            .is_none()
    }) {
        return Err(Error::Unsupported(
            "Standard 14 Symbol/ZapfDingbats Differences metrics are not supported".into(),
        ));
    }
    Ok(())
}

impl Base14 {
    fn identity_name(self) -> &'static [u8] {
        match self {
            Self::Courier => b"Courier",
            Self::CourierBold => b"Courier-Bold",
            Self::CourierOblique => b"Courier-Oblique",
            Self::CourierBoldOblique => b"Courier-BoldOblique",
            Self::Helvetica => b"Helvetica",
            Self::HelveticaBold => b"Helvetica-Bold",
            Self::HelveticaOblique => b"Helvetica-Oblique",
            Self::HelveticaBoldOblique => b"Helvetica-BoldOblique",
            Self::TimesRoman => b"Times-Roman",
            Self::TimesBold => b"Times-Bold",
            Self::TimesItalic => b"Times-Italic",
            Self::TimesBoldItalic => b"Times-BoldItalic",
            Self::Symbol => b"Symbol",
            Self::ZapfDingbats => b"ZapfDingbats",
        }
    }

    fn width(self, encoding: FallbackEncoding, code: u8) -> Option<f64> {
        if matches!(encoding, FallbackEncoding::MacRoman) {
            return fallback_char(encoding, code).and_then(|scalar| self.glyph_width(scalar));
        }
        let index = usize::from(code);
        let widths = match self {
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                return Some(600.0);
            }
            Self::Helvetica | Self::HelveticaOblique => match encoding {
                FallbackEncoding::WinAnsi => &metrics::HELVETICA_WIN_ANSI,
                _ => &metrics::HELVETICA_STANDARD,
            },
            Self::HelveticaBold | Self::HelveticaBoldOblique => match encoding {
                FallbackEncoding::WinAnsi => &metrics::HELVETICA_BOLD_WIN_ANSI,
                _ => &metrics::HELVETICA_BOLD_STANDARD,
            },
            Self::TimesRoman => match encoding {
                FallbackEncoding::WinAnsi => &metrics::TIMES_ROMAN_WIN_ANSI,
                _ => &metrics::TIMES_ROMAN_STANDARD,
            },
            Self::TimesBold => match encoding {
                FallbackEncoding::WinAnsi => &metrics::TIMES_BOLD_WIN_ANSI,
                _ => &metrics::TIMES_BOLD_STANDARD,
            },
            Self::TimesItalic => match encoding {
                FallbackEncoding::WinAnsi => &metrics::TIMES_ITALIC_WIN_ANSI,
                _ => &metrics::TIMES_ITALIC_STANDARD,
            },
            Self::TimesBoldItalic => match encoding {
                FallbackEncoding::WinAnsi => &metrics::TIMES_BOLD_ITALIC_WIN_ANSI,
                _ => &metrics::TIMES_BOLD_ITALIC_STANDARD,
            },
            Self::Symbol => &metrics::SYMBOL,
            Self::ZapfDingbats => &metrics::ZAPF_DINGBATS,
        };
        Some(f64::from(widths[index]))
    }

    fn glyph_width(self, scalar: char) -> Option<f64> {
        if matches!(
            self,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique
        ) {
            return Some(600.0);
        }
        let (standard, win_ansi) = match self {
            Self::Helvetica | Self::HelveticaOblique => {
                (&metrics::HELVETICA_STANDARD, &metrics::HELVETICA_WIN_ANSI)
            }
            Self::HelveticaBold | Self::HelveticaBoldOblique => (
                &metrics::HELVETICA_BOLD_STANDARD,
                &metrics::HELVETICA_BOLD_WIN_ANSI,
            ),
            Self::TimesRoman => (
                &metrics::TIMES_ROMAN_STANDARD,
                &metrics::TIMES_ROMAN_WIN_ANSI,
            ),
            Self::TimesBold => (&metrics::TIMES_BOLD_STANDARD, &metrics::TIMES_BOLD_WIN_ANSI),
            Self::TimesItalic => (
                &metrics::TIMES_ITALIC_STANDARD,
                &metrics::TIMES_ITALIC_WIN_ANSI,
            ),
            Self::TimesBoldItalic => (
                &metrics::TIMES_BOLD_ITALIC_STANDARD,
                &metrics::TIMES_BOLD_ITALIC_WIN_ANSI,
            ),
            Self::Symbol | Self::ZapfDingbats => return None,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                return None;
            }
        };
        encoded_scalar_width(FallbackEncoding::Standard, standard, scalar)
            .or_else(|| encoded_scalar_width(FallbackEncoding::WinAnsi, win_ansi, scalar))
    }

    fn vertical_metrics(self) -> (f64, f64) {
        match self {
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                (629.0, -157.0)
            }
            Self::Helvetica
            | Self::HelveticaBold
            | Self::HelveticaOblique
            | Self::HelveticaBoldOblique => (718.0, -207.0),
            Self::TimesRoman => (683.0, -217.0),
            Self::TimesBold => (676.0, -205.0),
            Self::TimesItalic => (683.0, -205.0),
            Self::TimesBoldItalic => (699.0, -205.0),
            Self::Symbol => (1010.0, -293.0),
            Self::ZapfDingbats => (820.0, -143.0),
        }
    }
}

fn encoded_scalar_width(
    encoding: FallbackEncoding,
    widths: &[u16; 256],
    scalar: char,
) -> Option<f64> {
    (0..=u8::MAX)
        .position(|code| fallback_char(encoding, code) == Some(scalar))
        .map(|code| f64::from(widths[code]))
}

fn load_base14(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
) -> Result<Option<Base14>> {
    let Some(base_font) = dictionary.get(b"BaseFont".as_slice()) else {
        return Ok(None);
    };
    let base_font = resolve_object(pdf, base_font.clone(), max_indirections)?;
    let name = match base_font {
        PdfObject::Name(name) => name,
        PdfObject::Null => return Ok(None),
        _ => return unresolved("BaseFont is not a name"),
    };
    Ok(match name.as_slice() {
        b"Courier" => Some(Base14::Courier),
        b"Courier-Bold" => Some(Base14::CourierBold),
        b"Courier-Oblique" => Some(Base14::CourierOblique),
        b"Courier-BoldOblique" => Some(Base14::CourierBoldOblique),
        b"Helvetica" => Some(Base14::Helvetica),
        b"Helvetica-Bold" => Some(Base14::HelveticaBold),
        b"Helvetica-Oblique" => Some(Base14::HelveticaOblique),
        b"Helvetica-BoldOblique" => Some(Base14::HelveticaBoldOblique),
        b"Times-Roman" => Some(Base14::TimesRoman),
        b"Times-Bold" => Some(Base14::TimesBold),
        b"Times-Italic" => Some(Base14::TimesItalic),
        b"Times-BoldItalic" => Some(Base14::TimesBoldItalic),
        b"Symbol" => Some(Base14::Symbol),
        b"ZapfDingbats" => Some(Base14::ZapfDingbats),
        _ => None,
    })
}

fn validate_subtype(dictionary: &PdfDict) -> Result<SimpleSubtype> {
    match dictionary.get(b"Subtype".as_slice()) {
        Some(PdfObject::Name(name)) if name.as_slice() == b"Type1" => Ok(SimpleSubtype::Type1),
        // deliberate: MMType1 uses declared encoding and metrics, but cannot claim program
        // identity until the selected variation instance is represented in the identity input.
        Some(PdfObject::Name(name)) if name.as_slice() == b"MMType1" => Ok(SimpleSubtype::MMType1),
        Some(PdfObject::Name(name)) if name.as_slice() == b"TrueType" => {
            Ok(SimpleSubtype::TrueType)
        }
        Some(PdfObject::Name(name)) if name.as_slice() == b"Type3" => Ok(SimpleSubtype::Type3),
        Some(PdfObject::Name(name)) if name.as_slice() == b"Type0" => Err(Error::Unsupported(
            "Type0 font cannot be decoded as a simple font".into(),
        )),
        Some(PdfObject::Name(name)) => Err(Error::Unsupported(format!(
            "font subtype /{} is not supported",
            String::from_utf8_lossy(name)
        ))),
        _ => unresolved("font dictionary has no valid Subtype"),
    }
}

fn identity_domain(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
    subtype: SimpleSubtype,
) -> Result<Option<FontIdentityDomain>> {
    if matches!(subtype, SimpleSubtype::Type3 | SimpleSubtype::MMType1) {
        return Ok(None);
    }
    if matches!(subtype, SimpleSubtype::Type1) {
        let domain = dictionary
            .get(b"FontDescriptor".as_slice())
            .map(|descriptor| resolve_object(pdf, descriptor.clone(), max_indirections))
            .transpose()?
            .and_then(|descriptor| match descriptor {
                PdfObject::Dictionary(descriptor)
                    if descriptor.contains_key(b"FontFile3".as_slice())
                        && !descriptor.contains_key(b"FontFile".as_slice())
                        && !descriptor.contains_key(b"FontFile2".as_slice()) =>
                {
                    Some(FontIdentityDomain::SimpleType1C)
                }
                _ => None,
            })
            .unwrap_or(FontIdentityDomain::SimpleType1BuiltIn);
        return Ok(Some(domain));
    }
    let Some(descriptor) = dictionary.get(b"FontDescriptor".as_slice()) else {
        return Ok(None);
    };
    let descriptor = resolve_object(pdf, descriptor.clone(), max_indirections)?;
    let PdfObject::Dictionary(descriptor) = descriptor else {
        return Ok(None);
    };
    let Some(flags) = descriptor.get(b"Flags".as_slice()) else {
        return Ok(None);
    };
    let flags = resolve_object(pdf, flags.clone(), max_indirections)?;
    let PdfObject::Integer(flags) = flags else {
        return Ok(None);
    };
    let Ok(flags) = u32::try_from(flags) else {
        return Ok(None);
    };
    let symbolic = flags & 4 != 0;
    let nonsymbolic = flags & 32 != 0;
    Ok(match (symbolic, nonsymbolic) {
        (true, false) => Some(FontIdentityDomain::SimpleTrueTypeSymbolic),
        (false, true) => Some(FontIdentityDomain::SimpleTrueTypeNonsymbolic),
        _ => None,
    })
}

fn validate_type3(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
    first_char: u8,
    widths: Option<&[f64]>,
) -> Result<Type3Metadata> {
    let identity_resources = type3_identity_resources(pdf, dictionary, limits);
    let matrix = dictionary
        .get(b"FontMatrix".as_slice())
        .ok_or_else(|| Error::Unresolved("Type 3 font has no FontMatrix".into()))?;
    let matrix = resolve_object(pdf, matrix.clone(), limits.max_indirections)?;
    let PdfObject::Array(matrix) = matrix else {
        return unresolved("Type 3 FontMatrix is not an array");
    };
    if matrix.len() != 6 {
        return unresolved("Type 3 FontMatrix does not contain six numbers");
    }
    let matrix = matrix
        .into_iter()
        .map(|value| {
            let value = resolve_object(pdf, value, limits.max_indirections)?;
            finite_number(&value, "Type 3 FontMatrix value")
        })
        .collect::<Result<Vec<_>>>()?;
    let [a, b, c, d, e, f] = matrix.as_slice() else {
        return unresolved("Type 3 FontMatrix does not contain six numbers");
    };
    let determinant = a.mul_add(*d, -(b * c));
    if !determinant.is_finite() {
        return unresolved("Type 3 FontMatrix determinant is not finite");
    }
    if determinant == 0.0 {
        return unresolved("Type 3 FontMatrix is degenerate");
    }
    // Only axis-aligned non-negative scales are supported in the horizontal simple-font model.
    if *b != 0.0 || *c != 0.0 || *e != 0.0 || *f != 0.0 || *a < 0.0 {
        return Err(Error::Unsupported(
            "Type 3 FontMatrix cannot be represented by horizontal simple-font metrics".into(),
        ));
    }
    let horizontal_scale_1000_em = scale_type3_axis(*a, "horizontal")?;
    let vertical_scale_1000_em = scale_type3_axis(*d, "vertical")?;

    let font_bbox = dictionary
        .get(b"FontBBox".as_slice())
        .ok_or_else(|| Error::Unresolved("Type 3 font has no FontBBox".into()))?;
    let font_bbox = resolve_object(pdf, font_bbox.clone(), limits.max_indirections)?;
    let PdfObject::Array(font_bbox) = font_bbox else {
        return unresolved("Type 3 FontBBox is not an array");
    };
    if font_bbox.len() != 4 {
        return unresolved("Type 3 FontBBox does not contain four numbers");
    }
    let font_bbox = font_bbox
        .into_iter()
        .map(|value| {
            let value = resolve_object(pdf, value, limits.max_indirections)?;
            finite_number(&value, "Type 3 FontBBox value")
        })
        .collect::<Result<Vec<_>>>()?;
    if font_bbox[0] > font_bbox[2] || font_bbox[1] >= font_bbox[3] {
        return unresolved("Type 3 FontBBox has invalid bounds");
    }
    let y0 = scale_type3_metric(
        font_bbox[1],
        vertical_scale_1000_em,
        "Type 3 FontBBox y-coordinate",
    )?;
    let y1 = scale_type3_metric(
        font_bbox[3],
        vertical_scale_1000_em,
        "Type 3 FontBBox y-coordinate",
    )?;
    let descent = y0.min(y1);
    let ascent = y0.max(y1);
    if ascent <= descent {
        return unresolved("Type 3 FontMatrix collapses the FontBBox vertical extent");
    }

    if !dictionary.contains_key(b"Encoding".as_slice()) {
        return unresolved("Type 3 font has no Encoding");
    }
    let widths = widths.ok_or_else(|| Error::Unresolved("Type 3 font has no Widths".into()))?;
    let last_char = dictionary
        .get(b"LastChar".as_slice())
        .ok_or_else(|| Error::Unresolved("Type 3 font has no LastChar".into()))?;
    let last_char = resolve_object(pdf, last_char.clone(), limits.max_indirections)?;
    let PdfObject::Integer(last_char) = last_char else {
        return unresolved("Type 3 LastChar is not an integer");
    };
    let last_char = u8::try_from(last_char)
        .map_err(|_| Error::Unresolved("Type 3 LastChar is outside the byte range".into()))?;
    if last_char < first_char || widths.len() != usize::from(last_char - first_char) + 1 {
        return unresolved("Type 3 Widths does not match FirstChar and LastChar");
    }

    let char_procs = dictionary
        .get(b"CharProcs".as_slice())
        .ok_or_else(|| Error::Unresolved("Type 3 font has no CharProcs".into()))?;
    let char_procs = resolve_object(pdf, char_procs.clone(), limits.max_indirections)?;
    let PdfObject::Dictionary(char_procs) = char_procs else {
        return unresolved("Type 3 CharProcs is not a dictionary");
    };
    if char_procs.len() > limits.max_simple_width_entries {
        return Err(Error::LimitExceeded {
            resource: "Type 3 CharProcs entries",
            limit: limits.max_simple_width_entries,
        });
    }
    for char_proc in char_procs.values() {
        let char_proc = resolve_object(pdf, char_proc.clone(), limits.max_indirections)?;
        if !matches!(char_proc, PdfObject::Stream(_)) {
            return unresolved("Type 3 CharProc is not a stream");
        }
    }
    Ok(Type3Metadata {
        char_procs,
        identity_resources,
        horizontal_scale_1000_em,
        ascent,
        descent,
    })
}

fn type3_identity_resources(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
) -> Type3IdentityResources {
    let Some(resources) = dictionary.get(b"Resources".as_slice()) else {
        return Type3IdentityResources::Independent;
    };
    let Ok(PdfObject::Dictionary(mut resources)) =
        resolve_object(pdf, resources.clone(), limits.max_indirections)
    else {
        return Type3IdentityResources::Unavailable;
    };
    if resources.is_empty() {
        return Type3IdentityResources::Independent;
    }
    if let Some(proc_set) = resources.remove(b"ProcSet".as_slice()) {
        let Ok(PdfObject::Array(proc_set)) = resolve_object(pdf, proc_set, limits.max_indirections)
        else {
            return Type3IdentityResources::Unavailable;
        };
        if proc_set.len() > limits.max_simple_width_entries
            || !proc_set.into_iter().all(|name| {
                matches!(
                    resolve_object(pdf, name, limits.max_indirections),
                    Ok(PdfObject::Name(name))
                        if matches!(
                            name.as_slice(),
                            b"PDF" | b"Text" | b"ImageB" | b"ImageC" | b"ImageI"
                        )
                )
            })
        {
            return Type3IdentityResources::Unavailable;
        }
    }
    if resources.is_empty() {
        Type3IdentityResources::Independent
    } else {
        Type3IdentityResources::Dependent(PdfObject::Dictionary(resources))
    }
}

fn scale_type3_axis(scale: f64, axis: &str) -> Result<f64> {
    let normalized = scale * 1000.0;
    if !normalized.is_finite() || normalized == 0.0 {
        return unresolved(&format!(
            "Type 3 FontMatrix {axis} scale cannot be represented in 1000-em units"
        ));
    }
    Ok(normalized)
}

fn scale_type3_metric(value: f64, scale_1000_em: f64, context: &str) -> Result<f64> {
    let scaled = value * scale_1000_em;
    if !scaled.is_finite() {
        return unresolved(&format!("{context} is not finite after FontMatrix scaling"));
    }
    Ok(scaled)
}

fn load_encoding(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
    base14: Option<Base14>,
    allowed_difference_names: Option<&PdfDict>,
) -> Result<LoadedEncoding> {
    let resolved_encoding = match dictionary.get(b"Encoding".as_slice()) {
        Some(encoding) => match resolve_object(pdf, encoding.clone(), limits.max_indirections)? {
            PdfObject::Null => None,
            resolved => Some(resolved),
        },
        None => None,
    };
    let Some(encoding) = resolved_encoding else {
        let base = built_in_encoding(base14);
        return Ok(LoadedEncoding {
            base,
            differences: BTreeMap::new(),
            identity_ambiguous: false,
            standard14_identity_encoding: if base14.is_some() {
                Some(base.identity_name())
            } else {
                None
            },
            explicit_encoding: false,
        });
    };
    match encoding {
        PdfObject::Name(name) => {
            let base = if name.as_slice() == b"SymbolSetEncoding" {
                // This producer-specific encoding has no public code-to-Unicode
                // table. Only exact ToUnicode entries are usable; every gap
                // remains unmapped and the explicit encoding disables program
                // selector identity.
                FallbackEncoding::Unknown
            } else {
                encoding_name(&name)?
            };
            Ok(LoadedEncoding {
                base,
                differences: BTreeMap::new(),
                identity_ambiguous: false,
                standard14_identity_encoding: if base14.is_some()
                    && !matches!(base, FallbackEncoding::Unknown)
                {
                    Some(base.identity_name())
                } else {
                    None
                },
                explicit_encoding: true,
            })
        }
        PdfObject::Dictionary(encoding) => {
            let base = match encoding.get(b"BaseEncoding".as_slice()) {
                Some(base) => match resolve_object(pdf, base.clone(), limits.max_indirections)? {
                    PdfObject::Name(name) => encoding_name(&name)?,
                    PdfObject::Null => built_in_encoding(base14),
                    _ => return unresolved("font BaseEncoding is not a name"),
                },
                None => built_in_encoding(base14),
            };
            let (differences, identity_ambiguous, differences_present) =
                match encoding.get(b"Differences".as_slice()) {
                    Some(differences) => {
                        let differences =
                            resolve_object(pdf, differences.clone(), limits.max_indirections)?;
                        match differences {
                            PdfObject::Array(differences) => {
                                if differences.len() > MAX_ENCODING_DIFFERENCE_ELEMENTS {
                                    return Err(Error::LimitExceeded {
                                        resource: "simple-font Encoding Differences elements",
                                        limit: MAX_ENCODING_DIFFERENCE_ELEMENTS,
                                    });
                                }
                                let (diffs, ambiguous) = parse_differences(
                                    pdf,
                                    &differences,
                                    limits.max_indirections,
                                    allowed_difference_names,
                                )?;
                                (diffs, ambiguous, true)
                            }
                            PdfObject::Null => (BTreeMap::new(), false, false),
                            _ => return unresolved("font Encoding Differences is not an array"),
                        }
                    }
                    None => (BTreeMap::new(), false, false),
                };
            let standard14_identity_encoding = if differences_present {
                // Differences dictionaries change code-selector meaning and
                // must not be canonicalized into Standard 14 font identity.
                None
            } else if base14.is_some() {
                match base {
                    FallbackEncoding::Standard
                    | FallbackEncoding::WinAnsi
                    | FallbackEncoding::MacRoman
                    | FallbackEncoding::Symbol
                    | FallbackEncoding::ZapfDingbats => Some(base.identity_name()),
                    FallbackEncoding::Unknown => None,
                }
            } else {
                None
            };
            Ok(LoadedEncoding {
                base,
                differences,
                identity_ambiguous,
                standard14_identity_encoding,
                explicit_encoding: true,
            })
        }
        _ => unresolved("font Encoding is not a name or dictionary"),
    }
}

fn has_embedded_font_program(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
) -> Result<bool> {
    let Some(descriptor) = dictionary.get(b"FontDescriptor".as_slice()) else {
        return Ok(false);
    };
    let descriptor = resolve_object(pdf, descriptor.clone(), max_indirections)?;
    let PdfObject::Dictionary(descriptor) = descriptor else {
        return Ok(false);
    };
    Ok(descriptor.contains_key(b"FontFile".as_slice())
        || descriptor.contains_key(b"FontFile2".as_slice())
        || descriptor.contains_key(b"FontFile3".as_slice()))
}

impl FallbackEncoding {
    fn identity_name(self) -> &'static [u8] {
        match self {
            Self::Standard => b"standard",
            Self::WinAnsi => b"win-ansi",
            Self::MacRoman => b"mac-roman",
            Self::Symbol => b"symbol-built-in",
            Self::ZapfDingbats => b"zapf-dingbats-built-in",
            Self::Unknown => b"unknown",
        }
    }
}

fn built_in_encoding(base14: Option<Base14>) -> FallbackEncoding {
    match base14 {
        Some(Base14::Symbol) => FallbackEncoding::Symbol,
        Some(Base14::ZapfDingbats) => FallbackEncoding::ZapfDingbats,
        Some(_) => FallbackEncoding::Standard,
        None => FallbackEncoding::Unknown,
    }
}

fn parse_differences(
    pdf: &dyn ParsedPdf,
    differences: &[PdfObject],
    max_indirections: usize,
    allowed_names: Option<&PdfDict>,
) -> Result<(BTreeMap<u8, DifferenceMapping>, bool)> {
    let mut mappings = BTreeMap::new();
    let mut identity_ambiguous = false;
    let mut next_code = None;
    let mut needs_name = false;

    for difference in differences {
        let difference = resolve_object(pdf, difference.clone(), max_indirections)?;
        match difference {
            PdfObject::Integer(code) => {
                if needs_name {
                    return unresolved("font Encoding Differences code has no following name");
                }
                next_code = Some(u16::from(u8::try_from(code).map_err(|_| {
                    Error::Unresolved(
                        "font Encoding Differences code is outside the byte range".into(),
                    )
                })?));
                needs_name = true;
            }
            PdfObject::Name(name) => {
                let code = next_code.ok_or_else(|| {
                    Error::Unresolved("font Encoding Differences name precedes a code".into())
                })?;
                let code = u8::try_from(code).map_err(|_| {
                    Error::Unresolved("font Encoding Differences extends beyond code 255".into())
                })?;
                let mut mapping = glyph_name_mapping(&name);
                mapping.missing_type3_procedure =
                    allowed_names.is_some_and(|allowed| !allowed.contains_key(&name));
                identity_ambiguous |= mappings.insert(code, mapping).is_some();
                next_code = Some(u16::from(code) + 1);
                needs_name = false;
            }
            _ => {
                return unresolved("font Encoding Differences contains a non-integer/non-name");
            }
        }
    }
    if needs_name {
        return unresolved("font Encoding Differences code has no following name");
    }
    Ok((mappings, identity_ambiguous))
}

fn encoding_name(name: &[u8]) -> Result<FallbackEncoding> {
    match name {
        b"WinAnsiEncoding" => Ok(FallbackEncoding::WinAnsi),
        b"MacRomanEncoding" => Ok(FallbackEncoding::MacRoman),
        b"StandardEncoding" => Ok(FallbackEncoding::Standard),
        _ => Err(Error::Unsupported(format!(
            "simple-font encoding /{} is not supported",
            String::from_utf8_lossy(name)
        ))),
    }
}

fn load_widths(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
) -> Result<(u8, Option<Vec<f64>>)> {
    let Some(widths) = dictionary.get(b"Widths".as_slice()) else {
        return Ok((0, None));
    };
    let first_char = match dictionary.get(b"FirstChar".as_slice()) {
        Some(value) => match resolve_object(pdf, value.clone(), limits.max_indirections)? {
            PdfObject::Integer(value) => u8::try_from(value)
                .map_err(|_| Error::Unresolved("FirstChar is outside the byte range".into()))?,
            _ => return unresolved("Widths requires an integer FirstChar"),
        },
        None => return unresolved("Widths requires an integer FirstChar"),
    };
    let widths = resolve_object(pdf, widths.clone(), limits.max_indirections)?;
    let PdfObject::Array(widths) = widths else {
        return unresolved("font Widths is not an array");
    };
    if widths.len() > limits.max_simple_width_entries {
        return Err(Error::LimitExceeded {
            resource: "simple-font width entries",
            limit: limits.max_simple_width_entries,
        });
    }
    if usize::from(first_char)
        .checked_add(widths.len())
        .is_none_or(|end| end > 256)
    {
        return unresolved("font Widths extends beyond the byte code range");
    }
    let widths = widths
        .into_iter()
        .map(|width| {
            let width = resolve_object(pdf, width, limits.max_indirections)?;
            non_negative_number(&width, "font width")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((first_char, Some(widths)))
}

fn load_descriptor(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
    base14: Option<Base14>,
) -> Result<(Option<f64>, Option<f64>, f64)> {
    let fallback_metrics = || {
        base14.map_or((None, None, 0.0), |font| {
            let (ascent, descent) = font.vertical_metrics();
            (Some(ascent), Some(descent), 0.0)
        })
    };
    let Some(descriptor) = dictionary.get(b"FontDescriptor".as_slice()) else {
        return Ok(fallback_metrics());
    };
    let descriptor = resolve_object(pdf, descriptor.clone(), max_indirections)?;
    let descriptor = match descriptor {
        PdfObject::Dictionary(descriptor) => descriptor,
        PdfObject::Null => return Ok(fallback_metrics()),
        _ => return unresolved("FontDescriptor is not a dictionary"),
    };
    let fallback = base14.map(Base14::vertical_metrics);
    let ascent = optional_number(pdf, &descriptor, b"Ascent", max_indirections)?
        .or_else(|| fallback.map(|value| value.0));
    let descent = optional_number(pdf, &descriptor, b"Descent", max_indirections)?
        .or_else(|| fallback.map(|value| value.1));
    let (ascent, descent) = apply_bbox_vertical_fallback((ascent, descent), || {
        load_descriptor_bbox(pdf, &descriptor, max_indirections)
    })?;
    let missing_width = descriptor
        .get(b"MissingWidth".as_slice())
        .map(|value| {
            let value = resolve_object(pdf, value.clone(), max_indirections)?;
            non_negative_number(&value, "MissingWidth")
        })
        .transpose()?
        .unwrap_or(0.0);
    Ok((ascent, descent, missing_width))
}

fn glyph_name_mapping(name: &[u8]) -> DifferenceMapping {
    let Some((unicode, metric_scalar)) = adobe_glyph_name(name) else {
        return DifferenceMapping {
            glyph_name: name.to_vec(),
            unicode: UnicodeMapping::Unmapped,
            metric_scalar: None,
            missing_type3_procedure: false,
        };
    };
    DifferenceMapping {
        glyph_name: name.to_vec(),
        unicode: UnicodeMapping::Mapped(unicode),
        metric_scalar,
        missing_type3_procedure: false,
    }
}

fn adobe_glyph_name(name: &[u8]) -> Option<(String, Option<char>)> {
    if name.len() > super::agl::MAX_GLYPH_NAME_BYTES {
        return None;
    }
    let base = name.split(|byte| *byte == b'.').next()?;
    if base.is_empty() {
        return None;
    }

    let mut text = String::new();
    let mut metric_scalar = None;
    let mut components = 0usize;
    for component in base.split(|byte| *byte == b'_') {
        let component_text = adobe_glyph_component(component)?;
        if component_text.chars().count() == 1 {
            metric_scalar = component_text.chars().next();
        }
        text.push_str(&component_text);
        components += 1;
    }
    if components != 1 {
        metric_scalar = None;
    }
    Some((text, metric_scalar))
}

fn adobe_glyph_component(name: &[u8]) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    if let Some(text) = super::agl::lookup(name) {
        return Some(text.into());
    }
    if let Some(hex) = name.strip_prefix(b"uni")
        && !hex.is_empty()
        && hex.len().is_multiple_of(4)
    {
        let mut text = String::new();
        for digits in hex.as_chunks::<4>().0 {
            text.push(parse_unicode_scalar(digits)?);
        }
        return Some(text);
    }
    if let Some(hex) = name.strip_prefix(b"u")
        && (4..=6).contains(&hex.len())
    {
        return Some(parse_unicode_scalar(hex)?.into());
    }
    None
}

fn parse_unicode_scalar(hex: &[u8]) -> Option<char> {
    if !hex
        .iter()
        .all(|digit| digit.is_ascii_digit() || matches!(digit, b'A'..=b'F'))
    {
        return None;
    }
    let value = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
    char::from_u32(value)
}

fn fallback_char(encoding: FallbackEncoding, code: u8) -> Option<char> {
    match encoding {
        FallbackEncoding::Standard => return standard_encoding_char(code),
        FallbackEncoding::MacRoman => return mac_roman_encoding_char(code),
        FallbackEncoding::Symbol | FallbackEncoding::ZapfDingbats | FallbackEncoding::Unknown => {
            return None;
        }
        FallbackEncoding::WinAnsi => {}
    }
    if code.is_ascii_graphic() || code == b' ' {
        return Some(char::from(code));
    }
    match code {
        0x80 => Some('\u{20ac}'),
        0x82 => Some('\u{201a}'),
        0x83 => Some('\u{0192}'),
        0x84 => Some('\u{201e}'),
        0x85 => Some('\u{2026}'),
        0x86 => Some('\u{2020}'),
        0x87 => Some('\u{2021}'),
        0x88 => Some('\u{02c6}'),
        0x89 => Some('\u{2030}'),
        0x8a => Some('\u{0160}'),
        0x8b => Some('\u{2039}'),
        0x8c => Some('\u{0152}'),
        0x8e => Some('\u{017d}'),
        0x91 => Some('\u{2018}'),
        0x92 => Some('\u{2019}'),
        0x93 => Some('\u{201c}'),
        0x94 => Some('\u{201d}'),
        0x95 => Some('\u{2022}'),
        0x96 => Some('\u{2013}'),
        0x97 => Some('\u{2014}'),
        0x98 => Some('\u{02dc}'),
        0x99 => Some('\u{2122}'),
        0x9a => Some('\u{0161}'),
        0x9b => Some('\u{203a}'),
        0x9c => Some('\u{0153}'),
        0x9e => Some('\u{017e}'),
        0x9f => Some('\u{0178}'),
        0xa0..=0xff => char::from_u32(u32::from(code)),
        _ => None,
    }
}

fn mac_roman_encoding_char(code: u8) -> Option<char> {
    match code {
        b' '..=b'~' => Some(char::from(code)),
        0x80..=0xff => char::from_u32(u32::from(MAC_ROMAN_UNICODE[usize::from(code - 0x80)])),
        _ => None,
    }
}

fn standard_encoding_char(code: u8) -> Option<char> {
    match code {
        32..=126 if code != 39 && code != 96 => Some(char::from(code)),
        39 => Some('\u{2019}'),
        96 => Some('\u{2018}'),
        161 => Some('\u{00a1}'),
        162 => Some('\u{00a2}'),
        163 => Some('\u{00a3}'),
        164 => Some('\u{2044}'),
        165 => Some('\u{00a5}'),
        166 => Some('\u{0192}'),
        167 => Some('\u{00a7}'),
        168 => Some('\u{00a4}'),
        169 => Some('\''),
        170 => Some('\u{201c}'),
        171 => Some('\u{00ab}'),
        172 => Some('\u{2039}'),
        173 => Some('\u{203a}'),
        174 => Some('\u{fb01}'),
        175 => Some('\u{fb02}'),
        177 => Some('\u{2013}'),
        178 => Some('\u{2020}'),
        179 => Some('\u{2021}'),
        180 => Some('\u{00b7}'),
        182 => Some('\u{00b6}'),
        183 => Some('\u{2022}'),
        184 => Some('\u{201a}'),
        185 => Some('\u{201e}'),
        186 => Some('\u{201d}'),
        187 => Some('\u{00bb}'),
        188 => Some('\u{2026}'),
        189 => Some('\u{2030}'),
        191 => Some('\u{00bf}'),
        193 => Some('`'),
        194 => Some('\u{00b4}'),
        195 => Some('\u{02c6}'),
        196 => Some('\u{02dc}'),
        197 => Some('\u{00af}'),
        198 => Some('\u{02d8}'),
        199 => Some('\u{02d9}'),
        200 => Some('\u{00a8}'),
        202 => Some('\u{02da}'),
        203 => Some('\u{00b8}'),
        205 => Some('\u{02dd}'),
        206 => Some('\u{02db}'),
        207 => Some('\u{02c7}'),
        208 => Some('\u{2014}'),
        225 => Some('\u{00c6}'),
        227 => Some('\u{00aa}'),
        232 => Some('\u{0141}'),
        233 => Some('\u{00d8}'),
        234 => Some('\u{0152}'),
        235 => Some('\u{00ba}'),
        241 => Some('\u{00e6}'),
        245 => Some('\u{0131}'),
        248 => Some('\u{0142}'),
        249 => Some('\u{00f8}'),
        250 => Some('\u{0153}'),
        251 => Some('\u{00df}'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::pdf::font::cmap::CMapLimits;
    use crate::pdf::font::load_font_identity;
    use crate::pdf::{DecodedStream, ObjectRef, PageRef, PdfVersion, RawStream};

    use super::*;

    const LIMITS: FontDecoderLimits = FontDecoderLimits {
        max_indirections: 4,
        max_simple_width_entries: 256,
        max_cid_width_entries: 16,
        max_decoded_font_bytes: 4096,
        cmap: CMapLimits {
            max_entries: 64,
            max_code_bytes: 4,
            max_output_scalars: 64,
        },
    };

    #[test]
    fn decodes_win_ansi_with_widths_and_descriptor_metrics() -> Result<()> {
        let descriptor = PdfObject::Dictionary(PdfDict::from([
            (b"Ascent".to_vec(), PdfObject::Integer(700)),
            (b"Descent".to_vec(), PdfObject::Integer(-200)),
            (b"MissingWidth".to_vec(), PdfObject::Integer(500)),
        ]));
        let font = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"TrueType".to_vec())),
            (
                b"Encoding".to_vec(),
                PdfObject::Name(b"WinAnsiEncoding".to_vec()),
            ),
            (b"FirstChar".to_vec(), PdfObject::Integer(65)),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(600), PdfObject::Integer(610)]),
            ),
            (b"FontDescriptor".to_vec(), descriptor),
        ]));

        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        assert!(loaded.identity_source.is_none());
        let glyphs = loaded
            .decoder
            .decode(&[b'A', b'B', 0x80, 0x81], 4, usize::MAX)?;

        assert_eq!(loaded.decoded_font_bytes, 0);
        assert_eq!(loaded.decoder.cmap_entry_count(), 0);
        assert_eq!(loaded.decoder.ascent_1000_em(), 700.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -200.0);
        assert_eq!(glyphs[0], glyph(b'A', "A", 600.0));
        assert_eq!(glyphs[1], glyph(b'B', "B", 610.0));
        assert_eq!(glyphs[2], glyph(0x80, "€", 500.0));
        assert_eq!(glyphs[3].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[3].width_1000_em, 500.0);
        Ok(())
    }

    #[test]
    fn decodes_pdf_mac_roman_and_uses_glyph_metrics() -> Result<()> {
        let font = font_with_encoding(PdfObject::Name(b"MacRomanEncoding".to_vec()));
        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let glyphs = loaded
            .decoder
            .decode(&[0x87, 0xca, 0xdb, 0xde, 0xf0], 5, usize::MAX)?;

        assert_eq!(
            glyphs
                .iter()
                .map(|glyph| glyph.mapping.clone())
                .collect::<Vec<_>>(),
            [
                mapped_text("á"),
                mapped_text(" "),
                mapped_text("¤"),
                mapped_text("ﬁ"),
                mapped_text("\u{f8ff}"),
            ]
        );
        for (glyph, scalar) in glyphs.iter().zip(['á', ' ', '¤', 'ﬁ']) {
            assert_eq!(
                glyph.width_1000_em,
                Base14::Helvetica
                    .glyph_width(scalar)
                    .ok_or_else(|| Error::Unresolved("fixture glyph should have metrics".into()))?
            );
        }
        assert_eq!(glyphs[4].width_1000_em, 0.0);
        Ok(())
    }

    #[test]
    fn uses_descriptor_bbox_when_vertical_metrics_are_degenerate() -> Result<()> {
        let font = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"FirstChar".to_vec(), PdfObject::Integer(65)),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(600)]),
            ),
            (
                b"FontDescriptor".to_vec(),
                PdfObject::Dictionary(PdfDict::from([
                    (b"Ascent".to_vec(), PdfObject::Integer(0)),
                    (b"Descent".to_vec(), PdfObject::Integer(0)),
                    (
                        b"FontBBox".to_vec(),
                        PdfObject::Array(vec![
                            PdfObject::Integer(-196),
                            PdfObject::Integer(-322),
                            PdfObject::Integer(1502),
                            PdfObject::Integer(937),
                        ]),
                    ),
                ])),
            ),
        ]));

        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;

        assert_eq!(loaded.decoder.ascent_1000_em(), 937.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -322.0);
        Ok(())
    }

    #[test]
    fn explicit_encodings_never_claim_program_selector_identity() -> Result<()> {
        let font = |encoding: &[u8]| {
            PdfObject::Dictionary(PdfDict::from([
                (b"Subtype".to_vec(), PdfObject::Name(b"TrueType".to_vec())),
                (
                    b"BaseFont".to_vec(),
                    PdfObject::Name(b"FixtureEmbedded".to_vec()),
                ),
                (b"Encoding".to_vec(), PdfObject::Name(encoding.to_vec())),
                (b"FirstChar".to_vec(), PdfObject::Integer(65)),
                (
                    b"Widths".to_vec(),
                    PdfObject::Array(vec![PdfObject::Integer(600)]),
                ),
                (
                    b"FontDescriptor".to_vec(),
                    PdfObject::Dictionary(PdfDict::from([
                        (b"Ascent".to_vec(), PdfObject::Integer(700)),
                        (b"Descent".to_vec(), PdfObject::Integer(-200)),
                        (b"FontFile2".to_vec(), PdfObject::Reference(object_ref(99))),
                    ])),
                ),
            ]))
        };

        let win_ansi =
            SimpleFontDecoder::load(&MockPdf::default(), &font(b"WinAnsiEncoding"), LIMITS)?;
        let standard =
            SimpleFontDecoder::load(&MockPdf::default(), &font(b"StandardEncoding"), LIMITS)?;

        assert!(win_ansi.identity_source.is_none());
        assert!(standard.identity_source.is_none());
        Ok(())
    }

    #[test]
    fn selects_type1c_identity_but_keeps_explicit_encodings_unidentified() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([(
                object_ref(1),
                PdfObject::Stream(PdfDict::from([(
                    b"Subtype".to_vec(),
                    PdfObject::Name(b"Type1C".to_vec()),
                )])),
            )]),
            streams: HashMap::from([(object_ref(1), b"Type1C program".to_vec())]),
        };
        let font = |encoding: Option<PdfObject>| {
            let mut dictionary = PdfDict::from([
                (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
                (b"FirstChar".to_vec(), PdfObject::Integer(65)),
                (
                    b"Widths".to_vec(),
                    PdfObject::Array(vec![PdfObject::Integer(600)]),
                ),
                (
                    b"FontDescriptor".to_vec(),
                    PdfObject::Dictionary(PdfDict::from([
                        (b"Ascent".to_vec(), PdfObject::Integer(700)),
                        (b"Descent".to_vec(), PdfObject::Integer(-200)),
                        (b"FontFile3".to_vec(), PdfObject::Reference(object_ref(1))),
                    ])),
                ),
            ]);
            if let Some(encoding) = encoding {
                dictionary.insert(b"Encoding".to_vec(), encoding);
            }
            PdfObject::Dictionary(dictionary)
        };

        let implicit = SimpleFontDecoder::load(&pdf, &font(None), LIMITS)?;
        let source = implicit
            .identity_source
            .ok_or_else(|| Error::Unresolved("Type1C fixture should have identity".into()))?;
        assert_eq!(
            load_font_identity(&pdf, &source, usize::MAX)?.decoded_bytes,
            b"Type1C program".len()
        );

        let explicit = SimpleFontDecoder::load(
            &pdf,
            &font(Some(PdfObject::Name(b"StandardEncoding".to_vec()))),
            LIMITS,
        )?;
        assert!(explicit.identity_source.is_none());
        Ok(())
    }

    #[test]
    fn accepts_mm_type1_through_the_declared_simple_font_path() -> Result<()> {
        let mut font = standard_font(b"Helvetica");
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"Subtype".to_vec(), PdfObject::Name(b"MMType1".to_vec()));

        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        assert!(loaded.identity_source.is_none());
        assert_eq!(
            loaded.decoder.decode(b"A", 1, usize::MAX)?,
            [glyph(b'A', "A", 667.0)]
        );
        Ok(())
    }

    #[test]
    fn mm_type1_instances_never_claim_embedded_program_identity() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Stream(PdfDict::new()))]),
            streams: HashMap::from([(object_ref(1), b"shared MMType1 program".to_vec())]),
        };
        let font = |base_font: &[u8]| {
            PdfObject::Dictionary(PdfDict::from([
                (b"Subtype".to_vec(), PdfObject::Name(b"MMType1".to_vec())),
                (b"BaseFont".to_vec(), PdfObject::Name(base_font.to_vec())),
                (b"FirstChar".to_vec(), PdfObject::Integer(65)),
                (
                    b"Widths".to_vec(),
                    PdfObject::Array(vec![PdfObject::Integer(600)]),
                ),
                (
                    b"FontDescriptor".to_vec(),
                    PdfObject::Dictionary(PdfDict::from([
                        (b"Ascent".to_vec(), PdfObject::Integer(700)),
                        (b"Descent".to_vec(), PdfObject::Integer(-200)),
                        (b"FontFile".to_vec(), PdfObject::Reference(object_ref(1))),
                    ])),
                ),
            ]))
        };

        for instance in [b"FixtureMM-Light".as_slice(), b"FixtureMM-Bold"] {
            let loaded = SimpleFontDecoder::load(&pdf, &font(instance), LIMITS)?;
            assert!(loaded.identity_source.is_none());
            assert_eq!(
                loaded.decoder.decode(b"A", 1, usize::MAX)?[0].mapping,
                UnicodeMapping::Unmapped
            );
        }
        Ok(())
    }

    #[test]
    fn truetype_symbolic_policy_separates_identity_and_rejects_ambiguous_flags() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Stream(PdfDict::new()))]),
            streams: HashMap::from([(object_ref(1), b"same TrueType program".to_vec())]),
        };
        let font = |flags: Option<PdfObject>| {
            let mut descriptor = PdfDict::from([
                (b"Ascent".to_vec(), PdfObject::Integer(700)),
                (b"Descent".to_vec(), PdfObject::Integer(-200)),
                (b"FontFile2".to_vec(), PdfObject::Reference(object_ref(1))),
            ]);
            if let Some(flags) = flags {
                descriptor.insert(b"Flags".to_vec(), flags);
            }
            PdfObject::Dictionary(PdfDict::from([
                (b"Subtype".to_vec(), PdfObject::Name(b"TrueType".to_vec())),
                (
                    b"BaseFont".to_vec(),
                    PdfObject::Name(b"FixtureEmbedded".to_vec()),
                ),
                (b"FirstChar".to_vec(), PdfObject::Integer(65)),
                (
                    b"Widths".to_vec(),
                    PdfObject::Array(vec![PdfObject::Integer(600)]),
                ),
                (
                    b"FontDescriptor".to_vec(),
                    PdfObject::Dictionary(descriptor),
                ),
            ]))
        };
        let symbolic_font =
            SimpleFontDecoder::load(&pdf, &font(Some(PdfObject::Integer(4))), LIMITS)?;
        let nonsymbolic_font =
            SimpleFontDecoder::load(&pdf, &font(Some(PdfObject::Integer(32))), LIMITS)?;
        assert_eq!(
            symbolic_font.decoder.decode(b"A", 1, usize::MAX)?[0].glyph_id,
            nonsymbolic_font.decoder.decode(b"A", 1, usize::MAX)?[0].glyph_id
        );
        let symbolic = symbolic_font
            .identity_source
            .ok_or_else(|| Error::Unresolved("symbolic fixture should have identity".into()))?;
        let nonsymbolic = nonsymbolic_font
            .identity_source
            .ok_or_else(|| Error::Unresolved("nonsymbolic fixture should have identity".into()))?;

        let symbolic = load_font_identity(&pdf, &symbolic, usize::MAX)?;
        let nonsymbolic = load_font_identity(&pdf, &nonsymbolic, usize::MAX)?;
        assert_ne!(symbolic.hash, nonsymbolic.hash);

        for flags in [
            None,
            Some(PdfObject::Integer(0)),
            Some(PdfObject::Integer(36)),
        ] {
            let loaded = SimpleFontDecoder::load(&pdf, &font(flags), LIMITS)?;
            assert!(loaded.identity_source.is_none());
        }
        Ok(())
    }

    #[test]
    fn resolves_indirect_font_parts_and_uses_to_unicode() -> Result<()> {
        let cmap = b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
                     2 beginbfchar <41> <0041> <80> <20AC> endbfchar"
            .to_vec();
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), cmap.clone());
        pdf.objects.insert(
            object_ref(2),
            PdfObject::Array(vec![PdfObject::Integer(620)]),
        );
        pdf.objects.insert(
            object_ref(3),
            PdfObject::Dictionary(PdfDict::from([
                (b"Ascent".to_vec(), PdfObject::Integer(700)),
                (b"Descent".to_vec(), PdfObject::Integer(-200)),
                (b"MissingWidth".to_vec(), PdfObject::Integer(480)),
            ])),
        );
        let font = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"FirstChar".to_vec(), PdfObject::Integer(65)),
            (b"Widths".to_vec(), PdfObject::Reference(object_ref(2))),
            (
                b"FontDescriptor".to_vec(),
                PdfObject::Reference(object_ref(3)),
            ),
            (b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1))),
        ]));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(&[b'A', 0x80, b'B'], 3, usize::MAX)?;

        assert_eq!(loaded.decoded_font_bytes, cmap.len());
        assert_eq!(loaded.decoder.cmap_entry_count(), 3);
        assert_eq!(glyphs[0], glyph(b'A', "A", 620.0));
        assert_eq!(glyphs[1], glyph(0x80, "€", 480.0));
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
        Ok(())
    }

    #[test]
    fn resolves_indirect_widths_and_descriptor_metrics() -> Result<()> {
        let mut pdf = MockPdf::default();
        pdf.objects.insert(object_ref(1), PdfObject::Integer(65));
        pdf.objects.insert(object_ref(2), PdfObject::Integer(620));
        pdf.objects.insert(object_ref(3), PdfObject::Integer(630));
        pdf.objects.insert(object_ref(4), PdfObject::Integer(700));
        pdf.objects.insert(object_ref(5), PdfObject::Integer(-200));
        pdf.objects.insert(object_ref(6), PdfObject::Integer(480));
        pdf.objects.insert(
            object_ref(7),
            PdfObject::Dictionary(PdfDict::from([
                (b"Ascent".to_vec(), PdfObject::Reference(object_ref(4))),
                (b"Descent".to_vec(), PdfObject::Reference(object_ref(5))),
                (
                    b"MissingWidth".to_vec(),
                    PdfObject::Reference(object_ref(6)),
                ),
            ])),
        );
        let font = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"TrueType".to_vec())),
            (
                b"Encoding".to_vec(),
                PdfObject::Name(b"WinAnsiEncoding".to_vec()),
            ),
            (b"FirstChar".to_vec(), PdfObject::Reference(object_ref(1))),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![
                    PdfObject::Reference(object_ref(2)),
                    PdfObject::Reference(object_ref(3)),
                ]),
            ),
            (
                b"FontDescriptor".to_vec(),
                PdfObject::Reference(object_ref(7)),
            ),
        ]));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(&[b'A', b'B', 0x81], 3, usize::MAX)?;

        assert_eq!(loaded.decoder.ascent_1000_em(), 700.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -200.0);
        assert_eq!(glyphs[0], glyph(b'A', "A", 620.0));
        assert_eq!(glyphs[1], glyph(b'B', "B", 630.0));
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[2].width_1000_em, 480.0);
        Ok(())
    }

    #[test]
    fn treats_null_to_unicode_as_absent() -> Result<()> {
        let mut font = font_with_encoding(PdfObject::Name(b"WinAnsiEncoding".to_vec()));
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Null);

        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(b"A", 1, usize::MAX)?;

        assert_eq!(loaded.decoded_font_bytes, 0);
        assert_eq!(loaded.decoder.cmap_entry_count(), 0);
        assert_eq!(glyphs[0].mapping, UnicodeMapping::Mapped("A".into()));
        Ok(())
    }

    #[test]
    fn partial_to_unicode_prefers_mappings_then_falls_back_with_aggregate_limits() -> Result<()> {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
                     1 beginbfchar <41> <005A> endbfchar"
            .to_vec();
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), cmap);
        let mut font = font_with_encoding(PdfObject::Name(b"WinAnsiEncoding".to_vec()));
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        assert_eq!(
            loaded.decoder.decode(b"AB", 2, usize::MAX)?,
            [glyph(b'A', "Z", 667.0), glyph(b'B', "B", 667.0)]
        );
        assert!(matches!(
            loaded.decoder.decode(b"AB", 2, 1),
            Err(Error::LimitExceeded {
                resource: "decoded Unicode text bytes",
                limit: 1,
            })
        ));
        Ok(())
    }

    #[test]
    fn symbol_set_encoding_uses_only_exact_to_unicode_entries() -> Result<()> {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
                     2 beginbfchar <41> <0041> <42> <20AC> endbfchar"
            .to_vec();
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), cmap);
        let mut font = symbol_set_font_with_widths();
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;

        assert!(loaded.identity_source.is_none());
        assert_eq!(
            loaded.decoder.decode(b"AB", 2, usize::MAX)?,
            [glyph(b'A', "A", 600.0), glyph(b'B', "€", 610.0)]
        );
        Ok(())
    }

    #[test]
    fn symbol_set_encoding_gaps_remain_unmapped_without_identity() -> Result<()> {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
                     1 beginbfchar <41> <005A> endbfchar"
            .to_vec();
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), cmap);
        let mut partial = symbol_set_font_with_widths();
        let PdfObject::Dictionary(dictionary) = &mut partial else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));

        let partial = SimpleFontDecoder::load(&pdf, &partial, LIMITS)?;
        let absent =
            SimpleFontDecoder::load(&MockPdf::default(), &symbol_set_font_with_widths(), LIMITS)?;

        assert!(partial.identity_source.is_none());
        assert_eq!(
            partial.decoder.decode(b"AB", 2, usize::MAX)?[0].mapping,
            mapped_text("Z")
        );
        assert_eq!(
            partial.decoder.decode(b"AB", 2, usize::MAX)?[1].mapping,
            UnicodeMapping::Unmapped
        );
        assert!(absent.identity_source.is_none());
        assert!(
            absent
                .decoder
                .decode(b"A", 1, usize::MAX)?
                .iter()
                .all(|glyph| glyph.mapping == UnicodeMapping::Unmapped)
        );
        Ok(())
    }

    #[test]
    fn symbol_set_encoding_does_not_use_standard_14_fallback_widths() {
        let font = font_with_encoding(PdfObject::Name(b"SymbolSetEncoding".to_vec()));

        assert!(matches!(
            SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS),
            Err(Error::Unsupported(message))
                if message.contains("SymbolSetEncoding requires explicit Widths")
        ));
    }

    #[test]
    fn partial_to_unicode_falls_back_outside_a_narrow_codespace() -> Result<()> {
        let cmap = b"1 begincodespacerange <41> <41> endcodespacerange \
                     1 beginbfchar <41> <005A> endbfchar"
            .to_vec();
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), cmap);
        let mut font = font_with_encoding(PdfObject::Name(b"StandardEncoding".to_vec()));
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        assert_eq!(
            loaded.decoder.decode(b"AB", 2, usize::MAX)?,
            [glyph(b'A', "Z", 667.0), glyph(b'B', "B", 667.0)]
        );
        Ok(())
    }

    #[test]
    fn explicit_unmapped_to_unicode_entry_does_not_fall_back() -> Result<()> {
        let cmap = b"1 begincodespacerange <41> <42> endcodespacerange \
                     2 beginbfchar <41> <005A> <42> <D800> endbfchar"
            .to_vec();
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), cmap);
        let mut font = font_with_encoding(PdfObject::Name(b"StandardEncoding".to_vec()));
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(b"AB", 2, usize::MAX)?;
        assert_eq!(glyphs[0], glyph(b'A', "Z", 667.0));
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);
        Ok(())
    }

    #[test]
    fn partial_to_unicode_does_not_guess_unknown_differences() -> Result<()> {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
                     1 beginbfchar <41> <005A> endbfchar"
            .to_vec();
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), cmap);
        let mut font = font_with_differences(PdfObject::Array(vec![
            PdfObject::Integer(66),
            PdfObject::Name(b"UnknownGlyph".to_vec()),
        ]));
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(b"AB", 2, usize::MAX)?;
        assert_eq!(glyphs[0], glyph(b'A', "Z", 667.0));
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);
        Ok(())
    }

    #[test]
    fn decodes_encoding_differences_with_resets_ligatures_and_fallback() -> Result<()> {
        let font = font_with_differences(PdfObject::Array(vec![
            PdfObject::Integer(65),
            PdfObject::Name(b"Aacute".to_vec()),
            PdfObject::Name(b"fi".to_vec()),
            PdfObject::Integer(70),
            PdfObject::Name(b"fl".to_vec()),
            PdfObject::Integer(80),
            PdfObject::Name(b"Lslash".to_vec()),
            PdfObject::Name(b"bullet".to_vec()),
            PdfObject::Name(b"endash".to_vec()),
            PdfObject::Integer(90),
            PdfObject::Name(b"adieresis".to_vec()),
            PdfObject::Name(b"UnknownGlyph".to_vec()),
        ]));

        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let glyphs =
            loaded
                .decoder
                .decode(&[65, 66, 67, 70, 71, 80, 81, 82, 90, 91], 10, usize::MAX)?;

        let mappings = glyphs
            .iter()
            .map(|glyph| glyph.mapping.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            mappings,
            [
                mapped_text("Á"),
                mapped_text("ﬁ"),
                mapped_text("C"),
                mapped_text("ﬂ"),
                mapped_text("G"),
                mapped_text("Ł"),
                mapped_text("•"),
                mapped_text("–"),
                mapped_text("ä"),
                UnicodeMapping::Unmapped,
            ]
        );
        assert!(matches!(
            loaded.decoder.decode(&[66], 1, 1),
            Err(Error::LimitExceeded {
                resource: "decoded Unicode text bytes",
                limit: 1,
            })
        ));
        Ok(())
    }

    #[test]
    fn resolves_adobe_glyph_names_and_algorithmic_forms() {
        assert_eq!(adobe_glyph_name(b"Euro"), Some(("€".into(), Some('€'))));
        assert_eq!(adobe_glyph_name(b"uni20AC"), Some(("€".into(), Some('€'))));
        assert_eq!(adobe_glyph_name(b"u1F600"), Some(("😀".into(), Some('😀'))));
        assert_eq!(adobe_glyph_name(b"A.swash"), Some(("A".into(), Some('A'))));
        assert_eq!(adobe_glyph_name(b"A_Euro"), Some(("A€".into(), None)));
        assert_eq!(adobe_glyph_name(b"fi"), Some(("ﬁ".into(), Some('ﬁ'))));
        assert_eq!(adobe_glyph_name(b"fl"), Some(("ﬂ".into(), Some('ﬂ'))));
        assert_eq!(adobe_glyph_name(b"uni20ac"), None);
        assert_eq!(adobe_glyph_name(b"u1f600"), None);

        let overlong_composite = std::iter::repeat_n("A", 33).collect::<Vec<_>>().join("_");
        assert!(overlong_composite.len() > super::super::agl::MAX_GLYPH_NAME_BYTES);
        assert_eq!(adobe_glyph_name(overlong_composite.as_bytes()), None);

        for invalid in [b"UnknownGlyph".as_slice(), b"uniD800", b"u110000", b"A__B"] {
            assert_eq!(adobe_glyph_name(invalid), None);
            assert!(matches!(
                glyph_name_mapping(invalid).unicode,
                UnicodeMapping::Unmapped
            ));
        }
    }

    #[test]
    fn rejects_unavailable_difference_metrics_without_explicit_widths() -> Result<()> {
        let encoding = PdfObject::Dictionary(PdfDict::from([(
            b"Differences".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"space".to_vec()),
            ]),
        )]));
        let without_widths = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(b"Symbol".to_vec())),
            (b"Encoding".to_vec(), encoding.clone()),
        ]));
        assert!(matches!(
            SimpleFontDecoder::load(&MockPdf::default(), &without_widths, LIMITS),
            Err(Error::Unsupported(message))
                if message
                    == "Standard 14 Symbol/ZapfDingbats Differences metrics are not supported"
        ));

        let with_widths = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(b"Symbol".to_vec())),
            (b"Encoding".to_vec(), encoding),
            (b"FirstChar".to_vec(), PdfObject::Integer(65)),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(321)]),
            ),
        ]));
        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &with_widths, LIMITS)?;
        assert_eq!(
            loaded.decoder.decode(b"A", 1, usize::MAX)?,
            [glyph(b'A', " ", 321.0)]
        );
        Ok(())
    }

    #[test]
    fn resolves_indirect_encoding_and_differences() -> Result<()> {
        let mut pdf = MockPdf::default();
        pdf.objects.insert(
            object_ref(1),
            PdfObject::Dictionary(PdfDict::from([
                (
                    b"BaseEncoding".to_vec(),
                    PdfObject::Reference(object_ref(3)),
                ),
                (b"Differences".to_vec(), PdfObject::Reference(object_ref(2))),
            ])),
        );
        pdf.objects.insert(
            object_ref(2),
            PdfObject::Array(vec![
                PdfObject::Reference(object_ref(4)),
                PdfObject::Reference(object_ref(5)),
            ]),
        );
        pdf.objects
            .insert(object_ref(3), PdfObject::Name(b"StandardEncoding".to_vec()));
        pdf.objects.insert(object_ref(4), PdfObject::Integer(65));
        pdf.objects
            .insert(object_ref(5), PdfObject::Name(b"fi".to_vec()));
        let font = font_with_encoding(PdfObject::Reference(object_ref(1)));

        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        assert_eq!(
            loaded.decoder.decode(b"AB", 2, usize::MAX)?,
            [glyph(b'A', "ﬁ", 500.0), glyph(b'B', "B", 667.0)]
        );
        Ok(())
    }

    #[test]
    fn differences_without_base_encoding_use_only_known_builtins() -> Result<()> {
        let standard = font_with_encoding(PdfObject::Dictionary(PdfDict::from([(
            b"Differences".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(65), PdfObject::Name(b"i".to_vec())]),
        )])));
        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &standard, LIMITS)?;
        assert_eq!(
            loaded.decoder.decode(b"AB", 2, usize::MAX)?,
            [glyph(b'A', "i", 222.0), glyph(b'B', "B", 667.0)]
        );

        let nonstandard = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (
                b"BaseFont".to_vec(),
                PdfObject::Name(b"CustomFont".to_vec()),
            ),
            (b"FirstChar".to_vec(), PdfObject::Integer(65)),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(610), PdfObject::Integer(620)]),
            ),
            (
                b"FontDescriptor".to_vec(),
                PdfObject::Dictionary(PdfDict::from([
                    (b"Ascent".to_vec(), PdfObject::Integer(700)),
                    (b"Descent".to_vec(), PdfObject::Integer(-200)),
                ])),
            ),
            (
                b"Encoding".to_vec(),
                PdfObject::Dictionary(PdfDict::from([(
                    b"Differences".to_vec(),
                    PdfObject::Array(vec![PdfObject::Integer(65), PdfObject::Name(b"A".to_vec())]),
                )])),
            ),
        ]));
        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &nonstandard, LIMITS)?;
        let glyphs = loaded.decoder.decode(b"AB", 2, usize::MAX)?;
        assert_eq!(glyphs[0], glyph(b'A', "A", 610.0));
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[1].width_1000_em, 620.0);

        for (name, expected) in [
            (b"Helvetica".as_slice(), FallbackEncoding::Standard),
            (b"Symbol".as_slice(), FallbackEncoding::Symbol),
            (b"ZapfDingbats".as_slice(), FallbackEncoding::ZapfDingbats),
        ] {
            let loaded =
                SimpleFontDecoder::load(&MockPdf::default(), &standard_font(name), LIMITS)?;
            assert_eq!(loaded.decoder.fallback, expected);
        }
        Ok(())
    }

    #[test]
    fn rejects_malformed_encoding_differences() {
        let malformed = [
            PdfObject::Array(vec![PdfObject::Name(b"A".to_vec())]),
            PdfObject::Array(vec![PdfObject::Integer(-1), PdfObject::Name(b"A".to_vec())]),
            PdfObject::Array(vec![
                PdfObject::Integer(256),
                PdfObject::Name(b"A".to_vec()),
            ]),
            PdfObject::Array(vec![PdfObject::Integer(65), PdfObject::Integer(66)]),
            PdfObject::Array(vec![PdfObject::Integer(65)]),
            PdfObject::Array(vec![
                PdfObject::Integer(255),
                PdfObject::Name(b"A".to_vec()),
                PdfObject::Name(b"B".to_vec()),
            ]),
            PdfObject::Array(vec![PdfObject::Integer(65), PdfObject::Array(Vec::new())]),
            PdfObject::Name(b"not-an-array".to_vec()),
        ];

        for differences in malformed {
            assert!(matches!(
                SimpleFontDecoder::load(
                    &MockPdf::default(),
                    &font_with_differences(differences),
                    LIMITS
                ),
                Err(Error::Unresolved(_))
            ));
        }
    }

    #[test]
    fn bounds_and_indirectly_resolves_encoding_differences() {
        let too_many = PdfObject::Array(
            (0..=MAX_ENCODING_DIFFERENCE_ELEMENTS)
                .map(|_| PdfObject::Integer(0))
                .collect(),
        );
        assert!(matches!(
            SimpleFontDecoder::load(
                &MockPdf::default(),
                &font_with_differences(too_many),
                LIMITS
            ),
            Err(Error::LimitExceeded {
                resource: "simple-font Encoding Differences elements",
                limit: MAX_ENCODING_DIFFERENCE_ELEMENTS,
            })
        ));

        let mut pdf = MockPdf::default();
        pdf.objects.insert(
            object_ref(1),
            PdfObject::Dictionary(PdfDict::from([(
                b"Differences".to_vec(),
                PdfObject::Array(vec![PdfObject::Reference(object_ref(2))]),
            )])),
        );
        pdf.objects
            .insert(object_ref(2), PdfObject::Reference(object_ref(2)));
        assert!(matches!(
            SimpleFontDecoder::load(
                &pdf,
                &font_with_encoding(PdfObject::Reference(object_ref(1))),
                LIMITS
            ),
            Err(Error::LimitExceeded {
                resource: "font object indirections",
                limit: 4,
            })
        ));
    }

    #[test]
    fn rejects_cid_fonts_and_resource_limit_overruns() -> Result<()> {
        let pdf = MockPdf::default();
        let cid = PdfObject::Dictionary(PdfDict::from([(
            b"Subtype".to_vec(),
            PdfObject::Name(b"Type0".to_vec()),
        )]));
        assert!(matches!(
            SimpleFontDecoder::load(&pdf, &cid, LIMITS),
            Err(Error::Unsupported(_))
        ));

        let too_many_widths = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"FirstChar".to_vec(), PdfObject::Integer(0)),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(500); 2]),
            ),
        ]));
        assert!(matches!(
            SimpleFontDecoder::load(
                &pdf,
                &too_many_widths,
                FontDecoderLimits {
                    max_simple_width_entries: 1,
                    ..LIMITS
                }
            ),
            Err(Error::LimitExceeded {
                resource: "simple-font width entries",
                limit: 1
            })
        ));

        let base_font = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(b"Helvetica".to_vec())),
        ]));
        let loaded = SimpleFontDecoder::load(&pdf, &base_font, LIMITS)?;
        assert_eq!(loaded.decoder.ascent_1000_em(), 718.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -207.0);
        assert_eq!(
            loaded.decoder.decode(b"A", 1, usize::MAX)?[0].width_1000_em,
            667.0
        );
        assert!(matches!(
            loaded.decoder.decode(b"AB", 1, usize::MAX),
            Err(Error::LimitExceeded {
                resource: "decoded simple-font glyphs",
                limit: 1
            })
        ));
        Ok(())
    }

    #[test]
    fn uses_standard_encoding_glyph_mapping() -> Result<()> {
        let font = standard_font(b"Times-Roman");
        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(&[39, 96, 174, 225], 4, usize::MAX)?;
        assert_eq!(glyphs[0].mapping, UnicodeMapping::Mapped("’".into()));
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Mapped("‘".into()));
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Mapped("ﬁ".into()));
        assert_eq!(glyphs[3].mapping, UnicodeMapping::Mapped("Æ".into()));
        Ok(())
    }

    #[test]
    fn provides_metrics_for_every_standard_14_font() -> Result<()> {
        let expected = [
            (b"Courier".as_slice(), 600.0, 629.0, -157.0),
            (b"Courier-Bold".as_slice(), 600.0, 629.0, -157.0),
            (b"Courier-Oblique".as_slice(), 600.0, 629.0, -157.0),
            (b"Courier-BoldOblique".as_slice(), 600.0, 629.0, -157.0),
            (b"Helvetica".as_slice(), 667.0, 718.0, -207.0),
            (b"Helvetica-Bold".as_slice(), 722.0, 718.0, -207.0),
            (b"Helvetica-Oblique".as_slice(), 667.0, 718.0, -207.0),
            (b"Helvetica-BoldOblique".as_slice(), 722.0, 718.0, -207.0),
            (b"Times-Roman".as_slice(), 722.0, 683.0, -217.0),
            (b"Times-Bold".as_slice(), 722.0, 676.0, -205.0),
            (b"Times-Italic".as_slice(), 611.0, 683.0, -205.0),
            (b"Times-BoldItalic".as_slice(), 667.0, 699.0, -205.0),
            (b"Symbol".as_slice(), 722.0, 1010.0, -293.0),
            (b"ZapfDingbats".as_slice(), 692.0, 820.0, -143.0),
        ];
        for (name, width, ascent, descent) in expected {
            let loaded =
                SimpleFontDecoder::load(&MockPdf::default(), &standard_font(name), LIMITS)?;
            assert_eq!(
                loaded.decoder.decode(b"A", 1, usize::MAX)?[0].width_1000_em,
                width
            );
            assert_eq!(loaded.decoder.ascent_1000_em(), ascent);
            assert_eq!(loaded.decoder.descent_1000_em(), descent);
        }

        let unknown = standard_font(b"UnknownSans");
        assert!(matches!(
            SimpleFontDecoder::load(&MockPdf::default(), &unknown, LIMITS),
            Err(Error::Unresolved(_))
        ));
        Ok(())
    }

    #[test]
    fn unencoded_standard_14_fonts_have_canonical_glyph_identity() -> Result<()> {
        let pdf = MockPdf::default();
        let symbol = SimpleFontDecoder::load(&pdf, &standard_font(b"Symbol"), LIMITS)?;
        let dingbats = SimpleFontDecoder::load(&pdf, &standard_font(b"ZapfDingbats"), LIMITS)?;

        assert_eq!(
            symbol.decoder.decode(b"A", 1, usize::MAX)?[0].mapping,
            UnicodeMapping::Unmapped
        );
        let symbol_identity = load_font_identity(
            &pdf,
            &symbol
                .identity_source
                .ok_or_else(|| Error::Unresolved("Symbol should have identity".into()))?,
            usize::MAX,
        )?;
        let dingbats_identity = load_font_identity(
            &pdf,
            &dingbats
                .identity_source
                .ok_or_else(|| Error::Unresolved("ZapfDingbats should have identity".into()))?,
            usize::MAX,
        )?;

        assert_eq!(symbol_identity.decoded_bytes, 0);
        assert_ne!(symbol_identity.hash, dingbats_identity.hash);
        Ok(())
    }

    #[test]
    fn named_standard_14_encodings_are_part_of_canonical_identity() -> Result<()> {
        let pdf = MockPdf::default();
        let load = |encoding: &[u8]| {
            let mut font = standard_font(b"Helvetica");
            let PdfObject::Dictionary(dictionary) = &mut font else {
                unreachable!();
            };
            dictionary.insert(b"Encoding".to_vec(), PdfObject::Name(encoding.to_vec()));
            SimpleFontDecoder::load(&pdf, &font, LIMITS)
        };
        let win_ansi = load_font_identity(
            &pdf,
            &load(b"WinAnsiEncoding")?
                .identity_source
                .ok_or_else(|| Error::Unresolved("WinAnsi identity is missing".into()))?,
            usize::MAX,
        )?;
        let standard = load_font_identity(
            &pdf,
            &load(b"StandardEncoding")?
                .identity_source
                .ok_or_else(|| Error::Unresolved("Standard identity is missing".into()))?,
            usize::MAX,
        )?;

        assert_eq!(win_ansi.decoded_bytes, 0);
        assert_ne!(win_ansi.hash, standard.hash);
        Ok(())
    }

    #[test]
    fn explicit_standard_14_widths_use_missing_width_outside_the_range() -> Result<()> {
        let font = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(b"Helvetica".to_vec())),
            (b"FirstChar".to_vec(), PdfObject::Integer(65)),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(610)]),
            ),
            (
                b"FontDescriptor".to_vec(),
                PdfObject::Dictionary(PdfDict::from([
                    (b"Ascent".to_vec(), PdfObject::Integer(700)),
                    (b"MissingWidth".to_vec(), PdfObject::Integer(480)),
                ])),
            ),
        ]));

        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(b"AB", 2, usize::MAX)?;
        assert_eq!(glyphs[0].width_1000_em, 610.0);
        assert_eq!(glyphs[1].width_1000_em, 480.0);
        assert_eq!(loaded.decoder.ascent_1000_em(), 700.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -207.0);
        Ok(())
    }

    #[test]
    fn does_not_treat_subset_names_as_standard_14_fonts() {
        let subset = PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (
                b"BaseFont".to_vec(),
                PdfObject::Name(b"ABCDEF+Helvetica".to_vec()),
            ),
        ]));

        assert!(matches!(
            SimpleFontDecoder::load(&MockPdf::default(), &subset, LIMITS),
            Err(Error::Unresolved(_))
        ));
    }

    #[test]
    fn decodes_standard_matrix_type3_fonts_without_running_char_procs() -> Result<()> {
        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &standard_type3_font(), LIMITS)?;

        assert!(loaded.identity_source.is_none());
        assert_eq!(
            loaded.decoder.decode(b"AB", 2, usize::MAX)?,
            [glyph(b'A', "A", 600.0), glyph(b'B', "B", 610.0)]
        );
        assert_eq!(loaded.decoder.ascent_1000_em(), 700.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -200.0);
        Ok(())
    }

    #[test]
    fn binds_type3_encoding_names_to_stable_char_proc_glyph_ids() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(11), PdfObject::Stream(PdfDict::new())),
                (object_ref(12), PdfObject::Stream(PdfDict::new())),
            ]),
            streams: HashMap::from([
                (object_ref(11), b"c3 program".to_vec()),
                (object_ref(12), b"c8 program".to_vec()),
            ]),
        };
        let mut font = standard_type3_font();
        set_type3_encoding_and_char_procs(
            &mut font,
            vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"c3".to_vec()),
                PdfObject::Name(b"c8".to_vec()),
            ],
            PdfDict::from([
                (b"c3".to_vec(), PdfObject::Reference(object_ref(11))),
                (b"c8".to_vec(), PdfObject::Reference(object_ref(12))),
            ]),
        );
        let first = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        let first_glyphs = first.decoder.decode(b"AB", 2, usize::MAX)?;
        let first_identity = load_font_identity(
            &pdf,
            first
                .identity_source
                .as_ref()
                .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?,
            usize::MAX,
        )?;

        let second_pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(91), PdfObject::Stream(PdfDict::new())),
                (object_ref(92), PdfObject::Stream(PdfDict::new())),
            ]),
            streams: HashMap::from([
                (object_ref(91), b"c8 program".to_vec()),
                (object_ref(92), b"c3 program".to_vec()),
            ]),
        };
        let mut remapped_font = standard_type3_font();
        set_type3_encoding_and_char_procs(
            &mut remapped_font,
            vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"c8".to_vec()),
                PdfObject::Name(b"c3".to_vec()),
            ],
            PdfDict::from([
                (b"c8".to_vec(), PdfObject::Reference(object_ref(91))),
                (b"c3".to_vec(), PdfObject::Reference(object_ref(92))),
            ]),
        );
        let second = SimpleFontDecoder::load(&second_pdf, &remapped_font, LIMITS)?;
        let second_glyphs = second.decoder.decode(b"AB", 2, usize::MAX)?;
        let second_identity = load_font_identity(
            &second_pdf,
            second
                .identity_source
                .as_ref()
                .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?,
            usize::MAX,
        )?;

        assert_eq!(first_identity.hash, second_identity.hash);
        assert_eq!(first_glyphs[0].mapping, UnicodeMapping::Unmapped);
        assert_eq!(first_glyphs[1].mapping, UnicodeMapping::Unmapped);
        assert_eq!(first_glyphs[0].glyph_id, second_glyphs[1].glyph_id);
        assert_eq!(first_glyphs[1].glyph_id, second_glyphs[0].glyph_id);
        assert_ne!(first_glyphs[0].glyph_id, first_glyphs[1].glyph_id);
        Ok(())
    }

    #[test]
    fn type3_identity_tracks_resources_and_rejects_invalid_proc_sets() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(1), PdfObject::Stream(PdfDict::new())),
                (object_ref(2), PdfObject::Stream(PdfDict::new())),
                (
                    object_ref(3),
                    PdfObject::Dictionary(PdfDict::from([(
                        b"ProcSet".to_vec(),
                        PdfObject::Reference(object_ref(4)),
                    )])),
                ),
                (
                    object_ref(4),
                    PdfObject::Array(vec![
                        PdfObject::Name(b"PDF".to_vec()),
                        PdfObject::Reference(object_ref(5)),
                    ]),
                ),
                (object_ref(5), PdfObject::Name(b"Text".to_vec())),
            ]),
            streams: HashMap::from([
                (object_ref(1), b"c3 program".to_vec()),
                (object_ref(2), b"c8 program".to_vec()),
            ]),
        };
        let mut font = standard_type3_font();
        set_type3_encoding_and_char_procs(
            &mut font,
            vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"c3".to_vec()),
                PdfObject::Name(b"c8".to_vec()),
            ],
            PdfDict::from([
                (b"c3".to_vec(), PdfObject::Reference(object_ref(1))),
                (b"c8".to_vec(), PdfObject::Reference(object_ref(2))),
            ]),
        );
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"Resources".to_vec(), PdfObject::Reference(object_ref(3)));
        assert!(
            SimpleFontDecoder::load(&pdf, &font, LIMITS)?
                .identity_source
                .is_some()
        );

        let unsafe_resources = [
            PdfObject::Dictionary(PdfDict::from([(
                b"ProcSet".to_vec(),
                PdfObject::Array(vec![PdfObject::Name(b"Unknown".to_vec())]),
            )])),
            PdfObject::Dictionary(PdfDict::from([(
                b"ProcSet".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(1)]),
            )])),
            PdfObject::Dictionary(PdfDict::from([(
                b"ProcSet".to_vec(),
                PdfObject::Array(vec![PdfObject::Name(b"PDF".to_vec()); 257]),
            )])),
            PdfObject::Reference(object_ref(99)),
        ];
        for resources in unsafe_resources {
            let mut unsafe_font = font.clone();
            let PdfObject::Dictionary(dictionary) = &mut unsafe_font else {
                unreachable!();
            };
            dictionary.insert(b"Resources".to_vec(), resources);
            let loaded = SimpleFontDecoder::load(&pdf, &unsafe_font, LIMITS)?;
            assert!(loaded.identity_source.is_none());
            assert_eq!(
                loaded.decoder.decode(b"A", 1, usize::MAX)?[0].mapping,
                UnicodeMapping::Unmapped
            );
        }

        let mut mapped_font = standard_type3_font();
        let PdfObject::Dictionary(dictionary) = &mut mapped_font else {
            unreachable!();
        };
        dictionary.insert(
            b"Resources".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                b"XObject".to_vec(),
                PdfObject::Dictionary(PdfDict::new()),
            )])),
        );
        let mapped = SimpleFontDecoder::load(&MockPdf::default(), &mapped_font, LIMITS)?;
        assert!(mapped.identity_source.is_none());
        assert_eq!(
            mapped.decoder.decode(b"A", 1, usize::MAX)?[0].mapping,
            mapped_text("A")
        );
        Ok(())
    }

    #[test]
    fn type3_to_unicode_wins_and_ambiguous_encoding_has_no_identity() -> Result<()> {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
                     1 beginbfchar <41> <005A> endbfchar"
            .to_vec();
        let pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(1), PdfObject::Stream(PdfDict::new())),
                (object_ref(2), PdfObject::Stream(PdfDict::new())),
                (object_ref(3), PdfObject::Stream(PdfDict::new())),
            ]),
            streams: HashMap::from([
                (object_ref(1), b"c3 program".to_vec()),
                (object_ref(2), b"c8 program".to_vec()),
                (object_ref(3), cmap),
            ]),
        };
        let mut font = standard_type3_font();
        set_type3_encoding_and_char_procs(
            &mut font,
            vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"c3".to_vec()),
                PdfObject::Name(b"c8".to_vec()),
            ],
            PdfDict::from([
                (b"c3".to_vec(), PdfObject::Reference(object_ref(1))),
                (b"c8".to_vec(), PdfObject::Reference(object_ref(2))),
            ]),
        );
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(3)));
        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(b"AB", 2, usize::MAX)?;
        assert!(loaded.identity_source.is_some());
        assert_eq!(glyphs[0].mapping, mapped_text("Z"));
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);

        set_type3_encoding_and_char_procs(
            &mut font,
            vec![
                PdfObject::Integer(65),
                PdfObject::Name(b"c3".to_vec()),
                PdfObject::Integer(65),
                PdfObject::Name(b"c8".to_vec()),
            ],
            PdfDict::from([
                (b"c3".to_vec(), PdfObject::Reference(object_ref(1))),
                (b"c8".to_vec(), PdfObject::Reference(object_ref(2))),
            ]),
        );
        let ambiguous = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        assert!(ambiguous.identity_source.is_none());
        assert_eq!(
            ambiguous.decoder.decode(b"A", 1, usize::MAX)?[0].mapping,
            mapped_text("Z")
        );
        Ok(())
    }

    #[test]
    fn normalizes_axis_aligned_type3_font_matrices_to_1000_em() -> Result<()> {
        let mut font = standard_type3_font();
        set_type3_numbers(
            &mut font,
            b"FontMatrix",
            [0.020_004_3, 0.0, 0.0, 0.020_004_3, 0.0, 0.0],
        );
        set_type3_numbers(&mut font, b"FontBBox", [-6.0, -11.0, 44.0, 38.0]);
        set_type3_numbers(&mut font, b"Widths", [30.0, 47.0]);

        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let scale = 0.020_004_3 * 1000.0;
        let glyphs = loaded.decoder.decode(b"AB", 2, usize::MAX)?;
        assert_eq!(glyphs[0], glyph(b'A', "A", 30.0 * scale));
        assert_eq!(glyphs[1], glyph(b'B', "B", 47.0 * scale));
        assert_eq!(loaded.decoder.ascent_1000_em(), 38.0 * scale);
        assert_eq!(loaded.decoder.descent_1000_em(), -11.0 * scale);

        set_type3_numbers(
            &mut font,
            b"FontMatrix",
            [0.009_994_51, 0.0, 0.0, 0.009_994_51, 0.0, 0.0],
        );
        set_type3_numbers(&mut font, b"FontBBox", [-20.8, -30.2, 131.4, 88.0]);
        set_type3_numbers(&mut font, b"Widths", [50.0, 118.8]);
        let second = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let second_scale = 0.009_994_51 * 1000.0;
        assert_eq!(
            second.decoder.decode(b"B", 1, usize::MAX)?[0].width_1000_em,
            118.8 * second_scale
        );
        assert_eq!(second.decoder.ascent_1000_em(), 88.0 * second_scale);
        assert_eq!(second.decoder.descent_1000_em(), -30.2 * second_scale);

        set_type3_numbers(&mut font, b"FontMatrix", [0.02, 0.0, 0.0, -0.01, 0.0, 0.0]);
        set_type3_numbers(&mut font, b"FontBBox", [-6.0, -11.0, 44.0, 38.0]);
        set_type3_numbers(&mut font, b"Widths", [30.0, 47.0]);
        let reflected = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        assert_eq!(reflected.decoder.ascent_1000_em(), 110.0);
        assert_eq!(reflected.decoder.descent_1000_em(), -380.0);
        assert_eq!(
            reflected.decoder.decode(b"A", 1, usize::MAX)?[0].width_1000_em,
            600.0
        );
        Ok(())
    }

    #[test]
    fn classifies_unrepresentable_and_degenerate_type3_font_matrices() {
        let load = |matrix| {
            let mut font = standard_type3_font();
            set_type3_numbers(&mut font, b"FontMatrix", matrix);
            SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)
        };

        for matrix in [
            [0.0, 0.001, -0.001, 0.0, 0.0, 0.0],
            [0.001, 0.0001, 0.0, 0.001, 0.0, 0.0],
            [0.001, 0.0, 0.0, 0.001, 1.0, 0.0],
            [-0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
        ] {
            assert!(matches!(
                load(matrix),
                Err(Error::Unsupported(message))
                    if message
                        == "Type 3 FontMatrix cannot be represented by horizontal simple-font metrics"
            ));
        }

        for matrix in [
            [0.001, 0.0, 0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 1.0, 0.0, 0.0],
            [f64::NAN, 0.0, 0.0, 0.001, 0.0, 0.0],
            [f64::MAX, 0.0, 0.0, 1.0, 0.0, 0.0],
        ] {
            assert!(matches!(load(matrix), Err(Error::Unresolved(_))));
        }
    }

    #[test]
    fn unused_type3_char_proc_gap_does_not_block_defined_codes() -> Result<()> {
        let mut font = standard_type3_font();
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        let Some(PdfObject::Dictionary(procedures)) = dictionary.get_mut(b"CharProcs".as_slice())
        else {
            unreachable!();
        };
        procedures.remove(b"B".as_slice());
        let loaded = SimpleFontDecoder::load(&MockPdf::default(), &font, LIMITS)?;
        let glyphs = loaded.decoder.decode(b"A", 1, usize::MAX)?;
        assert_eq!(glyphs.len(), 1);
        assert_eq!(glyphs[0].mapping, mapped_text("A"));
        assert_eq!(glyphs[0].raw_code, b"A");
        assert!(matches!(
            loaded.decoder.decode(b"B", 1, usize::MAX),
            Err(Error::Unresolved(_))
        ));
        assert!(matches!(
            loaded.decoder.decode(b"AB", 2, usize::MAX),
            Err(Error::Unresolved(_))
        ));
        Ok(())
    }

    #[test]
    fn type3_to_unicode_cannot_hide_a_missing_procedure() -> Result<()> {
        let mut font = standard_type3_font();
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"CharProcs".to_vec(), PdfObject::Dictionary(PdfDict::new()));
        dictionary.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));
        let mut pdf = MockPdf::default();
        pdf.objects
            .insert(object_ref(1), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(1), b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <005A> endbfchar".to_vec());
        let loaded = SimpleFontDecoder::load(&pdf, &font, LIMITS)?;
        assert!(
            matches!(loaded.decoder.decode(b"A", 1, usize::MAX), Err(Error::Unresolved(message)) if message == "Type 3 font code references a missing CharProc")
        );
        Ok(())
    }

    fn set_type3_numbers<const N: usize>(font: &mut PdfObject, key: &[u8], values: [f64; N]) {
        let PdfObject::Dictionary(dictionary) = font else {
            unreachable!();
        };
        dictionary.insert(
            key.to_vec(),
            PdfObject::Array(values.into_iter().map(PdfObject::Real).collect()),
        );
    }

    fn set_type3_encoding_and_char_procs(
        font: &mut PdfObject,
        differences: Vec<PdfObject>,
        char_procs: PdfDict,
    ) {
        let PdfObject::Dictionary(dictionary) = font else {
            unreachable!();
        };
        dictionary.insert(
            b"Encoding".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                b"Differences".to_vec(),
                PdfObject::Array(differences),
            )])),
        );
        dictionary.insert(b"CharProcs".to_vec(), PdfObject::Dictionary(char_procs));
    }

    fn glyph(code: u8, text: &str, width: f64) -> DecodedGlyph {
        DecodedGlyph {
            raw_code: vec![code],
            mapping: UnicodeMapping::Mapped(text.into()),
            glyph_id: u16::from(code),
            width_1000_em: width,
            vertical: None,
        }
    }

    fn mapped_text(text: &str) -> UnicodeMapping {
        UnicodeMapping::Mapped(text.into())
    }

    fn standard_type3_font() -> PdfObject {
        PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type3".to_vec())),
            (
                b"FontMatrix".to_vec(),
                PdfObject::Array(vec![
                    PdfObject::Real(0.001),
                    PdfObject::Integer(0),
                    PdfObject::Integer(0),
                    PdfObject::Real(0.001),
                    PdfObject::Integer(0),
                    PdfObject::Integer(0),
                ]),
            ),
            (
                b"FontBBox".to_vec(),
                PdfObject::Array(vec![
                    PdfObject::Integer(-100),
                    PdfObject::Integer(-200),
                    PdfObject::Integer(900),
                    PdfObject::Integer(700),
                ]),
            ),
            (b"FirstChar".to_vec(), PdfObject::Integer(65)),
            (b"LastChar".to_vec(), PdfObject::Integer(66)),
            (
                b"Widths".to_vec(),
                PdfObject::Array(vec![PdfObject::Integer(600), PdfObject::Integer(610)]),
            ),
            (
                b"FontDescriptor".to_vec(),
                PdfObject::Dictionary(PdfDict::from([
                    (b"Ascent".to_vec(), PdfObject::Integer(700)),
                    (b"Descent".to_vec(), PdfObject::Integer(-200)),
                ])),
            ),
            (
                b"Encoding".to_vec(),
                PdfObject::Dictionary(PdfDict::from([(
                    b"Differences".to_vec(),
                    PdfObject::Array(vec![
                        PdfObject::Integer(65),
                        PdfObject::Name(b"A".to_vec()),
                        PdfObject::Name(b"B".to_vec()),
                    ]),
                )])),
            ),
            (
                b"CharProcs".to_vec(),
                PdfObject::Dictionary(PdfDict::from([
                    (b"A".to_vec(), PdfObject::Stream(PdfDict::new())),
                    (b"B".to_vec(), PdfObject::Stream(PdfDict::new())),
                ])),
            ),
        ]))
    }

    fn font_with_differences(differences: PdfObject) -> PdfObject {
        font_with_encoding(PdfObject::Dictionary(PdfDict::from([
            (
                b"BaseEncoding".to_vec(),
                PdfObject::Name(b"WinAnsiEncoding".to_vec()),
            ),
            (b"Differences".to_vec(), differences),
        ])))
    }

    fn font_with_encoding(encoding: PdfObject) -> PdfObject {
        PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(b"Helvetica".to_vec())),
            (b"Encoding".to_vec(), encoding),
        ]))
    }

    fn symbol_set_font_with_widths() -> PdfObject {
        let mut font = font_with_encoding(PdfObject::Name(b"SymbolSetEncoding".to_vec()));
        let PdfObject::Dictionary(dictionary) = &mut font else {
            unreachable!();
        };
        dictionary.insert(b"FirstChar".to_vec(), PdfObject::Integer(65));
        dictionary.insert(
            b"Widths".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(600), PdfObject::Integer(610)]),
        );
        font
    }

    fn standard_font(name: &[u8]) -> PdfObject {
        PdfObject::Dictionary(PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(name.to_vec())),
        ]))
    }

    fn object_ref(object_number: u32) -> ObjectRef {
        ObjectRef {
            object_number,
            generation: 0,
        }
    }

    #[derive(Default)]
    struct MockPdf {
        objects: HashMap<ObjectRef, PdfObject>,
        streams: HashMap<ObjectRef, Vec<u8>>,
    }

    impl ParsedPdf for MockPdf {
        fn version(&self) -> PdfVersion {
            PdfVersion { major: 1, minor: 7 }
        }

        fn trailer(&self) -> Result<PdfDict> {
            Ok(PdfDict::new())
        }

        fn resolve(&self, reference: ObjectRef) -> Result<PdfObject> {
            self.objects
                .get(&reference)
                .cloned()
                .ok_or_else(|| Error::Backend("missing mock object".into()))
        }

        fn pages(&self) -> Result<Vec<PageRef>> {
            Ok(Vec::new())
        }

        fn page_dict(&self, _page: PageRef) -> Result<PdfDict> {
            Err(Error::Backend("unused mock method".into()))
        }

        fn raw_stream(&self, _reference: ObjectRef) -> Result<RawStream> {
            Err(Error::Backend("unused mock method".into()))
        }

        fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream> {
            Ok(DecodedStream {
                dictionary: PdfDict::new(),
                bytes: self
                    .streams
                    .get(&reference)
                    .cloned()
                    .ok_or_else(|| Error::Backend("missing mock stream".into()))?,
            })
        }
    }

    #[test]
    fn load_base14_with_exceeded_indirections_returns_limit_exceeded() {
        let pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Name(b"Helvetica".to_vec()))]),
            streams: HashMap::new(),
        };
        let dict = PdfDict::from([(b"BaseFont".to_vec(), PdfObject::Reference(object_ref(1)))]);
        assert!(matches!(
            load_base14(&pdf, &dict, 0),
            Err(Error::LimitExceeded {
                resource: "font object indirections",
                limit: 0,
            })
        ));
    }

    #[test]
    fn load_base14_with_cyclic_reference_returns_limit_exceeded() {
        let pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Reference(object_ref(1)))]),
            streams: HashMap::new(),
        };
        let dict = PdfDict::from([(b"BaseFont".to_vec(), PdfObject::Reference(object_ref(1)))]);
        assert!(matches!(
            load_base14(&pdf, &dict, 4),
            Err(Error::LimitExceeded {
                resource: "font object indirections",
                limit: 4,
            })
        ));
    }

    #[test]
    fn load_base14_with_wrong_type_returns_unresolved() {
        let pdf = MockPdf::default();
        let dict = PdfDict::from([(b"BaseFont".to_vec(), PdfObject::Integer(42))]);
        assert!(matches!(
            load_base14(&pdf, &dict, 32),
            Err(Error::Unresolved(desc)) if desc.contains("BaseFont is not a name")
        ));
    }

    #[test]
    fn load_encoding_with_cyclic_reference_returns_limit_exceeded() {
        let pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Reference(object_ref(1)))]),
            streams: HashMap::new(),
        };
        let dict = PdfDict::from([(b"Encoding".to_vec(), PdfObject::Reference(object_ref(1)))]);
        assert!(matches!(
            load_encoding(
                &pdf,
                &dict,
                FontDecoderLimits {
                    max_indirections: 4,
                    ..LIMITS
                },
                None,
                None
            ),
            Err(Error::LimitExceeded {
                resource: "font object indirections",
                limit: 4,
            })
        ));
    }

    #[test]
    fn load_encoding_with_wrong_type_returns_unresolved() {
        let pdf = MockPdf::default();
        let dict = PdfDict::from([(b"Encoding".to_vec(), PdfObject::Integer(42))]);
        assert!(matches!(
            load_encoding(&pdf, &dict, LIMITS, None, None),
            Err(Error::Unresolved(desc)) if desc.contains("font Encoding is not a name or dictionary")
        ));
    }

    #[test]
    fn load_encoding_with_wrong_base_encoding_type_returns_unresolved() {
        let pdf = MockPdf::default();
        let dict = PdfDict::from([(
            b"Encoding".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                b"BaseEncoding".to_vec(),
                PdfObject::Integer(42),
            )])),
        )]);
        assert!(matches!(
            load_encoding(&pdf, &dict, LIMITS, None, None),
            Err(Error::Unresolved(desc)) if desc.contains("font BaseEncoding is not a name")
        ));
    }

    #[test]
    fn load_encoding_with_unknown_named_encoding_returns_unsupported() {
        let pdf = MockPdf::default();
        let dict = PdfDict::from([(
            b"Encoding".to_vec(),
            PdfObject::Name(b"CustomUnknownEncoding".to_vec()),
        )]);
        assert!(matches!(
            load_encoding(&pdf, &dict, LIMITS, None, None),
            Err(Error::Unsupported(desc)) if desc.contains("simple-font encoding /CustomUnknownEncoding is not supported")
        ));
    }
}
