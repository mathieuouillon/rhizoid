//! One `RNTuple` page of any column type, bit width and element count.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Page<'a> {
    column: u16,
    bits: u16,
    range: Option<(f64, f64)>,
    elements: u16,
    raw: &'a [u8],
}

fuzz_target!(|p: Page<'_>| {
    rhizoid_io::__fuzz::rntuple_page(p.column, p.bits, p.range, p.raw, usize::from(p.elements));
});
