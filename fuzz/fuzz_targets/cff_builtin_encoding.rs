#![no_main]

use libfuzzer_sys::fuzz_target;
use pdfdelta_core::fuzzing::fuzz_cff_builtin_encoding;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_INPUT_BYTES {
        fuzz_cff_builtin_encoding(data);
    }
});
