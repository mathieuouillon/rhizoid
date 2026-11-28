//! Directory records, key lists and free segments.

use rhizoid_source::FileOffset;

use super::buffer::{RBuffer, WBuffer};
use super::error::{Error, Result};
use super::key::KeyHeader;
use super::time::{Datime, Uuid};

/// The `TDirectoryFile` class version ROOT writes.
const DIRECTORY_VERSION: i16 = 5;
const LARGE: i16 = 1000;

/// A directory record: where its key list is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryRecord {
    pub(crate) version: i16,
    pub(crate) created: Datime,
    pub(crate) modified: Datime,
    pub(crate) nbytes_keys: u32,
    pub(crate) nbytes_name: u32,
    pub(crate) seek_dir: FileOffset,
    pub(crate) seek_parent: FileOffset,
    pub(crate) seek_keys: FileOffset,
    pub(crate) uuid: Uuid,
}

impl DirectoryRecord {
    /// On-disk size in both forms: the small form is padded with 12 zero bytes
    /// so it can be rewritten in the large form in place.
    pub(crate) const LEN: usize = 60;

    pub(crate) fn decode(r: &mut RBuffer<'_>) -> Result<Self> {
        let version = r.read_i16()?;
        let created = Datime::from_raw(r.read_u32()?);
        let modified = Datime::from_raw(r.read_u32()?);
        let nbytes_keys = r.read_len32("fNbytesKeys")?;
        let nbytes_name = r.read_len32("fNbytesName")?;
        let large = version > LARGE;
        let seek_dir = r.read_seek(large)?;
        let seek_parent = r.read_seek(large)?;
        let seek_keys = r.read_seek(large)?;
        // Version % 1000 < 2 has no UUID.
        let uuid = if version % LARGE > 1 && r.remaining() >= Uuid::LEN {
            Uuid::decode(r)?
        } else {
            Uuid::NIL
        };
        Ok(Self {
            version,
            created,
            modified,
            nbytes_keys,
            nbytes_name,
            seek_dir,
            seek_parent,
            seek_keys,
            uuid,
        })
    }

    pub(crate) fn new(
        created: Datime,
        nbytes_name: u32,
        seek_dir: FileOffset,
        seek_parent: FileOffset,
        uuid: Uuid,
    ) -> Self {
        Self {
            version: DIRECTORY_VERSION,
            created,
            modified: created,
            nbytes_keys: 0,
            nbytes_name,
            seek_dir,
            seek_parent,
            seek_keys: FileOffset::new(0),
            uuid,
        }
    }

    pub(crate) fn encode(&self, w: &mut WBuffer) -> Result<()> {
        let large =
            self.seek_dir.is_large() || self.seek_parent.is_large() || self.seek_keys.is_large();
        w.write_i16(if large {
            DIRECTORY_VERSION + LARGE
        } else {
            DIRECTORY_VERSION
        });
        w.write_u32(self.created.raw());
        w.write_u32(self.modified.raw());
        w.write_len32(self.nbytes_keys)?;
        w.write_len32(self.nbytes_name)?;
        w.write_seek(self.seek_dir, large)?;
        w.write_seek(self.seek_parent, large)?;
        w.write_seek(self.seek_keys, large)?;
        self.uuid.encode(w);
        if !large {
            w.write_zeros(12);
        }
        Ok(())
    }
}

/// Decodes a key list's data: `nkeys` followed by key headers. Each key's
/// offsets must lie inside the file, as ROOT checks.
pub(crate) fn decode_key_list(r: &mut RBuffer<'_>, file_len: u64) -> Result<Vec<KeyHeader>> {
    let nkeys = r.read_len32("nkeys")?;
    let mut keys = Vec::new();
    keys.try_reserve(usize::try_from(nkeys).unwrap_or(0).min(r.remaining() / 26))
        .map_err(|_| Error::malformed("key list too large for memory"))?;
    for _ in 0..nkeys {
        let key = KeyHeader::decode(r)?;
        let inside = |o: FileOffset| (64..=file_len).contains(&o.get());
        if !inside(key.seek_key) || !(key.seek_pdir.get() == 0 || inside(key.seek_pdir)) {
            return Err(Error::malformed(format!(
                "key `{}` points outside the file ({}, parent {})",
                key.name, key.seek_key, key.seek_pdir
            )));
        }
        keys.push(key);
    }
    Ok(keys)
}

