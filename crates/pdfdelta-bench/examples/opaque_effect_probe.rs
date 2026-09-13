//! Restricted, source-bound experiment in relative opaque-effect equivalence.
//! This executable supplies no production text, ownership or completion claims.

use std::{
    env,
    fs::File,
    io::{self, Read},
    sync::Arc,
};

use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};
use serde_json::json;
use sha2::{Digest, Sha256};

#[path = "opaque_effect_probe/acquire.rs"]
mod acquire;
#[path = "opaque_effect_probe/fixtures.rs"]
mod fixtures;
#[path = "opaque_effect_probe/profile.rs"]
mod profile;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--fixtures"] {
        serde_json::to_writer_pretty(io::stdout().lock(), &fixtures::experiment())?;
        return Ok(());
    }
    let [old, new] = args.as_slice() else {
        return Err("usage: opaque_effect_probe OLD.pdf NEW.pdf | --fixtures".into());
    };
    let capture = |path| -> Result<_, Box<dyn std::error::Error>> {
        let limits = ParseLimits::default();
        let mut bytes = Vec::new();
        File::open(path)?
            .take(limits.max_input_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limits.max_input_bytes {
            return Err("input byte limit".into());
        }
        let input_sha256 = digest(&bytes);
        let pdf = LopdfParser.parse(Arc::from(bytes), limits)?;
        Ok(acquire::capture(pdf.as_ref(), &input_sha256))
    };
    let (old, new) = (capture(old)?, capture(new)?);
    let comparisons: Vec<_> = old
        .pages
        .iter()
        .zip(&new.pages)
        .map(|(a, b)| profile::compare(a, b))
        .collect();
    serde_json::to_writer_pretty(
        io::stdout().lock(),
        &json!({
            "version": 1,
            "profile": profile::PROFILE,
            "claim": "relative deterministic primitive paint under this restricted profile",
            "certifies_text_inventory": false,
            "certifies_comparison_complete": false,
            "same_page_count": old.pages.len() == new.pages.len(),
            "old": old,
            "new": new,
            "page_comparisons": comparisons,
        }),
    )?;
    Ok(())
}
