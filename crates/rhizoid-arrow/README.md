# rhizoid-arrow

[Apache Arrow](https://arrow.apache.org) for
[`rhizoid-io`](https://crates.io/crates/rhizoid-io): `TTree` columns as
Arrow arrays, and trees as record batches, one per cluster or of a fixed
size.

```rust
use rhizoid_arrow::TreeExt;
use rhizoid_io::Row;
use rhizoid_io::tree::BatchSize;
# let dir = std::env::temp_dir().join(format!("rhizoid-arrow-readme-{}", std::process::id()));
# std::fs::create_dir_all(&dir)?;
# let path = dir.join("events.root");

#[derive(Row)]
struct Event {
    run: i32,
    #[row(count = "n")]
    pt: Vec<f32>,
}
rhizoid_io::create(&path, |f| {
    let mut t = f.tree::<Event>("Events")?;
    t.fill(&Event { run: 1, pt: vec![1.5, 2.5] })?;
    t.fill(&Event { run: 2, pt: vec![] })?;
    t.finish()
})?;

let tree = rhizoid_io::File::open(&path)?.tree("Events")?;
for batch in tree.record_batches(["run", "pt"], BatchSize::Cluster)? {
    let batch = batch?;
    assert_eq!((batch.num_rows(), batch.num_columns()), (2, 2));
}
// One column on its own.
let column = tree.read("pt", ..)?;
let array = rhizoid_arrow::to_arrow(&column)?;
assert_eq!(array.len(), 2);
# std::fs::remove_dir_all(&dir).ok();
# Ok::<(), Box<dyn std::error::Error>>(())
```

Its types are Arrow's, so it makes a breaking release with each `arrow`
major. License: MIT.