/// Encodes a key list's data.
pub(crate) fn encode_key_list(keys: &[KeyHeader], w: &mut WBuffer) -> Result<()> {
    let n = u32::try_from(keys.len()).map_err(|_| Error::malformed("too many keys"))?;
    w.write_len32(n)?;
    for k in keys {
        k.encode(w)?;
    }
    Ok(())
}

/// One free segment: the first and last free byte, inclusive.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct FreeSegment {
    pub(crate) first: FileOffset,
    pub(crate) last: FileOffset,
}

/// `TFree` class version.
const FREE_VERSION: i16 = 1;

pub(crate) fn decode_free_list(r: &mut RBuffer<'_>) -> Result<Vec<FreeSegment>> {
    let mut out = Vec::new();
    while r.remaining() >= 2 {
        let version = r.read_i16()?;
        if version == 0 {
            break; // zero padding after the last segment
        }
        let large = version > LARGE;
        let first = r.read_seek(large)?;
        let last = r.read_seek(large)?;
        out.push(FreeSegment { first, last });
    }
    Ok(out)
}

pub(crate) fn encode_free_list(segments: &[FreeSegment], w: &mut WBuffer) -> Result<()> {
    for s in segments {
        let large = s.last.is_large();
        w.write_i16(if large {
            FREE_VERSION + LARGE
        } else {
            FREE_VERSION
        });
        w.write_seek(s.first, large)?;
        w.write_seek(s.last, large)?;
    }
    Ok(())
}

