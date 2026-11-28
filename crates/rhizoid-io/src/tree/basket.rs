//! `TBasket` records: a key whose header carries the basket fields,
//! then the (compressed) entries, then the entry offsets.

use crate::compress;
use crate::core::buffer::RBuffer;
use crate::core::error::{Error, ErrorKind, Result};
use crate::core::key::KeyHeader;

/// A decoded basket: the entry bytes and where each entry starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Basket {
    /// The entry bytes (up to `fLast`).
    pub(crate) data: Vec<u8>,
    /// `entries + 1` positions in `data`, when entries vary in size.
    pub(crate) offsets: Option<Vec<usize>>,
    /// `fNevBuf`: the number of entries.
    pub(crate) entries: usize,
    /// `fNevBufSize`: the size of a fixed-size entry.
    pub(crate) entry_size: usize,
    /// Bytes after decompression.
    pub(crate) uncompressed: usize,
    /// The key length: where `data` starts within the basket's buffer.
    pub(crate) key_len: usize,
}

impl Basket {
    /// Where entry `i` starts in `data`.
    pub(crate) fn entry_start(&self, i: usize) -> Result<usize> {
        match &self.offsets {
            Some(o) => o.get(i).copied().ok_or_else(out_of_range),
            None => Ok(i * self.entry_size),
        }
    }

    /// The bytes of entry `i` (counted within the basket).
    pub(crate) fn entry(&self, i: usize) -> Result<&[u8]> {
        let range = match &self.offsets {
            Some(o) => {
                *o.get(i).ok_or_else(out_of_range)?..*o.get(i + 1).ok_or_else(out_of_range)?
            }
            None => i * self.entry_size..(i + 1) * self.entry_size,
        };
        self.data.get(range).ok_or_else(out_of_range)
    }

    /// The bytes of entries `lo..hi`, back to back.
    pub(crate) fn entries(&self, lo: usize, hi: usize) -> Result<&[u8]> {
        let range = match &self.offsets {
            Some(o) => *o.get(lo).ok_or_else(out_of_range)?..*o.get(hi).ok_or_else(out_of_range)?,
            None => lo * self.entry_size..hi * self.entry_size,
        };
        self.data.get(range).ok_or_else(out_of_range)
    }
}

#[cold]
fn out_of_range() -> Error {
    Error::malformed("entry outside its basket")
}

/// Decodes a basket record (key and data), allowing at most `max_bytes`
/// after decompression.
pub(crate) fn decode(record: &[u8], max_bytes: u64) -> Result<Basket> {
    let mut cursor = RBuffer::new(record, 0);
    let key = KeyHeader::decode(&mut cursor)?;
    // The basket fields close the key header (TBasket::Streamer).
    cursor.read_i16()?; // TBasket version
    cursor.read_i32()?; // fBufferSize
    let mut entry_size = cursor.read_i32()?;
    if entry_size < 0 {
        entry_size = entry_size
            .checked_neg()
            .ok_or_else(|| Error::malformed("fNevBufSize"))?;
        cursor.read_u8()?; // fIOBits
    }
    let entries = cursor.read_len32("fNevBuf")?;
    let last = cursor.read_len32("fLast")?;
    cursor.read_u8()?; // flag
    let key_len = usize::from(key.key_len);
    if cursor.position() > key_len {
        return Err(Error::malformed("basket fields run past the key"));
    }
    let nbytes = usize::try_from(key.nbytes).map_err(|_| Error::malformed("Nbytes"))?;
    if nbytes <= key_len {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "baskets embedded in the tree object are not supported yet",
        ));
    }
    if u64::from(key.obj_len) > max_bytes {
        return Err(Error::new(
            ErrorKind::LimitExceeded,
            format!(
                "basket of {} bytes exceeds the {max_bytes}-byte limit",
                key.obj_len
            ),
        ));
    }
    let stored = record
        .get(key_len..nbytes)
        .ok_or_else(|| Error::malformed("basket record is truncated"))?;
    let obj_len = usize::try_from(key.obj_len).map_err(|_| Error::malformed("ObjLen"))?;
    let mut raw = if stored.len() == obj_len {
        stored.to_vec()
    } else {
        compress::decompress(stored, obj_len)?
    };
    let border = usize::try_from(last)
        .ok()
        .and_then(|l| l.checked_sub(key_len))
        .filter(|&b| b <= raw.len())
        .ok_or_else(|| Error::malformed(format!("fLast {last} outside the basket")))?;
    let entries = usize::try_from(entries).map_err(|_| Error::malformed("fNevBuf"))?;
    let offsets = if border == raw.len() {
        None
    } else {
        // A count, then entries + 1 positions counted from the key start;
        // the last one is not reliable, so the border replaces it.
        let tail = raw.get(border..).unwrap_or_default();
        let mut offsets_buf = RBuffer::new(tail, 0);
        let n = usize::try_from(offsets_buf.read_len32("offset count")?)
            .map_err(|_| Error::malformed("offset count"))?;
        if n < entries || n.saturating_mul(4) > offsets_buf.remaining() {
            return Err(Error::malformed(format!(
                "basket of {entries} entries has {n} entry offsets"
            )));
        }
        let mut offsets = Vec::with_capacity(entries + 1);
        let mut previous = 0;
        for raw in offsets_buf.take(entries * 4)?.as_chunks::<4>().0 {
            let at = usize::try_from(i32::from_be_bytes(*raw))
                .ok()
                .and_then(|p| p.checked_sub(key_len))
                .filter(|&p| p <= border)
                .ok_or_else(|| Error::malformed("entry offset outside the basket"))?;
            if at < previous {
                return Err(Error::malformed("entry offsets go backwards"));
            }
            previous = at;
            offsets.push(at);
        }
        offsets.push(border);
        Some(offsets)
    };
    let uncompressed = raw.len();
    raw.truncate(border);
    Ok(Basket {
        data: raw,
        offsets,
        entries,
        entry_size: usize::try_from(entry_size).map_err(|_| Error::malformed("fNevBufSize"))?,
        uncompressed,
        key_len,
    })
}

