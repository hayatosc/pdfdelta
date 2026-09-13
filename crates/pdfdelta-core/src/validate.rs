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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_negative_validation_rejects_non_finite_and_negative_values() {
        for value in [0.0, -0.0, 1.0, f64::MAX] {
            validate_non_negative("value", value).expect("finite non-negative values are valid");
        }
        for value in [-0.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                validate_non_negative("value", value).is_err(),
                "{value} must be rejected"
            );
        }
    }

    #[test]
    fn unit_interval_validation_accepts_only_finite_bounded_values() {
        for value in [0.0, -0.0, 0.5, 1.0] {
            validate_unit_interval("value", value)
                .expect("values inside the unit interval are valid");
        }
        for value in [
            -0.1,
            1.0 + f64::EPSILON,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert!(
                validate_unit_interval("value", value).is_err(),
                "{value} must be rejected"
            );
        }
    }
}
