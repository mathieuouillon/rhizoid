//! Entry points for the fuzz targets (`fuzz/`): the decoders of untrusted
//! bytes, reached directly rather than through a whole file. Feature
//! `_fuzz`; not part of the API.

use crate::core::buffer::RBuffer;
use crate::core::directory::{DirectoryRecord, decode_free_list, decode_key_list};
use crate::core::header::FileHeader;
use crate::core::key::KeyHeader;
use crate::core::time::Uuid;

/// ROOT's compressed-block framing and codecs. The first four bytes are the
/// claimed uncompressed size (capped at 16 MiB), the rest the blocks.
pub fn decompress(data: &[u8]) {
    let Some((len, blocks)) = data.split_first_chunk::<4>() else {
        return;
    };
    let obj_len = usize::try_from(u32::from_le_bytes(*len) % (16 << 20)).unwrap_or(0);
    let _ = crate::compress::decompress(blocks, obj_len);
}

/// The fixed records, each decoded from the start of `data`.
pub fn records(data: &[u8]) {
    let _ = FileHeader::decode(data);
    let _ = KeyHeader::decode(&mut RBuffer::new(data, 0));
    let _ = DirectoryRecord::decode(&mut RBuffer::new(data, 0));
    let _ = decode_key_list(&mut RBuffer::new(data, 0), u64::MAX);
    let _ = decode_free_list(&mut RBuffer::new(data, 0));
    let _ = Uuid::decode(&mut RBuffer::new(data, 0));
}

/// The `StreamerInfo` list, then objects decoded with the descriptions.
pub fn streamer_infos(data: &[u8]) {
    crate::streamer::fuzz_decode(data);
}

/// The `RNTuple` anchor, header, footer and page list.
pub fn rntuple_envelopes(data: &[u8]) {
    crate::ntuple::fuzz_envelopes(data);
}

/// One `RNTuple` page.
pub fn rntuple_page(column: u16, bits: u16, range: Option<(f64, f64)>, raw: &[u8], n: usize) {
    crate::ntuple::fuzz_page(column, bits, range, raw, n);
}

/// A `TBasket` record, alone and embedded.
pub fn basket(data: &[u8]) {
    crate::tree::fuzz_basket(data);
}

/// The XRootD client's response parsers, and a server's byte stream
/// through its demultiplexer.
#[cfg(all(feature = "xrootd", not(target_arch = "wasm32")))]
pub fn xrootd_responses(data: &[u8]) {
    crate::io::xrootd_proto::fuzz_parsers(data);
    crate::io::xrootd::fuzz_stream(data);
}
