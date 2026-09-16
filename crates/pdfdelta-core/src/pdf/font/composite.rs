use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use crate::{
    Error, Result,
    pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject},
};

use super::{
    cmap::{ToUnicodeCMap, UnicodeMapping, parse_identity_cid_encoding},
    common::{
        FontIdentityDomain, FontIdentitySource, apply_bbox_vertical_fallback, finite_number,
        load_descriptor_bbox, load_to_unicode_with_width, non_negative_number,
        resolve_font_identity_source, resolve_object, resolve_stream_reference, unresolved,
    },
    decoder::{DecodedGlyph, FontDecoderLimits, VerticalGlyphMetrics, WritingMode},
};

mod predefined;
mod vertical;

use predefined::Encoding;

#[derive(Clone, Debug)]
struct VerticalMetrics {
    overrides: BTreeMap<u16, VerticalGlyphMetrics>,
    displacement_y_1000_em: f64,
    origin_y_1000_em: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct CompositeFontDecoder {
    to_unicode: Option<ToUnicodeCMap>,
    predefined_unicode: Option<ToUnicodeCMap>,
    collection_max_cid: Option<u16>,
    widths: Arc<BTreeMap<u16, f64>>,
    default_width: f64,
    ascent: f64,
    descent: f64,
    encoding: Encoding,
    writing_mode: WritingMode,
    vertical: Option<VerticalMetrics>,
}

#[derive(Clone, Debug)]
struct LoadedEncoding {
    writing_mode: WritingMode,
    encoding: Encoding,
    decoded_bytes: usize,
}

pub(super) struct LoadedCompositeFont {
    pub(super) decoder: CompositeFontDecoder,
    pub(super) identity_source: Option<FontIdentitySource>,
    pub(super) external_identity_allowed: bool,
    pub(super) decoded_font_bytes: usize,
    pub(super) cid_width_entries: usize,
}

/// Width tables belong to one immutable parsed PDF, through `FontDecoderCache`.
/// Only successful font loads publish shared tables; all other metrics stay local.
pub(super) type WidthTables = HashMap<ObjectRef, Arc<BTreeMap<u16, f64>>>;

impl CompositeFontDecoder {
    #[cfg(test)]
    pub(super) fn load(
        pdf: &dyn ParsedPdf,
        dictionary: &PdfDict,
        limits: FontDecoderLimits,
    ) -> Result<LoadedCompositeFont> {
        Self::load_shared(pdf, dictionary, limits, &mut WidthTables::new())
    }

    pub(super) fn load_shared(
        pdf: &dyn ParsedPdf,
        dictionary: &PdfDict,
        limits: FontDecoderLimits,
        tables: &mut WidthTables,
    ) -> Result<LoadedCompositeFont> {
        let encoding = load_encoding(pdf, dictionary, limits)?;
        let writing_mode = encoding.writing_mode;
        let (descendant, reference) = load_descendant(pdf, dictionary, limits.max_indirections)?;
        let collection_max_cid = if matches!(encoding.encoding, Encoding::UniJisUtf16(_)) {
            Some(predefined::collection_max_cid(
                pdf,
                &descendant,
                limits.max_indirections,
            )?)
        } else {
            None
        };
        let default_width = load_default_width(pdf, &descendant, limits.max_indirections)?;
        let shared = reference.and_then(|reference| tables.get(&reference));
        let (widths, new_width_entries) = if let Some(widths) = shared {
            (Arc::clone(widths), 0)
        } else {
            let widths = load_widths(pdf, &descendant, limits)?;
            let entries = widths.len();
            (Arc::new(widths), entries)
        };
        let vertical = load_vertical_metrics(
            pdf,
            &descendant,
            limits,
            writing_mode,
            default_width,
            &widths,
            new_width_entries,
        )?;
        let (ascent, descent) = load_metrics(pdf, &descendant, limits.max_indirections)?;
        let identity_domain = identity_domain(pdf, &descendant, limits.max_indirections)?;
        let external_identity_allowed = identity_domain.is_some();
        let identity_source = identity_domain
            .map(|domain| {
                resolve_font_identity_source(pdf, &descendant, limits.max_indirections, domain)
            })
            .transpose()?
            .flatten();
        let remaining_font_bytes = limits
            .max_decoded_font_bytes
            .checked_sub(encoding.decoded_bytes)
            .ok_or(Error::LimitExceeded {
                resource: "decoded font bytes",
                limit: limits.max_decoded_font_bytes,
            })?;
        let to_unicode_limits = FontDecoderLimits {
            max_decoded_font_bytes: remaining_font_bytes,
            cmap: super::cmap::CMapLimits {
                max_entries: limits
                    .cmap
                    .max_entries
                    .saturating_sub(encoding.encoding.entry_count()),
                ..limits.cmap
            },
            ..limits
        };
        let (to_unicode, decoded_to_unicode_bytes) = load_to_unicode_with_width(
            pdf,
            dictionary,
            to_unicode_limits,
            encoding.encoding.fixed_width(),
        )?;
        if collection_max_cid.is_some()
            && to_unicode
                .as_ref()
                .is_some_and(|cmap| !cmap.uses_only_widths(&[2, 4]))
        {
            return unresolved("UniJIS-UTF16-H ToUnicode codespaces must use two or four bytes");
        }
        let predefined_unicode = if collection_max_cid.is_some() && to_unicode.is_none() {
            if predefined::UNICODE_BYTES.len() > remaining_font_bytes {
                return Err(Error::LimitExceeded {
                    resource: "decoded ToUnicode bytes",
                    limit: remaining_font_bytes,
                });
            }
            Some(super::cmap::parse_to_unicode_for_width(
                predefined::UNICODE_BYTES,
                to_unicode_limits.cmap,
                2,
            )?)
        } else {
            None
        };
        let decoded_to_unicode_bytes = if predefined_unicode.is_some() {
            predefined::UNICODE_BYTES.len()
        } else {
            decoded_to_unicode_bytes
        };
        let decoded_font_bytes = encoding
            .decoded_bytes
            .checked_add(decoded_to_unicode_bytes)
            .ok_or(Error::LimitExceeded {
                resource: "decoded font bytes",
                limit: limits.max_decoded_font_bytes,
            })?;

        if let Some(reference) = reference {
            tables
                .entry(reference)
                .or_insert_with(|| Arc::clone(&widths));
        }
        Ok(LoadedCompositeFont {
            cid_width_entries: new_width_entries
                + vertical.as_ref().map_or(0, |v| v.overrides.len()),
            decoder: Self {
                to_unicode,
                predefined_unicode,
                collection_max_cid,
                widths,
                default_width,
                ascent,
                descent,
                encoding: encoding.encoding,
                writing_mode,
                vertical,
            },
            identity_source,
            external_identity_allowed,
            decoded_font_bytes,
        })
    }

