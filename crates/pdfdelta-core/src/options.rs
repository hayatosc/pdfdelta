//! Layout and bounded-work options shared by the multi-channel path.
//!
//! These options never select or configure a text comparison. The flow engine
//! keeps its own extraction and ordering options.

use crate::{
    Error, Result,
    layout::{BlockOptions, LineOptions},
};

/// Minimum shared anchor length used by the native footer matcher.
pub const DEFAULT_ANCHOR_MIN_TOKENS: usize = 16;

/// Layout and work limits used to build evidence-backed views.
///
/// The footer work limit bounds one shared multi-channel matching phase; it is
/// not a text-comparison budget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PipelineOptions {
    pub line: LineOptions,
    pub block: BlockOptions,
    /// Maximum retained work for repeated terminal-line matching across both
    /// document sides.
    pub footer_work_limit: usize,
    /// Minimum literal prefix tokens used to identify a terminal line.
    pub footer_min_prefix_tokens: usize,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            line: LineOptions::default(),
            block: BlockOptions::default(),
            footer_work_limit: 32_000_000,
            footer_min_prefix_tokens: DEFAULT_ANCHOR_MIN_TOKENS,
        }
    }
}

impl PipelineOptions {
    /// Returns these options with the bounded footer work scaled uniformly.
    ///
    /// Layout parameters are unchanged. A scale below one is rejected so a
    /// caller cannot weaken configured limits.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] when `scale` is non-finite or
    /// less than one.
    pub fn scaled_limits(mut self, scale: f64) -> Result<Self> {
        let scale = validate_limit_scale(scale)?;
        self.footer_work_limit = scaled(self.footer_work_limit, scale);
        Ok(self)
    }
}

fn scaled(value: usize, scale: f64) -> usize {
    ((value as f64 * scale) as usize).max(value)
}

/// Validates a multiplier for comparison resource limits.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] when `scale` is non-finite or
/// less than one, which would weaken configured limits.
pub fn validate_limit_scale(scale: f64) -> Result<f64> {
    if !scale.is_finite() || scale < 1.0 {
        return Err(Error::InvalidConfiguration(format!(
            "limit scale must be a finite value >= 1.0 so configured defaults are never weakened, found {scale}"
        )));
    }
    Ok(scale)
}