/// Decodes a basket embedded in the tree object (`TBranch::fBaskets`), as
/// `TBasket::Streamer` writes it: the key with the basket fields, the entry
/// offsets when entries vary in size, then the buffer (a copy of the key,
/// then the entries up to `fLast`). Embedded baskets are never compressed.
pub(crate) fn decode_embedded(bytes: &[u8]) -> Result<Basket> {
    // The key of an embedded basket has `fNbytes` 0, so it is not a valid
    // record key: read the fields uproot reads. The basket fields are the
    // last 19 bytes of the key.
    let mut cursor = RBuffer::new(bytes, 0);
    cursor.skip(4 + 2 + 4 + 4)?; // fNbytes, version, fObjlen, fDatime
    let key_len =
        usize::try_from(cursor.read_i16()?).map_err(|_| Error::malformed("negative key length"))?;
    let fields = key_len
        .checked_sub(19)
        .ok_or_else(|| Error::malformed(format!("embedded basket key of {key_len} bytes")))?;
    cursor.seek(fields)?;
    cursor.read_i16()?; // TBasket version
    cursor.read_i32()?; // fBufferSize
    let entry_size = cursor.read_i32()?.unsigned_abs();
    let entries =
        usize::try_from(cursor.read_len32("fNevBuf")?).map_err(|_| Error::malformed("fNevBuf"))?;
    let last =
        usize::try_from(cursor.read_len32("fLast")?).map_err(|_| Error::malformed("fLast"))?;
    cursor.seek(key_len)?;
    let entry_size = usize::try_from(entry_size).map_err(|_| Error::malformed("fNevBufSize"))?;
    let border = last
        .checked_sub(key_len)
        .ok_or_else(|| Error::malformed(format!("fLast {last} before the end of the key")))?;
    let offsets = if entry_size.saturating_mul(entries).saturating_add(key_len) == last {
        None
    } else {
        let n = usize::try_from(cursor.read_len32("offset count")?)
            .map_err(|_| Error::malformed("offset count"))?;
        if n < entries || n.saturating_mul(4) > cursor.remaining() {
            return Err(Error::malformed(format!(
                "embedded basket of {entries} entries has {n} offsets"
            )));
        }
        let mut offsets = Vec::with_capacity(entries + 1);
        for i in 0..n {
            let at = cursor.read_i32()?;
            if i < entries {
                let at = usize::try_from(at)
                    .ok()
                    .and_then(|p| p.checked_sub(key_len))
                    .filter(|&p| p <= border)
                    .ok_or_else(|| Error::malformed("entry offset outside the basket"))?;
                offsets.push(at);
            }
        }
        offsets.push(border);
        if offsets.windows(2).any(|w| matches!(w, [a, b] if a > b)) {
            return Err(Error::malformed("entry offsets go backwards"));
        }
        Some(offsets)
    };
    cursor.skip(key_len)?; // the buffer starts with a copy of the key
    let data = cursor.take(border)?.to_vec();
    Ok(Basket {
        uncompressed: data.len(),
        data,
        offsets,
        entries,
        entry_size,
        key_len,
    })
}

