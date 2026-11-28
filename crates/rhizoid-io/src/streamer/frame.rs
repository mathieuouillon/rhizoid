//! Object framing: byte counts, versions, class tags and references.
//!
//! Every positional value here (class tags, object references) counts from
//! the start of the key, which is what [`RBuffer::key_position`] and
//! [`WBuffer::key_position`] return.

use std::collections::HashMap;

use crate::core::buffer::{RBuffer, WBuffer};
use crate::core::error::{Context, Diagnostic, DiagnosticKind, Error, ErrorKind, Result};

/// Bit 30 marks a byte count (`kByteCountMask`).
pub(crate) const BYTE_COUNT_MASK: u32 = 0x4000_0000;
/// A class name follows (`kNewClassTag`).
pub(crate) const NEW_CLASS_TAG: u32 = 0xFFFF_FFFF;
/// Bit 31 marks a reference to an already-seen class (`kClassMask`).
pub(crate) const CLASS_MASK: u32 = 0x8000_0000;
/// Added to every map position so that none equals the null tag (`kMapOffset`).
pub(crate) const MAP_OFFSET: u32 = 2;
/// OR-ed into the version of member-wise collections (`kStreamedMemberWise`).
pub(crate) const MEMBER_WISE: i16 = 0x4000;
/// The longest class name accepted after `kNewClassTag`.
const MAX_CLASS_NAME: usize = 4096;
/// `TObject::kIsReferenced`: a process-ID index follows the bits.
const IS_REFERENCED: u32 = 0x10;
/// `kIsOnHeap | kNotDeleted`, which ROOT ≤ 6.28 and uproot write.
pub(crate) const TOBJECT_DEFAULT_BITS: u32 = 0x0300_0000;

/// An object header: optional byte count, then the version.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Header {
    pub(crate) version: i16,
    /// Where the object ends in the buffer, when it has a byte count.
    pub(crate) end: Option<usize>,
}

/// Reads an object header. The byte count is optional: without bit 30 the
/// first two bytes are the version.
pub(crate) fn read_header(r: &mut RBuffer<'_>) -> Result<Header> {
    let start = r.position();
    let word = r.peek_u32()?;
    let end = if word & BYTE_COUNT_MASK == 0 {
        None
    } else {
        r.skip(4)?;
        let count = usize::try_from(word & !BYTE_COUNT_MASK)
            .map_err(|_| Error::malformed("byte count too large"))?;
        let end = start + 4 + count;
        if count < 2 || end > r.len() {
            return Err(Error::malformed(format!(
                "byte count {count} runs past the end of the object"
            )));
        }
        Some(end)
    };
    let version = r.read_i16()?;
    // Foreign classes (no ClassDef) write version 0 + a checksum.
    if version <= 0 && end.is_some_and(|e| e - start >= 10) {
        r.skip(4)?;
    }
    Ok(Header { version, end })
}

/// What to do when an object's byte count disagrees with what was read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum OnMismatch {
    /// Reposition to the declared end and record a diagnostic, as ROOT does.
    Reposition,
    /// Fail with `ErrorKind::Strict`.
    Fail,
}

