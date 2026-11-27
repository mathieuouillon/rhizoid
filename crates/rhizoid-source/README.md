# rhizoid-source

The byte-source traits of the rhizoid ecosystem: positioned reads and
writes, synchronous and async. Implement them to read ROOT files from any
storage with [`rhizoid-io`](https://crates.io/crates/rhizoid-io); in-memory
buffers, files and shared pointers implement them already.

- `ReadAt`: `len`, `read_at(range)`, and `read_ranges` (one request for
  several ranges, where the source can).
- `AsyncReadAt` and `LocalAsyncReadAt` (not `Send`, for the browser).
- `WriteAt`: positioned writes, `flush`, and `sync` (a durability barrier,
  which update mode requires).

```rust
use std::io;
use std::ops::Range;

use bytes::Bytes;
use rhizoid_source::{FileOffset, ReadAt};

/// A source that serves a fixed buffer, counting the bytes it hands out.
struct Counting {
    data: Vec<u8>,
    served: std::sync::atomic::AtomicU64,
}

impl ReadAt for Counting {
    fn len(&self) -> io::Result<u64> {
        Ok(self.data.len() as u64)
    }
    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes> {
        let bytes = self.data.read_at(range)?;
        self.served
            .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(bytes)
    }
}

let source = Counting { data: (0..=255).collect(), served: Default::default() };
let head = source.read_at(FileOffset::new(0)..FileOffset::new(4))?;
assert_eq!(&head[..], [0, 1, 2, 3]);
// Past the end is an error, never a panic.
assert!(source.read_at(FileOffset::new(250)..FileOffset::new(300)).is_err());
# Ok::<(), io::Error>(())
```

This crate aims never to leave 1.x. License: MIT.
