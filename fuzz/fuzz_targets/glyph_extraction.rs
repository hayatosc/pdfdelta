#![no_main]

use libfuzzer_sys::fuzz_target;
use pdfdelta_core::fuzzing::fuzz_glyph_extraction;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_INPUT_BYTES {
        fuzz_glyph_extraction(data);
    }
});
