//! [`Inline`]: a sync source read from async code.

use std::io;
use std::ops::Range;

use bytes::Bytes;
use rhizoid_source::{
    AsyncReadAt, BoxFuture, FileOffset, LocalAsyncReadAt, LocalBoxFuture, ReadAt,
};

/// Presents a sync source as an async one, reading inside the future: each
/// read blocks the task that polls it.
///
/// Right for sources that answer at once (memory, memory maps, local files
/// on a fast disk, tests). A slow sync source belongs on a blocking thread
/// (`spawn_blocking`) or behind a real async source.
#[derive(Debug)]
pub struct Inline<S>(pub S);

impl<S: ReadAt> AsyncReadAt for Inline<S> {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        Box::pin(async move { self.0.len() })
    }

    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move { self.0.read_at(range) })
    }

    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(async move { self.0.read_ranges(&ranges) })
    }
}

impl<S: ReadAt> LocalAsyncReadAt for Inline<S> {
    fn len(&self) -> LocalBoxFuture<'_, io::Result<u64>> {
        Box::pin(async move { self.0.len() })
    }

    fn read_at(&self, range: Range<FileOffset>) -> LocalBoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move { self.0.read_at(range) })
    }

    fn read_ranges(
        &self,
        ranges: Vec<Range<FileOffset>>,
    ) -> LocalBoxFuture<'_, io::Result<Vec<Bytes>>> {
        Box::pin(async move { self.0.read_ranges(&ranges) })
    }
}
