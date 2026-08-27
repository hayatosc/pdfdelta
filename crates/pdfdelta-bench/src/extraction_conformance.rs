//! Versioned ingress for position-aware extraction snapshots produced by an
//! external implementation.

use std::sync::Arc;

use pdfdelta_core::{
    extraction_conformance::{
        GeometryTolerance, PrimitiveExtractionSnapshot, SnapshotGlyph, SnapshotMismatch,
        compare_snapshots,
    },
    model::{DecodedText, FontProgramHash, PageId, Rect, Vec2},
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{BenchError, Result};

pub const EXTRACTION_ORACLE_SCHEMA_VERSION: u32 = 1;
pub const MAX_EXTRACTION_ORACLE_BYTES: usize = 64 * 1024 * 1024;

const MAX_PRODUCER_NAME_BYTES: usize = 128;
const MAX_PRODUCER_VERSION_BYTES: usize = 64;
const MAX_PARSER_FAMILY_BYTES: usize = 128;
const SHA256_HEX_BYTES: usize = 64;

/// Auditable identity declared by the external snapshot producer.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtractionOracleProducer {
    pub name: String,
    pub version: String,
    pub parser_family: String,
}

/// Result of comparing one external snapshot with the default extractor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionConformanceRecord {
    pub producer: ExtractionOracleProducer,
    pub expected_glyphs: usize,
    pub actual_glyphs: usize,
    pub mismatch: Option<SnapshotMismatch>,
}

