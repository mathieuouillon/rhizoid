//! `TBasket` records, as stored in their own key and embedded in a tree.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rhizoid_io::__fuzz::basket(data);
});
