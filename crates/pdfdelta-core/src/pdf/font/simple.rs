use std::collections::BTreeMap;

use crate::{
    Error, Result,
    pdf::{ParsedPdf, PdfDict, PdfObject},
};

use super::cmap::{ToUnicodeCMap, UnicodeMapping};
use super::common::{
    FontIdentityDomain, FontIdentitySource, load_to_unicode, non_negative_number, optional_number,
    resolve_font_identity_source, resolve_object,
};
use super::decoder::{DecodedGlyph, FontDecoderLimits};
use super::metrics;

const MAX_ENCODING_DIFFERENCE_ELEMENTS: usize = 512;

#[derive(Clone, Debug)]
pub(crate) struct SimpleFontDecoder {
    to_unicode: Option<ToUnicodeCMap>,
    fallback: FallbackEncoding,
    differences: BTreeMap<u8, DifferenceMapping>,
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
    pub(crate) identity_source: Option<FontIdentitySource>,
    pub(crate) decoded_font_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FallbackEncoding {
    Standard,
    WinAnsi,
    Symbol,
    ZapfDingbats,
    Unknown,
}

struct LoadedEncoding {
    base: FallbackEncoding,
    differences: BTreeMap<u8, DifferenceMapping>,
}

#[derive(Clone, Debug)]
struct DifferenceMapping {
    unicode: UnicodeMapping,
    metric_scalar: Option<char>,
}

impl LoadedEncoding {
    fn standard() -> Self {
        Self {
            base: FallbackEncoding::Standard,
            differences: BTreeMap::new(),
        }
    }
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
    TrueType,
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

        let base14 = load_base14(&dictionary)?;
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
        let (to_unicode, decoded_to_unicode_bytes) = load_to_unicode(pdf, &dictionary, limits)?;
        let identity_source = if dictionary.contains_key(b"Encoding".as_slice()) {
            None
        } else {
            identity_domain(pdf, &dictionary, limits.max_indirections, subtype)?
                .map(|domain| {
                    resolve_font_identity_source(pdf, &dictionary, limits.max_indirections, domain)
                })
                .transpose()?
                .flatten()
        };
        let encoding = if to_unicode.is_some() {
            // deliberate: ToUnicode supplies text mappings, but Standard 14 fonts without
            // explicit Widths still need their base encoding for metric lookup. Differences do
            // not prevent that narrower use of BaseEncoding.
            if widths.is_none() && base14.is_some() {
                load_encoding(pdf, &dictionary, limits, base14)?
            } else {
                LoadedEncoding::standard()
            }
        } else {
            load_encoding(pdf, &dictionary, limits, base14)?
        };
        validate_difference_metrics(base14, widths.as_deref(), &encoding.differences)?;

        Ok(LoadedSimpleFont {
            decoder: Self {
                to_unicode,
                fallback: encoding.base,
                differences: encoding.differences,
                first_char,
                widths,
                missing_width,
                base14,
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
            let mapping = self
                .differences
                .get(code)
                .map(|difference| difference.unicode.clone())
                .unwrap_or_else(|| {
                    fallback_char(self.fallback, *code)
                        .map_or(UnicodeMapping::Unmapped, |character| {
                            UnicodeMapping::Mapped(character.into())
                        })
                });
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
            if let Some(difference) = self.differences.get(&code) {
                return difference
                    .metric_scalar
                    .and_then(|scalar| self.base14.and_then(|font| font.glyph_width(scalar)))
                    .unwrap_or(self.missing_width);
            }
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
                unreachable!()
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

fn validate_subtype(dictionary: &PdfDict) -> Result<SimpleSubtype> {
    match dictionary.get(b"Subtype".as_slice()) {
        Some(PdfObject::Name(name)) if name.as_slice() == b"Type1" => Ok(SimpleSubtype::Type1),
        Some(PdfObject::Name(name)) if name.as_slice() == b"TrueType" => {
            Ok(SimpleSubtype::TrueType)
        }
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
    if matches!(subtype, SimpleSubtype::Type1) {
        return Ok(Some(FontIdentityDomain::SimpleType1BuiltIn));
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

fn load_encoding(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
    base14: Option<Base14>,
) -> Result<LoadedEncoding> {
    let Some(encoding) = dictionary.get(b"Encoding".as_slice()) else {
        let base = match base14 {
            Some(Base14::Symbol) => FallbackEncoding::Symbol,
            Some(Base14::ZapfDingbats) => FallbackEncoding::ZapfDingbats,
            Some(_) => FallbackEncoding::Standard,
            None => FallbackEncoding::Unknown,
        };
        return Ok(LoadedEncoding {
            base,
            differences: BTreeMap::new(),
        });
    };
    match resolve_object(pdf, encoding.clone(), limits.max_indirections)? {
        PdfObject::Name(name) => Ok(LoadedEncoding {
            base: encoding_name(&name)?,
            differences: BTreeMap::new(),
        }),
        PdfObject::Dictionary(encoding) => {
            let base = match encoding.get(b"BaseEncoding".as_slice()) {
                Some(base) => match resolve_object(pdf, base.clone(), limits.max_indirections)? {
                    PdfObject::Name(name) => encoding_name(&name)?,
                    _ => return unresolved("font BaseEncoding is not a name"),
                },
                None => built_in_encoding(base14),
            };
            let differences = match encoding.get(b"Differences".as_slice()) {
                Some(differences) => {
                    let differences =
                        resolve_object(pdf, differences.clone(), limits.max_indirections)?;
                    let PdfObject::Array(differences) = differences else {
                        return unresolved("font Encoding Differences is not an array");
                    };
                    if differences.len() > MAX_ENCODING_DIFFERENCE_ELEMENTS {
                        return Err(Error::LimitExceeded {
                            resource: "simple-font Encoding Differences elements",
                            limit: MAX_ENCODING_DIFFERENCE_ELEMENTS,
                        });
                    }
                    parse_differences(pdf, &differences, limits.max_indirections)?
                }
                None => BTreeMap::new(),
            };
            Ok(LoadedEncoding { base, differences })
        }
        _ => unresolved("font Encoding is not a name or dictionary"),
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
) -> Result<BTreeMap<u8, DifferenceMapping>> {
    let mut mappings = BTreeMap::new();
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
                let mapping = glyph_name_mapping(&name);
                mappings.insert(code, mapping);
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
    Ok(mappings)
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
    limits: FontDecoderLimits,
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

fn glyph_name_mapping(name: &[u8]) -> DifferenceMapping {
    let Some((unicode, metric_scalar)) = adobe_glyph_name(name) else {
        return DifferenceMapping {
            unicode: UnicodeMapping::Unmapped,
            metric_scalar: None,
        };
    };
    DifferenceMapping {
        unicode: UnicodeMapping::Mapped(unicode),
        metric_scalar,
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
        for digits in hex.chunks_exact(4) {
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

        assert_eq!(loaded.decoded_font_bytes, cmap.len());
        assert_eq!(loaded.decoder.cmap_entry_count(), 3);
        assert_eq!(glyphs[0], glyph(b'A', "A", 620.0));
        assert_eq!(glyphs[1], glyph(0x80, "€", 480.0));
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
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

    fn mapped_text(text: &str) -> UnicodeMapping {
        UnicodeMapping::Mapped(text.into())
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