impl ExtractionConformanceRecord {
    pub const fn passed(&self) -> bool {
        self.mismatch.is_none()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionOracle {
    schema_version: u32,
    producer: ExtractionOracleProducer,
    input_sha256: String,
    glyphs: Vec<OracleGlyph>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleGlyph {
    text: OracleText,
    page: u32,
    render_order: u32,
    bbox: OracleRect,
    baseline: OraclePoint,
    direction: OraclePoint,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OracleText {
    Mapped {
        value: String,
    },
    Unmapped {
        font_identity_sha256: String,
        glyph_id: u16,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleRect {
    min: OraclePoint,
    max: OraclePoint,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OraclePoint {
    x: f64,
    y: f64,
}

/// Compares one bounded external oracle document with pdfdelta's default
/// parser-backed extractor.
///
/// The oracle producer is responsible for generating position-aware glyphs.
/// This function records its declared parser family but does not certify that
/// the implementation is independent.
///
/// # Errors
///
/// Returns [`BenchError::InvalidInput`] for an oversized or malformed oracle,
/// invalid metadata, a PDF checksum mismatch, invalid glyph evidence, or an
/// invalid tolerance. Returns [`BenchError::Core`] when the default PDF parser
/// or glyph extractor cannot produce complete evidence.
pub fn evaluate_extraction_conformance(
    pdf: Arc<[u8]>,
    oracle_json: &[u8],
    geometry_tolerance: f64,
) -> Result<ExtractionConformanceRecord> {
    if oracle_json.len() > MAX_EXTRACTION_ORACLE_BYTES {
        return Err(invalid(format!(
            "extraction oracle must not exceed {MAX_EXTRACTION_ORACLE_BYTES} bytes"
        )));
    }
    let tolerance =
        GeometryTolerance::new(geometry_tolerance).map_err(|error| invalid(error.to_string()))?;
    let oracle: ExtractionOracle = serde_json::from_slice(oracle_json)
        .map_err(|error| invalid(format!("cannot parse extraction oracle JSON: {error}")))?;
    validate_oracle_header(&oracle, pdf.as_ref())?;

    let ExtractionOracle {
        producer, glyphs, ..
    } = oracle;
    let expected = oracle_snapshot(glyphs)?;
    let expected_glyphs = expected.glyphs.len();

    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let document = source
        .extract_outcome(pdf, ParseLimits::default(), ExtractionLimits::default())
        .map_err(|source| BenchError::Core {
            stage: "extraction conformance",
            source,
        })?
        .into_complete()
        .map_err(|source| BenchError::Core {
            stage: "extraction conformance",
            source,
        })?;
    let actual = PrimitiveExtractionSnapshot::from(&document);
    let actual_glyphs = actual.glyphs.len();
    let mismatch = compare_snapshots(&expected, &actual, tolerance).err();

    Ok(ExtractionConformanceRecord {
        producer,
        expected_glyphs,
        actual_glyphs,
        mismatch,
    })
}

fn validate_oracle_header(oracle: &ExtractionOracle, pdf: &[u8]) -> Result<()> {
    if oracle.schema_version != EXTRACTION_ORACLE_SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported extraction oracle schema version {}; expected {EXTRACTION_ORACLE_SCHEMA_VERSION}",
            oracle.schema_version
        )));
    }
    validate_identity(
        "producer name",
        &oracle.producer.name,
        MAX_PRODUCER_NAME_BYTES,
    )?;
    validate_identity(
        "producer version",
        &oracle.producer.version,
        MAX_PRODUCER_VERSION_BYTES,
    )?;
    validate_identity(
        "producer parser family",
        &oracle.producer.parser_family,
        MAX_PARSER_FAMILY_BYTES,
    )?;
    let expected_hash = parse_sha256("input_sha256", &oracle.input_sha256)?;
    let actual_hash = Sha256::digest(pdf);
    if expected_hash[..] != actual_hash[..] {
        return Err(invalid(format!(
            "extraction oracle input_sha256 does not match the input PDF: expected {}, got {}",
            oracle.input_sha256,
            lowercase_hex(&actual_hash)
        )));
    }
    Ok(())
}

fn validate_identity(field: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.is_empty() || value.trim() != value {
        return Err(invalid(format!(
            "{field} must be non-empty without surrounding whitespace"
        )));
    }
    if value.len() > max_bytes {
        return Err(invalid(format!(
            "{field} must not exceed {max_bytes} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

fn oracle_snapshot(glyphs: Vec<OracleGlyph>) -> Result<PrimitiveExtractionSnapshot> {
    let limits = ExtractionLimits::default();
    if glyphs.len() > limits.max_glyphs {
        return Err(invalid(format!(
            "extraction oracle exceeds the {}-glyph limit",
            limits.max_glyphs
        )));
    }

    let mut mapped_text_bytes = 0_usize;
    let mut snapshot = Vec::with_capacity(glyphs.len());
    for (index, glyph) in glyphs.into_iter().enumerate() {
        validate_geometry(index, glyph.bbox, glyph.baseline, glyph.direction)?;
        let text = match glyph.text {
            OracleText::Mapped { value } => {
                if value.is_empty() {
                    return Err(invalid(format!(
                        "extraction oracle glyph {index} mapped text must not be empty"
                    )));
                }
                mapped_text_bytes =
                    mapped_text_bytes.checked_add(value.len()).ok_or_else(|| {
                        invalid("extraction oracle mapped text byte count overflowed")
                    })?;
                if mapped_text_bytes > limits.max_total_decoded_bytes {
                    return Err(invalid(format!(
                        "extraction oracle exceeds the {}-byte mapped text limit",
                        limits.max_total_decoded_bytes
                    )));
                }
                DecodedText::Mapped(value)
            }
            OracleText::Unmapped {
                font_identity_sha256,
                glyph_id,
            } => DecodedText::Unmapped {
                font_hash: FontProgramHash(
                    parse_sha256("font_identity_sha256", &font_identity_sha256)?.to_vec(),
                ),
                glyph_id,
            },
        };
        snapshot.push(SnapshotGlyph {
            text,
            page: PageId(glyph.page),
            render_order: glyph.render_order,
            bbox: Rect {
                min: glyph.bbox.min.into(),
                max: glyph.bbox.max.into(),
            },
            baseline: glyph.baseline.into(),
            direction: glyph.direction.into(),
            provenance: None,
        });
    }
    Ok(PrimitiveExtractionSnapshot::new(snapshot))
}

fn validate_geometry(
    index: usize,
    bbox: OracleRect,
    baseline: OraclePoint,
    direction: OraclePoint,
) -> Result<()> {
    let coordinates = [
        ("bbox.min.x", bbox.min.x),
        ("bbox.min.y", bbox.min.y),
        ("bbox.max.x", bbox.max.x),
        ("bbox.max.y", bbox.max.y),
        ("baseline.x", baseline.x),
        ("baseline.y", baseline.y),
        ("direction.x", direction.x),
        ("direction.y", direction.y),
    ];
    if let Some((field, value)) = coordinates
        .into_iter()
        .find(|(_, value)| !value.is_finite())
    {
        return Err(invalid(format!(
            "extraction oracle glyph {index} {field} must be finite, got {value}"
        )));
    }
    if bbox.min.x > bbox.max.x || bbox.min.y > bbox.max.y {
        return Err(invalid(format!(
            "extraction oracle glyph {index} bbox minimum must not exceed its maximum"
        )));
    }
    if direction.x.hypot(direction.y) <= f64::EPSILON {
        return Err(invalid(format!(
            "extraction oracle glyph {index} direction must not be zero"
        )));
    }
    Ok(())
}

fn parse_sha256(field: &str, value: &str) -> Result<[u8; 32]> {
    if value.len() != SHA256_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(format!(
            "{field} must be a lowercase 64-character SHA-256 hex digest"
        )));
    }
    let mut digest = [0_u8; 32];
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for (output, pair) in digest.iter_mut().zip(pairs) {
        let high = hex_nibble(pair[0]);
        let low = hex_nibble(pair[1]);
        *output = (high << 4) | low;
    }
    Ok(digest)
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!(),
    }
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

impl From<OraclePoint> for Vec2 {
    fn from(point: OraclePoint) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

fn invalid(message: impl Into<String>) -> BenchError {
    BenchError::InvalidInput(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_schema_versions_before_extraction() {
        let json = br#"{
            "schema_version": 2,
            "producer": {
                "name": "fixture-extractor",
                "version": "1.0.0",
                "parser_family": "fixture-parser"
            },
            "input_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "glyphs": []
        }"#;

        let error = evaluate_extraction_conformance(Arc::from([]), json, 0.25)
            .expect_err("unknown schema must fail");
        assert!(
            error
                .to_string()
                .contains("unsupported extraction oracle schema")
        );
    }

    #[test]
    fn rejects_blank_producer_identity_before_extraction() {
        let json = br#"{
            "schema_version": 1,
            "producer": {
                "name": "",
                "version": "1.0.0",
                "parser_family": "fixture-parser"
            },
            "input_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "glyphs": []
        }"#;

        let error = evaluate_extraction_conformance(Arc::from([]), json, 0.25)
            .expect_err("blank producer identity must fail");
        assert!(
            error
                .to_string()
                .contains("producer name must be non-empty")
        );
    }

    #[test]
    fn rejects_oracle_for_a_different_pdf_before_extraction() {
        let json = br#"{
            "schema_version": 1,
            "producer": {
                "name": "fixture-extractor",
                "version": "1.0.0",
                "parser_family": "fixture-parser"
            },
            "input_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "glyphs": []
        }"#;

        let error = evaluate_extraction_conformance(Arc::from(&b"not empty"[..]), json, 0.25)
            .expect_err("wrong PDF digest must fail");
        assert!(error.to_string().contains("does not match the input PDF"));
    }

    #[test]
    fn preserves_unmapped_font_identity_and_glyph_id() {
        let oracle: ExtractionOracle = serde_json::from_str(
            r#"{
                "schema_version": 1,
                "producer": {
                    "name": "fixture-extractor",
                    "version": "1.0.0",
                    "parser_family": "fixture-parser"
                },
                "input_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "glyphs": [{
                    "text": {
                        "kind": "unmapped",
                        "font_identity_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                        "glyph_id": 42
                    },
                    "page": 0,
                    "render_order": 0,
                    "bbox": {
                        "min": { "x": 1.0, "y": 2.0 },
                        "max": { "x": 3.0, "y": 4.0 }
                    },
                    "baseline": { "x": 1.0, "y": 2.0 },
                    "direction": { "x": 1.0, "y": 0.0 }
                }]
            }"#,
        )
        .expect("unmapped oracle parses");

        let snapshot = oracle_snapshot(oracle.glyphs).expect("unmapped oracle validates");

        assert_eq!(
            snapshot.glyphs[0].text,
            DecodedText::Unmapped {
                font_hash: FontProgramHash(vec![0; 32]),
                glyph_id: 42,
            }
        );
    }

    #[test]
    fn rejects_empty_mapped_text() {
        let error = oracle_snapshot(vec![mapped_glyph("", OraclePoint { x: 1.0, y: 0.0 })])
            .expect_err("empty mapped text must fail");

        assert!(error.to_string().contains("mapped text must not be empty"));
    }

    #[test]
    fn rejects_zero_writing_direction() {
        let error = oracle_snapshot(vec![mapped_glyph("A", OraclePoint { x: 0.0, y: 0.0 })])
            .expect_err("zero direction must fail");

        assert!(error.to_string().contains("direction must not be zero"));
    }

    fn mapped_glyph(value: &str, direction: OraclePoint) -> OracleGlyph {
        OracleGlyph {
            text: OracleText::Mapped {
                value: value.to_owned(),
            },
            page: 0,
            render_order: 0,
            bbox: OracleRect {
                min: OraclePoint { x: 1.0, y: 2.0 },
                max: OraclePoint { x: 3.0, y: 4.0 },
            },
            baseline: OraclePoint { x: 1.0, y: 2.0 },
            direction,
        }
    }
}
