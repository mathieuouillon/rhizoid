//! Compression: ROOT's 9-byte block framing and its codecs.
//!
//! Decoding supports zlib (`ZL`), LZ4 (`L4`), Zstandard (`ZS`) and LZMA
//! (`XZ`), all in pure Rust. Encoding supports zlib and LZ4 in this version.

use std::io::{Read, Write};

use crate::core::error::{Error, ErrorKind, Result};

/// The largest block: sizes are stored in 3 bytes.
const MAX_BLOCK: usize = 0xFF_FFFF;
const HEADER: usize = 9;
/// Objects this small are never compressed, as in ROOT.
const MIN_COMPRESS: usize = 256;

/// A compression algorithm, as numbered in ROOT's settings.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Algorithm {
    /// zlib (deflate), algorithm 1; also 0, "use the global default".
    Zlib,
    /// LZMA (xz), algorithm 2.
    Lzma,
    /// ROOT's old deflate variant, algorithm 3 (read only).
    OldRoot,
    /// LZ4, algorithm 4.
    Lz4,
    /// Zstandard, algorithm 5.
    Zstd,
}

impl Algorithm {
    fn code(self) -> i32 {
        match self {
            Self::Zlib => 1,
            Self::Lzma => 2,
            Self::OldRoot => 3,
            Self::Lz4 => 4,
            Self::Zstd => 5,
        }
    }
}

/// A compression setting: an algorithm and a level, or none.
///
/// Fields are private, so an invalid setting cannot be constructed.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Compression {
    algorithm: Option<Algorithm>,
    level: u8,
}

impl Compression {
    /// No compression.
    pub const NONE: Self = Self {
        algorithm: None,
        level: 0,
    };

    /// zlib at level `L` (1–9), checked at compile time. ROOT's default is 1.
    pub const fn zlib<const L: u8>() -> Self {
        const { assert!(L >= 1 && L <= 9, "zlib level must be in 1..=9") };
        Self {
            algorithm: Some(Algorithm::Zlib),
            level: L,
        }
    }

    /// LZ4 at level `L` (1–9), checked at compile time.
    pub const fn lz4<const L: u8>() -> Self {
        const { assert!(L >= 1 && L <= 9, "lz4 level must be in 1..=9") };
        Self {
            algorithm: Some(Algorithm::Lz4),
            level: L,
        }
    }

    /// A setting chosen at run time.
    ///
    /// # Errors
    ///
    /// `ErrorKind::Unsupported` if the level is not in `0..=9` (`0` means none).
    pub fn new(algorithm: Algorithm, level: u8) -> Result<Self> {
        match level {
            0 => Ok(Self::NONE),
            1..=9 => Ok(Self {
                algorithm: Some(algorithm),
                level,
            }),
            _ => Err(Error::new(
                ErrorKind::Unsupported,
                format!("compression level {level} is not in 0..=9"),
            )),
        }
    }

    /// The algorithm, or `None` for no compression.
    pub fn algorithm(self) -> Option<Algorithm> {
        self.algorithm
    }

    /// The level (0 = none).
    pub fn level(self) -> u8 {
        self.level
    }

    /// ROOT's integer form, `algorithm * 100 + level`.
    pub fn setting(self) -> i32 {
        self.algorithm
            .map_or(0, |a| a.code() * 100 + i32::from(self.level))
    }
}

impl Default for Compression {
    /// zlib level 1, ROOT's compiled-in default.
    fn default() -> Self {
        Self::zlib::<1>()
    }
}

impl TryFrom<i32> for Compression {
    type Error = Error;

    /// Parses ROOT's `algorithm * 100 + level` form (e.g. `505` = Zstandard 5).
    fn try_from(setting: i32) -> Result<Self> {
        let (alg, level) = (setting / 100, setting % 100);
        let algorithm = match alg {
            0 | 1 => Algorithm::Zlib,
            2 => Algorithm::Lzma,
            3 => Algorithm::OldRoot,
            4 => Algorithm::Lz4,
            5 => Algorithm::Zstd,
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("unknown compression setting {setting}"),
                ));
            }
        };
        let level = u8::try_from(level).map_err(|_| {
            Error::new(
                ErrorKind::Unsupported,
                format!("bad compression setting {setting}"),
            )
        })?;
        Self::new(algorithm, level)
    }
}

fn le24(b: &[u8]) -> usize {
    b.iter()
        .rev()
        .fold(0usize, |acc, &x| (acc << 8) | usize::from(x))
}

