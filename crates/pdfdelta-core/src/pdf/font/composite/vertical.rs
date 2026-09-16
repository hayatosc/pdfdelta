//! Explicit CID vertical metrics share the horizontal metric-entry budget.

use super::{
    BTreeMap, Error, FontDecoderLimits, ParsedPdf, PdfDict, PdfObject, Result,
    VerticalGlyphMetrics, finite_number, reserve_widths, resolve_cid, resolve_object, unresolved,
};

pub(super) fn load(
    pdf: &dyn ParsedPdf,
    descendant: &PdfDict,
    limits: FontDecoderLimits,
    horizontal_entries: usize,
) -> Result<BTreeMap<u16, VerticalGlyphMetrics>> {
    let Some(object) = descendant.get(b"W2".as_slice()) else {
        return Ok(BTreeMap::new());
    };
    let object = resolve_object(pdf, object.clone(), limits.max_indirections)?;
    let PdfObject::Array(entries) = object else {
        return unresolved("CID font W2 is not an array");
    };
    let mut parsed = BTreeMap::new();
    let mut cursor = 0;
    while cursor < entries.len() {
        let start = resolve_cid(
            pdf,
            &entries[cursor],
            limits.max_indirections,
            "CID vertical metric start",
        )?;
        let second = entries
            .get(cursor + 1)
            .ok_or_else(|| Error::Unresolved("CID font W2 entry is truncated".into()))?;
        let second = resolve_object(pdf, second.clone(), limits.max_indirections)?;
        let (end, values, repeated, consumed) = match second {
            PdfObject::Array(values) => {
                if values.is_empty() || values.len() % 3 != 0 {
                    return unresolved("CID font W2 array must contain complete metric triples");
                }
                let end = usize::from(start)
                    .checked_add(values.len() / 3 - 1)
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| {
                        Error::Unresolved("CID font W2 range exceeds the CID range".into())
                    })?;
                (end, values, false, 2)
            }
            PdfObject::Integer(end) => {
                let end = u16::try_from(end).map_err(|_| {
                    Error::Unresolved("CID vertical metric end is outside the CID range".into())
                })?;
                if end < start {
                    return unresolved("CID font W2 range end precedes its start");
                }
                let values = entries
                    .get(cursor + 2..cursor + 5)
                    .ok_or_else(|| Error::Unresolved("CID font W2 range is truncated".into()))?;
                (end, values.to_vec(), true, 5)
            }
            _ => return unresolved("CID font W2 entry has an invalid metric form"),
        };
        if parsed.range(start..=end).next().is_some() {
            return unresolved("CID font W2 ranges overlap");
        }
        reserve_widths(
            horizontal_entries + parsed.len(),
            usize::from(end - start) + 1,
            limits.max_cid_width_entries,
        )?;
        let mut triples = values.as_chunks::<3>().0.iter();
        let mut previous = None;
        for cid in start..=end {
            let metrics = if let (true, Some(metrics)) = (repeated, previous) {
                metrics
            } else {
                let triple = triples
                    .next()
                    .ok_or_else(|| Error::Unresolved("CID font W2 triple is missing".into()))?;
                let number = |i: usize| {
                    let value = resolve_object(pdf, triple[i].clone(), limits.max_indirections)?;
                    finite_number(&value, "CID vertical metric")
                };
                let metrics = VerticalGlyphMetrics {
                    displacement_y_1000_em: number(0)?,
                    origin_x_1000_em: number(1)?,
                    origin_y_1000_em: number(2)?,
                };
                if metrics.displacement_y_1000_em >= 0.0 {
                    return Err(Error::Unsupported(
                        "CID vertical metrics with non-downward displacement are not supported"
                            .into(),
                    ));
                }
                previous = Some(metrics);
                metrics
            };
            parsed.insert(cid, metrics);
        }
        cursor += consumed;
    }
    Ok(parsed)
}