    pub(super) fn decode(
        &self,
        input: &[u8],
        max_output_glyphs: usize,
        max_mapped_text_bytes: usize,
    ) -> Result<Vec<DecodedGlyph>> {
        if let Some(width) = self.encoding.fixed_width() {
            if !input.len().is_multiple_of(width) {
                return if width == 2 {
                    unresolved("Identity Type0 text code has an odd number of bytes")
                } else {
                    unresolved("Type0 text bytes are truncated for the encoding width")
                };
            }
            if input.len() / width > max_output_glyphs {
                return Err(Error::LimitExceeded {
                    resource: "decoded composite-font glyphs",
                    limit: max_output_glyphs,
                });
            }
        }
        let mut glyphs = Vec::with_capacity(
            self.encoding
                .fixed_width()
                .map_or(0, |width| input.len() / width),
        );
        let mut mapped_text_bytes = 0usize;
        let mut remaining = input;
        while !remaining.is_empty() {
            if glyphs.len() >= max_output_glyphs {
                return Err(Error::LimitExceeded {
                    resource: "decoded composite-font glyphs",
                    limit: max_output_glyphs,
                });
            }
            let (width, glyph_id, notdef) = self.encoding.next_code(remaining)?;
            let source = &remaining[..width];
            remaining = &remaining[width..];
            if self
                .collection_max_cid
                .is_some_and(|maximum| glyph_id > maximum)
            {
                return unresolved("Type0 CID exceeds the declared Adobe-Japan1 supplement");
            }
            let mapping = if let Some(cmap) = &self.to_unicode {
                cmap.exact_mapping_entry(source)
            } else if !notdef {
                self.predefined_unicode
                    .as_ref()
                    .and_then(|cmap| cmap.exact_mapping_entry(&glyph_id.to_be_bytes()))
            } else {
                None
            }
            .unwrap_or(UnicodeMapping::Unmapped);
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
            let width_1000_em = self
                .widths
                .get(&glyph_id)
                .copied()
                .unwrap_or(self.default_width);
            glyphs.push(DecodedGlyph {
                raw_code: source.to_vec(),
                mapping,
                glyph_id,
                width_1000_em,
                vertical: self.vertical.as_ref().map(|vertical| {
                    vertical
                        .overrides
                        .get(&glyph_id)
                        .copied()
                        .unwrap_or(VerticalGlyphMetrics {
                            displacement_y_1000_em: vertical.displacement_y_1000_em,
                            origin_x_1000_em: width_1000_em / 2.0,
                            origin_y_1000_em: vertical.origin_y_1000_em,
                        })
                }),
            });
        }
        Ok(glyphs)
    }

    pub(super) fn cmap_entry_count(&self) -> usize {
        self.to_unicode
            .as_ref()
            .map_or(0, ToUnicodeCMap::entry_count)
            + self
                .predefined_unicode
                .as_ref()
                .map_or(0, ToUnicodeCMap::entry_count)
            + self.encoding.entry_count()
    }

    pub(super) fn writing_mode(&self) -> WritingMode {
        self.writing_mode
    }

    pub(super) fn ascent_1000_em(&self) -> f64 {
        self.ascent
    }

    pub(super) fn descent_1000_em(&self) -> f64 {
        self.descent
    }
}

fn load_encoding(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
) -> Result<LoadedEncoding> {
    let encoding = dictionary
        .get(b"Encoding".as_slice())
        .ok_or_else(|| Error::Unresolved("Type0 font has no Encoding".into()))?;
    match resolve_object(pdf, encoding.clone(), limits.max_indirections)? {
        PdfObject::Name(name) if name.as_slice() == b"Identity-H" => Ok(LoadedEncoding {
            writing_mode: WritingMode::Horizontal,
            encoding: Encoding::Identity(2),
            decoded_bytes: 0,
        }),
        PdfObject::Name(name) if name.as_slice() == b"Identity-V" => Ok(LoadedEncoding {
            writing_mode: WritingMode::Vertical,
            encoding: Encoding::Identity(2),
            decoded_bytes: 0,
        }),
        PdfObject::Name(name) if name.as_slice() == b"UniJIS-UTF16-H" => Ok(LoadedEncoding {
            writing_mode: WritingMode::Horizontal,
            encoding: predefined::load(limits)?,
            decoded_bytes: predefined::ENCODING_BYTES.len(),
        }),
        PdfObject::Name(name) => Err(Error::Unsupported(format!(
            "Type0 encoding /{} is not supported",
            String::from_utf8_lossy(&name)
        ))),
        PdfObject::Stream(_) => {
            let reference =
                resolve_stream_reference(pdf, encoding, limits.max_indirections, "Type0 Encoding")?;
            let stream = pdf.decoded_stream(reference)?;
            if stream.bytes.len() > limits.max_decoded_font_bytes {
                return Err(Error::LimitExceeded {
                    resource: "decoded Type0 Encoding bytes",
                    limit: limits.max_decoded_font_bytes,
                });
            }
            let parsed = parse_identity_cid_encoding(&stream.bytes, limits.cmap)?;
            Ok(LoadedEncoding {
                writing_mode: if parsed.vertical {
                    WritingMode::Vertical
                } else {
                    WritingMode::Horizontal
                },
                encoding: Encoding::Identity(parsed.source_width),
                decoded_bytes: stream.bytes.len(),
            })
        }
        PdfObject::Dictionary(_) => Err(Error::Unsupported(
            "direct custom Type0 encoding CMaps are not supported".into(),
        )),
        _ => unresolved("Type0 Encoding is not a name or CMap"),
    }
}

fn load_descendant(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
) -> Result<(PdfDict, Option<ObjectRef>)> {
    let descendants = dictionary
        .get(b"DescendantFonts".as_slice())
        .ok_or_else(|| Error::Unresolved("Type0 font has no DescendantFonts".into()))?;
    let descendants = resolve_object(pdf, descendants.clone(), max_indirections)?;
    let PdfObject::Array(mut descendants) = descendants else {
        return unresolved("Type0 DescendantFonts is not an array");
    };
    match descendants.len() {
        0 => return unresolved("Type0 font has no descendant font"),
        1 => {}
        _ => return unresolved("Type0 font must have exactly one descendant font"),
    }
    let descendant = descendants
        .pop()
        .ok_or_else(|| Error::Unresolved("Type0 font has no descendant font".into()))?;
    let reference = match &descendant {
        PdfObject::Reference(reference) => Some(pdf.terminal_reference(*reference)?),
        _ => None,
    };
    let descendant = resolve_object(pdf, descendant, max_indirections)?;
    let PdfObject::Dictionary(descendant) = descendant else {
        return unresolved("Type0 descendant font is not a dictionary");
    };
    match descendant.get(b"Subtype".as_slice()) {
        Some(PdfObject::Name(name))
            if matches!(name.as_slice(), b"CIDFontType0" | b"CIDFontType2") => {}
        Some(PdfObject::Name(name)) => {
            return Err(Error::Unresolved(format!(
                "Type0 descendant subtype /{} is invalid",
                String::from_utf8_lossy(name)
            )));
        }
        _ => return unresolved("Type0 descendant font has no valid Subtype"),
    }
    Ok((descendant, reference))
}

fn identity_domain(
    pdf: &dyn ParsedPdf,
    descendant: &PdfDict,
    max_indirections: usize,
) -> Result<Option<FontIdentityDomain>> {
    match descendant.get(b"Subtype".as_slice()) {
        Some(PdfObject::Name(name)) if name.as_slice() == b"CIDFontType0" => {
            Ok(Some(FontIdentityDomain::CidFontType0))
        }
        Some(PdfObject::Name(name)) if name.as_slice() == b"CIDFontType2" => {
            let Some(mapping) = descendant.get(b"CIDToGIDMap".as_slice()) else {
                return Ok(Some(FontIdentityDomain::CidFontType2Identity));
            };
            match resolve_object(pdf, mapping.clone(), max_indirections)? {
                PdfObject::Name(name) if name.as_slice() == b"Identity" => {
                    Ok(Some(FontIdentityDomain::CidFontType2Identity))
                }
                _ => Ok(None),
            }
        }
        _ => unresolved("Type0 descendant font has no valid Subtype"),
    }
}

fn load_default_width(
    pdf: &dyn ParsedPdf,
    descendant: &PdfDict,
    max_indirections: usize,
) -> Result<f64> {
    descendant
        .get(b"DW".as_slice())
        .map_or(Ok(1000.0), |width| {
            let width = resolve_object(pdf, width.clone(), max_indirections)?;
            non_negative_number(&width, "CID default width")
        })
}

fn load_widths(
    pdf: &dyn ParsedPdf,
    descendant: &PdfDict,
    limits: FontDecoderLimits,
) -> Result<BTreeMap<u16, f64>> {
    let Some(widths) = descendant.get(b"W".as_slice()) else {
        return Ok(BTreeMap::new());
    };
    let widths = resolve_object(pdf, widths.clone(), limits.max_indirections)?;
    let PdfObject::Array(widths) = widths else {
        return unresolved("CID font W is not an array");
    };
    let mut parsed = BTreeMap::new();
    let mut cursor = 0usize;
    while cursor < widths.len() {
        let start = resolve_cid(
            pdf,
            &widths[cursor],
            limits.max_indirections,
            "CID width start",
        )?;
        let second = widths
            .get(cursor + 1)
            .ok_or_else(|| Error::Unresolved("CID font W entry is truncated".into()))?;
        let second = resolve_object(pdf, second.clone(), limits.max_indirections)?;
        match second {
            PdfObject::Array(values) => {
                if values.is_empty() {
                    return unresolved("CID font W width array is empty");
                }
                let count = values.len();
                let end = usize::from(start)
                    .checked_add(count - 1)
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| {
                        Error::Unresolved("CID font W range exceeds the CID range".into())
                    })?;
                validate_width_overlap(&parsed, start, end)?;
                reserve_widths(parsed.len(), count, limits.max_cid_width_entries)?;
                for (offset, width) in values.into_iter().enumerate() {
                    let width = resolve_object(pdf, width, limits.max_indirections)?;
                    let offset = u16::try_from(offset).map_err(|_| {
                        Error::Unresolved("CID font W range exceeds the CID range".into())
                    })?;
                    let cid = start.checked_add(offset).ok_or_else(|| {
                        Error::Unresolved("CID font W range exceeds the CID range".into())
                    })?;
                    parsed.insert(cid, non_negative_number(&width, "CID width")?);
                }
                cursor += 2;
            }
            PdfObject::Integer(end) => {
                let end = u16::try_from(end).map_err(|_| {
                    Error::Unresolved("CID width end is outside the CID range".into())
                })?;
                if end < start {
                    return unresolved("CID font W range end precedes its start");
                }
                let width = widths
                    .get(cursor + 2)
                    .ok_or_else(|| Error::Unresolved("CID font W range is truncated".into()))?;
                let width = resolve_object(pdf, width.clone(), limits.max_indirections)?;
                let width = non_negative_number(&width, "CID width")?;
                let count = usize::from(end - start) + 1;
                validate_width_overlap(&parsed, start, end)?;
                reserve_widths(parsed.len(), count, limits.max_cid_width_entries)?;
                for cid in start..=end {
                    parsed.insert(cid, width);
                }
                cursor += 3;
            }
            _ => return unresolved("CID font W entry has an invalid width form"),
        }
    }
    Ok(parsed)
}

