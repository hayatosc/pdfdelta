use std::collections::BTreeMap;

use crate::{
    Error, Result,
    pdf::{ParsedPdf, PdfDict, PdfObject},
};

use super::{
    cmap::ToUnicodeCMap,
    common::{
        FontIdentityDomain, FontIdentitySource, finite_number, load_to_unicode,
        non_negative_number, resolve_font_identity_source, resolve_object,
    },
    decoder::{DecodedGlyph, FontDecoderLimits, VerticalGlyphMetrics, WritingMode},
};

#[derive(Clone, Copy, Debug)]
struct DefaultVerticalMetrics {
    displacement_y_1000_em: f64,
    origin_y_1000_em: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct CompositeFontDecoder {
    to_unicode: ToUnicodeCMap,
    widths: BTreeMap<u16, f64>,
    default_width: f64,
    ascent: f64,
    descent: f64,
    writing_mode: WritingMode,
    vertical: Option<DefaultVerticalMetrics>,
}

pub(super) struct LoadedCompositeFont {
    pub(super) decoder: CompositeFontDecoder,
    pub(super) identity_source: Option<FontIdentitySource>,
    pub(super) decoded_font_bytes: usize,
    pub(super) cid_width_entries: usize,
}

impl CompositeFontDecoder {
    pub(super) fn load(
        pdf: &dyn ParsedPdf,
        dictionary: &PdfDict,
        limits: FontDecoderLimits,
    ) -> Result<LoadedCompositeFont> {
        let writing_mode = validate_encoding(pdf, dictionary, limits.max_indirections)?;
        let descendant = load_descendant(pdf, dictionary, limits.max_indirections)?;
        let default_width = load_default_width(pdf, &descendant, limits.max_indirections)?;
        let widths = load_widths(pdf, &descendant, limits)?;
        let vertical = load_vertical_metrics(
            pdf,
            &descendant,
            limits.max_indirections,
            writing_mode,
            default_width,
            &widths,
        )?;
        let (ascent, descent) = load_metrics(pdf, &descendant, limits.max_indirections)?;
        let (to_unicode, decoded_to_unicode_bytes) = load_to_unicode(pdf, dictionary, limits, 2)?;
        let to_unicode = to_unicode.ok_or_else(|| {
            Error::Unsupported("Identity Type0 fonts without ToUnicode are not supported".into())
        })?;
        let identity_source = identity_domain(pdf, &descendant, limits.max_indirections)?
            .map(|domain| {
                resolve_font_identity_source(pdf, &descendant, limits.max_indirections, domain)
            })
            .transpose()?
            .flatten();

        Ok(LoadedCompositeFont {
            cid_width_entries: widths.len(),
            decoder: Self {
                to_unicode,
                widths,
                default_width,
                ascent,
                descent,
                writing_mode,
                vertical,
            },
            identity_source,
            decoded_font_bytes: decoded_to_unicode_bytes,
        })
    }

    pub(super) fn decode(
        &self,
        input: &[u8],
        max_output_glyphs: usize,
        max_mapped_text_bytes: usize,
    ) -> Result<Vec<DecodedGlyph>> {
        if !input.len().is_multiple_of(2) {
            return unresolved("Identity Type0 text code has an odd number of bytes");
        }
        let glyph_count = input.len() / 2;
        if glyph_count > max_output_glyphs {
            return Err(Error::LimitExceeded {
                resource: "decoded composite-font glyphs",
                limit: max_output_glyphs,
            });
        }

        let decoded = self.to_unicode.decode(input, max_mapped_text_bytes)?;
        if decoded.len() != glyph_count || decoded.iter().any(|code| code.source.len() != 2) {
            return unresolved("Identity Type0 ToUnicode source code is not two bytes");
        }
        decoded
            .into_iter()
            .map(|decoded| {
                let raw_code: [u8; 2] = decoded.source.as_slice().try_into().map_err(|_| {
                    Error::Unresolved(
                        "Identity Type0 ToUnicode source code is not two bytes".into(),
                    )
                })?;
                let glyph_id = u16::from_be_bytes(raw_code);
                let width_1000_em = self
                    .widths
                    .get(&glyph_id)
                    .copied()
                    .unwrap_or(self.default_width);
                Ok(DecodedGlyph {
                    raw_code: decoded.source,
                    mapping: decoded.mapping,
                    glyph_id,
                    width_1000_em,
                    vertical: self.vertical.map(|vertical| VerticalGlyphMetrics {
                        displacement_y_1000_em: vertical.displacement_y_1000_em,
                        origin_x_1000_em: width_1000_em / 2.0,
                        origin_y_1000_em: vertical.origin_y_1000_em,
                    }),
                })
            })
            .collect()
    }

