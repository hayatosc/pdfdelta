pub mod candidate_eval;
pub mod candidate_profile;
pub mod canonical;
pub mod cases;
pub mod evaluation;
pub mod evaluator;
pub mod extraction_conformance;
pub mod generalization;
pub mod generalization_matrix;
pub mod generalization_report;
pub mod mutation;
pub mod renderers;
pub mod revisions;
pub mod sensitivity;

mod error;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;

pub use error::{BenchError, Result};