fn load_vertical_metrics(
    pdf: &dyn ParsedPdf,
    descendant: &PdfDict,
    limits: FontDecoderLimits,
    writing_mode: WritingMode,
    default_width: f64,
    widths: &BTreeMap<u16, f64>,
    new_width_entries: usize,
) -> Result<Option<VerticalMetrics>> {
    if matches!(writing_mode, WritingMode::Horizontal) {
        return Ok(None);
    }
    let max_indirections = limits.max_indirections;
    if default_width <= 0.0 || widths.values().any(|width| *width <= 0.0) {
        return unresolved("Identity-V font has a non-positive horizontal width");
    }

    let (origin_y_1000_em, displacement_y_1000_em) = match descendant.get(b"DW2".as_slice()) {
        None => (880.0, -1000.0),
        Some(metrics) => {
            let metrics = resolve_object(pdf, metrics.clone(), max_indirections)?;
            let PdfObject::Array(metrics) = metrics else {
                return unresolved("CID font DW2 is not an array");
            };
            if metrics.len() != 2 {
                return unresolved("CID font DW2 does not contain two numbers");
            }
            let mut metrics = metrics.into_iter();
            let origin_y = metrics
                .next()
                .ok_or_else(|| Error::Unresolved("CID font DW2 is truncated".into()))?;
            let displacement_y = metrics
                .next()
                .ok_or_else(|| Error::Unresolved("CID font DW2 is truncated".into()))?;
            let origin_y = resolve_object(pdf, origin_y, max_indirections)?;
            let displacement_y = resolve_object(pdf, displacement_y, max_indirections)?;
            (
                finite_number(&origin_y, "CID default vertical origin")?,
                finite_number(&displacement_y, "CID default vertical displacement")?,
            )
        }
    };
    if displacement_y_1000_em >= 0.0 {
        return Err(Error::Unsupported(
            "Identity-V fonts with non-downward default displacement are not supported".into(),
        ));
    }
    Ok(Some(VerticalMetrics {
        overrides: vertical::load(pdf, descendant, limits, new_width_entries)?,
        displacement_y_1000_em,
        origin_y_1000_em,
    }))
}

fn resolve_cid(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    max_indirections: usize,
    context: &str,
) -> Result<u16> {
    match resolve_object(pdf, object.clone(), max_indirections)? {
        PdfObject::Integer(value) => u16::try_from(value)
            .map_err(|_| Error::Unresolved(format!("{context} is outside the CID range"))),
        _ => unresolved(&format!("{context} is not an integer")),
    }
}

fn reserve_widths(current: usize, added: usize, limit: usize) -> Result<()> {
    if current.checked_add(added).is_none_or(|total| total > limit) {
        return Err(Error::LimitExceeded {
            resource: "CID width entries",
            limit,
        });
    }
    Ok(())
}

fn validate_width_overlap(widths: &BTreeMap<u16, f64>, start: u16, end: u16) -> Result<()> {
    if widths.range(start..=end).next().is_some() {
        return unresolved("CID font W ranges overlap");
    }
    Ok(())
}

fn load_metrics(
    pdf: &dyn ParsedPdf,
    descendant: &PdfDict,
    max_indirections: usize,
) -> Result<(f64, f64)> {
    let descriptor = descendant
        .get(b"FontDescriptor".as_slice())
        .ok_or_else(|| Error::Unresolved("CID font has no FontDescriptor".into()))?;
    let descriptor = resolve_object(pdf, descriptor.clone(), max_indirections)?;
    let PdfObject::Dictionary(descriptor) = descriptor else {
        return unresolved("CID FontDescriptor is not a dictionary");
    };
    let mut ascent = load_optional_metric(pdf, &descriptor, b"Ascent", "ascent", max_indirections)?;
    let mut descent =
        load_optional_metric(pdf, &descriptor, b"Descent", "descent", max_indirections)?;
    (ascent, descent) = apply_bbox_vertical_fallback((ascent, descent), || {
        load_descriptor_bbox(pdf, &descriptor, max_indirections)
    })?;
    let ascent = ascent.ok_or_else(|| Error::Unresolved("CID font has no ascent metric".into()))?;
    let descent =
        descent.ok_or_else(|| Error::Unresolved("CID font has no descent metric".into()))?;
    Ok((ascent, descent))
}

