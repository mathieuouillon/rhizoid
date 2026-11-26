//! The record walker: steps from `fBEGIN` by `Nbytes`, like ROOT's recovery
//! [§3, RB§1, RB§4].

use rhizoid_source::{FileOffset, ReadAt};

use super::buffer::RBuffer;
use super::directory::FreeSegment;
use super::error::{Context, Error, Result};
use super::key::KeyHeader;

/// The largest key header the walker reads in one go.
const MAX_KEY_PROBE: u64 = 64 * 1024;

/// One step of the walk.
#[derive(Debug)]
pub(crate) enum Step {
    /// A record with a valid key header.
    Record(KeyHeader),
    /// Free space: either marked by a negative `Nbytes` (ROOT) or listed in
    /// the free list without a marker (uproot's reserved space).
    Gap { len: u64 },
}

/// Walks every record between `begin` and `end`, skipping the free segments
/// in `free`. Stops at the first structure that cannot be decoded and
/// returns the error alongside the steps walked so far.
pub(crate) fn walk(
    source: &dyn ReadAt,
    begin: FileOffset,
    end: FileOffset,
    free: &[FreeSegment],
) -> (Vec<Step>, Option<Error>) {
    let mut steps = Vec::new();
    let mut at = begin;
    while at < end {
        // Free space listed in the free list may carry no marker at all.
        if let Some(seg) = free.iter().find(|s| s.first <= at && at <= s.last) {
            let stop = seg.last.get().saturating_add(1).min(end.get());
            steps.push(Step::Gap {
                len: stop - at.get(),
            });
            at = FileOffset::new(stop);
            continue;
        }
        match step(source, at, end) {
            Ok((s, len)) => {
                steps.push(s);
                match at.checked_add(len) {
                    Some(next) if len > 0 => at = next,
                    _ => {
                        return (
                            steps,
                            Some(
                                Error::malformed("zero-length record")
                                    .with_context(Context::Offset(at)),
                            ),
                        );
                    }
                }
            }
            Err(e) => return (steps, Some(e)),
        }
    }
    (steps, None)
}

fn step(source: &dyn ReadAt, at: FileOffset, end: FileOffset) -> Result<(Step, u64)> {
    let room = end.get() - at.get();
    let head = source.read_at(at..at.checked_add(room.min(4)).unwrap_or(at))?;
    let nbytes = RBuffer::new(&head, 0).at(at).read_i32()?;
    if nbytes < 0 {
        let len = u64::from(nbytes.unsigned_abs());
        return Ok((Step::Gap { len }, len));
    }
    if nbytes == 0 {
        return Err(Error::malformed("zero Nbytes where a record was expected")
            .with_context(Context::Offset(at)));
    }
    let nbytes = u64::try_from(nbytes).map_err(|_| Error::malformed("negative Nbytes"))?;
    let probe = nbytes.min(room).min(MAX_KEY_PROBE);
    let bytes = source.read_at(at..at.checked_add(probe).unwrap_or(at))?;
    let key = KeyHeader::decode(&mut RBuffer::new(&bytes, 0).at(at))?;
    if key.seek_key != at {
        return Err(
            Error::malformed(format!("key claims to be at {}", key.seek_key))
                .with_context(Context::Offset(at)),
        );
    }
    Ok((Step::Record(key), nbytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::WBuffer;
    use crate::core::key::KeyForm;
    use crate::core::time::Datime;

    fn record(at: u64, data: usize) -> Vec<u8> {
        let data_len = u32::try_from(data).expect("small");
        let k = KeyHeader::for_record(
            KeyForm::ByPosition,
            FileOffset::new(at),
            FileOffset::new(100),
            "TObjString",
            "s",
            "",
            data_len,
            data_len,
            1,
            Datime::REPRODUCIBLE,
        )
        .expect("key");
        let mut w = WBuffer::new();
        k.encode(&mut w).expect("encode");
        w.write_zeros(data);
        w.into_inner()
    }

    #[test]
    fn walks_records_marked_gaps_and_listed_free_space() {
        let mut file = vec![0u8; 100];
        let r1 = record(100, 10);
        file.extend_from_slice(&r1);
        let gap_at = file.len();
        file.extend_from_slice(&(-20i32).to_be_bytes()); // ROOT-style marked gap
        file.resize(gap_at + 20, 0);
        let free_at = file.len();
        file.resize(free_at + 50, 0); // uproot-style unmarked reserved space
        let r2_at = u64::try_from(file.len()).expect("small");
        file.extend_from_slice(&record(r2_at, 5));
        let end = FileOffset::new(u64::try_from(file.len()).expect("small"));
        let free_first = FileOffset::new(u64::try_from(free_at).expect("small"));
        let free = [FreeSegment {
            first: free_first,
            last: FileOffset::new(free_first.get() + 49),
        }];

        let (steps, err) = walk(&file, FileOffset::new(100), end, &free);
        assert!(err.is_none(), "{err:?}");
        let kinds: Vec<&str> = steps
            .iter()
            .map(|s| match s {
                Step::Record(_) => "R",
                Step::Gap { .. } => "G",
            })
            .collect();
        assert_eq!(kinds, ["R", "G", "G", "R"]);

        // Without the free list, the unmarked space stops the walk cleanly.
        let (steps, err) = walk(&file, FileOffset::new(100), end, &[]);
        assert_eq!(steps.len(), 2);
        assert_eq!(err.expect("stops").offset(), Some(free_first));
    }
}