/// On-disk size of a free list.
pub(crate) fn free_list_len(segments: &[FreeSegment]) -> usize {
    segments
        .iter()
        .map(|s| if s.last.is_large() { 18 } else { 10 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::key::KeyForm;

    #[test]
    fn directory_record_is_60_bytes_in_both_forms() {
        let mut d = DirectoryRecord::new(
            Datime::now(),
            58,
            FileOffset::new(100),
            FileOffset::new(0),
            Uuid::new_random(),
        );
        d.seek_keys = FileOffset::new(4000);
        d.nbytes_keys = 90;
        let mut w = WBuffer::new();
        d.encode(&mut w).expect("encode");
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), DirectoryRecord::LEN);
        assert_eq!(
            DirectoryRecord::decode(&mut RBuffer::new(&bytes, 0)).expect("decode"),
            d
        );

        d.seek_keys = FileOffset::new(3_000_000_000);
        let mut w = WBuffer::new();
        d.encode(&mut w).expect("encode");
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), DirectoryRecord::LEN);
        let back = DirectoryRecord::decode(&mut RBuffer::new(&bytes, 0)).expect("decode");
        assert_eq!(back.seek_keys, d.seek_keys);
        assert_eq!(back.version, 1005);
    }

    /// A small-form record of `version`, then `tail`.
    fn record(version: i16, tail: &[u8]) -> Vec<u8> {
        let mut w = WBuffer::new();
        w.write_i16(version);
        w.write_u32(0);
        w.write_u32(0);
        w.write_u32(90);
        w.write_u32(58);
        for seek in [100, 0, 4000] {
            w.write_u32(seek);
        }
        let mut bytes = w.into_inner();
        bytes.extend_from_slice(tail);
        bytes
    }

    /// Versions below 2 have no UUID, though bytes follow; a record may
    /// end before it; version 1000 is still the small form.
    #[test]
    fn directory_records_of_every_version_decode() {
        let mut uuid = WBuffer::new();
        Uuid::new_random().encode(&mut uuid);
        let uuid = uuid.into_inner();
        let decode = |bytes: &[u8]| DirectoryRecord::decode(&mut RBuffer::new(bytes, 0));
        for version in [0, 1] {
            let d = decode(&record(version, &uuid)).expect("decode");
            assert_eq!(d.uuid, Uuid::NIL, "version {version}");
        }
        let d = decode(&record(5, &uuid)).expect("decode");
        assert_ne!(d.uuid, Uuid::NIL);
        let d = decode(&record(5, &uuid[..4])).expect("a record without its UUID");
        assert_eq!(d.uuid, Uuid::NIL);
        let d = decode(&record(LARGE, &[])).expect("version 1000");
        assert_eq!(
            (d.seek_dir, d.seek_keys),
            (FileOffset::new(100), FileOffset::new(4000))
        );
    }

    /// Any one large offset makes the whole record large.
    #[test]
    fn one_large_offset_makes_a_large_directory() {
        let big = FileOffset::new(3_000_000_000);
        for which in 0..3 {
            let mut d = DirectoryRecord::new(
                Datime::REPRODUCIBLE,
                58,
                FileOffset::new(100),
                FileOffset::new(0),
                Uuid::NIL,
            );
            match which {
                0 => d.seek_dir = big,
                1 => d.seek_parent = big,
                _ => d.seek_keys = big,
            }
            let mut w = WBuffer::new();
            d.encode(&mut w).expect("encode");
            let back =
                DirectoryRecord::decode(&mut RBuffer::new(&w.into_inner(), 0)).expect("decode");
            assert_eq!(back.version, DIRECTORY_VERSION + LARGE, "offset {which}");
            assert_eq!(
                (back.seek_dir, back.seek_parent, back.seek_keys),
                (d.seek_dir, d.seek_parent, d.seek_keys)
            );
        }
    }

    #[test]
    fn key_list_round_trips_and_rejects_wild_offsets() {
        let k = KeyHeader::for_record(
            KeyForm::ByPosition,
            FileOffset::new(500),
            FileOffset::new(100),
            "TH1D",
            "h",
            "a histogram",
            300,
            200,
            1,
            Datime::REPRODUCIBLE,
        )
        .expect("build");
        let mut w = WBuffer::new();
        encode_key_list(std::slice::from_ref(&k), &mut w).expect("encode");
        let bytes = w.into_inner();
        assert_eq!(
            decode_key_list(&mut RBuffer::new(&bytes, 0), 10_000).expect("decode"),
            vec![k.clone()]
        );
        assert!(decode_key_list(&mut RBuffer::new(&bytes, 0), 400).is_err());

        // A key with no parent (`fSeekPdir` 0, as the top directory's).
        let top = KeyHeader {
            seek_pdir: FileOffset::new(0),
            ..k
        };
        let mut w = WBuffer::new();
        encode_key_list(std::slice::from_ref(&top), &mut w).expect("encode");
        assert_eq!(
            decode_key_list(&mut RBuffer::new(&w.into_inner(), 0), 10_000).expect("decode"),
            vec![top]
        );
    }

    /// Large segments round-trip; version 1000 is still the small form.
    #[test]
    fn free_segments_of_both_forms_decode() {
        let segs = [FreeSegment {
            first: FileOffset::new(9000),
            last: FileOffset::new(3_000_000_000),
        }];
        let mut w = WBuffer::new();
        encode_free_list(&segs, &mut w).expect("encode");
        assert_eq!(w.len(), 18);
        assert_eq!(
            decode_free_list(&mut RBuffer::new(&w.into_inner(), 0)).expect("decode"),
            segs
        );
        let mut w = WBuffer::new();
        w.write_i16(LARGE);
        w.write_u32(300);
        w.write_u32(400);
        assert_eq!(
            decode_free_list(&mut RBuffer::new(&w.into_inner(), 0)).expect("decode"),
            [FreeSegment {
                first: FileOffset::new(300),
                last: FileOffset::new(400),
            }]
        );
    }

    #[test]
    fn free_lists_round_trip_and_stop_at_padding() {
        let segs = [
            FreeSegment {
                first: FileOffset::new(300),
                last: FileOffset::new(400),
            },
            FreeSegment {
                first: FileOffset::new(9000),
                last: FileOffset::new(2_000_000_000),
            },
        ];
        let mut w = WBuffer::new();
        encode_free_list(&segs, &mut w).expect("encode");
        assert_eq!(w.len(), free_list_len(&segs));
        w.write_zeros(8);
        let bytes = w.into_inner();
        assert_eq!(
            decode_free_list(&mut RBuffer::new(&bytes, 0)).expect("decode"),
            segs
        );
    }
}
