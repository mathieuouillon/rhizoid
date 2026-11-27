//! What an XRootD server sends: every response parser of the `root://`
//! client (headers, errors, redirects, waits, login, open and stat
//! answers, `kXR_readv` answers, URLs), and the byte stream through the
//! demultiplexer that hands responses to requests, with requests in flight.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rhizoid_io::__fuzz::xrootd_responses(data);
});
