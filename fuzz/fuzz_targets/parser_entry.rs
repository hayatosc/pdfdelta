#![no_main]

use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_PAGES_TO_VISIT: usize = 16;

const LIMITS: ParseLimits = ParseLimits {
    max_input_bytes: MAX_INPUT_BYTES,
    max_objects: 2_048,
    max_recursion_depth: 16,
    max_decoded_stream_bytes: 64 * 1024,
    max_total_object_stream_bytes: 256 * 1024,
    max_pages: MAX_PAGES_TO_VISIT,
};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let parser = LopdfParser;
    if let Ok(pdf) = parser.parse(Arc::from(data), LIMITS) {
        let _ = pdf.version();
        let _ = pdf.trailer();
        let _ = pdf.issues();
        if let Ok(pages) = pdf.pages() {
            for page in pages.into_iter().take(MAX_PAGES_TO_VISIT) {
                let _ = pdf.page_dict(page);
            }
        }
    }
});
