//! Page decoding: column element types and encodings of `RNTuple` 1.0.
//!
//! Every function here is pure: decompressed page bytes in, values out.
//! Encodings undone: byte-plane split (`Split*`), zigzag (signed split
//! integers), per-page delta (split offsets), bit packing (`Bit`,
//! `Real32Trunc`, `Real32Quant`), and IEEE half precision (`Real16`).

use crate::core::error::{Error, ErrorKind, Result};
use crate::streamer::{Array, Scalar};

/// Column element types (the specification's codes).
pub(crate) mod kind {
    pub(crate) const BIT: u16 = 0x00;
    pub(crate) const BYTE: u16 = 0x01;
    pub(crate) const CHAR: u16 = 0x02;
    pub(crate) const INT8: u16 = 0x03;
    pub(crate) const UINT8: u16 = 0x04;
    pub(crate) const INT16: u16 = 0x05;
    pub(crate) const UINT16: u16 = 0x06;
    pub(crate) const INT32: u16 = 0x07;
    pub(crate) const UINT32: u16 = 0x08;
    pub(crate) const INT64: u16 = 0x09;
    pub(crate) const UINT64: u16 = 0x0A;
    pub(crate) const REAL16: u16 = 0x0B;
    pub(crate) const REAL32: u16 = 0x0C;
    pub(crate) const REAL64: u16 = 0x0D;
    pub(crate) const INDEX32: u16 = 0x0E;
    pub(crate) const INDEX64: u16 = 0x0F;
    pub(crate) const SWITCH: u16 = 0x10;
    pub(crate) const SPLIT_INT16: u16 = 0x11;
    pub(crate) const SPLIT_UINT16: u16 = 0x12;
    pub(crate) const SPLIT_INT32: u16 = 0x13;
    pub(crate) const SPLIT_UINT32: u16 = 0x14;
    pub(crate) const SPLIT_INT64: u16 = 0x15;
    pub(crate) const SPLIT_UINT64: u16 = 0x16;
    pub(crate) const SPLIT_REAL16: u16 = 0x17;
    pub(crate) const SPLIT_REAL32: u16 = 0x18;
    pub(crate) const SPLIT_REAL64: u16 = 0x19;
    pub(crate) const SPLIT_INDEX32: u16 = 0x1A;
    pub(crate) const SPLIT_INDEX64: u16 = 0x1B;
    pub(crate) const REAL32_TRUNC: u16 = 0x1C;
    pub(crate) const REAL32_QUANT: u16 = 0x1D;
}

use kind::{
    BIT, BYTE, CHAR, INDEX32, INDEX64, INT8, INT16, INT32, INT64, REAL16, REAL32, REAL32_QUANT,
    REAL32_TRUNC, REAL64, SPLIT_INDEX32, SPLIT_INDEX64, SPLIT_INT16, SPLIT_INT32, SPLIT_INT64,
    SPLIT_REAL16, SPLIT_REAL32, SPLIT_REAL64, SPLIT_UINT16, SPLIT_UINT32, SPLIT_UINT64, SWITCH,
    UINT8, UINT16, UINT32, UINT64,
};

/// The values a column type decodes to; `None` for the switch type.
pub(crate) fn scalar_of(column: u16) -> Option<Scalar> {
    Some(match column {
        BIT => Scalar::Bool,
        BYTE | CHAR | UINT8 => Scalar::U8,
        INT8 => Scalar::I8,
        INT16 | SPLIT_INT16 => Scalar::I16,
        UINT16 | SPLIT_UINT16 => Scalar::U16,
        INT32 | SPLIT_INT32 => Scalar::I32,
        UINT32 | SPLIT_UINT32 => Scalar::U32,
        INT64 | SPLIT_INT64 => Scalar::I64,
        UINT64 | SPLIT_UINT64 | INDEX32 | INDEX64 | SPLIT_INDEX32 | SPLIT_INDEX64 => Scalar::U64,
        REAL16 | SPLIT_REAL16 | REAL32 | SPLIT_REAL32 | REAL32_TRUNC | REAL32_QUANT => Scalar::F32,
        REAL64 | SPLIT_REAL64 => Scalar::F64,
        _ => return None,
    })
}

/// Bytes of a page of `n` elements of `bits` bits each.
pub(crate) fn page_bytes(column: u16, bits: u16, n: u64) -> u64 {
    let bits = if column == SWITCH {
        96
    } else {
        u64::from(bits)
    };
    (n * bits).div_ceil(8)
}

