use crate::{
    Error, Result,
    pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject},
};

use super::{
    cmap::{ToUnicodeCMap, parse_to_unicode},
    decoder::FontDecoderLimits,
};

pub(super) fn load_to_unicode(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
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
) -> Result<ObjectRef> {
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

pub(super) fn resolve_object(
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

pub(super) fn optional_number(dictionary: &PdfDict, key: &[u8]) -> Result<Option<f64>> {
    dictionary
        .get(key)
        .map(|value| finite_number(value, "font metric"))
        .transpose()
}

pub(super) fn non_negative_number(object: &PdfObject, context: &str) -> Result<f64> {
    let value = finite_number(object, context)?;
    if value < 0.0 {
        return unresolved(&format!("{context} is negative"));
    }
    Ok(value)
}

pub(super) fn finite_number(object: &PdfObject, context: &str) -> Result<f64> {
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

fn unresolved<T>(message: &str) -> Result<T> {
    Err(Error::Unresolved(message.into()))
}
