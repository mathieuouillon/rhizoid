//! The `StreamerInfo` list (class descriptions), and objects decoded with
//! the descriptions it yields.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rhizoid_io::__fuzz::streamer_infos(data);
});
