//! The file header, keys, directory records, key lists, free lists and
//! UUIDs, each decoded from the same bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rhizoid_io::__fuzz::records(data);
});
