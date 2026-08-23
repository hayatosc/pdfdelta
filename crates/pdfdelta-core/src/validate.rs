//! Shared option validation helpers for layout and alignment configuration.

use crate::{Error, Result};

pub(crate) fn validate_non_negative(name: &str, value: f64) -> Result<()> {
    if value.is_finite() && value >= 0.0 {
        return Ok(());
    }
    Err(Error::InvalidConfiguration(format!(
        "{name} must be finite and non-negative"
    )))
}

pub(crate) fn validate_unit_interval(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(Error::InvalidConfiguration(format!(
            "{name} must be finite and between 0 and 1"
        )));
    }
    Ok(())
}
