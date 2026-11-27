//! ROOT's compressed-block framing and every codec (zlib, LZMA, LZ4, zstd,
//! old ROOT): the first four bytes are the claimed uncompressed size.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rhizoid_io::__fuzz::decompress(data);
});
