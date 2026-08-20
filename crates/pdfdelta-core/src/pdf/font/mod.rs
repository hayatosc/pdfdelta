mod agl;
pub(crate) mod cmap;
mod common;
mod composite;
mod decoder;
mod metrics;
mod simple;

pub(crate) use cmap::UnicodeMapping;
pub(crate) use common::{FontIdentitySource, load_font_identity};
pub(crate) use decoder::{DecodedGlyph, FontDecoder, FontDecoderLimits};