/// A decoded page.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Decoded {
    Values(Array),
    /// `(index, tag)` per element: which alternative of a variant, and
    /// where in it (cluster-local).
    Switch(Vec<(u64, u32)>),
}

#[cold]
fn short(n: usize, have: usize) -> Error {
    Error::new(
        ErrorKind::UnexpectedEof,
        format!("a page of {n} elements has only {have} bytes"),
    )
}

/// Undoes the byte-plane split: byte `b` of element `i` is at `b·n + i`.
fn unsplit<const S: usize>(raw: &[u8], n: usize) -> Result<Vec<[u8; S]>> {
    if raw.len() < n * S {
        return Err(short(n, raw.len()));
    }
    let mut out = vec![[0u8; S]; n];
    for (b, plane) in raw.chunks_exact(n.max(1)).take(S).enumerate() {
        for (i, &byte) in plane.iter().enumerate().take(n) {
            if let Some(e) = out.get_mut(i) {
                e[b] = byte;
            }
        }
    }
    Ok(out)
}

/// Elements stored back to back, little-endian.
fn plain<const S: usize>(raw: &[u8], n: usize) -> Result<Vec<[u8; S]>> {
    if raw.len() < n * S {
        return Err(short(n, raw.len()));
    }
    Ok(raw.as_chunks::<S>().0.iter().take(n).copied().collect())
}

fn elements<const S: usize>(raw: &[u8], n: usize, split: bool) -> Result<Vec<[u8; S]>> {
    if split {
        unsplit::<S>(raw, n)
    } else {
        plain::<S>(raw, n)
    }
}

/// `n` values of `nbits` bits from a little-endian bit stream.
fn bit_fields(raw: &[u8], n: usize, nbits: u32) -> Result<Vec<u64>> {
    if !(1..=32).contains(&nbits) {
        return Err(Error::malformed(format!("{nbits}-bit packed floats")));
    }
    let width = usize::try_from(nbits).unwrap_or(32);
    if raw.len() < (n * width).div_ceil(8) {
        return Err(short(n, raw.len()));
    }
    let mask = (1u64 << nbits) - 1;
    Ok((0..n)
        .map(|i| {
            let bit = i * width;
            let mut word = [0u8; 8];
            for (k, slot) in word.iter_mut().enumerate() {
                *slot = raw.get(bit / 8 + k).copied().unwrap_or(0);
            }
            (u64::from_le_bytes(word) >> (bit % 8)) & mask
        })
        .collect())
}

/// IEEE 754 half precision to single.
pub(crate) fn half_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1F);
    let man = u32::from(h & 0x3FF);
    let bits = match (exp, man) {
        (0, 0) => sign,
        (0, m) => {
            // Subnormal: normalise.
            let shift = m.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | (((m << shift) & 0x3FF) << 13)
        }
        (0x1F, m) => sign | 0x7F80_0000 | (m << 13),
        (e, m) => sign | ((e + 112) << 23) | (m << 13),
    };
    f32::from_bits(bits)
}

fn zigzag(u: u64) -> i64 {
    i64::from_ne_bytes((u >> 1).to_ne_bytes()) ^ -i64::from_ne_bytes((u & 1).to_ne_bytes())
}

