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
