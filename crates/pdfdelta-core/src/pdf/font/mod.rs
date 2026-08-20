mod agl;
pub(crate) mod cmap;
mod decoder;
mod metrics;
mod simple;

pub(crate) use cmap::UnicodeMapping;
pub(crate) use decoder::{DecodedGlyph, FontDecoder, FontDecoderLimits};