fn load_optional_metric(
    pdf: &dyn ParsedPdf,
    descriptor: &PdfDict,
    key: &[u8],
    context: &str,
    max_indirections: usize,
) -> Result<Option<f64>> {
    descriptor
        .get(key)
        .map(|value| {
            let value = resolve_object(pdf, value.clone(), max_indirections)?;
            finite_number(&value, context)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::pdf::{
        DecodedStream, ObjectRef, PageRef, PdfVersion, RawStream,
        font::cmap::{CMapLimits, UnicodeMapping},
    };

    use super::*;

    const LIMITS: FontDecoderLimits = FontDecoderLimits {
        max_indirections: 4,
        max_simple_width_entries: 256,
        max_cid_width_entries: 16,
        max_decoded_font_bytes: 4096,
        cmap: CMapLimits {
            max_entries: 16,
            max_code_bytes: 4,
            max_output_scalars: 16,
        },
    };
    const TO_UNICODE: &[u8] = b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
        1 beginbfrange <01> <0005> <0041> endbfrange";

    fn unijis_fixture() -> (MockPdf, PdfDict, FontDecoderLimits) {
        let mut descendant = descendant_with_widths(PdfObject::Array(vec![
            PdfObject::Integer(34),
            PdfObject::Array(vec![PdfObject::Integer(510)]),
            PdfObject::Integer(3531),
            PdfObject::Array(vec![PdfObject::Integer(920)]),
            PdfObject::Integer(19130),
            PdfObject::Array(vec![PdfObject::Integer(980)]),
        ]));
        let PdfObject::Dictionary(d) = &mut descendant else {
            unreachable!()
        };
        d.insert(
            b"CIDSystemInfo".to_vec(),
            PdfObject::Dictionary(PdfDict::from([
                (b"Registry".to_vec(), PdfObject::String(b"Adobe".to_vec())),
                (b"Ordering".to_vec(), PdfObject::String(b"Japan1".to_vec())),
                (b"Supplement".to_vec(), PdfObject::Integer(5)),
            ])),
        );
        let pdf = MockPdf::with_descendant(descendant);
        let mut font = font_dictionary();
        font.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"UniJIS-UTF16-H".to_vec()),
        );
        font.remove(b"ToUnicode".as_slice());
        let limits = FontDecoderLimits {
            max_decoded_font_bytes: 1_000_000,
            cmap: CMapLimits {
                max_entries: 100_000,
                max_output_scalars: 100_000,
                ..LIMITS.cmap
            },
            ..LIMITS
        };
        (pdf, font, limits)
    }

    #[test]
    fn decodes_unijis_utf16_through_cids_with_raw_surrogate_codes() -> Result<()> {
        let (pdf, font, limits) = unijis_fixture();
        let loaded = CompositeFontDecoder::load(&pdf, &font, limits)?;
        let glyphs =
            loaded
                .decoder
                .decode(&[0, 0x41, 0x5b, 0xcc, 0xd8, 0x84, 0xdf, 0x50], 3, 100)?;
        assert_eq!(
            glyphs.iter().map(|g| g.glyph_id).collect::<Vec<_>>(),
            [34, 3531, 19130]
        );
        assert_eq!(
            glyphs.iter().map(|g| g.width_1000_em).collect::<Vec<_>>(),
            [510.0, 920.0, 980.0]
        );
        assert_eq!(glyphs[0].mapping, mapped("A"));
        assert_eq!(glyphs[1].mapping, mapped("富"));
        assert_eq!(glyphs[2].raw_code, [0xd8, 0x84, 0xdf, 0x50]);
        assert_eq!(glyphs[2].mapping, mapped("\u{31350}"));
        Ok(())
    }

    #[test]
    fn unijis_preserves_canonical_unicode_sequences_and_notdef_evidence() -> Result<()> {
        let (pdf, font, limits) = unijis_fixture();
        let loaded = CompositeFontDecoder::load(&pdf, &font, limits)?;
        let glyphs = loaded
            .decoder
            .decode(&[0x82, 0xa6, 0, 0, 0, 0x5c], 3, 100)?;
        assert_eq!(glyphs[0].mapping, mapped("芦\u{e0100}"));
        assert_eq!(glyphs[0].glyph_id, 1142);
        assert_eq!(glyphs[1].raw_code, [0, 0]);
        assert_eq!(glyphs[1].glyph_id, 1);
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[2].glyph_id, 97);
        assert_eq!(glyphs[2].mapping, mapped("\\"));
        for input in [
            &[0][..],
            &[0xd8, 0],
            &[0xd8, 0, 0, 0x41],
            &[0xdc, 0],
            &[0xff, 0xff],
        ] {
            assert!(matches!(
                loaded.decoder.decode(input, 10, 100),
                Err(Error::Unresolved(_))
            ));
        }
        assert!(matches!(
            loaded.decoder.decode(&[0, 0x41, 0, 0x42], 1, 100),
            Err(Error::LimitExceeded {
                resource: "decoded composite-font glyphs",
                ..
            })
        ));
        assert!(matches!(
            loaded.decoder.decode(&[0x82, 0xa6], 1, 6),
            Err(Error::LimitExceeded {
                resource: "decoded Unicode text bytes",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn unijis_explicit_tounicode_takes_priority_without_filling_gaps() -> Result<()> {
        let (mut pdf, mut font, limits) = unijis_fixture();
        font.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1)));
        pdf.streams.insert(object_ref(1), b"3 begincodespacerange <0000> <D7FF> <D800DC00> <DBFFDFFF> <E000> <FFFF> endcodespacerange 3 beginbfchar <0041> <005A> <5BCC> <D800> <D884DF50> <0058> endbfchar".to_vec());
        let loaded = CompositeFontDecoder::load(&pdf, &font, limits)?;
        let glyphs = loaded.decoder.decode(
            &[0, 0x41, 0, 0x42, 0x5b, 0xcc, 0xd8, 0x84, 0xdf, 0x50],
            4,
            100,
        )?;
        assert_eq!(glyphs[0].mapping, mapped("Z"));
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[3].mapping, mapped("X"));
        assert_eq!(glyphs[3].glyph_id, 19130);
        pdf.streams.insert(object_ref(1), b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <005A> endbfchar".to_vec());
        assert!(matches!(
            CompositeFontDecoder::load(&pdf, &font, limits),
            Err(Error::Unresolved(_))
        ));
        Ok(())
    }

    #[test]
    fn unijis_charges_bundled_maps_against_existing_limits() -> Result<()> {
        let (pdf, font, limits) = unijis_fixture();
        let loaded = CompositeFontDecoder::load(&pdf, &font, limits)?;
        let bytes = loaded.decoded_font_bytes;
        let entries = loaded.decoder.cmap_entry_count();
        assert_eq!(bytes, 482_336);
        assert!(entries > 15_927);
        for reduced in [
            FontDecoderLimits {
                max_decoded_font_bytes: bytes - 1,
                ..limits
            },
            FontDecoderLimits {
                cmap: CMapLimits {
                    max_entries: entries - 1,
                    ..limits.cmap
                },
                ..limits
            },
            FontDecoderLimits {
                cmap: CMapLimits {
                    max_entries: 15_926,
                    ..limits.cmap
                },
                ..limits
            },
            FontDecoderLimits {
                cmap: CMapLimits {
                    max_code_bytes: 2,
                    ..limits.cmap
                },
                ..limits
            },
            FontDecoderLimits {
                cmap: CMapLimits {
                    max_output_scalars: 16,
                    ..limits.cmap
                },
                ..limits
            },
        ] {
            assert!(matches!(
                CompositeFontDecoder::load(&pdf, &font, reduced),
                Err(Error::LimitExceeded { .. })
            ));
        }
        CompositeFontDecoder::load(
            &pdf,
            &font,
            FontDecoderLimits {
                max_decoded_font_bytes: bytes,
                cmap: CMapLimits {
                    max_entries: entries,
                    ..limits.cmap
                },
                ..limits
            },
        )?;
        Ok(())
    }

    #[test]
    fn unijis_validates_collection_and_declared_supplement() -> Result<()> {
        let (mut pdf, font, limits) = unijis_fixture();
        for (key, value) in [
            (b"Registry".as_slice(), PdfObject::String(b"Other".to_vec())),
            (b"Ordering".as_slice(), PdfObject::String(b"GB1".to_vec())),
            (b"Supplement".as_slice(), PdfObject::Integer(8)),
            (b"Supplement".as_slice(), PdfObject::Integer(-1)),
        ] {
            let (mut invalid, _, _) = unijis_fixture();
            let Some(PdfObject::Dictionary(d)) = invalid.objects.get_mut(&object_ref(2)) else {
                unreachable!()
            };
            let Some(PdfObject::Dictionary(info)) = d.get_mut(b"CIDSystemInfo".as_slice()) else {
                unreachable!()
            };
            info.insert(key.to_vec(), value);
            assert!(matches!(
                CompositeFontDecoder::load(&invalid, &font, limits),
                Err(Error::Unresolved(_) | Error::Unsupported(_))
            ));
        }
        let Some(PdfObject::Dictionary(d)) = pdf.objects.get_mut(&object_ref(2)) else {
            unreachable!()
        };
        let Some(PdfObject::Dictionary(info)) = d.get_mut(b"CIDSystemInfo".as_slice()) else {
            unreachable!()
        };
        info.insert(b"Supplement".to_vec(), PdfObject::Integer(0));
        info.insert(b"Registry".to_vec(), PdfObject::Reference(object_ref(3)));
        pdf.objects
            .insert(object_ref(3), PdfObject::String(b"Adobe".to_vec()));
        let loaded = CompositeFontDecoder::load(&pdf, &font, limits)?;
        assert_eq!(
            loaded.decoder.decode(&[0, 0x41], 1, 100)?[0].mapping,
            mapped("A")
        );
        assert!(matches!(
            loaded.decoder.decode(&[0xd8, 0x84, 0xdf, 0x50], 1, 100),
            Err(Error::Unresolved(_))
        ));
        Ok(())
    }

    #[test]
    fn shares_only_native_width_tables_and_retains_per_font_character_mappings() -> Result<()> {
        use super::super::decoder::{FontDecoder, FontDecoderCache};
        let mut pdf = MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Array(vec![PdfObject::Integer(500)]),
        ])));
        pdf.objects
            .insert(object_ref(3), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(
            object_ref(3),
            String::from_utf8_lossy(TO_UNICODE)
                .replace("<0041>", "<0051>")
                .into_bytes(),
        );
        let mut second = font_dictionary();
        second.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(3)));
        let mut cache = FontDecoderCache::new(&pdf);
        let first = cache.load(&PdfObject::Dictionary(font_dictionary()), LIMITS)?;
        let second = cache.load(
            &PdfObject::Dictionary(second),
            FontDecoderLimits {
                max_cid_width_entries: 0,
                ..LIMITS
            },
        )?;
        assert_eq!(first.cid_width_entries, 1);
        assert_eq!(second.cid_width_entries, 0);
        let FontDecoder::Composite(a) = &first.decoder else {
            unreachable!()
        };
        let FontDecoder::Composite(b) = &second.decoder else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(&a.widths, &b.widths));
        let a = first.decoder.decode(&[0, 1], 1, 100)?;
        let b = second.decoder.decode(&[0, 1], 1, 100)?;
        assert_eq!(a[0].mapping, mapped("A"));
        assert_eq!(b[0].mapping, mapped("Q"));
        assert_eq!(a[0].width_1000_em, 500.0);
        assert_eq!(b[0].width_1000_em, 500.0);
        Ok(())
    }

    #[test]
    fn shared_widths_never_publish_failed_loads_or_cross_pdf_boundaries() -> Result<()> {
        use super::super::decoder::FontDecoderCache;
        let pdf = |width| {
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Array(vec![PdfObject::Integer(width)]),
            ])))
        };
        let a = pdf(500);
        let b = pdf(700);
        let mut cache = FontDecoderCache::new(&a);
        let mut bad = font_dictionary();
        bad.insert(b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(999)));
        assert!(cache.load(&PdfObject::Dictionary(bad), LIMITS).is_err());
        assert!(matches!(
            cache.load(
                &PdfObject::Dictionary(font_dictionary()),
                FontDecoderLimits {
                    max_cid_width_entries: 0,
                    ..LIMITS
                }
            ),
            Err(Error::LimitExceeded {
                resource: "CID width entries",
                ..
            })
        ));
        let first = cache.load(&PdfObject::Dictionary(font_dictionary()), LIMITS)?;
        let second =
            FontDecoderCache::new(&b).load(&PdfObject::Dictionary(font_dictionary()), LIMITS)?;
        assert_eq!(first.cid_width_entries, 1);
        assert_eq!(second.cid_width_entries, 1);
        assert_eq!(
            second.decoder.decode(&[0, 1], 1, 100)?[0].width_1000_em,
            700.0
        );
        Ok(())
    }

    #[test]
    fn shared_horizontal_widths_do_not_hide_new_vertical_metric_costs() -> Result<()> {
        use super::super::decoder::FontDecoderCache;
        let mut descendant = descendant_with_widths(PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Array(vec![PdfObject::Integer(500)]),
        ]));
        let PdfObject::Dictionary(dict) = &mut descendant else {
            unreachable!()
        };
        dict.insert(
            b"W2".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Array(vec![
                    PdfObject::Integer(-1000),
                    PdfObject::Integer(250),
                    PdfObject::Integer(800),
                ]),
            ]),
        );
        let pdf = MockPdf::with_descendant(descendant);
        let mut cache = FontDecoderCache::new(&pdf);
        assert_eq!(
            cache
                .load(&PdfObject::Dictionary(font_dictionary()), LIMITS)?
                .cid_width_entries,
            1
        );
        let mut vertical = font_dictionary();
        vertical.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"Identity-V".to_vec()),
        );
        let font = PdfObject::Dictionary(vertical);
        assert!(matches!(
            cache.load(
                &font,
                FontDecoderLimits {
                    max_cid_width_entries: 0,
                    ..LIMITS
                }
            ),
            Err(Error::LimitExceeded {
                resource: "CID width entries",
                ..
            })
        ));
        let result = cache.load(
            &font,
            FontDecoderLimits {
                max_cid_width_entries: 1,
                ..LIMITS
            },
        )?;
        assert_eq!(result.cid_width_entries, 1);
        assert!(
            result.decoder.decode(&[0, 1], 1, 100)?[0]
                .vertical
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn decodes_identity_h_codes_with_cid_widths_and_metrics() -> Result<()> {
        let descendant = descendant_with_widths(PdfObject::Array(vec![
            PdfObject::Integer(3),
            PdfObject::Integer(4),
            PdfObject::Integer(700),
            PdfObject::Integer(1),
            PdfObject::Array(vec![PdfObject::Integer(500), PdfObject::Integer(600)]),
        ]));
        let pdf = MockPdf::with_descendant(descendant);
        let loaded = CompositeFontDecoder::load(&pdf, &font_dictionary(), LIMITS)?;
        let glyphs = loaded
            .decoder
            .decode(&[0, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6], 6, usize::MAX)?;

        assert_eq!(loaded.decoded_font_bytes, TO_UNICODE.len());
        assert_eq!(loaded.decoder.cmap_entry_count(), 6);
        assert_eq!(loaded.decoder.ascent_1000_em(), 880.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -120.0);
        assert_eq!(
            glyphs
                .iter()
                .map(|glyph| (
                    glyph.raw_code.clone(),
                    glyph.glyph_id,
                    glyph.mapping.clone(),
                    glyph.width_1000_em,
                ))
                .collect::<Vec<_>>(),
            [
                (vec![0, 1], 1, mapped("A"), 500.0),
                (vec![0, 2], 2, mapped("B"), 600.0),
                (vec![0, 3], 3, mapped("C"), 700.0),
                (vec![0, 4], 4, mapped("D"), 700.0),
                (vec![0, 5], 5, mapped("E"), 900.0),
                (vec![0, 6], 6, UnicodeMapping::Unmapped, 900.0),
            ]
        );
        Ok(())
    }

    #[test]
    fn decodes_bounded_one_byte_identity_cmap_streams() -> Result<()> {
        const ENCODING: &[u8] = b"/WMode 0 def \
            1 begincodespacerange <00> <FF> endcodespacerange \
            1 begincidrange <00> <FF> 0 endcidrange";
        let mut pdf =
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        pdf.objects
            .insert(object_ref(3), PdfObject::Stream(PdfDict::new()));
        pdf.streams.insert(object_ref(3), ENCODING.to_vec());
        let mut font = font_dictionary();
        font.insert(b"Encoding".to_vec(), PdfObject::Reference(object_ref(3)));
        let limits = FontDecoderLimits {
            cmap: CMapLimits {
                max_entries: 300,
                ..LIMITS.cmap
            },
            ..LIMITS
        };

        let loaded = CompositeFontDecoder::load(&pdf, &font, limits)?;
        let glyphs = loaded.decoder.decode(&[1, 2, 6], 3, usize::MAX)?;

        assert_eq!(loaded.decoded_font_bytes, ENCODING.len() + TO_UNICODE.len());
        assert_eq!(loaded.decoder.writing_mode(), WritingMode::Horizontal);
        assert_eq!(glyphs[0].raw_code, [1]);
        assert_eq!(glyphs[0].glyph_id, 1);
        assert_eq!(glyphs[0].mapping, mapped("A"));
        assert_eq!(glyphs[1].mapping, mapped("B"));
        assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
        Ok(())
    }

    #[test]
    fn uses_descriptor_bbox_when_cid_vertical_metrics_are_missing() -> Result<()> {
        let mut descendant = descendant_with_widths(PdfObject::Array(Vec::new()));
        let PdfObject::Dictionary(dictionary) = &mut descendant else {
            unreachable!();
        };
        dictionary.insert(
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                b"FontBBox".to_vec(),
                PdfObject::Array(vec![
                    PdfObject::Integer(-600),
                    PdfObject::Integer(-200),
                    PdfObject::Integer(1300),
                    PdfObject::Integer(1000),
                ]),
            )])),
        );
        let pdf = MockPdf::with_descendant(descendant);

        let loaded = CompositeFontDecoder::load(&pdf, &font_dictionary(), LIMITS)?;

        assert_eq!(loaded.decoder.ascent_1000_em(), 1000.0);
        assert_eq!(loaded.decoder.descent_1000_em(), -200.0);
        Ok(())
    }

    #[test]
    fn identity_encodings_use_fixed_width_for_sparse_and_explicit_unmapped_entries() -> Result<()> {
        let mut pdf =
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        pdf.streams.insert(
            object_ref(1),
            b"1 begincodespacerange <0001> <0002> endcodespacerange \
              2 beginbfchar <0001> <0041> <0002> <D800> endbfchar"
                .to_vec(),
        );

        for encoding in [b"Identity-H".as_slice(), b"Identity-V".as_slice()] {
            let mut font = font_dictionary();
            font.insert(b"Encoding".to_vec(), PdfObject::Name(encoding.to_vec()));
            let loaded = CompositeFontDecoder::load(&pdf, &font, LIMITS)?;
            let glyphs =
                loaded
                    .decoder
                    .decode(&[0x00, 0x01, 0x00, 0x02, 0x00, 0x03], 3, usize::MAX)?;

            assert_eq!(glyphs[0].raw_code, [0x00, 0x01]);
            assert_eq!(glyphs[0].mapping, mapped("A"));
            assert_eq!(glyphs[1].raw_code, [0x00, 0x02]);
            assert_eq!(glyphs[1].glyph_id, 2);
            assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);
            assert_eq!(glyphs[2].raw_code, [0x00, 0x03]);
            assert_eq!(glyphs[2].glyph_id, 3);
            assert_eq!(glyphs[2].mapping, UnicodeMapping::Unmapped);
        }
        Ok(())
    }

    #[test]
    fn rejects_odd_identity_h_input_and_bounds_glyphs() -> Result<()> {
        let pdf = MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        let loaded = CompositeFontDecoder::load(&pdf, &font_dictionary(), LIMITS)?;

        assert!(matches!(
            loaded.decoder.decode(&[0, 1, 0], 2, usize::MAX),
            Err(Error::Unresolved(message)) if message.contains("odd number of bytes")
        ));
        assert!(matches!(
            loaded.decoder.decode(&[0, 1, 0, 2], 1, usize::MAX),
            Err(Error::LimitExceeded {
                resource: "decoded composite-font glyphs",
                limit: 1,
            })
        ));
        Ok(())
    }

    #[test]
    fn defaults_cid_width_and_allows_missing_to_unicode() -> Result<()> {
        let mut descendant = descendant_with_widths(PdfObject::Array(Vec::new()));
        let PdfObject::Dictionary(ref mut dictionary) = descendant else {
            unreachable!();
        };
        dictionary.remove(b"DW".as_slice());
        let pdf = MockPdf::with_descendant(descendant);
        let loaded = CompositeFontDecoder::load(&pdf, &font_dictionary(), LIMITS)?;
        assert_eq!(
            loaded.decoder.decode(&[0, 5], 1, usize::MAX)?[0].width_1000_em,
            1000.0
        );

        let mut without_to_unicode = font_dictionary();
        without_to_unicode.remove(b"ToUnicode".as_slice());
        let loaded = CompositeFontDecoder::load(&pdf, &without_to_unicode, LIMITS)?;
        let glyph = &loaded.decoder.decode(&[0x12, 0x34], 1, usize::MAX)?[0];
        assert!(loaded.identity_source.is_none());
        assert_eq!(loaded.decoded_font_bytes, 0);
        assert_eq!(loaded.decoder.cmap_entry_count(), 0);
        assert_eq!(glyph.raw_code, [0x12, 0x34]);
        assert_eq!(glyph.glyph_id, 0x1234);
        assert_eq!(glyph.mapping, UnicodeMapping::Unmapped);
        Ok(())
    }

    #[test]
    fn allows_missing_to_unicode_with_stable_descendant_identity() -> Result<()> {
        let mut descendant = descendant_with_widths(PdfObject::Array(Vec::new()));
        let PdfObject::Dictionary(descendant) = &mut descendant else {
            unreachable!();
        };
        descendant.insert(
            b"Subtype".to_vec(),
            PdfObject::Name(b"CIDFontType2".to_vec()),
        );
        let Some(PdfObject::Dictionary(descriptor)) =
            descendant.get_mut(b"FontDescriptor".as_slice())
        else {
            unreachable!();
        };
        descriptor.insert(b"FontFile2".to_vec(), PdfObject::Reference(object_ref(3)));
        let mut pdf = MockPdf::with_descendant(PdfObject::Dictionary(descendant.clone()));
        pdf.objects
            .insert(object_ref(3), PdfObject::Stream(PdfDict::new()));
        let mut font = font_dictionary();
        font.remove(b"ToUnicode".as_slice());

        let loaded = CompositeFontDecoder::load(&pdf, &font, LIMITS)?;
        let glyphs = loaded
            .decoder
            .decode(&[0x12, 0x34, 0xAB, 0xCD], 2, usize::MAX)?;

        assert!(loaded.identity_source.is_some());
        assert_eq!(loaded.decoded_font_bytes, 0);
        assert_eq!(loaded.decoder.cmap_entry_count(), 0);
        assert_eq!(glyphs[0].raw_code, [0x12, 0x34]);
        assert_eq!(glyphs[0].glyph_id, 0x1234);
        assert_eq!(glyphs[0].mapping, UnicodeMapping::Unmapped);
        assert_eq!(glyphs[1].raw_code, [0xAB, 0xCD]);
        assert_eq!(glyphs[1].glyph_id, 0xABCD);
        assert_eq!(glyphs[1].mapping, UnicodeMapping::Unmapped);
        Ok(())
    }

    #[test]
    fn decodes_identity_v_with_default_vertical_metrics() -> Result<()> {
        let mut descendant = descendant_with_widths(PdfObject::Array(Vec::new()));
        let PdfObject::Dictionary(dictionary) = &mut descendant else {
            unreachable!();
        };
        dictionary.insert(
            b"DW2".to_vec(),
            PdfObject::Array(vec![PdfObject::Integer(880), PdfObject::Integer(-1000)]),
        );
        let pdf = MockPdf::with_descendant(descendant);
        let mut font = font_dictionary();
        font.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"Identity-V".to_vec()),
        );

        let loaded = CompositeFontDecoder::load(&pdf, &font, LIMITS)?;
        let glyph = loaded.decoder.decode(&[0, 1], 1, usize::MAX)?[0].clone();

        assert_eq!(loaded.decoder.writing_mode(), WritingMode::Vertical);
        assert_eq!(
            glyph.vertical,
            Some(VerticalGlyphMetrics {
                displacement_y_1000_em: -1000.0,
                origin_x_1000_em: 450.0,
                origin_y_1000_em: 880.0,
            })
        );
        Ok(())
    }

    #[test]
    fn decodes_identity_v_per_cid_vertical_metrics() -> Result<()> {
        let mut descendant = descendant_with_widths(PdfObject::Array(Vec::new()));
        let PdfObject::Dictionary(dictionary) = &mut descendant else {
            unreachable!()
        };
        let numbers = |values: &[i64]| {
            PdfObject::Array(values.iter().copied().map(PdfObject::Integer).collect())
        };
        dictionary.insert(
            b"W2".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                numbers(&[-900, 400, 850, -1100, 450, 900]),
                PdfObject::Integer(4),
                PdfObject::Integer(5),
                PdfObject::Integer(-1200),
                PdfObject::Integer(600),
                PdfObject::Integer(950),
            ]),
        );
        let pdf = MockPdf::with_descendant(descendant);
        let mut font = font_dictionary();
        font.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"Identity-V".to_vec()),
        );
        let loaded = CompositeFontDecoder::load(&pdf, &font, LIMITS)?;
        assert_eq!(loaded.cid_width_entries, 4);
        let glyphs = loaded
            .decoder
            .decode(&[0, 1, 0, 2, 0, 3, 0, 4, 0, 5], 5, usize::MAX)?;
        for (glyph, (dy, x, y)) in glyphs.iter().zip([
            (-900.0, 400.0, 850.0),
            (-1100.0, 450.0, 900.0),
            (-1000.0, 450.0, 880.0),
            (-1200.0, 600.0, 950.0),
            (-1200.0, 600.0, 950.0),
        ]) {
            assert_eq!(
                glyph.vertical,
                Some(VerticalGlyphMetrics {
                    displacement_y_1000_em: dy,
                    origin_x_1000_em: x,
                    origin_y_1000_em: y,
                })
            );
        }
        Ok(())
    }

    fn vertical_fixture(metrics: PdfObject) -> (MockPdf, PdfDict) {
        let mut descendant = descendant_with_widths(PdfObject::Array(Vec::new()));
        let PdfObject::Dictionary(dictionary) = &mut descendant else {
            unreachable!()
        };
        dictionary.insert(b"W2".to_vec(), metrics);
        let mut font = font_dictionary();
        font.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"Identity-V".to_vec()),
        );
        (MockPdf::with_descendant(descendant), font)
    }

    #[test]
    fn rejects_malformed_vertical_metric_tables() {
        let ints = |values: &[i64]| {
            PdfObject::Array(values.iter().copied().map(PdfObject::Integer).collect())
        };
        for value in [
            PdfObject::Integer(1),
            ints(&[1]),
            ints(&[1, 2, -1000, 500]),
            ints(&[2, 1, -1000, 500, 880]),
            ints(&[-1, 2, -1000, 500, 880]),
            ints(&[1, 65536, -1000, 500, 880]),
            ints(&[1, 2, -1000, 500, 880, 2, 3, -1000, 500, 880]),
            PdfObject::Array(vec![PdfObject::Integer(1), ints(&[-1000, 500])]),
            PdfObject::Array(vec![PdfObject::Integer(1), ints(&[])]),
            PdfObject::Array(vec![
                PdfObject::Integer(65535),
                ints(&[-1000, 500, 880, -1000, 500, 880]),
            ]),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Array(vec![
                    PdfObject::Real(f64::NAN),
                    PdfObject::Integer(500),
                    PdfObject::Integer(880),
                ]),
            ]),
        ] {
            let (pdf, font) = vertical_fixture(value);
            assert!(matches!(
                CompositeFontDecoder::load(&pdf, &font, LIMITS),
                Err(Error::Unresolved(_))
            ));
        }
    }

    #[test]
    fn vertical_metric_ranges_share_the_horizontal_entry_limit() -> Result<()> {
        let (mut pdf, font) = vertical_fixture(PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Integer(2),
            PdfObject::Integer(-1000),
            PdfObject::Integer(500),
            PdfObject::Integer(880),
        ]));
        let PdfObject::Dictionary(descendant) = pdf
            .objects
            .get_mut(&object_ref(2))
            .expect("fixture descendant")
        else {
            unreachable!()
        };
        descendant.insert(
            b"W".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Integer(2),
                PdfObject::Integer(1000),
            ]),
        );
        for limit in [3, 4] {
            let result = CompositeFontDecoder::load(
                &pdf,
                &font,
                FontDecoderLimits {
                    max_cid_width_entries: limit,
                    ..LIMITS
                },
            );
            if limit == 3 {
                assert!(matches!(
                    result,
                    Err(Error::LimitExceeded {
                        resource: "CID width entries",
                        limit: 3
                    })
                ));
            } else {
                assert_eq!(result?.cid_width_entries, 4);
            }
        }
        Ok(())
    }

    #[test]
    fn vertical_metrics_resolve_indirect_entries_and_preserve_downward_policy() -> Result<()> {
        let (mut pdf, font) = vertical_fixture(PdfObject::Reference(object_ref(3)));
        pdf.objects.insert(
            object_ref(3),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Reference(object_ref(4)),
            ]),
        );
        pdf.objects.insert(
            object_ref(4),
            PdfObject::Array(vec![
                PdfObject::Reference(object_ref(5)),
                PdfObject::Integer(-20),
                PdfObject::Integer(850),
            ]),
        );
        for displacement in [-900, 0, 900] {
            pdf.objects
                .insert(object_ref(5), PdfObject::Integer(displacement));
            let result = CompositeFontDecoder::load(&pdf, &font, LIMITS);
            if displacement < 0 {
                let glyph = result?.decoder.decode(&[0, 1], 1, usize::MAX)?[0].clone();
                assert_eq!(
                    glyph.vertical,
                    Some(VerticalGlyphMetrics {
                        displacement_y_1000_em: -900.0,
                        origin_x_1000_em: -20.0,
                        origin_y_1000_em: 850.0
                    })
                );
            } else {
                assert!(matches!(result, Err(Error::Unsupported(_))));
            }
        }
        Ok(())
    }

    #[test]
    fn empty_vertical_table_uses_defaults_and_horizontal_fonts_ignore_it() -> Result<()> {
        let (pdf, mut font) = vertical_fixture(PdfObject::Array(Vec::new()));
        let loaded = CompositeFontDecoder::load(&pdf, &font, LIMITS)?;
        assert_eq!(loaded.cid_width_entries, 0);
        assert_eq!(
            loaded.decoder.decode(&[0, 1], 1, usize::MAX)?[0]
                .vertical
                .expect("vertical fixture")
                .origin_x_1000_em,
            450.0
        );
        font.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"Identity-H".to_vec()),
        );
        let (pdf, _) = vertical_fixture(PdfObject::Integer(1));
        assert!(
            CompositeFontDecoder::load(&pdf, &font, LIMITS)?
                .decoder
                .decode(&[0, 1], 1, usize::MAX)?[0]
                .vertical
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn custom_cid_to_gid_maps_never_claim_cid_selector_identity() -> Result<()> {
        let mut pdf =
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        let PdfObject::Dictionary(descendant) = pdf
            .objects
            .get_mut(&object_ref(2))
            .expect("fixture descendant should exist")
        else {
            unreachable!();
        };
        descendant.insert(
            b"Subtype".to_vec(),
            PdfObject::Name(b"CIDFontType2".to_vec()),
        );
        descendant.insert(b"CIDToGIDMap".to_vec(), PdfObject::Reference(object_ref(3)));
        let Some(PdfObject::Dictionary(descriptor)) =
            descendant.get_mut(b"FontDescriptor".as_slice())
        else {
            unreachable!();
        };
        descriptor.insert(b"FontFile2".to_vec(), PdfObject::Reference(object_ref(99)));
        pdf.objects
            .insert(object_ref(3), PdfObject::Stream(PdfDict::new()));

        let loaded = CompositeFontDecoder::load(&pdf, &font_dictionary(), LIMITS)?;

        assert!(loaded.identity_source.is_none());
        Ok(())
    }

    #[test]
    fn rejects_unsupported_encodings_and_descendants() {
        let pdf = MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        let mut missing_encoding = font_dictionary();
        missing_encoding.remove(b"Encoding".as_slice());
        assert!(matches!(
            CompositeFontDecoder::load(&pdf, &missing_encoding, LIMITS),
            Err(Error::Unresolved(message)) if message.contains("has no Encoding")
        ));

        for encoding in [
            PdfObject::Name(b"Adobe-Japan1-UCS2".to_vec()),
            PdfObject::Dictionary(PdfDict::new()),
        ] {
            let mut font = font_dictionary();
            font.insert(b"Encoding".to_vec(), encoding);
            assert!(matches!(
                CompositeFontDecoder::load(&pdf, &font, LIMITS),
                Err(Error::Unsupported(_))
            ));
        }

        let mut multiple = font_dictionary();
        multiple.insert(
            b"DescendantFonts".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Reference(object_ref(2)),
                PdfObject::Reference(object_ref(2)),
            ]),
        );
        assert!(matches!(
            CompositeFontDecoder::load(&pdf, &multiple, LIMITS),
            Err(Error::Unresolved(message)) if message.contains("exactly one descendant")
        ));

        for descendants in [PdfObject::Null, PdfObject::Array(Vec::new())] {
            let mut font = font_dictionary();
            font.insert(b"DescendantFonts".to_vec(), descendants);
            assert!(matches!(
                CompositeFontDecoder::load(&pdf, &font, LIMITS),
                Err(Error::Unresolved(_))
            ));
        }

        let mut invalid_pdf =
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        invalid_pdf.objects.insert(
            object_ref(2),
            PdfObject::Dictionary(PdfDict::from([(
                b"Subtype".to_vec(),
                PdfObject::Name(b"Type1".to_vec()),
            )])),
        );
        assert!(matches!(
            CompositeFontDecoder::load(&invalid_pdf, &font_dictionary(), LIMITS),
            Err(Error::Unresolved(message))
                if message == "Type0 descendant subtype /Type1 is invalid"
        ));

        let mut harmless_keys = font_dictionary();
        harmless_keys.insert(b"UseCMap".to_vec(), PdfObject::Name(b"Parent".to_vec()));
        let mut harmless_pdf =
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        let PdfObject::Dictionary(descendant) = harmless_pdf
            .objects
            .get_mut(&object_ref(2))
            .expect("fixture descendant should exist")
        else {
            unreachable!();
        };
        descendant.insert(b"UseCMap".to_vec(), PdfObject::Name(b"Parent".to_vec()));
        let loaded = CompositeFontDecoder::load(&harmless_pdf, &harmless_keys, LIMITS)
            .expect("dictionary-level UseCMap keys should be harmless");
        assert_eq!(
            loaded
                .decoder
                .decode(&[0, 1], 1, usize::MAX)
                .expect("Identity-H text should decode")[0]
                .mapping,
            mapped("A")
        );

        let mut inherited_cmap_pdf =
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(Vec::new())));
        inherited_cmap_pdf
            .streams
            .insert(object_ref(1), [TO_UNICODE, b" /Parent usecmap"].concat());
        assert!(matches!(
            CompositeFontDecoder::load(&inherited_cmap_pdf, &font_dictionary(), LIMITS),
            Err(Error::Unsupported(message)) if message.contains("inheritance")
        ));
    }

    #[test]
    fn rejects_invalid_or_overlapping_width_entries() {
        let invalid = [
            PdfObject::Name(b"not-an-array".to_vec()),
            PdfObject::Array(vec![PdfObject::Integer(1)]),
            PdfObject::Array(vec![PdfObject::Integer(1), PdfObject::Array(Vec::new())]),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Array(vec![PdfObject::Integer(-1)]),
            ]),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Array(vec![PdfObject::Integer(500), PdfObject::Integer(600)]),
                PdfObject::Integer(2),
                PdfObject::Integer(3),
                PdfObject::Integer(700),
            ]),
            PdfObject::Array(vec![
                PdfObject::Integer(2),
                PdfObject::Integer(3),
                PdfObject::Integer(700),
                PdfObject::Integer(1),
                PdfObject::Array(vec![PdfObject::Integer(500), PdfObject::Integer(600)]),
            ]),
            PdfObject::Array(vec![
                PdfObject::Integer(3),
                PdfObject::Integer(2),
                PdfObject::Integer(700),
            ]),
            PdfObject::Array(vec![
                PdfObject::Integer(65536),
                PdfObject::Array(vec![PdfObject::Integer(500)]),
            ]),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Array(vec![PdfObject::Real(f64::NAN)]),
            ]),
        ];

        for widths in invalid {
            let pdf = MockPdf::with_descendant(descendant_with_widths(widths));
            assert!(matches!(
                CompositeFontDecoder::load(&pdf, &font_dictionary(), LIMITS),
                Err(Error::Unresolved(_))
            ));
        }
    }

    #[test]
    fn bounds_width_entries_and_indirections() {
        let pdf = MockPdf::with_descendant(descendant_with_widths(PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Array(vec![
                PdfObject::Integer(500),
                PdfObject::Integer(600),
                PdfObject::Integer(700),
            ]),
        ])));
        assert!(matches!(
            CompositeFontDecoder::load(
                &pdf,
                &font_dictionary(),
                FontDecoderLimits {
                    max_cid_width_entries: 2,
                    ..LIMITS
                }
            ),
            Err(Error::LimitExceeded {
                resource: "CID width entries",
                limit: 2,
            })
        ));

        let mut cyclic =
            MockPdf::with_descendant(descendant_with_widths(PdfObject::Reference(object_ref(3))));
        cyclic
            .objects
            .insert(object_ref(3), PdfObject::Reference(object_ref(3)));
        assert!(matches!(
            CompositeFontDecoder::load(&cyclic, &font_dictionary(), LIMITS),
            Err(Error::LimitExceeded {
                resource: "font object indirections",
                limit: 4,
            })
        ));
    }

    fn mapped(text: &str) -> UnicodeMapping {
        UnicodeMapping::Mapped(text.into())
    }

    fn font_dictionary() -> PdfDict {
        PdfDict::from([
            (b"Subtype".to_vec(), PdfObject::Name(b"Type0".to_vec())),
            (
                b"Encoding".to_vec(),
                PdfObject::Name(b"Identity-H".to_vec()),
            ),
            (
                b"DescendantFonts".to_vec(),
                PdfObject::Array(vec![PdfObject::Reference(object_ref(2))]),
            ),
            (b"ToUnicode".to_vec(), PdfObject::Reference(object_ref(1))),
        ])
    }

    fn descendant_with_widths(widths: PdfObject) -> PdfObject {
        PdfObject::Dictionary(PdfDict::from([
            (
                b"Subtype".to_vec(),
                PdfObject::Name(b"CIDFontType0".to_vec()),
            ),
            (b"DW".to_vec(), PdfObject::Integer(900)),
            (b"W".to_vec(), widths),
            (
                b"FontDescriptor".to_vec(),
                PdfObject::Dictionary(PdfDict::from([
                    (b"Ascent".to_vec(), PdfObject::Integer(880)),
                    (b"Descent".to_vec(), PdfObject::Integer(-120)),
                ])),
            ),
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

    impl MockPdf {
        fn with_descendant(descendant: PdfObject) -> Self {
            Self {
                objects: HashMap::from([
                    (object_ref(1), PdfObject::Stream(PdfDict::new())),
                    (object_ref(2), descendant),
                ]),
                streams: HashMap::from([(object_ref(1), TO_UNICODE.to_vec())]),
            }
        }
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
