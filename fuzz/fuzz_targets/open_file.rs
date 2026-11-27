//! Any bytes, opened as a ROOT file and read in full through the public API.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rhizoid_fuzz::read_everything(data);
});
