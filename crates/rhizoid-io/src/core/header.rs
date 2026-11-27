//! The file header.

use rhizoid_source::FileOffset;

use super::buffer::{RBuffer, WBuffer};
use super::error::{Error, ErrorKind, Result};
use super::time::Uuid;

/// Versions at or above this mark the large (8-byte offset) header.
const LARGE_VERSION: i32 = 1_000_000;

/// The file header: where everything else is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHeader {
    pub(crate) version: i32,
    pub(crate) begin: u32,
    pub(crate) end: FileOffset,
    pub(crate) seek_free: FileOffset,
    pub(crate) nbytes_free: u32,
    pub(crate) nfree: u32,
    pub(crate) nbytes_name: u32,
    pub(crate) units: u8,
    pub(crate) compress: i32,
    pub(crate) seek_info: FileOffset,
    pub(crate) nbytes_info: u32,
    pub(crate) uuid: Uuid,
}

impl FileHeader {
    /// How many bytes to read to be sure to hold the header.
    pub(crate) const PROBE_LEN: u64 = 4096;

    /// ROOT's format version, without the large-file marker (e.g. `63406`).
    #[doc(alias = "fVersion")]
    pub fn version(&self) -> i32 {
        self.version
    }

    /// Offset of the first record (100 in every file ROOT writes).
    #[doc(alias = "fBEGIN")]
    pub fn begin(&self) -> FileOffset {
        FileOffset::new(u64::from(self.begin))
    }

    /// First byte past the last record.
    #[doc(alias = "fEND")]
    pub fn end(&self) -> FileOffset {
        self.end
    }

    /// Offset of the free-segments record.
    #[doc(alias = "fSeekFree")]
    pub fn seek_free(&self) -> FileOffset {
        self.seek_free
    }

    /// Offset of the `StreamerInfo` record.
    #[doc(alias = "fSeekInfo")]
    pub fn seek_info(&self) -> FileOffset {
        self.seek_info
    }

    /// Size of the `StreamerInfo` record.
    #[doc(alias = "fNbytesInfo")]
    pub fn nbytes_info(&self) -> u32 {
        self.nbytes_info
    }

    /// Size of the `TFile` key plus the file's name and title.
    #[doc(alias = "fNbytesName")]
    pub fn nbytes_name(&self) -> u32 {
        self.nbytes_name
    }

    /// The default compression setting (`algorithm * 100 + level`).
    #[doc(alias = "fCompress")]
    pub fn compression_setting(&self) -> i32 {
        self.compress
    }

    /// Whether offsets in the header are 8 bytes wide.
    pub fn is_large(&self) -> bool {
        self.units == 8
    }

    /// The file's UUID.
    pub fn uuid(&self) -> &Uuid {
        &self.uuid
    }

