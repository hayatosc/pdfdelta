//! Strict validation of `FlateDecode` stages inside the PDF backend.
//!
//! The bundled `lopdf` decoder treats corrupt zlib streams as best effort: it
//! warns, optionally retries raw deflate, and returns partial or empty bytes.
//! That silently turns a damaged page into an empty one and a malformed Form
//! into a paint-free one, which breaks the extraction contract that uncertain
//! content is never empty. This module re-validates every `FlateDecode` stage
//! with a strict RFC 1950 decode before the backend accepts the library's
//! decoded bytes.
//!
//! Stage walking is sequential and linear: the raw content is borrowed for
//! the first stage, and each preceding stage is decoded at most once with the
//! library's own facilities to produce the next stage's input. A chain of `n`
//! Flate stages costs `n` strict inflations plus at most `n - 1` library stage
//! decodes, never a repeated prefix decode.
//!
//! Coverage is intentionally narrow: only `FlateDecode` stages are validated.
//! Streams without a Flate stage, filter stages other than Flate, and the
//! library's internal load-time paths (cross-reference streams) keep their
//! existing behavior. A `/Filter` entry that exists but is neither a name nor
//! an array is rejected instead of silently decoding to raw bytes; an absent
//! or null `/Filter` means the stream is uncompressed.

use flate2::{Decompress, FlushDecompress, Status};
use lopdf::{Dictionary, Object, Stream};

use crate::{Error, Result, pdf::ParseLimits};

/// Output buffer for the validation pass. The validated output is discarded,
/// so a fixed buffer keeps validation memory independent of the decoded size;
/// only the byte count is compared against the caller's limit.
const VALIDATION_BUFFER_BYTES: usize = 64 * 1024;

/// Validates every `FlateDecode` stage of `stream` before it is decoded.
///
/// # Errors
///
/// Returns [`Error::Unresolved`] when a Flate stage has an empty encoded
/// payload, a malformed zlib header, a truncated deflate body, or a failing
/// Adler-32 checksum, and when a present `/Filter` entry has an unsupported
/// type. Returns [`Error::LimitExceeded`] when a stage decodes to more bytes
/// than `limits.max_decoded_stream_bytes`, matching the library decoder's
/// per-stage limit.
pub(super) fn validate_stream_flate_stages(
    stream: &Stream,
    limits: ParseLimits,
    context: &str,
) -> Result<()> {
    let filters = match stream.filters() {
        Ok(filters) => filters,
        Err(error) => {
            return match stream.dict.get(b"Filter") {
                Ok(Object::Null) | Err(_) => Ok(()),
                Ok(_) => Err(Error::Unresolved(format!(
                    "PDF stream {context}: malformed Filter declaration: {error}"
                ))),
            };
        }
    };
    if !filters.iter().any(|filter| *filter == b"FlateDecode") {
        return Ok(());
    }
    // `decoded_prefix` holds the decoded output of the stages processed so far
    // and is a size-bounded allocation: at most one stage output, capped by the
    // per-stage decoded limit. The raw content is borrowed, never cloned.
    let mut decoded_prefix: Option<Vec<u8>> = None;
    for (index, filter) in filters.iter().enumerate() {
        let input = decoded_prefix
            .as_deref()
            .unwrap_or(stream.content.as_slice());
        if *filter == b"FlateDecode" {
            validate_zlib_stage(input, index, limits, context)?;
        }
        if !filters[index + 1..]
            .iter()
            .any(|later| *later == b"FlateDecode")
        {
            break;
        }
        decoded_prefix = Some(decode_single_stage(
            stream, filter, input, index, limits, context,
        )?);
    }
    Ok(())
}

/// Decodes exactly one filter stage with the library, so the next stage's
/// input is available without re-decoding earlier stages. The temporary stream
/// carries the original `/DecodeParms`, preserving the library's per-stage
/// predictor and parameter semantics.
fn decode_single_stage(
    stream: &Stream,
    filter: &[u8],
    input: &[u8],
    index: usize,
    limits: ParseLimits,
    context: &str,
) -> Result<Vec<u8>> {
    let mut dictionary = Dictionary::new();
    dictionary.set("Filter", Object::Name(filter.to_vec()));
    if let Ok(parameters) = stream.dict.get(b"DecodeParms") {
        dictionary.set("DecodeParms", parameters.clone());
    }
    Stream::new(dictionary, input.to_vec())
        .get_plain_content_with_limit(limits.max_decoded_stream_bytes)
        .map_err(|error| {
            super::lopdf::map_lopdf_error(
                error,
                &format!("decoding stage {index} of stream {context}"),
                limits,
            )
        })
}

/// Strictly inflates one zlib stage and requires a real stream end.
///
/// `flate2` reports [`Status::StreamEnd`] only after the deflate body and the
/// Adler-32 trailer are complete, so a missing or corrupt trailer is a
/// truncation error rather than a successful short read. Trailing bytes after
/// the trailer are left unconsumed: RFC 1950 places them outside the zlib
/// stream and valid PDFs may carry them.
fn validate_zlib_stage(
    input: &[u8],
    index: usize,
    limits: ParseLimits,
    context: &str,
) -> Result<()> {
    if input.is_empty() {
        return Err(malformed(context, index, "encoded payload is empty"));
    }
    if !zlib_header_is_valid(input) {
        return Err(malformed(context, index, "invalid zlib header"));
    }
    let limit = u64::try_from(limits.max_decoded_stream_bytes)
        .map_err(|_| Error::Backend("FlateDecode limit does not fit in u64".into()))?;
    let mut decoder = Decompress::new(true);
    let mut buffer = vec![0_u8; VALIDATION_BUFFER_BYTES];
    loop {
        let consumed_before = decoder.total_in();
        let produced_before = decoder.total_out();
        let status = decoder
            .decompress(
                &input[usize::try_from(consumed_before).unwrap_or(input.len())..],
                &mut buffer,
                FlushDecompress::None,
            )
            .map_err(|error| malformed(context, index, &error.to_string()))?;
        let consumed = decoder.total_in() - consumed_before;
        let produced = decoder.total_out() - produced_before;
        if decoder.total_out() > limit {
            return Err(Error::LimitExceeded {
                resource: "PDF decoded stream bytes",
                limit: limits.max_decoded_stream_bytes,
            });
        }
        match status {
            Status::StreamEnd => return Ok(()),
            Status::Ok | Status::BufError => {
                if consumed == 0 && produced == 0 {
                    return Err(malformed(
                        context,
                        index,
                        "stream ends before the zlib trailer",
                    ));
                }
            }
        }
    }
}

/// Checks the RFC 1950 two-byte header: compression method 8, window size at
/// most 32 KiB, and the FCHECK remainder that makes the header a multiple of
/// 31. Rejecting the header explicitly keeps the diagnostic exact: `flate2`
/// reports a bad header as a no-progress read, which would otherwise be
/// indistinguishable from a truncated stream.
fn zlib_header_is_valid(input: &[u8]) -> bool {
    let [cmf, flg, ..] = input else {
        return false;
    };
    cmf & 0x0f == 8 && cmf >> 4 <= 7 && (u16::from(*cmf) * 256 + u16::from(*flg)) % 31 == 0
}

fn malformed(context: &str, index: usize, detail: &str) -> Error {
    Error::Unresolved(format!(
        "PDF stream {context}: FlateDecode stage {index} is malformed: {detail}"
    ))
}