/// Decompresses a record's data into exactly `obj_len` bytes, walking the
/// 9-byte block headers.
pub(crate) fn decompress(data: &[u8], obj_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(obj_len).map_err(|_| {
        Error::new(
            ErrorKind::LimitExceeded,
            format!("cannot allocate {obj_len} bytes"),
        )
    })?;
    let mut pos = 0usize;
    while out.len() < obj_len {
        let header = data.get(pos..pos + HEADER).ok_or_else(|| {
            Error::new(
                ErrorKind::UnexpectedEof,
                "truncated compression block header",
            )
        })?;
        let (tag, method) = ([header[0], header[1]], header[2]);
        let c_size = le24(&header[3..6]);
        let u_size = le24(&header[6..9]);
        let payload = data
            .get(pos + HEADER..pos + HEADER + c_size)
            .ok_or_else(|| Error::new(ErrorKind::UnexpectedEof, "truncated compression block"))?;
        if out.len() + u_size > obj_len {
            return Err(Error::new(
                ErrorKind::Decompress,
                "blocks decompress to more than ObjLen",
            ));
        }
        let start = out.len();
        match &tag {
            b"ZL" => zlib_decode(payload, u_size, &mut out)?,
            b"L4" => lz4_decode(payload, u_size, &mut out)?,
            b"ZS" => zstd_decode(payload, &mut out)?,
            b"XZ" => xz_decode(payload, &mut out)?,
            b"CS" => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "ROOT's old compression algorithm (CS) is not supported",
                ));
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::UnknownCompression,
                    format!(
                        "unknown compression tag {:?} (method {method})",
                        String::from_utf8_lossy(&tag)
                    ),
                ));
            }
        }
        if out.len() - start != u_size {
            return Err(Error::new(
                ErrorKind::Decompress,
                format!(
                    "block decompressed to {} bytes, expected {u_size}",
                    out.len() - start
                ),
            ));
        }
        pos += HEADER + c_size;
    }
    Ok(out)
}

fn zlib_decode(payload: &[u8], u_size: usize, out: &mut Vec<u8>) -> Result<()> {
    let mut dec =
        flate2::read::ZlibDecoder::new(payload).take(u64::try_from(u_size).unwrap_or(u64::MAX) + 1);
    dec.read_to_end(out)
        .map_err(|e| Error::new(ErrorKind::Decompress, "zlib stream is corrupt").with_source(e))?;
    Ok(())
}

fn lz4_decode(payload: &[u8], u_size: usize, out: &mut Vec<u8>) -> Result<()> {
    let (checksum, block) = payload
        .split_at_checked(8)
        .ok_or_else(|| Error::new(ErrorKind::UnexpectedEof, "truncated LZ4 checksum"))?;
    let expected = u64::from_be_bytes(
        checksum
            .try_into()
            .map_err(|_| Error::malformed("bad LZ4 checksum"))?,
    );
    if xxhash_rust::xxh64::xxh64(block, 0) != expected {
        return Err(Error::new(
            ErrorKind::Decompress,
            "LZ4 block checksum mismatch",
        ));
    }
    let start = out.len();
    out.resize(start + u_size, 0);
    let n = lz4_flex::block::decompress_into(block, &mut out[start..])
        .map_err(|e| Error::new(ErrorKind::Decompress, format!("LZ4 block is corrupt: {e}")))?;
    out.truncate(start + n);
    Ok(())
}

fn zstd_decode(payload: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut dec = ruzstd::decoding::StreamingDecoder::new(payload)
        .map_err(|e| Error::new(ErrorKind::Decompress, format!("zstd frame is corrupt: {e}")))?;
    dec.read_to_end(out)
        .map_err(|e| Error::new(ErrorKind::Decompress, "zstd stream is corrupt").with_source(e))?;
    Ok(())
}

fn xz_decode(payload: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut input = payload;
    lzma_rs::xz_decompress(&mut input, out)
        .map_err(|e| Error::new(ErrorKind::Decompress, format!("xz stream is corrupt: {e}")))
}

/// Compresses an object's data with ROOT's policy: nothing at level 0
/// or for `ObjLen ≤ 256`, and the whole object raw if any block fails to
/// shrink. Returns `None` when the data must be stored raw.
pub(crate) fn compress(setting: Compression, data: &[u8]) -> Result<Option<Vec<u8>>> {
    if data.len() <= MIN_COMPRESS {
        return Ok(None);
    }
    compress_basket(setting, data)
}

