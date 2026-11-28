//! Provenance and small numeric helpers shared by benchmark tooling.
//!
//! Quality records for the generated fixture matrix live in [`crate::evaluator`].
//! The removed revision evaluation schema is not resurrected here; the
//! remaining helpers are report- and engine-neutral.

use pdfdelta_core::span::ChangeKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{BenchError, Result};

/// Whether a document was used to change implementation or tuning choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuningUse {
    Unused,
    UsedForFix,
}

impl TuningUse {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unused => "unused",
            Self::UsedForFix => "used_for_fix",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "unused" => Some(Self::Unused),
            "used_for_fix" | "used_for_tuning" => Some(Self::UsedForFix),
            _ => None,
        }
    }
}

/// Provenance that must travel with a revision pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkProvenance {
    pub document_series_id: String,
    pub derivation_group: String,
    pub producer_family: String,
    pub producer_version: String,
    pub annotation_scope: String,
    pub first_evaluated: String,
    pub tuning_use: TuningUse,
}

impl BenchmarkProvenance {
    /// Parses the seven provenance columns appended to a manifest row.
    ///
    /// # Errors
    ///
    /// Rejects missing columns, blank or control-character values, invalid
    /// dates, and unknown scopes or tuning uses.
    pub fn parse(columns: &[&str], line_number: usize) -> Result<Self> {
        const PROVENANCE_COLUMNS: [&str; 7] = [
            "document_series_id",
            "derivation_group",
            "producer_family",
            "producer_version",
            "annotation_scope",
            "first_evaluated",
            "tuning_use",
        ];
        if columns.len() != PROVENANCE_COLUMNS.len() {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has {} provenance columns, expected {}",
                columns.len(),
                PROVENANCE_COLUMNS.len()
            )));
        }
        let fields = columns
            .iter()
            .zip(PROVENANCE_COLUMNS)
            .map(|(value, name)| require_nonblank(value, name, line_number))
            .collect::<Result<Vec<_>>>()?;
        let tuning_use = TuningUse::parse(&fields[6]).ok_or_else(|| {
            BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has invalid tuning_use {:?}",
                fields[6]
            ))
        })?;
        validate_date(&fields[5], "first_evaluated", line_number)?;
        if !matches!(
            fields[4].as_str(),
            "none" | "complete" | "partial" | "scoped_complete"
        ) {
            return Err(BenchError::InvalidInput(format!(
                "revision manifest row {line_number} has invalid annotation_scope {:?}",
                fields[4]
            )));
        }
        Ok(Self {
            document_series_id: fields[0].clone(),
            derivation_group: fields[1].clone(),
            producer_family: fields[2].clone(),
            producer_version: fields[3].clone(),
            annotation_scope: fields[4].clone(),
            first_evaluated: fields[5].clone(),
            tuning_use,
        })
    }

    /// A known producer family remains useful evidence when its version is
    /// unavailable; unknown families do not count as producer validation.
    #[must_use]
    pub fn known_producer(&self) -> bool {
        !self.producer_family.eq_ignore_ascii_case("unknown")
    }
}

fn require_nonblank(value: &str, column: &str, line_number: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.contains(['\t', '\r', '\n']) {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has blank or invalid {column}"
        )));
    }
    Ok(value.to_owned())
}

fn validate_date(value: &str, column: &str, line_number: usize) -> Result<()> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if !valid_shape {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {column} {value:?}; expected YYYY-MM-DD"
        )));
    }
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    let year = value[0..4].parse::<u16>().unwrap_or_default();
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    };
    if max_day == 0 || !(1..=max_day).contains(&day) {
        return Err(BenchError::InvalidInput(format!(
            "revision manifest row {line_number} has invalid {column} {value:?}"
        )));
    }
    Ok(())
}

/// Formats bytes as lowercase hexadecimal.
#[must_use]
pub fn hex_digest(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

/// Hashes bytes using the same lowercase SHA-256 representation as the
/// revision manifest.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

/// Exact ratio, or `None` for an empty denominator.
pub(crate) fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

/// Stable lowercase label for a semantic change kind.
pub(crate) fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Replacement => "replacement",
        ChangeKind::Insertion => "insertion",
        ChangeKind::Deletion => "deletion",
        ChangeKind::Move => "move",
    }
}
