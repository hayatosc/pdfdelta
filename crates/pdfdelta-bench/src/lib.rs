pub mod candidate_eval;
pub mod canonical;
pub mod cases;
pub mod evaluator;
pub mod mutation;
pub mod renderers;
pub mod revisions;

mod error;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;

pub use error::{BenchError, Result};
