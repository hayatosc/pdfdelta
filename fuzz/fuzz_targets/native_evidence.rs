#![no_main]

use libfuzzer_sys::fuzz_target;
use pdfdelta_core::fuzzing::fuzz_native_evidence;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_INPUT_BYTES {
        let _ = fuzz_native_evidence(data);
    }
});