#[cfg(test)]
mod tests {
    use rhizoid_source::FileOffset;

    use super::*;
    use crate::core::buffer::WBuffer;
    use crate::core::key::KeyForm;
    use crate::core::time::Datime;

    /// The basket fields: version, fBufferSize, fNevBufSize, fNevBuf, fLast
    /// and a flag byte.
    const FIELDS: usize = 2 + 4 + 4 + 4 + 4 + 1;

    /// The key length; a negative entry size adds the fIOBits byte.
    fn key_len(entry_size: i32) -> usize {
        KeyHeader::encoded_len("TBasket", "x", "t", true) + FIELDS + usize::from(entry_size < 0)
    }

    fn small(n: usize) -> u32 {
        u32::try_from(n).expect("small")
    }

    /// The key, `nbytes` and `obj_len` given, with the basket fields.
    fn key(entry_size: i32, entries: u32, payload: usize, nbytes: u32, obj_len: u32) -> Vec<u8> {
        let len = key_len(entry_size);
        let mut key = KeyHeader::for_record(
            KeyForm::AlwaysLarge,
            FileOffset::new(1000),
            FileOffset::new(100),
            "TBasket",
            "x",
            "t",
            obj_len,
            0,
            0,
            Datime::REPRODUCIBLE,
        )
        .expect("key");
        key.key_len = u16::try_from(len).expect("small");
        key.nbytes = nbytes;
        let mut w = WBuffer::new();
        key.encode(&mut w).expect("encode");
        w.write_i16(3);
        w.write_i32(32000);
        w.write_i32(entry_size);
        if entry_size < 0 {
            w.write_u8(0);
        }
        w.write_u32(entries);
        w.write_u32(small(len + payload));
        w.write_u8(0);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), len);
        bytes
    }

    /// A stored (uncompressed) basket record: the entries, then `tail`.
    fn record(entry_size: i32, entries: u32, payload: &[u8], tail: &[u8]) -> Vec<u8> {
        let data = small(payload.len() + tail.len());
        let nbytes = small(key_len(entry_size)) + data;
        let mut bytes = key(entry_size, entries, payload.len(), nbytes, data);
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(tail);
        bytes
    }

    /// An offset block: `count`, then positions counted from the key start.
    fn offsets(key_len: usize, count: u32, at: &[usize]) -> Vec<u8> {
        let mut w = WBuffer::new();
        w.write_u32(count);
        for &p in at {
            w.write_i32(i32::try_from(key_len + p).expect("small"));
        }
        w.into_inner()
    }

    const PAYLOAD: &[u8] = b"abcdefgh";

    fn check_varying(b: &Basket) {
        assert_eq!(b.offsets, Some(vec![0, 3, 3, 8]));
        assert_eq!(b.data, PAYLOAD);
        assert_eq!(b.entry(0).expect("entry"), b"abc");
        assert_eq!(b.entry(1).expect("entry"), b"");
        assert_eq!(b.entry(2).expect("entry"), b"defgh");
        assert_eq!(b.entries(1, 3).expect("entries"), b"defgh");
        assert_eq!(b.entry_start(2).expect("start"), 3);
        assert!(b.entry(3).is_err());
    }

    /// Entries of one size are found by it; a negative size is followed by
    /// the fIOBits byte.
    #[test]
    fn fixed_size_entries_are_found_by_their_size() {
        let payload: Vec<u8> = (0..12).collect();
        for size in [4, -4] {
            let b = decode(&record(size, 3, &payload, &[]), 1 << 20).expect("decode");
            assert_eq!((b.entries, b.entry_size, &b.offsets), (3, 4, &None));
            assert_eq!(b.key_len, key_len(size));
            assert_eq!(b.uncompressed, 12);
            assert_eq!(b.entry_start(2).expect("start"), 8);
            assert_eq!(b.entry(1).expect("entry"), &payload[4..8]);
            assert_eq!(b.entry(2).expect("entry"), &payload[8..12]);
            assert_eq!(b.entries(1, 3).expect("entries"), &payload[4..12]);
            assert_eq!(b.entries(0, 1).expect("entries"), &payload[..4]);
            assert!(b.entry(3).is_err());
        }
    }

    /// Entries of 3, 0 and 5 bytes, found by their offsets: as many as
    /// entries, one more (as ROOT writes), or more bytes than they need.
    #[test]
    fn varying_entries_are_found_by_their_offsets() {
        let kl = key_len(0);
        for (count, at) in [
            (3, &[0, 3, 3][..]),
            (4, &[0, 3, 3, 8][..]),
            (3, &[0, 3, 3, 8][..]),
        ] {
            let tail = offsets(kl, count, at);
            let b = decode(&record(0, 3, PAYLOAD, &tail), 1 << 20).expect("decode");
            check_varying(&b);
            assert_eq!(b.uncompressed, PAYLOAD.len() + tail.len());
        }
        // Fewer offsets than entries, more than the bytes hold, going
        // backwards, or past the entries.
        for (count, at) in [
            (2, &[0, 3, 3][..]),
            (9, &[0, 3, 3][..]),
            (3, &[0, 5, 3][..]),
            (3, &[0, 3, 9][..]),
        ] {
            let tail = offsets(kl, count, at);
            let e = decode(&record(0, 3, PAYLOAD, &tail), 1 << 20).expect_err("bad offsets");
            assert_eq!(e.kind(), ErrorKind::Malformed, "{count} {at:?}: {e}");
        }
    }

    /// The limit applies to the bytes after decompression, inclusively.
    #[test]
    fn baskets_past_the_limit_are_refused() {
        let payload: Vec<u8> = (0..12).collect();
        let r = record(4, 3, &payload, &[]);
        assert!(decode(&r, 12).is_ok());
        assert_eq!(
            decode(&r, 11).expect_err("too large").kind(),
            ErrorKind::LimitExceeded
        );
        // A record of its key alone is a basket kept in the tree object.
        let alone = key(4, 0, 0, small(key_len(4)), 0);
        assert_eq!(
            decode(&alone, 1 << 20).expect_err("embedded").kind(),
            ErrorKind::Unsupported
        );
    }

    /// An embedded basket: the key (fNbytes 0), the offsets of varying
    /// entries, then a copy of the key and the entries.
    fn embedded(entry_size: i32, payload: &[u8], offsets: Option<Vec<u8>>) -> Vec<u8> {
        let head = key(entry_size, 3, payload.len(), 0, 0);
        let mut bytes = head.clone();
        bytes.extend(offsets.unwrap_or_default());
        bytes.extend_from_slice(&head);
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn embedded_baskets_decode() {
        let payload: Vec<u8> = (0..12).collect();
        let b = decode_embedded(&embedded(4, &payload, None)).expect("fixed");
        assert_eq!((b.entries, b.entry_size, &b.offsets), (3, 4, &None));
        assert_eq!(b.data, payload);
        assert_eq!(b.entry(1).expect("entry"), &payload[4..8]);

        let kl = key_len(0);
        for (count, at) in [(3, &[0, 3, 3][..]), (4, &[0, 3, 3, 8][..])] {
            let b = decode_embedded(&embedded(0, PAYLOAD, Some(offsets(kl, count, at))))
                .expect("varying");
            check_varying(&b);
            assert_eq!(b.key_len, kl);
        }
        for (count, at) in [
            (2, &[0, 3][..]),
            (30, &[0, 3, 3][..]),
            (3, &[0, 5, 3][..]),
            (3, &[0, 3, 9][..]),
        ] {
            let e = decode_embedded(&embedded(0, PAYLOAD, Some(offsets(kl, count, at))))
                .expect_err("bad offsets");
            assert_eq!(e.kind(), ErrorKind::Malformed, "{count} {at:?}: {e}");
        }
    }
}
