//! The `RNTuple` anchor, header, footer and page-list envelopes (checksums
//! are not verified in fuzz builds, so the decoders behind them are reached).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rhizoid_io::__fuzz::rntuple_envelopes(data);
});