    /// Decodes the header from the first bytes of a file.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.get(..4) != Some(b"root") {
            return Err(Error::new(
                ErrorKind::NotRootFile,
                "not a ROOT file (missing `root` magic)",
            ));
        }
        let mut r = RBuffer::new(bytes, 0).at(FileOffset::new(0));
        r.skip(4)?;
        let raw_version = r.read_i32()?;
        let large = raw_version >= LARGE_VERSION;
        let version = if large {
            raw_version - LARGE_VERSION
        } else {
            raw_version
        };
        let begin = r.read_len32("fBEGIN")?;
        let end = r.read_seek(large)?;
        let seek_free = r.read_seek(large)?;
        let nbytes_free = r.read_len32("fNbytesFree")?;
        let nfree = r.read_len32("nfree")?;
        let nbytes_name = r.read_len32("fNbytesName")?;
        let units = r.read_u8()?;
        let compress = r.read_i32()?;
        let seek_info = r.read_seek(large)?;
        let nbytes_info = r.read_len32("fNbytesInfo")?;
        let uuid = Uuid::decode(&mut r)?;

        if u64::from(begin) > end.get() {
            return Err(Error::malformed(format!(
                "fBEGIN ({begin}) is past fEND ({end})"
            )));
        }
        if !(10..=10_000).contains(&nbytes_name) {
            return Err(Error::malformed(format!(
                "fNbytesName out of range ({nbytes_name})"
            )));
        }
        Ok(Self {
            version,
            begin,
            end,
            seek_free,
            nbytes_free,
            nfree,
            nbytes_name,
            units,
            compress,
            seek_info,
            nbytes_info,
            uuid,
        })
    }

    /// Encodes the header into exactly `fBEGIN` bytes, choosing the large form
    /// when `fEND` is past `kStartBigFile`.
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let large = self.end.is_large();
        let mut w = WBuffer::new();
        w.write_bytes(b"root");
        w.write_i32(if large {
            self.version + LARGE_VERSION
        } else {
            self.version
        });
        w.write_len32(self.begin)?;
        w.write_seek(self.end, large)?;
        w.write_seek(self.seek_free, large)?;
        w.write_len32(self.nbytes_free)?;
        w.write_len32(self.nfree)?;
        w.write_len32(self.nbytes_name)?;
        w.write_u8(if large { 8 } else { 4 });
        w.write_i32(self.compress);
        w.write_seek(self.seek_info, large)?;
        w.write_len32(self.nbytes_info)?;
        self.uuid.encode(&mut w);
        let begin =
            usize::try_from(self.begin).map_err(|_| Error::malformed("fBEGIN too large"))?;
        if w.len() > begin {
            return Err(Error::malformed("header does not fit before fBEGIN"));
        }
        w.write_zeros(begin - w.len());
        Ok(w.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(end: u64) -> FileHeader {
        FileHeader {
            version: 63_406,
            begin: 100,
            end: FileOffset::new(end),
            seek_free: FileOffset::new(end - 60),
            nbytes_free: 60,
            nfree: 1,
            nbytes_name: 58,
            units: 4,
            compress: 505,
            seek_info: FileOffset::new(1234),
            nbytes_info: 400,
            uuid: Uuid::new_random(),
        }
    }

    #[test]
    fn small_header_round_trips() {
        let h = sample(5_000);
        let bytes = h.encode().expect("encode");
        assert_eq!(bytes.len(), 100);
        let back = FileHeader::decode(&bytes).expect("decode");
        assert_eq!(back, h);
        assert_eq!(back.nbytes_info(), 400);
        assert_eq!(back.compression_setting(), 505);
        assert!(!back.is_large());
    }

    /// The small header takes 63 bytes: `fBEGIN` may be exactly there.
    #[test]
    fn the_header_fits_before_begin_exactly() {
        let mut h = sample(5_000);
        h.begin = 63;
        assert_eq!(h.encode().expect("fits").len(), 63);
        h.begin = 62;
        assert_eq!(
            h.encode().expect_err("one byte short").kind(),
            ErrorKind::Malformed
        );
    }

    #[test]
    fn large_header_switches_form_past_2gb() {
        let mut h = sample(3_000_000_000);
        let bytes = h.encode().expect("encode");
        let back = FileHeader::decode(&bytes).expect("decode");
        h.units = 8;
        assert_eq!(back, h);
        assert!(back.is_large());
        assert_eq!(
            i32::from_be_bytes(bytes[4..8].try_into().expect("4 bytes")),
            1_063_406
        );
    }

    #[test]
    fn rejects_non_root_and_bad_begin() {
        assert_eq!(
            FileHeader::decode(b"PK\x03\x04").expect_err("zip").kind(),
            ErrorKind::NotRootFile
        );
        let mut h = sample(5_000);
        h.begin = 6_000;
        let mut bytes = h.encode().unwrap_or_default();
        if bytes.is_empty() {
            // fBEGIN past the end cannot even be encoded within 100 bytes; craft by hand.
            bytes = sample(5_000).encode().expect("encode");
            bytes[8..12].copy_from_slice(&6_000i32.to_be_bytes());
        }
        assert_eq!(
            FileHeader::decode(&bytes).expect_err("bad begin").kind(),
            ErrorKind::Malformed
        );
    }
}
