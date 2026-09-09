use super::{
    EvidenceLimits, EvidenceStore, FieldValue, InterpretationStatus, SourceRef, StructuredValue,
};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
pub struct FormReadingLimits {
    pub max_checks: usize,
    pub max_observations: usize,
    pub max_reading_references: usize,
}

impl Default for FormReadingLimits {
    fn default() -> Self {
        Self {
            max_checks: 1_000_000,
            max_observations: 100_000,
            max_reading_references: 100_000,
        }
    }
}

/// Equality concerns one literal OCR reading, never completeness or author intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormReadingStatus {
    SameLiteralReading,
    DifferentLiteralReading,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormAppearanceReading {
    pub field: u64,
    pub widget: usize,
    pub readings: Vec<u64>,
    pub sources: Vec<SourceRef>,
    pub interpretation: InterpretationStatus,
    pub status: FormReadingStatus,
    pub unresolved: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormAppearanceAnalysis {
    pub observations: Vec<FormAppearanceReading>,
    pub unexamined_regions: Vec<u64>,
    /// False means some widget/readings were not examined within the work budget.
    pub exhaustive: bool,
}

/// Associates literal OCR lines with native text-field widget locations.
/// Crossing boundaries and competing/multiple lines remain unresolved; no reading
/// is rewritten, concatenated, or substituted for a saved field value.
///
/// # Errors
/// Rejects invalid source evidence. Work exhaustion retains earlier observations
/// and an explicit incomplete analysis without changing the source store.
pub fn assess_form_appearances(
    store: &EvidenceStore,
    limits: FormReadingLimits,
) -> Result<FormAppearanceAnalysis> {
    store.validate(EvidenceLimits::default())?;
    let mut by_region = BTreeMap::new();
    for reading in &store.structured {
        if let StructuredValue::RecognizedText {
            region,
            pixel_bounds,
            text,
            ..
        } = &reading.value
        {
            by_region.entry(*region).or_insert_with(Vec::new).push((
                reading.id,
                *pixel_bounds,
                text,
            ));
        }
    }
    let mut result = FormAppearanceAnalysis {
        observations: Vec::new(),
        unexamined_regions: Vec::new(),
        exhaustive: true,
    };
    let mut unexamined = BTreeSet::new();
    let mut checks = 0usize;
    let mut references = 0usize;
    for field in &store.structured {
        let StructuredValue::FormField {
            field_type,
            value,
            widgets,
            ..
        } = &field.value
        else {
            continue;
        };
        for (index, widget) in widgets.iter().enumerate() {
            if result.observations.len() == limits.max_observations {
                result.exhaustive = false;
                if let Some(crop) = &widget.crop {
                    unexamined.insert(crop.page_region);
                }
                continue;
            }
            let mut observation = FormAppearanceReading {
                field: field.id,
                widget: index,
                readings: Vec::new(),
                sources: vec![SourceRef::Structured { element: field.id }],
                interpretation: InterpretationStatus::Inferred,
                status: FormReadingStatus::Unresolved,
                unresolved: None,
            };
            let reason = if field_type.as_deref() != Some(b"Tx") {
                Some("appearance/value reading comparison requires a native text field")
            } else if let (FieldValue::Text(saved), Some(crop)) = (value, &widget.crop) {
                observation.sources.extend([
                    SourceRef::Rendered {
                        region: crop.page_region,
                    },
                    SourceRef::Rendered {
                        region: crop.region,
                    },
                ]);
                let mut texts = Vec::new();
                let mut boundary = false;
                let mut truncated = false;
                for (id, bounds, text) in by_region.get(&crop.page_region).into_iter().flatten() {
                    if checks == limits.max_checks {
                        truncated = true;
                        break;
                    }
                    checks += 1;
                    let area = crop.pixel_bounds;
                    if bounds[0] >= area[2]
                        || area[0] >= bounds[2]
                        || bounds[1] >= area[3]
                        || area[1] >= bounds[3]
                    {
                        continue;
                    }
                    if references == limits.max_reading_references {
                        truncated = true;
                        break;
                    }
                    references += 1;
                    observation.readings.push(*id);
                    observation
                        .sources
                        .push(SourceRef::Structured { element: *id });
                    boundary |= bounds[0] < area[0]
                        || bounds[1] < area[1]
                        || bounds[2] > area[2]
                        || bounds[3] > area[3];
                    texts.push(*text);
                }
                if truncated {
                    result.exhaustive = false;
                    unexamined.insert(crop.page_region);
                    Some("widget reading search budget exhausted")
                } else if boundary {
                    Some("a reading crosses the widget boundary")
                } else if let [text] = texts.as_slice() {
                    observation.status = if *text == saved {
                        FormReadingStatus::SameLiteralReading
                    } else {
                        FormReadingStatus::DifferentLiteralReading
                    };
                    None
                } else if texts.is_empty() {
                    Some("no contained OCR reading is available")
                } else {
                    Some(
                        "multiple OCR lines require an unresolved field reading-order interpretation",
                    )
                }
            } else {
                Some("saved text or source-checked widget crop is unavailable")
            };
            observation.unresolved = reason.map(str::to_owned);
            result.observations.push(observation);
        }
    }
    let mut owners = BTreeMap::new();
    for observation in &result.observations {
        for reading in &observation.readings {
            *owners.entry(*reading).or_insert(0usize) += 1;
        }
    }
    for observation in &mut result.observations {
        let incomplete_region = observation.sources.iter().any(|source| matches!(source, SourceRef::Rendered { region } if unexamined.contains(region)));
        let shared_reading = observation
            .readings
            .iter()
            .any(|reading| owners[reading] > 1);
        if observation.status != FormReadingStatus::Unresolved
            && (incomplete_region || shared_reading)
        {
            observation.status = FormReadingStatus::Unresolved;
            observation.unresolved = Some(
                if incomplete_region {
                    "widget reading associations on this raster remain unexamined"
                } else {
                    "an OCR reading overlaps multiple widget locations"
                }
                .into(),
            );
        }
    }
    result.unexamined_regions = unexamined.into_iter().collect();
    Ok(result)
}