/// Decodes one page of `n` elements of a column.
#[expect(clippy::too_many_lines, reason = "one arm per column type")]
pub(crate) fn decode(
    column: u16,
    bits: u16,
    range: Option<(f64, f64)>,
    raw: &[u8],
    n: usize,
) -> Result<Decoded> {
    let split = matches!(column, SPLIT_INT16..=SPLIT_INDEX64);
    let v = match column {
        BIT => {
            if raw.len() < n.div_ceil(8) {
                return Err(short(n, raw.len()));
            }
            Array::Bool(
                (0..n)
                    .map(|i| raw.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 == 1))
                    .collect(),
            )
        }
        BYTE | CHAR | UINT8 => Array::U8(raw.get(..n).ok_or_else(|| short(n, raw.len()))?.to_vec()),
        INT8 => Array::I8(
            raw.get(..n)
                .ok_or_else(|| short(n, raw.len()))?
                .iter()
                .map(|&b| i8::from_le_bytes([b]))
                .collect(),
        ),
        INT16 | SPLIT_INT16 => {
            let e = elements::<2>(raw, n, split)?;
            Array::I16(if split {
                e.iter()
                    .map(|b| {
                        i16::try_from(zigzag(u64::from(u16::from_le_bytes(*b)))).unwrap_or_default()
                    })
                    .collect()
            } else {
                e.iter().map(|b| i16::from_le_bytes(*b)).collect()
            })
        }
        UINT16 | SPLIT_UINT16 => Array::U16(
            elements::<2>(raw, n, split)?
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect(),
        ),
        INT32 | SPLIT_INT32 => {
            let e = elements::<4>(raw, n, split)?;
            Array::I32(if split {
                e.iter()
                    .map(|b| {
                        i32::try_from(zigzag(u64::from(u32::from_le_bytes(*b)))).unwrap_or_default()
                    })
                    .collect()
            } else {
                e.iter().map(|b| i32::from_le_bytes(*b)).collect()
            })
        }
        UINT32 | SPLIT_UINT32 => Array::U32(
            elements::<4>(raw, n, split)?
                .iter()
                .map(|b| u32::from_le_bytes(*b))
                .collect(),
        ),
        INT64 | SPLIT_INT64 => {
            let e = elements::<8>(raw, n, split)?;
            Array::I64(if split {
                e.iter().map(|b| zigzag(u64::from_le_bytes(*b))).collect()
            } else {
                e.iter().map(|b| i64::from_le_bytes(*b)).collect()
            })
        }
        UINT64 | SPLIT_UINT64 => Array::U64(
            elements::<8>(raw, n, split)?
                .iter()
                .map(|b| u64::from_le_bytes(*b))
                .collect(),
        ),
        REAL16 | SPLIT_REAL16 => Array::F32(
            elements::<2>(raw, n, split)?
                .iter()
                .map(|b| half_to_f32(u16::from_le_bytes(*b)))
                .collect(),
        ),
        REAL32 | SPLIT_REAL32 => Array::F32(
            elements::<4>(raw, n, split)?
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect(),
        ),
        REAL64 | SPLIT_REAL64 => Array::F64(
            elements::<8>(raw, n, split)?
                .iter()
                .map(|b| f64::from_le_bytes(*b))
                .collect(),
        ),
        INDEX32 | SPLIT_INDEX32 => {
            let mut v: Vec<u64> = elements::<4>(raw, n, split)?
                .iter()
                .map(|b| u64::from(u32::from_le_bytes(*b)))
                .collect();
            if split {
                // Delta-encoded within the page.
                let mut sum = 0u64;
                for x in &mut v {
                    sum = sum.wrapping_add(*x);
                    *x = sum;
                }
            }
            Array::U64(v)
        }
        INDEX64 | SPLIT_INDEX64 => {
            let mut v: Vec<u64> = elements::<8>(raw, n, split)?
                .iter()
                .map(|b| u64::from_le_bytes(*b))
                .collect();
            if split {
                let mut sum = 0u64;
                for x in &mut v {
                    sum = sum.wrapping_add(*x);
                    *x = sum;
                }
            }
            Array::U64(v)
        }
        SWITCH => {
            let e = plain::<12>(raw, n)?;
            return Ok(Decoded::Switch(
                e.iter()
                    .map(|b| {
                        let index =
                            u64::from_le_bytes(<[u8; 8]>::try_from(&b[..8]).unwrap_or_default());
                        let tag =
                            u32::from_le_bytes(<[u8; 4]>::try_from(&b[8..]).unwrap_or_default());
                        (index, tag)
                    })
                    .collect(),
            ));
        }
        REAL32_TRUNC => {
            let nbits = u32::from(bits);
            let fields = if nbits == 32 {
                plain::<4>(raw, n)?
                    .iter()
                    .map(|b| u64::from(u32::from_le_bytes(*b)))
                    .collect()
            } else {
                bit_fields(raw, n, nbits)?
            };
            Array::F32(
                fields
                    .iter()
                    .map(|&v| f32::from_bits(u32::try_from(v << (32 - nbits)).unwrap_or_default()))
                    .collect(),
            )
        }
        REAL32_QUANT => {
            let nbits = u32::from(bits);
            let (min, max) = range.ok_or_else(|| {
                Error::malformed("a quantized float column without a value range")
            })?;
            let fields = if nbits == 32 {
                plain::<4>(raw, n)?
                    .iter()
                    .map(|b| u64::from(u32::from_le_bytes(*b)))
                    .collect()
            } else {
                bit_fields(raw, n, nbits)?
            };
            let steps = f64::from(u32::try_from((1u64 << nbits) - 1).unwrap_or(u32::MAX));
            Array::F32(
                fields
                    .iter()
                    .map(|&q| narrow(min + to_f64(q) * (max - min) / steps))
                    .collect(),
            )
        }
        other => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("RNTuple column type {other:#04x}"),
            ));
        }
    };
    Ok(Decoded::Values(v))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "quantized floats are f32 in memory"
)]
fn narrow(x: f64) -> f32 {
    x as f32
}

