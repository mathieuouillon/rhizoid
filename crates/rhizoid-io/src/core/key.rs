//! Keys: the header in front of every record [§3, RB§4].

use rhizoid_source::FileOffset;

use super::buffer::{RBuffer, WBuffer, string_len};
use super::error::{Context, Error, Result, ResultExt};
use super::time::Datime;

/// Key versions above this store 8-byte offsets.
const LARGE_KEY: i16 = 1000;
/// The `TKey` class version ROOT writes.
pub(crate) const KEY_VERSION: i16 = 4;
/// The 8-byte `SeekPdir` word carries a process-ID offset in its high bits [RB§4].
const PID_SHIFT: u32 = 48;
const PID_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Whether a record kind always uses the 8-byte key form [RB§7, RB§9].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyForm {
    /// Baskets and `RNTuple` blobs.
    AlwaysLarge,
    /// Every other record: large only past `kStartBigFile`.
    ByPosition,
}

/// A decoded key header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyHeader {
    pub(crate) nbytes: u32,
    pub(crate) version: i16,
    pub(crate) obj_len: u32,
    pub(crate) datime: Datime,
    pub(crate) key_len: u16,
    /// Raw on-disk cycle: basket keys carry 0, so this is not a `Cycle` [RB§4].
    pub(crate) cycle: i16,
    pub(crate) seek_key: FileOffset,
    pub(crate) seek_pdir: FileOffset,
    pub(crate) pid_offset: u16,
    pub(crate) class_name: String,
    pub(crate) name: String,
    pub(crate) title: String,
}

impl KeyHeader {
    /// Decodes a key header. Rejects negative sizes and `Nbytes < KeyLen` [RB§4].
    pub(crate) fn decode(r: &mut RBuffer<'_>) -> Result<Self> {
        let nbytes = r.read_len32("Nbytes")?;
        let version = r.read_i16()?;
        let obj_len = r.read_len32("ObjLen")?;
        let datime = Datime::from_raw(r.read_u32()?);
        let key_len =
            u16::try_from(r.read_i16()?).map_err(|_| Error::malformed("negative KeyLen"))?;
        let cycle = r.read_i16()?;
        let large = version > LARGE_KEY;
        let seek_key = r.read_seek(large)?;
        let (seek_pdir, pid_offset) = if large {
            let raw = r.read_seek(true)?.get();
            let pid =
                u16::try_from(raw >> PID_SHIFT).map_err(|_| Error::malformed("bad pid offset"))?;
            (FileOffset::new(raw & PID_MASK), pid)
        } else {
            (r.read_seek(false)?, 0)
        };
        let class_name = r.read_string()?;
        let name = r.read_string()?;
        let title = r.read_string()?;
        if key_len == 0 || nbytes < u32::from(key_len) {
            return Err(Error::malformed(format!(
                "key sizes are inconsistent (Nbytes {nbytes}, KeyLen {key_len})"
            )))
            .context(Context::Key(name));
        }
        Ok(Self {
            nbytes,
            version,
            obj_len,
            datime,
            key_len,
            cycle,
            seek_key,
            seek_pdir,
            pid_offset,
            class_name,
            name,
            title,
        })
    }

    /// Whether this header uses 8-byte offsets.
    pub(crate) fn is_large(&self) -> bool {
        self.version > LARGE_KEY
    }

    /// Bytes of (possibly compressed) data after the key.
    pub(crate) fn data_len(&self) -> u32 {
        self.nbytes - u32::from(self.key_len)
    }

    /// Whether the data is compressed: stored bytes fewer than `ObjLen` [§3].
    pub(crate) fn is_compressed(&self) -> bool {
        self.obj_len > self.data_len()
    }

    /// The bytes of the whole record.
    pub(crate) fn record_range(&self) -> std::ops::Range<FileOffset> {
        let end = self
            .seek_key
            .checked_add(u64::from(self.nbytes))
            .unwrap_or(self.seek_key);
        self.seek_key..end
    }

    /// Whether the key holds a directory. ROOT stores `TDirectoryFile` keys
    /// with the class name `TDirectory` [RB§4].
    pub(crate) fn is_directory(&self) -> bool {
        matches!(self.class_name.as_str(), "TDirectory" | "TDirectoryFile")
    }

    /// On-disk size of the header in the given form.
    pub(crate) fn encoded_len(class_name: &str, name: &str, title: &str, large: bool) -> usize {
        18 + if large { 16 } else { 8 }
            + string_len(class_name)
            + string_len(name)
            + string_len(title)
    }

