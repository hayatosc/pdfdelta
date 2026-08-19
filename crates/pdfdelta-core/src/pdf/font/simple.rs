use crate::{
    Error, Result,
    pdf::{ParsedPdf, PdfDict, PdfObject},
};

use super::cmap::{CMapLimits, ToUnicodeCMap, UnicodeMapping, parse_to_unicode};
use super::metrics;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SimpleFontLimits {
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
pub(crate) struct SimpleFontDecoder {
    to_unicode: Option<ToUnicodeCMap>,
    fallback: FallbackEncoding,
    first_char: u8,
    widths: Option<Vec<f64>>,
    missing_width: f64,
    base14: Option<Base14>,
    ascent: f64,
    descent: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedSimpleFont {
    pub(crate) decoder: SimpleFontDecoder,
    pub(crate) decoded_to_unicode_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
enum FallbackEncoding {
    Standard,
    WinAnsi,
    Symbol,
    ZapfDingbats,
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

impl SimpleFontDecoder {
    pub(crate) fn load(
        pdf: &dyn ParsedPdf,
        font: &PdfObject,
        limits: SimpleFontLimits,
    ) -> Result<LoadedSimpleFont> {
        let font = resolve_object(pdf, font.clone(), limits.max_indirections)?;
        let PdfObject::Dictionary(dictionary) = font else {
            return unresolved("font resource is not a dictionary");
        };
        validate_subtype(&dictionary)?;

        let base14 = load_base14(&dictionary)?;
        let fallback = load_encoding(pdf, &dictionary, limits.max_indirections, base14)?;
        let (first_char, widths) = load_widths(pdf, &dictionary, limits)?;
        let (ascent, descent, missing_width) =
            load_descriptor(pdf, &dictionary, limits.max_indirections, base14)?;
        if widths.is_none() && base14.is_none() {
            return unresolved("font without Widths is not a recognized Standard 14 font");
        }
        let ascent =
            ascent.ok_or_else(|| Error::Unresolved("simple font has no ascent metric".into()))?;
        let descent =
            descent.ok_or_else(|| Error::Unresolved("simple font has no descent metric".into()))?;
        if base14.is_none()
            && !dictionary.contains_key(b"Encoding".as_slice())
            && !dictionary.contains_key(b"ToUnicode".as_slice())
        {
            return unresolved(
                "non-standard simple font has neither an Encoding nor a ToUnicode CMap",
            );
        }
        let (to_unicode, decoded_to_unicode_bytes) = load_to_unicode(pdf, &dictionary, limits)?;

        Ok(LoadedSimpleFont {
            decoder: Self {
                to_unicode,
                fallback,
                first_char,
                widths,
                missing_width,
                base14,
                ascent,
                descent,
            },
            decoded_to_unicode_bytes,
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
        if let Some(cmap) = &self.to_unicode {
            return cmap
                .decode(input, max_mapped_text_bytes)?
                .into_iter()
                .map(|decoded| {
                    if decoded.source.len() != 1 {
                        return unresolved("simple-font ToUnicode source code is not one byte");
                    }
                    Ok(self.decoded_glyph(decoded.source[0], decoded.mapping))
                })
                .collect();
        }

        let mut glyphs = Vec::with_capacity(input.len());
        let mut mapped_text_bytes = 0usize;
        for code in input {
            let mapping = match fallback_char(self.fallback, *code) {
                Some(character) => {
                    mapped_text_bytes = mapped_text_bytes.checked_add(character.len_utf8()).ok_or(
                        Error::LimitExceeded {
                            resource: "decoded Unicode text bytes",
                            limit: max_mapped_text_bytes,
                        },
                    )?;
                    if mapped_text_bytes > max_mapped_text_bytes {
                        return Err(Error::LimitExceeded {
                            resource: "decoded Unicode text bytes",
                            limit: max_mapped_text_bytes,
                        });
                    }
                    UnicodeMapping::Mapped(character.into())
                }
                None => UnicodeMapping::Unmapped,
            };
            glyphs.push(self.decoded_glyph(*code, mapping));
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

    fn decoded_glyph(&self, code: u8, mapping: UnicodeMapping) -> DecodedGlyph {
        DecodedGlyph {
            raw_code: vec![code],
            mapping,
            glyph_id: u16::from(code),
            width_1000_em: self.width(code),
        }
    }

    fn width(&self, code: u8) -> f64 {
        let Some(widths) = &self.widths else {
            return self
                .base14
                .map_or(self.missing_width, |font| font.width(self.fallback, code));
        };
        code.checked_sub(self.first_char)
            .and_then(|index| widths.get(usize::from(index)))
            .copied()
            .unwrap_or(self.missing_width)
    }
}

impl Base14 {
    fn width(self, encoding: FallbackEncoding, code: u8) -> f64 {
        let index = usize::from(code);
        let widths = match self {
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                return 600.0;
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
        f64::from(widths[index])
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

fn load_base14(dictionary: &PdfDict) -> Result<Option<Base14>> {
    let Some(base_font) = dictionary.get(b"BaseFont".as_slice()) else {
        return Ok(None);
    };
    let PdfObject::Name(name) = base_font else {
        return unresolved("BaseFont is not a name");
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

fn validate_subtype(dictionary: &PdfDict) -> Result<()> {
    match dictionary.get(b"Subtype".as_slice()) {
        Some(PdfObject::Name(name)) if matches!(name.as_slice(), b"Type1" | b"TrueType") => Ok(()),
        Some(PdfObject::Name(name)) if name.as_slice() == b"Type0" => Err(Error::Unsupported(
            "Type0/CID fonts are not implemented".into(),
        )),
        Some(PdfObject::Name(name)) => Err(Error::Unsupported(format!(
            "font subtype /{} is not supported",
            String::from_utf8_lossy(name)
        ))),
        _ => unresolved("font dictionary has no valid Subtype"),
    }
}

fn load_encoding(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
    base14: Option<Base14>,
) -> Result<FallbackEncoding> {
    let Some(encoding) = dictionary.get(b"Encoding".as_slice()) else {
        return Ok(match base14 {
            Some(Base14::Symbol) => FallbackEncoding::Symbol,
            Some(Base14::ZapfDingbats) => FallbackEncoding::ZapfDingbats,
            _ => FallbackEncoding::Standard,
        });
    };
    match resolve_object(pdf, encoding.clone(), max_indirections)? {
        PdfObject::Name(name) => encoding_name(&name),
        PdfObject::Dictionary(encoding) => {
            if encoding.contains_key(b"Differences".as_slice()) {
                return Err(Error::Unsupported(
                    "simple-font Encoding Differences are not implemented".into(),
                ));
            }
            match encoding.get(b"BaseEncoding".as_slice()) {
                Some(PdfObject::Name(name)) => encoding_name(name),
                None => Ok(FallbackEncoding::Standard),
                _ => unresolved("font BaseEncoding is not a name"),
            }
        }
        _ => unresolved("font Encoding is not a name or dictionary"),
    }
}

fn encoding_name(name: &[u8]) -> Result<FallbackEncoding> {
    match name {
        b"WinAnsiEncoding" => Ok(FallbackEncoding::WinAnsi),
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
    limits: SimpleFontLimits,
) -> Result<(u8, Option<Vec<f64>>)> {
    let Some(widths) = dictionary.get(b"Widths".as_slice()) else {
        return Ok((0, None));
    };
    let first_char = match dictionary.get(b"FirstChar".as_slice()) {
        Some(PdfObject::Integer(value)) => u8::try_from(*value)
            .map_err(|_| Error::Unresolved("FirstChar is outside the byte range".into()))?,
        _ => return unresolved("Widths requires an integer FirstChar"),
    };
    let widths = resolve_object(pdf, widths.clone(), limits.max_indirections)?;
    let PdfObject::Array(widths) = widths else {
        return unresolved("font Widths is not an array");
    };
    if widths.len() > limits.max_width_entries {
        return Err(Error::LimitExceeded {
            resource: "simple-font width entries",
            limit: limits.max_width_entries,
        });
    }
    if usize::from(first_char)
        .checked_add(widths.len())
        .is_none_or(|end| end > 256)
    {
        return unresolved("font Widths extends beyond the byte code range");
    }
    let widths = widths
        .iter()
        .map(|width| non_negative_number(width, "font width"))
        .collect::<Result<Vec<_>>>()?;
    Ok((first_char, Some(widths)))
}

fn load_descriptor(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
    base14: Option<Base14>,
) -> Result<(Option<f64>, Option<f64>, f64)> {
    let Some(descriptor) = dictionary.get(b"FontDescriptor".as_slice()) else {
        return Ok(base14.map_or((None, None, 0.0), |font| {
            let (ascent, descent) = font.vertical_metrics();
            (Some(ascent), Some(descent), 0.0)
        }));
    };
    let descriptor = resolve_object(pdf, descriptor.clone(), max_indirections)?;
    let PdfObject::Dictionary(descriptor) = descriptor else {
        return unresolved("FontDescriptor is not a dictionary");
    };
    let fallback = base14.map(Base14::vertical_metrics);
    let ascent = optional_number(&descriptor, b"Ascent")?.or_else(|| fallback.map(|value| value.0));
    let descent =
        optional_number(&descriptor, b"Descent")?.or_else(|| fallback.map(|value| value.1));
    let missing_width = descriptor
        .get(b"MissingWidth".as_slice())
        .map_or(Ok(0.0), |value| non_negative_number(value, "MissingWidth"))?;
    Ok((ascent, descent, missing_width))
}

fn load_to_unicode(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: SimpleFontLimits,
) -> Result<(Option<ToUnicodeCMap>, usize)> {
    let Some(to_unicode) = dictionary.get(b"ToUnicode".as_slice()) else {
        return Ok((None, 0));
    };
    let reference = resolve_stream_reference(pdf, to_unicode, limits.max_indirections)?;
    let stream = pdf.decoded_stream(reference)?;
    if stream.bytes.len() > limits.max_to_unicode_bytes {
        return Err(Error::LimitExceeded {
            resource: "decoded ToUnicode bytes",
            limit: limits.max_to_unicode_bytes,
        });
    }
    let byte_count = stream.bytes.len();
    let cmap = parse_to_unicode(&stream.bytes, limits.cmap)?;
    Ok((Some(cmap), byte_count))
}

fn resolve_stream_reference(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    max_indirections: usize,
) -> Result<crate::pdf::ObjectRef> {
    let mut current = object.clone();
    for depth in 0..=max_indirections {
        let PdfObject::Reference(reference) = current else {
            return match current {
                PdfObject::Stream(_) => Err(Error::Unsupported(
                    "direct ToUnicode streams are unavailable through the PDF facade".into(),
                )),
                _ => unresolved("ToUnicode is not a stream reference"),
            };
        };
        if depth == max_indirections {
            return limit_indirections(max_indirections);
        }
        current = pdf.resolve(reference)?;
        if matches!(current, PdfObject::Stream(_)) {
            return Ok(reference);
        }
    }
    limit_indirections(max_indirections)
}

fn resolve_object(
    pdf: &dyn ParsedPdf,
    mut object: PdfObject,
    max_indirections: usize,
) -> Result<PdfObject> {
    for depth in 0..=max_indirections {
        let PdfObject::Reference(reference) = object else {
            return Ok(object);
        };
        if depth == max_indirections {
            return limit_indirections(max_indirections);
        }
        object = pdf.resolve(reference)?;
    }
    limit_indirections(max_indirections)
}

fn limit_indirections<T>(limit: usize) -> Result<T> {
    Err(Error::LimitExceeded {
        resource: "font object indirections",
        limit,
    })
}

fn optional_number(dictionary: &PdfDict, key: &[u8]) -> Result<Option<f64>> {
    dictionary
        .get(key)
        .map(|value| finite_number(value, "font metric"))
        .transpose()
}

fn non_negative_number(object: &PdfObject, context: &str) -> Result<f64> {
    let value = finite_number(object, context)?;
    if value < 0.0 {
        return unresolved(&format!("{context} is negative"));
    }
    Ok(value)
}

fn finite_number(object: &PdfObject, context: &str) -> Result<f64> {
    let value = match object {
        PdfObject::Integer(value) => *value as f64,
        PdfObject::Real(value) => *value,
        _ => return unresolved(&format!("{context} is not numeric")),
    };
    if !value.is_finite() {
        return unresolved(&format!("{context} is not finite"));
    }
    Ok(value)
}

fn fallback_char(encoding: FallbackEncoding, code: u8) -> Option<char> {
    match encoding {
        FallbackEncoding::Standard => return standard_encoding_char(code),
        FallbackEncoding::Symbol | FallbackEncoding::ZapfDingbats => return None,
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

fn unresolved<T>(message: &str) -> Result<T> {
    Err(Error::Unresolved(message.into()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::pdf::{DecodedStream, ObjectRef, PageRef, PdfVersion, RawStream};

    use super::*;

    const LIMITS: SimpleFontLimits = SimpleFontLimits {
        max_indirections: 4,
        max_width_entries: 256,
        max_to_unicode_bytes: 4096,
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
        let glyphs = loaded
            .decoder
            .decode(&[b'A', b'B', 0x80, 0x81], 4, usize::MAX)?;

        assert_eq!(loaded.decoded_to_unicode_bytes, 0);
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
    fn resolves_indirect_font_parts_and_uses_to_unicode() -> Result<()> {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
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

        assert_eq!(loaded.decoded_to_unicode_bytes, cmap.len());
        assert_eq!(loaded.decoder.cmap_entry_count(), 3);
        assert_eq!(glyphs[0], glyph(b'A', "A", 620.0));
        assert_eq!(glyphs[1], glyph(0x80, "€", 480.0));
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
        Ok(())
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
                SimpleFontLimits {
                    max_width_entries: 1,
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

    fn glyph(code: u8, text: &str, width: f64) -> DecodedGlyph {
        DecodedGlyph {
            raw_code: vec![code],
            mapping: UnicodeMapping::Mapped(text.into()),
            glyph_id: u16::from(code),
            width_1000_em: width,
        }
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
}
