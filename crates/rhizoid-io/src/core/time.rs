//! ROOT's packed date (`TDatime`) and `TUUID`.

use core::fmt;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::time::{SystemTime, UNIX_EPOCH};

use super::buffer::{RBuffer, WBuffer};
use super::error::Result;

/// A date packed into 32 bits as ROOT's `TDatime` stores it:
/// `(year − 1995)`, month, day, hour, minute, second in 6/4/5/5/6/6 bits.
#[doc(alias = "TDatime")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Datime(u32);

impl Datime {
    /// The value ROOT writes in `kReproducible` mode.
    pub const REPRODUCIBLE: Self = Self(1);

    /// Wraps a raw packed value as read from a file.
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw packed value.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Packs a calendar date and time. Returns `None` if a field is out of range
    /// (years 1995–2058 are representable).
    pub fn from_parts(
        year: u32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<Self> {
        let y = year.checked_sub(1995)?;
        let ok = y < 64
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour < 24
            && minute < 60
            && second < 62;
        ok.then_some(Self(
            y << 26 | month << 22 | day << 17 | hour << 12 | minute << 6 | second,
        ))
    }

    /// Unpacks into `(year, month, day, hour, minute, second)`.
    pub const fn to_parts(self) -> (u32, u32, u32, u32, u32, u32) {
        let v = self.0;
        (
            (v >> 26) + 1995,
            (v >> 22) & 0xF,
            (v >> 17) & 0x1F,
            (v >> 12) & 0x1F,
            (v >> 6) & 0x3F,
            v & 0x3F,
        )
    }

    /// The current time, in UTC.
    pub fn now() -> Self {
        Self::from(SystemTime::now())
    }
}

impl From<SystemTime> for Datime {
    fn from(t: SystemTime) -> Self {
        let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let days = secs / 86_400;
        let rem = secs % 86_400;
        let (y, m, d) = civil_from_days(days);
        let hour = u32::try_from(rem / 3600).unwrap_or(0);
        let minute = u32::try_from(rem % 3600 / 60).unwrap_or(0);
        let second = u32::try_from(rem % 60).unwrap_or(0);
        Self::from_parts(y, m, d, hour, minute, second).unwrap_or(Self::REPRODUCIBLE)
    }
}

impl fmt::Display for Datime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day, hour, minute, second) = self.to_parts();
        write!(
            f,
            "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
        )
    }
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: u64) -> (u32, u32, u32) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        u32::try_from(y).unwrap_or(0),
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// ROOT's `TUUID` as stored on disk: a 2-byte version and 16 bytes.
#[doc(alias = "TUUID")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Uuid {
    version: u16,
    bytes: [u8; 16],
}

impl Uuid {
    /// On-disk size.
    pub(crate) const LEN: usize = 18;

    /// The all-zero UUID ROOT writes in `kReproducible` mode.
    pub const NIL: Self = Self {
        version: 1,
        bytes: [0; 16],
    };

    /// A new random UUID (version 4 layout). Not cryptographic.
    pub fn new_random() -> Self {
        let mut bytes = [0u8; 16];
        for (i, chunk) in bytes.chunks_mut(8).enumerate() {
            let mut h = RandomState::new().build_hasher();
            h.write_usize(i);
            h.write_u128(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
            );
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
        if let Some(b) = bytes.get_mut(6) {
            *b = (*b & 0x0F) | 0x40;
        }
        if let Some(b) = bytes.get_mut(8) {
            *b = (*b & 0x3F) | 0x80;
        }
        Self { version: 1, bytes }
    }

    /// Returns the 16 UUID bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.bytes
    }

    pub(crate) fn decode(r: &mut RBuffer<'_>) -> Result<Self> {
        let version = r.read_u16()?;
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(r.take(16)?);
        Ok(Self { version, bytes })
    }

    pub(crate) fn encode(&self, w: &mut WBuffer) {
        w.write_u16(self.version);
        w.write_bytes(&self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datime_packs_like_root() {
        let d = Datime::from_parts(2026, 11, 26, 13, 45, 7).expect("valid");
        assert_eq!(d.to_parts(), (2026, 11, 26, 13, 45, 7));
        assert_eq!(d.raw() >> 26, 31);
        assert_eq!(d.to_string(), "2026-11-26 13:45:07");
        assert!(Datime::from_parts(1994, 1, 1, 0, 0, 0).is_none());
    }

    #[test]
    fn civil_dates_are_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_418), (2025, 11, 26));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn uuid_round_trips() {
        let u = Uuid::new_random();
        let mut w = WBuffer::new();
        u.encode(&mut w);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), Uuid::LEN);
        let mut r = RBuffer::new(&bytes, 0);
        assert_eq!(Uuid::decode(&mut r).expect("decode"), u);
        assert_ne!(Uuid::new_random(), u);
    }

    #[test]
    fn parts_out_of_range_are_refused() {
        assert!(Datime::from_parts(1995, 1, 1, 0, 0, 0).is_some());
        assert!(Datime::from_parts(2058, 12, 31, 23, 59, 61).is_some());
        for (y, mo, d, h, mi, s) in [
            (1994, 12, 31, 0, 0, 0),
            (2059, 1, 1, 0, 0, 0),
            (2000, 0, 1, 0, 0, 0),
            (2000, 13, 1, 0, 0, 0),
            (2000, 1, 0, 0, 0, 0),
            (2000, 1, 32, 0, 0, 0),
            (2000, 1, 1, 24, 0, 0),
            (2000, 1, 1, 0, 60, 0),
            (2000, 1, 1, 0, 0, 62),
        ] {
            assert_eq!(
                Datime::from_parts(y, mo, d, h, mi, s),
                None,
                "{y}-{mo}-{d} {h}:{mi}:{s}"
            );
        }
    }

    /// A time is its UTC calendar date: a leap day, the eve of 2000.
    #[test]
    fn system_times_are_utc_dates() {
        let at = |secs| Datime::from(UNIX_EPOCH + std::time::Duration::from_secs(secs)).to_parts();
        assert_eq!(at(1_709_214_307), (2024, 2, 29, 13, 45, 7));
        assert_eq!(at(946_684_799), (1999, 12, 31, 23, 59, 59));
        assert_eq!(at(946_684_800), (2000, 1, 1, 0, 0, 0));
        // Before 1995: not representable.
        assert_eq!(Datime::from(UNIX_EPOCH), Datime::REPRODUCIBLE);
    }

    /// Random UUIDs have the version 4 layout: version nibble 4, variant 10.
    #[test]
    fn random_uuids_are_version_4() {
        for _ in 0..32 {
            let u = Uuid::new_random();
            assert_eq!(u.as_bytes()[6] >> 4, 4, "{u:?}");
            assert_eq!(u.as_bytes()[8] >> 6, 0b10, "{u:?}");
        }
    }
}