    /// Builds a header for a new record at `seek_key`, choosing the key form.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the on-disk fields one to one"
    )]
    pub(crate) fn for_record(
        form: KeyForm,
        seek_key: FileOffset,
        seek_pdir: FileOffset,
        class_name: &str,
        name: &str,
        title: &str,
        obj_len: u32,
        data_len: u32,
        cycle: i16,
        datime: Datime,
    ) -> Result<Self> {
        let large = form == KeyForm::AlwaysLarge || seek_key.is_large();
        let key_len = Self::encoded_len(class_name, name, title, large);
        let key_len =
            u16::try_from(key_len).map_err(|_| Error::malformed("key header too long"))?;
        let nbytes = u32::from(key_len)
            .checked_add(data_len)
            .ok_or_else(|| Error::malformed("record too large"))?;
        Ok(Self {
            nbytes,
            version: if large {
                KEY_VERSION + LARGE_KEY
            } else {
                KEY_VERSION
            },
            obj_len,
            datime,
            key_len,
            cycle,
            seek_key,
            seek_pdir,
            pid_offset: 0,
            class_name: class_name.to_owned(),
            name: name.to_owned(),
            title: title.to_owned(),
        })
    }

    /// Encodes the header.
    pub(crate) fn encode(&self, w: &mut WBuffer) -> Result<()> {
        let large = self.is_large();
        w.write_len32(self.nbytes)?;
        w.write_i16(self.version);
        w.write_len32(self.obj_len)?;
        w.write_u32(self.datime.raw());
        w.write_i16(i16::try_from(self.key_len).map_err(|_| Error::malformed("KeyLen too large"))?);
        w.write_i16(self.cycle);
        w.write_seek(self.seek_key, large)?;
        if large {
            let pdir = self.seek_pdir.get() | (u64::from(self.pid_offset) << PID_SHIFT);
            w.write_seek(FileOffset::new(pdir), true)?;
        } else {
            w.write_seek(self.seek_pdir, false)?;
        }
        w.write_string(&self.class_name)?;
        w.write_string(&self.name)?;
        w.write_string(&self.title)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn round_trip(h: &KeyHeader) -> KeyHeader {
        let mut w = WBuffer::new();
        h.encode(&mut w).expect("encode");
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), usize::from(h.key_len));
        KeyHeader::decode(&mut RBuffer::new(&bytes, 0)).expect("decode")
    }

    #[test]
    fn basket_keys_with_cycle_zero_decode() {
        let mut h = KeyHeader::for_record(
            KeyForm::AlwaysLarge,
            FileOffset::new(4096),
            FileOffset::new(100),
            "TBasket",
            "px",
            "events",
            100,
            80,
            0,
            Datime::REPRODUCIBLE,
        )
        .expect("build");
        h.pid_offset = 3;
        let back = round_trip(&h);
        assert_eq!(back, h);
        assert_eq!(back.cycle, 0);
        assert!(back.is_large());
        assert!(back.is_compressed());
    }

    #[test]
    fn rejects_nbytes_below_keylen() {
        let h = KeyHeader::for_record(
            KeyForm::ByPosition,
            FileOffset::new(100),
            FileOffset::new(0),
            "TFile",
            "f.root",
            "",
            0,
            0,
            1,
            Datime::REPRODUCIBLE,
        )
        .expect("build");
        let mut w = WBuffer::new();
        h.encode(&mut w).expect("encode");
        let mut bytes = w.into_inner();
        bytes[0..4].copy_from_slice(&5i32.to_be_bytes());
        assert!(KeyHeader::decode(&mut RBuffer::new(&bytes, 0)).is_err());
    }

    proptest! {
        #[test]
        fn key_headers_round_trip(
            seek in 100u64..4_000_000_000,
            name in "[a-zA-Z0-9_]{0,300}",
            title in ".{0,40}",
            obj_len in 0u32..1_000_000,
            data_len in 0u32..1_000_000,
            cycle in -5i16..500,
        ) {
            let h = KeyHeader::for_record(
                KeyForm::ByPosition, FileOffset::new(seek), FileOffset::new(100),
                "TH1D", &name, &title, obj_len, data_len, cycle, Datime::now(),
            ).expect("build");
            prop_assert_eq!(h.is_large(), seek > 2_000_000_000);
            prop_assert_eq!(round_trip(&h), h);
        }
    }
}
