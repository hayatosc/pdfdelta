pub mod alignment;
pub mod diff;
pub mod error;
pub mod extraction_conformance;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;
pub mod layout;
pub mod model;
pub mod normalize;
pub mod pdf;
pub mod pipeline;
pub mod report;
pub mod source;
mod validate;

pub use error::{Error, Result};
