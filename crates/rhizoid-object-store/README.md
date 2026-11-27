# rhizoid-object-store

Read ROOT files with [`rhizoid-io`](https://crates.io/crates/rhizoid-io)
from any [`object_store`](https://crates.io/crates/object_store): Amazon
S3, Google Cloud Storage, Azure Blob Storage, HTTP, or the local file
system. Reads are range requests; several ranges go out together.

```rust,no_run
use std::sync::Arc;

use object_store::local::LocalFileSystem;
use rhizoid_object_store::ObjectStoreSource;

// Any store: S3, GCS and Azure need `object_store`'s `aws`, `gcp` or
// `azure` feature in your own Cargo.toml.
let store = LocalFileSystem::new_with_prefix("/data")?;
let source = ObjectStoreSource::new(Arc::new(store), "events.root")?;
let file = rhizoid_io::File::from_source(source, rhizoid_io::ReadOptions::default())?;
for key in file.keys() {
    println!("{} ({})", key.name(), key.class_name());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Its types are `object_store`'s, so it makes a breaking release with each
`object_store` major. License: MIT.