/// Checks that the cursor sits at the end declared by `header`.
pub(crate) fn finish(
    r: &mut RBuffer<'_>,
    end: Option<usize>,
    class: &str,
    on_mismatch: OnMismatch,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<()> {
    let Some(end) = end else { return Ok(()) };
    let at = r.position();
    if at == end {
        return Ok(());
    }
    let message = format!("byte count mismatch (object ends at {end}, read up to {at})");
    match on_mismatch {
        OnMismatch::Fail => {
            Err(Error::new(ErrorKind::Strict, message)
                .with_context(Context::Class(class.to_owned())))
        }
        OnMismatch::Reposition => {
            diagnostics.push(Diagnostic::new(
                DiagnosticKind::Irregular,
                format!("{message} in class {class}"),
                r.file_offset(),
            ));
            r.seek(end)
        }
    }
}

/// The classes seen so far in one key's buffer, by map position.
#[derive(Debug, Default)]
pub(crate) struct ClassMap(HashMap<u32, String>);

/// What `ReadObjectAny` found.
#[derive(Debug)]
pub(crate) enum Tag {
    /// A null pointer.
    Null,
    /// A reference to an object read earlier, by map position.
    Ref(u32),
    /// A new object of `class` follows.
    Object {
        class: String,
        /// The map position under which the object is registered.
        position: u32,
        /// Where the object ends, when the tag carried a byte count.
        end: Option<usize>,
    },
}

/// Reads the class tag in front of a pointed-to object (`ReadObjectAny`).
pub(crate) fn read_tag(r: &mut RBuffer<'_>, classes: &mut ClassMap) -> Result<Tag> {
    let beg = r.position();
    let beg_key = r.key_position()?;
    let word = r.read_u32()?;
    let (tag, tag_key, end) = if word & BYTE_COUNT_MASK == 0 || word == NEW_CLASS_TAG {
        (word, None, None)
    } else {
        let tag_key = r.key_position()?;
        let count = usize::try_from(word & !BYTE_COUNT_MASK)
            .map_err(|_| Error::malformed("byte count too large"))?;
        let end = beg + 4 + count;
        if end > r.len() {
            return Err(Error::malformed(format!(
                "byte count {count} runs past the end of the buffer"
            )));
        }
        (r.read_u32()?, Some(tag_key), Some(end))
    };
    let position = beg_key + MAP_OFFSET;
    if tag & CLASS_MASK == 0 {
        return Ok(if tag == 0 { Tag::Null } else { Tag::Ref(tag) });
    }
    let class = if tag == NEW_CLASS_TAG {
        let name = r.read_cstring(MAX_CLASS_NAME)?;
        if let Some(at) = tag_key {
            classes.0.insert(at + MAP_OFFSET, name.clone());
        }
        name
    } else {
        let at = tag & !CLASS_MASK;
        classes
            .0
            .get(&at)
            .cloned()
            .ok_or_else(|| Error::malformed(format!("reference to unknown class tag {at}")))?
    };
    Ok(Tag::Object {
        class,
        position,
        end,
    })
}

/// The classes written so far in one key's buffer.
#[derive(Debug, Default)]
pub(crate) struct ClassTags(HashMap<String, u32>);

/// A write context that carries the class tags of its buffer.
pub(crate) trait HasClassTags {
    fn class_tags(&mut self) -> &mut ClassTags;
}

impl HasClassTags for ClassTags {
    fn class_tags(&mut self) -> &mut ClassTags {
        self
    }
}

/// Writes a pointed-to object (`WriteObjectAny`): byte count, class tag or
/// class reference, then `body`, which writes the object itself.
pub(crate) fn write_tagged<C: HasClassTags>(
    w: &mut WBuffer,
    cx: &mut C,
    class: &str,
    body: impl FnOnce(&mut WBuffer, &mut C) -> Result<()>,
) -> Result<()> {
    let start = w.len();
    w.write_u32(0);
    let known = cx.class_tags().0.get(class).copied();
    if let Some(at) = known {
        w.write_u32(at | CLASS_MASK);
    } else {
        let at = w.key_position()? + MAP_OFFSET;
        w.write_u32(NEW_CLASS_TAG);
        w.write_cstring(class);
        cx.class_tags().0.insert(class.to_owned(), at);
    }
    body(w, cx)?;
    w.patch_byte_count(start)
}

/// Starts an object with a byte-count slot and a version; returns the slot
/// position for [`WBuffer::patch_byte_count`].
pub(crate) fn begin_object(w: &mut WBuffer, version: i16) -> usize {
    let start = w.len();
    w.write_u32(0);
    w.write_i16(version);
    start
}

/// `TObject` as streamed by its hand-written streamer: bare version,
/// `fUniqueID`, `fBits`, and a process-ID index when referenced.
pub(crate) fn read_tobject(r: &mut RBuffer<'_>) -> Result<(u32, u32)> {
    let h = read_header(r)?;
    let unique_id = r.read_u32()?;
    let bits = r.read_u32()?;
    if bits & IS_REFERENCED != 0 {
        r.skip(2)?;
    }
    if let Some(end) = h.end {
        r.seek(end)?;
    }
    Ok((unique_id, bits))
}

/// Writes a `TObject` (version 1). The referenced bit is cleared, as no
/// process-ID table is written.
pub(crate) fn write_tobject(w: &mut WBuffer, unique_id: u32, bits: u32) {
    w.write_i16(1);
    w.write_u32(unique_id);
    w.write_u32(bits & !IS_REFERENCED);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A foreign class (version 0) writes a checksum after its version, if
    /// its object is long enough to hold one, wherever the object starts.
    #[test]
    fn foreign_headers_skip_their_checksum_when_they_have_one() {
        let object = |count: u32, tail: &[u8]| {
            let mut b = vec![0xEE; 8]; // what precedes the object
            b.extend_from_slice(&(count | BYTE_COUNT_MASK).to_be_bytes());
            b.extend_from_slice(&0_i16.to_be_bytes());
            b.extend_from_slice(tail);
            b
        };
        let header = |bytes: &[u8]| {
            let mut r = RBuffer::new(bytes, 0);
            r.skip(8).expect("skip");
            let h = read_header(&mut r).expect("header");
            (h.version, h.end, r.position())
        };
        // Version and two bytes of data: no checksum.
        assert_eq!(header(&object(4, &[1, 2])), (0, Some(16), 14));
        // Version, checksum, two bytes of data.
        assert_eq!(header(&object(8, &[9, 9, 9, 9, 1, 2])), (0, Some(20), 18));
    }

    #[test]
    fn tags_round_trip_with_class_references() {
        let origin = 64;
        let mut w = WBuffer::with_origin(origin);
        let mut cx = ClassTags::default();
        for v in [7, 8] {
            write_tagged(&mut w, &mut cx, "TFoo", |w, _| {
                let s = begin_object(w, 3);
                w.write_i32(v);
                w.patch_byte_count(s)
            })
            .expect("write");
        }
        w.write_u32(0);
        let bytes = w.into_inner();
        // The second object refers to the class by its tag.
        let names = bytes.windows(4).filter(|w| w == b"TFoo").count();
        assert_eq!(names, 1);
        let mut r = RBuffer::new(&bytes, origin);
        let mut classes = ClassMap::default();
        for v in [7, 8] {
            let Tag::Object { class, end, .. } = read_tag(&mut r, &mut classes).expect("tag")
            else {
                unreachable!("expected an object");
            };
            assert_eq!(class, "TFoo");
            let h = read_header(&mut r).expect("header");
            assert_eq!(h.version, 3);
            assert_eq!(r.read_i32().expect("value"), v);
            assert_eq!(Some(r.position()), end);
        }
        assert!(matches!(read_tag(&mut r, &mut classes), Ok(Tag::Null)));
    }

    #[test]
    fn mismatched_byte_counts_reposition_or_fail() {
        // Byte count says 8 bytes follow; the reader consumes only the version.
        let bytes = [0x40, 0, 0, 8, 0, 1, 0, 0, 0, 0, 0, 0];
        let mut r = RBuffer::new(&bytes, 0);
        let h = read_header(&mut r).expect("header");
        let mut diags = Vec::new();
        finish(&mut r, h.end, "TFoo", OnMismatch::Reposition, &mut diags).expect("lenient");
        assert_eq!(r.position(), 12);
        assert_eq!(diags.len(), 1);

        let mut r = RBuffer::new(&bytes, 0);
        let h = read_header(&mut r).expect("header");
        let e = finish(&mut r, h.end, "TFoo", OnMismatch::Fail, &mut diags).expect_err("strict");
        assert_eq!(e.kind(), ErrorKind::Strict);
    }

    #[test]
    fn byte_counts_past_the_end_are_malformed() {
        let bytes = [0x40, 0, 1, 0, 0, 1];
        let e = read_header(&mut RBuffer::new(&bytes, 0)).expect_err("too long");
        assert_eq!(e.kind(), ErrorKind::Malformed);
    }
}
