pub(crate) mod cmap;
mod metrics;
pub(crate) mod simple;

pub(crate) use cmap::UnicodeMapping;
pub(crate) use simple::{DecodedGlyph, SimpleFontDecoder, SimpleFontLimits};
