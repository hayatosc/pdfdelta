#![no_main]

use libfuzzer_sys::fuzz_target;
use pdfdelta_bench::fuzzing::fuzz_canonical_yaml;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() <= MAX_INPUT_BYTES {
        fuzz_canonical_yaml(data);
    }
});
