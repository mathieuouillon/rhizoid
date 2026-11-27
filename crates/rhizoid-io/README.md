# rhizoid-io

Read and write [CERN ROOT](https://root.cern) files in pure Rust, without
ROOT installed: objects and histograms, `TTree`s and `RNTuple`s, local or
remote, synchronous or async. Decoding is driven by the class descriptions
stored in each file, so classes the crate has never seen still read.

```toml
[dependencies]
rhizoid-io = "1.1"
```

## Histograms and other objects

Histograms, profiles, graphs, functions and strings are data models: they
hold what the file stores.

```rust
use rhizoid_io::objects::{Axis, Hist1};
# let dir = std::env::temp_dir().join(format!("rhizoid-readme-{}", std::process::id()));
# std::fs::create_dir_all(&dir)?;
# let path = dir.join("hists.root");

// Under- and overflow, then 3 bins.
let h = Hist1::<f64>::new("pt", "p_{T}", Axis::uniform(3, 0.0, 30.0)?)
    .with_values_flow(vec![0.0, 4.0, 9.0, 2.0, 0.0])?;
rhizoid_io::create(&path, |f| f.put("pt", &h))?;

let file = rhizoid_io::File::open(&path)?;
let back: Hist1<f64> = file.get("pt")?;
assert_eq!(back.values(), [4.0, 9.0, 2.0]);
// Any object, through its class description.
assert_eq!(file.get_dyn("pt")?.str("fTitle")?, "p_{T}");
# std::fs::remove_dir_all(&dir).ok();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Trees and RNTuples

Typed rows write and read trees and `RNTuple`s; columns read typed or
untyped, over any entry selection.

```rust
use rhizoid_io::Row;
# let dir = std::env::temp_dir().join(format!("rhizoid-readme-rows-{}", std::process::id()));
# std::fs::create_dir_all(&dir)?;
# let path = dir.join("events.root");

#[derive(Row, Debug, PartialEq)]
struct Event {
    run: i32,
    #[row(count = "nMuon")]
    muon_pt: Vec<f32>,
}

rhizoid_io::create(&path, |f| {
    let mut tree = f.tree::<Event>("Events")?;
    tree.fill(&Event { run: 1, muon_pt: vec![41.5, 20.25] })?;
    tree.fill(&Event { run: 2, muon_pt: vec![] })?;
    tree.finish()?;
    let mut ntuple = f.rntuple::<Event>("EventsNT")?;
    ntuple.fill(&Event { run: 3, muon_pt: vec![7.0] })?;
    ntuple.finish()
})?;

let file = rhizoid_io::File::open(&path)?;
let tree = file.tree("Events")?;
let runs: Vec<i32> = tree.branch::<i32>("run")?.read(..)?;
assert_eq!(runs, [1, 2]);
let first = tree.rows::<Event>()?.next().expect("a row")?;
assert_eq!(first.muon_pt, [41.5, 20.25]);
assert_eq!(file.rntuple("EventsNT")?.num_entries(), 1);
# std::fs::remove_dir_all(&dir).ok();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Features

- `derive` (default): `#[derive(RootObject)]` for user classes and
  `#[derive(Row)]` for rows.
- `http`: files over HTTP(S) (`File::open_url`).
- `xrootd`: files over XRootD (`File::open_url` with `root://`), in pure
  Rust over TCP: anonymous or `unix` login, redirects, vector reads.
- `async`: `File::open_async` and streams of batches; builds for WASM.
- `rayon`: batches read, and `RNTuple` pages compressed, in parallel.

## Untrusted input

Decoding never panics, uses no `unsafe`, and checks lengths read from a
file against [`ReadLimits`] before allocating. The decoders are fuzzed
nightly.

[`ReadLimits`]: https://docs.rs/rhizoid-io/latest/rhizoid_io/struct.ReadLimits.html

Minimum supported Rust version: 1.89. License: MIT.