/// Compresses a basket's data: as [`compress`], but whatever its size, as
/// `TBasket::WriteBuffer` does.
pub(crate) fn compress_basket(setting: Compression, data: &[u8]) -> Result<Option<Vec<u8>>> {
    let Some(algorithm) = setting.algorithm else {
        return Ok(None);
    };
    if setting.level == 0 || data.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(data.len() / 2);
    for block in data.chunks(MAX_BLOCK) {
        let (tag, method, body) = match algorithm {
            Algorithm::Zlib => (*b"ZL", 8u8, zlib_encode(block, setting.level)?),
            Algorithm::Lz4 => (*b"L4", 1u8, lz4_encode(block)),
            other => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("writing {other:?} compression is not supported yet"),
                ));
            }
        };
        if body.len() >= block.len() || body.len() > MAX_BLOCK {
            return Ok(None);
        }
        out.extend_from_slice(&tag);
        out.push(method);
        out.extend_from_slice(&le24_bytes(body.len()));
        out.extend_from_slice(&le24_bytes(block.len()));
        out.extend_from_slice(&body);
    }
    // Readers take a record whose stored size equals its object size for
    // uncompressed (`fNbytes - fKeylen < fObjLen` means compressed), so
    // the compressed form must be strictly smaller, headers included.
    if out.len() >= data.len() {
        return Ok(None);
    }
    Ok(Some(out))
}

fn le24_bytes(n: usize) -> [u8; 3] {
    let b = n.to_le_bytes();
    [b[0], b[1], b[2]]
}

fn zlib_encode(block: &[u8], level: u8) -> Result<Vec<u8>> {
    let mut enc =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(u32::from(level)));
    enc.write_all(block).map_err(Error::from)?;
    enc.finish().map_err(Error::from)
}

fn lz4_encode(block: &[u8]) -> Vec<u8> {
    let compressed = lz4_flex::block::compress(block);
    let mut body = Vec::with_capacity(8 + compressed.len());
    body.extend_from_slice(&xxhash_rust::xxh64::xxh64(&compressed, 0).to_be_bytes());
    body.extend_from_slice(&compressed);
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| u8::try_from(i % 7).unwrap_or(0) * 3)
            .collect()
    }

    #[test]
    fn small_objects_stay_raw() {
        assert!(
            compress(Compression::zlib::<1>(), &sample(256))
                .expect("ok")
                .is_none()
        );
        assert!(
            compress(Compression::NONE, &sample(10_000))
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn incompressible_data_stays_raw() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[0]
            })
            .collect();
        assert!(
            compress(Compression::zlib::<9>(), &noise)
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn settings_parse_like_root() {
        assert_eq!(
            Compression::try_from(505).expect("zstd").algorithm(),
            Some(Algorithm::Zstd)
        );
        assert_eq!(Compression::try_from(101).expect("zlib").setting(), 101);
        assert_eq!(Compression::try_from(0).expect("none"), Compression::NONE);
        assert!(Compression::try_from(250).is_err());
        assert!(Compression::new(Algorithm::Zlib, 12).is_err());
    }

    #[test]
    fn unknown_tag_is_reported() {
        let mut data = b"QQ\x08".to_vec();
        data.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0]);
        assert_eq!(
            decompress(&data, 1).expect_err("unknown").kind(),
            ErrorKind::UnknownCompression
        );
    }

    #[test]
    fn lz4_checksum_is_verified() {
        let data = sample(5000);
        let mut c = compress(Compression::lz4::<4>(), &data)
            .expect("ok")
            .expect("compressible");
        let last = c.len() - 1;
        c[last] ^= 0xFF;
        assert!(decompress(&c, data.len()).is_err());
    }

    proptest! {
        #[test]
        fn zlib_and_lz4_round_trip(len in 257usize..70_000, lz4: bool) {
            let data = sample(len);
            let setting = if lz4 { Compression::lz4::<1>() } else { Compression::zlib::<6>() };
            let c = compress(setting, &data).expect("ok").expect("compressible");
            prop_assert!(c.len() < data.len());
            prop_assert_eq!(decompress(&c, data.len()).expect("decompress"), data);
        }

        /// Baskets of any size: compressed only when strictly smaller,
        /// headers included, so readers never take them for raw.
        #[test]
        fn small_baskets_round_trip(
            data in proptest::collection::vec(prop_oneof![Just(0u8), Just(0xF9), any::<u8>()], 0..300),
            lz4: bool,
        ) {
            let setting = if lz4 { Compression::lz4::<1>() } else { Compression::zlib::<1>() };
            if let Some(c) = compress_basket(setting, &data).expect("ok") {
                prop_assert!(c.len() < data.len());
                prop_assert_eq!(decompress(&c, data.len()).expect("decompress"), data);
            }
        }

        #[test]
        fn garbage_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..200), obj_len in 0usize..1000) {
            let _ = decompress(&bytes, obj_len);
        }
    }
}