    pub(super) fn cmap_entry_count(&self) -> usize {
        self.to_unicode.entry_count()
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

fn validate_encoding(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
) -> Result<WritingMode> {
    let encoding = dictionary
        .get(b"Encoding".as_slice())
        .ok_or_else(|| Error::Unresolved("Type0 font has no Encoding".into()))?;
    match resolve_object(pdf, encoding.clone(), max_indirections)? {
        PdfObject::Name(name) if name.as_slice() == b"Identity-H" => Ok(WritingMode::Horizontal),
        PdfObject::Name(name) if name.as_slice() == b"Identity-V" => Ok(WritingMode::Vertical),
        PdfObject::Name(name) => Err(Error::Unsupported(format!(
            "Type0 encoding /{} is not supported",
            String::from_utf8_lossy(&name)
        ))),
        PdfObject::Dictionary(_) | PdfObject::Stream(_) => Err(Error::Unsupported(
            "custom Type0 encoding CMaps are not supported".into(),
        )),
        _ => unresolved("Type0 Encoding is not a name or CMap"),
    }
}

fn load_descendant(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
) -> Result<PdfDict> {
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
    Ok(descendant)
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
    max_indirections: usize,
    writing_mode: WritingMode,
    default_width: f64,
    widths: &BTreeMap<u16, f64>,
) -> Result<Option<DefaultVerticalMetrics>> {
    if matches!(writing_mode, WritingMode::Horizontal) {
        return Ok(None);
    }
    if descendant.contains_key(b"W2".as_slice()) {
        return Err(Error::Unsupported(
            "Identity-V fonts with per-CID W2 metrics are not supported".into(),
        ));
    }
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
    Ok(Some(DefaultVerticalMetrics {
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
    let ascent = load_metric(pdf, &descriptor, b"Ascent", "ascent", max_indirections)?;
    let descent = load_metric(pdf, &descriptor, b"Descent", "descent", max_indirections)?;
    Ok((ascent, descent))
}

fn load_metric(
    pdf: &dyn ParsedPdf,
    descriptor: &PdfDict,
    key: &[u8],
    context: &str,
    max_indirections: usize,
) -> Result<f64> {
    let value = descriptor
        .get(key)
        .ok_or_else(|| Error::Unresolved(format!("CID font has no {context} metric")))?;
    let value = resolve_object(pdf, value.clone(), max_indirections)?;
    finite_number(&value, context)
}

fn unresolved<T>(message: &str) -> Result<T> {
    Err(Error::Unresolved(message.into()))
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
    fn defaults_cid_width_and_requires_to_unicode() -> Result<()> {
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
        assert!(matches!(
            CompositeFontDecoder::load(&pdf, &without_to_unicode, LIMITS),
            Err(Error::Unsupported(message)) if message.contains("without ToUnicode")
        ));
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
    fn rejects_identity_v_per_cid_vertical_metrics() {
        let mut descendant = descendant_with_widths(PdfObject::Array(Vec::new()));
        let PdfObject::Dictionary(dictionary) = &mut descendant else {
            unreachable!();
        };
        dictionary.insert(b"W2".to_vec(), PdfObject::Array(Vec::new()));
        let pdf = MockPdf::with_descendant(descendant);
        let mut font = font_dictionary();
        font.insert(
            b"Encoding".to_vec(),
            PdfObject::Name(b"Identity-V".to_vec()),
        );

        assert!(matches!(
            CompositeFontDecoder::load(&pdf, &font, LIMITS),
            Err(Error::Unsupported(message)) if message.contains("per-CID W2")
        ));
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