fn to_f64(q: u64) -> f64 {
    f64::from(u32::try_from(q).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_zigzag_and_delta_round_trip() {
        // Three i32 values split into byte planes after zigzag: 1 → 2, -2 → 3, 300 → 600.
        let encoded: Vec<u32> = [2u32, 3, 600].to_vec();
        let mut raw = vec![0u8; 12];
        for (i, v) in encoded.iter().enumerate() {
            for (b, byte) in v.to_le_bytes().iter().enumerate() {
                raw[b * 3 + i] = *byte;
            }
        }
        assert_eq!(
            decode(SPLIT_INT32, 32, None, &raw, 3).expect("decode"),
            Decoded::Values(Array::I32(vec![1, -2, 300]))
        );
        // Offsets 3, 5, 9 as deltas 3, 2, 4.
        let mut raw = vec![0u8; 12];
        for (i, v) in [3u32, 2, 4].iter().enumerate() {
            for (b, byte) in v.to_le_bytes().iter().enumerate() {
                raw[b * 3 + i] = *byte;
            }
        }
        assert_eq!(
            decode(SPLIT_INDEX32, 32, None, &raw, 3).expect("decode"),
            Decoded::Values(Array::U64(vec![3, 5, 9]))
        );
    }

    #[test]
    fn bits_halves_and_packed_floats_decode() {
        assert_eq!(
            decode(BIT, 1, None, &[0b0000_0101], 3).expect("bits"),
            Decoded::Values(Array::Bool(vec![true, false, true]))
        );
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(half_to_f32(0x0001), 2.0_f32.powi(-24));
        // 1.5 truncated to 10 bits: the top 10 bits of 0x3FC00000.
        let top = 0x3FC0_0000u32 >> 22;
        let raw = top.to_le_bytes();
        assert_eq!(
            decode(REAL32_TRUNC, 10, None, &raw, 1).expect("trunc"),
            Decoded::Values(Array::F32(vec![1.5]))
        );
        // Quantized: 2 bits over [0, 3] → 0, 1, 2, 3.
        assert_eq!(
            decode(REAL32_QUANT, 2, Some((0.0, 3.0)), &[0b1110_0100], 4).expect("quant"),
            Decoded::Values(Array::F32(vec![0.0, 1.0, 2.0, 3.0]))
        );
        assert!(decode(INT32, 32, None, &[0, 0], 1).is_err());
    }

    #[test]
    fn half_floats_widen_exactly() {
        let cases: [(u16, f32); 11] = [
            (0x0000, 0.0),
            (0x3C00, 1.0),
            (0xC000, -2.0),
            (0x3555, 0.333_251_95),
            (0x7BFF, 65504.0),
            (0x0400, 2f32.powi(-14)),
            // Subnormals.
            (0x0001, 2f32.powi(-24)),
            (0x0200, 2f32.powi(-15)),
            (0x03FF, 1023.0 * 2f32.powi(-24)),
            (0x7C00, f32::INFINITY),
            (0xFC00, f32::NEG_INFINITY),
        ];
        for (h, f) in cases {
            assert_eq!(half_to_f32(h).to_bits(), f.to_bits(), "{h:#06x}");
        }
        assert_eq!(half_to_f32(0x8000).to_bits(), (-0.0f32).to_bits());
        assert!(half_to_f32(0x7E00).is_nan());
    }

    /// Pages shorter than their elements are errors, whatever the encoding.
    #[test]
    fn short_pages_are_errors() {
        let short = |column, bits, raw: &[u8], n| {
            decode(column, bits, None, raw, n)
                .expect_err("short page")
                .kind()
        };
        assert_eq!(short(REAL64, 64, &[0; 23], 3), ErrorKind::UnexpectedEof);
        assert_eq!(
            short(SPLIT_REAL64, 64, &[0; 23], 3),
            ErrorKind::UnexpectedEof
        );
        assert_eq!(short(BIT, 1, &[0; 1], 9), ErrorKind::UnexpectedEof);
        // 12-bit truncated floats: 3 of them take 5 bytes.
        assert_eq!(
            short(REAL32_TRUNC, 12, &[0; 4], 3),
            ErrorKind::UnexpectedEof
        );
        assert!(decode(REAL32_TRUNC, 12, None, &[0; 5], 3).is_ok());
        assert!(decode(SPLIT_REAL64, 64, None, &[0; 24], 3).is_ok());
    }
}
