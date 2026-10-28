//! Explicit, bounded local-token queries. This command never reads PDFs or
//! updates a comparison, and cannot certify a caller's proposed correspondence.

use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
};

use pdfdelta_core::{
    diff::{LocalTextEnvelopeLimits, LocalTextSide, MAX_ENVELOPE_CELLS, local_text_envelope},
    normalize::ComparableToken,
};
use serde::{Deserialize, Serialize};

const MAX_INPUT_BYTES: usize = 1_048_576;
const MAX_PAIRS: usize = 1_024;
const MAX_TOKENS: usize = 32_768;
const MAX_OUTPUT_BYTES: usize = 8_388_608;
const MAX_WORK: usize = 32_000_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    pairs: Vec<Pair>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    id: String,
    old: InputSide,
    new: InputSide,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum InputSide {
    Text(String),
    Tokens(TokenInput),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenInput {
    tokens: Vec<ComparableToken>,
    /// Caller assertions, not verified PDF provenance. False selects synthetic
    /// positions out of source-change counts while retaining them in alignment.
    source: Option<Vec<bool>>,
}

impl InputSide {
    fn tokens(self) -> Result<(Vec<ComparableToken>, Vec<bool>), String> {
        let (tokens, source) = match self {
            Self::Text(text) => (
                text.chars()
                    .map(ComparableToken::Scalar)
                    .collect::<Vec<_>>(),
                None,
            ),
            Self::Tokens(input) => (input.tokens, input.source),
        };
        if tokens.len() > MAX_TOKENS {
            return Err("input side exceeds 32768 tokens".into());
        }
        let source = source.unwrap_or_else(|| vec![true; tokens.len()]);
        if source.len() != tokens.len() {
            return Err("source mask length differs from tokens".into());
        }
        Ok((tokens, source))
    }
}

#[derive(Serialize)]
struct PairAnswer {
    id: String,
    work_used: usize,
    result: pdfdelta_core::diff::LocalTextEnvelopeResult,
}

#[derive(Serialize)]
struct Answer {
    schema_version: u32,
    interpretation: &'static str,
    input_basis: &'static str,
    strict_status_changed: bool,
    work_limit: usize,
    work_used: usize,
    results: Vec<PairAnswer>,
}

/// Never writes partial JSON: the bounded payload is buffered before stdout.
struct Output {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("output byte limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn query(
    path: &Path,
    max_work: usize,
    max_cells: usize,
    max_output_bytes: usize,
) -> Result<Vec<u8>, String> {
    if max_work > MAX_WORK
        || max_cells > MAX_ENVELOPE_CELLS
        || !(256..=MAX_OUTPUT_BYTES).contains(&max_output_bytes)
    {
        return Err("query limits exceed supported bounds (work 32000000, cells 262144, output 256..8388608)".into());
    }
    let reader: Box<dyn Read> = if path == Path::new("-") {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(path).map_err(|error| error.to_string())?)
    };
    let mut bytes = Vec::new();
    reader
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("input byte limit exceeded".into());
    }
    answer(&bytes, max_work, max_cells, max_output_bytes)
}

fn answer(
    bytes: &[u8],
    max_work: usize,
    max_cells: usize,
    max_output_bytes: usize,
) -> Result<Vec<u8>, String> {
    let request: Request = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if request.pairs.len() > MAX_PAIRS {
        return Err("input pair limit exceeded".into());
    }
    let mut remaining = max_work;
    // Stream each completed pair into the bounded buffer, so a large batch
    // cannot retain one full graph per request in memory.
    let mut output = Output {
        bytes: Vec::new(),
        limit: max_output_bytes.saturating_sub(1),
    };
    let mut results = Vec::new();
    for pair in request.pairs {
        if pair.id.len() > 256 {
            return Err("pair id byte limit exceeded".into());
        }
        let (old, old_source) = pair.old.tokens()?;
        let (new, new_source) = pair.new.tokens()?;
        let old_optional = vec![false; old.len()];
        let new_optional = vec![false; new.len()];
        let before = remaining;
        let result = local_text_envelope(
            LocalTextSide {
                tokens: &old,
                source: &old_source,
                optional: &old_optional,
            },
            LocalTextSide {
                tokens: &new,
                source: &new_source,
                optional: &new_optional,
            },
            &mut remaining,
            LocalTextEnvelopeLimits { max_cells },
        )
        .map_err(|error| error.to_string())?;
        let entry = PairAnswer {
            id: pair.id,
            work_used: before - remaining,
            result,
        };
        if !results.is_empty() {
            output.write_all(b",").map_err(|error| error.to_string())?;
        }
        serde_json::to_writer(&mut output, &entry).map_err(|error| error.to_string())?;
        results.push(());
    }
    // Serialize a small header through the same bounded writer, then insert the
    // already encoded entries. No user string is interpolated into JSON.
    let entries = output.bytes;
    let header = Answer {
        schema_version: 1,
        interpretation: "conditional_on_supplied_order_and_correspondence",
        input_basis: "caller_supplied_literal_tokens_and_source_flags",
        strict_status_changed: false,
        work_limit: max_work,
        work_used: max_work - remaining,
        results: Vec::new(),
    };
    let mut output = Output {
        bytes: Vec::new(),
        limit: max_output_bytes.saturating_sub(1),
    };
    serde_json::to_writer(&mut output, &header).map_err(|error| error.to_string())?;
    // `results` is the final field and was serialized as an empty array.
    let length = output.bytes.len();
    output.bytes.truncate(length - 2);
    output
        .write_all(&entries)
        .map_err(|error| error.to_string())?;
    output.write_all(b"]}").map_err(|error| error.to_string())?;
    // The common CLI emitter adds the single reserved newline.
    Ok(output.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_conditional_and_retains_joint_alternatives() {
        let bytes = answer(
            br#"{"pairs":[{"id":"cross","old":"AB","new":"BA"}]}"#,
            10000,
            100,
            10000,
        )
        .expect("bounded query");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(value["strict_status_changed"], false);
        assert_eq!(
            value["results"][0]["result"]["envelope"]["ambiguous_old"],
            serde_json::json!([0, 1])
        );
    }

    #[test]
    fn query_limits_return_no_truncated_result_or_false_equality() {
        let input = br#"{"pairs":[{"id":"x","old":"AAAA","new":"AA"}]}"#;
        assert!(answer(input, 10000, 100, 256).is_err());
        let bytes = answer(input, 0, 100, 10000).expect("budget refusal is data");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(value["results"][0]["result"]["status"], "unavailable");
        assert!(
            answer(
                br#"{"pairs":[{"id":"x","old":{"tokens":[],"source":[true]},"new":""}]}"#,
                100,
                100,
                10000
            )
            .is_err()
        );
        assert!(
            answer(
                br#"{"pairs":[],"correspondence_certified":true}"#,
                100,
                100,
                10000
            )
            .is_err()
        );
    }

    #[test]
    fn query_output_cap_includes_the_emitter_newline() {
        let input = br#"{"pairs":[{"id":"x","old":"A","new":"B"}]}"#;
        let result = answer(input, 10000, 100, 10000).expect("query");
        let exact = answer(input, 10000, 100, result.len() + 1).expect("exact cap");
        assert_eq!(exact.len() + 1, result.len() + 1);
        assert!(answer(input, 10000, 100, result.len()).is_err());
    }
}
