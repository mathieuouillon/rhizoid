# rhizoid

A Rust ecosystem for CERN ROOT files. **`rhizoid-io`**, its first member,
reads and writes ROOT files in pure Rust, without ROOT installed.

- **Objects:** any object reads through the class descriptions stored in
  the file. Histograms (TH1/TH2/TH3 of every cell type), profiles, graphs,
  `TF1`, `TNamed` and `TObjString` read and write as data models, like
  uproot's.
- **User classes:** `#[derive(RootObject)]` writes a struct with its own
  class description, which ROOT reads with its dictionary, and reads it
  back from any version of the class.
- **Trees:** branches read typed or untyped, flat or split (`std::vector`,
  `TClonesArray`s, maps, whole objects), with branch projection, entry
  selection, parallel batches and Arrow output. Typed rows
  (`#[derive(Row)]`) read trees and write them, user classes as split
  branches.
- **`RNTuple`:** every column type and encoding of format 1.0, read typed
  or untyped, and written from typed rows.
- **Remote and async:** HTTP(S) and object stores (S3, GCS, Azure) with
  bounded range coalescing, synchronously or with `async`/`.await`; builds
  for WASM and reads in the browser through `fetch`.
- **Updates:** `rhizoid_io::update` adds objects, subdirectories and
  trees to a file all or nothing: a crash leaves the old file or the new
  one.

**Checked against ROOT and uproot.** ROOT 6.26, 6.30, 6.34 and 6.42 read
what rhizoid writes (`RNTuple`s from 6.34; `TH1L`/`TH2L`/`TH3L` from 6.32,
the first version with them). Against ROOT 6.42, all 2130 branches of
numbers it reads in 58 files match
([report](tests/cross/coverage-report.md)); against uproot, 4178 branches
and 1938 `RNTuple` fields match ([trees](tests/cross/corpus-report.md),
[RNTuple](tests/cross/rntuple-report.md)).

## Crates

| Crate | What it is |
|---|---|
| [`rhizoid-io`](crates/rhizoid-io) | read and write ROOT files |
| [`rhizoid-source`](crates/rhizoid-source) | stable byte-source traits (`ReadAt`, `WriteAt`, …) |
| [`rhizoid-derive`](crates/rhizoid-derive) | `#[derive(RootObject)]`, `#[derive(Row)]` (re-exported by `rhizoid-io`) |
| [`rhizoid-arrow`](crates/rhizoid-arrow) | tree columns as Apache Arrow arrays and record batches |
| [`rhizoid-object-store`](crates/rhizoid-object-store) | files in S3, GCS, Azure and other object stores |
| [`rhizoid-cli`](crates/rhizoid-cli) | the `rhizoid` command |

## The `rhizoid` command

```text
rhizoid ls -l -r data.root        # list keys, recursively, with sizes
rhizoid ls https://host/data.root # any command takes an http(s) URL
rhizoid ls data.root:dir/sub      # list one directory
rhizoid check data.root           # walk and decompress every record
rhizoid header data.root          # print the file header
rhizoid dump data.root:dir/h      # print an object, member by member
rhizoid streamers data.root       # list the class descriptions in a file
rhizoid tree data.root:Events     # branches, types, baskets, clusters
rhizoid rntuple data.root:Events  # RNTuple fields, types, clusters
rhizoid scan data.root:Events -b MET_pt,Muon_pt -n 5  # a tree or an RNTuple
rhizoid new empty.root            # write a valid, empty file
```

## Library

```rust
use rhizoid_io::objects::{Axis, Hist1, Histogram};

// Objects are data models, as uproot's: build one from data (cells with
// under- and overflow, sums of squares, statistics)…
let h = Hist1::<f64>::new("h_pt", "p_{T}", Axis::uniform(4, 0.0, 200.0)?)
    .with_values_flow(vec![0.0, 3.0, 9.0, 4.0, 1.0, 2.0])?;
rhizoid_io::create("out.root", |f| f.put("h_pt", &h))?;

// …and read it back, typed or not: TH1/TH2/TH3 of every cell type,
// profiles, graphs, TF1, TNamed, TObjString.
let file = rhizoid_io::File::open("out.root")?;
let h: Hist1<f64> = file.get("h_pt")?;
println!("{} entries, cells {:?}", h.entries(), h.values());
let axes = h.axes();                       // `Histogram`: for histogram libraries
let obj = file.get_dyn("h_pt")?;           // any class, from its description
let color = obj.member("fLineColor")?;     // members by name, through bases

// TTree columns, typed and checked once when the branch is opened.
use rhizoid_io::columnar::Jagged;
let tree = file.tree("Events")?;
let met: Vec<f32> = tree.branch::<f32>("MET_pt")?.read(..)?;
let muons = tree.branch::<Jagged<f32>>("Muon_pt")?.read(0..1000)?;
for pts in &muons { /* pts: &[f32] */ }

// Several branches, selected entries, one batch per cluster.
use rhizoid_io::tree::EntrySelection;
let cut = EntrySelection::from_entry_list(&[3, 17, 42_000]);
for batch in tree.reader().projection(["MET_pt", "Muon_pt"])?.selection(&cut)?.build() {
    let met: Vec<f32> = batch?.get::<f32>("MET_pt")?;
}

// RNTuple fields, with the same column types and selections.
let nt = file.rntuple("Events")?;
let pts = nt.field::<Jagged<f32>>("Muon.pt")?.read(..)?;
let any = nt.read("Muon", 0..10)?;          // untyped: a Column

// Remote files (feature "http"): coalesced range requests, retried and checked.
let remote = rhizoid_io::File::open_url("https://example.org/data.root")?;

// Async (feature "async"): the same reads with .await, over any AsyncReadAt.
let file = rhizoid_io::File::open_async(rhizoid_io::io::AsyncHttpSource::open(url).await?).await?;
let tree = file.tree("Events").await?;
let met: Vec<f32> = tree.branch::<f32>("MET_pt")?.read(..).await?;   // one request
let mut stream = tree.reader().projection(["MET_pt", "Muon_pt"])?.build();
while let Some(cluster) = stream.next_cluster().await? {               // bytes in hand
    let batch = tokio::task::spawn_blocking(move || cluster.decode()).await??;
}

// User classes: written with their own description, read from any version.
#[derive(rhizoid_io::RootObject)]
#[root(class = "Hit", version = 2, rename_all = "root")]
struct Hit {
    #[root(base)] object: rhizoid_io::objects::Object,
    id: i32,                                  // int fId
    e: [f32; 3],                              // float fE[3]
    #[root(alias = "fCharge")] q: f64,        // also read from an older name
}
let hit: Hit = file.get("hit")?;

// Typed rows, checked once when opened.
#[derive(rhizoid_io::Row)]
struct Muons { #[row(rename = "nMuon")] n: u32, #[row(rename = "Muon_pt")] pt: Vec<f32> }
for m in tree.rows::<Muons>()? { let m = m?; }
for (met, pt) in tree.rows_of::<(f32, Vec<f32>), _>(["MET_pt", "Muon_pt"])? .map(Result::unwrap) {}

// Rows write flat trees: Muon_pt[nMuon]/F behind the counter nMuon/I.
#[derive(rhizoid_io::Row)]
struct Out { run: i32, #[row(rename = "Muon_pt", count = "nMuon")] pt: Vec<f32>, label: String }
rhizoid_io::create("skim.root", |f| {
    let mut t = f.tree::<Out>("Events")?;
    t.fill(&Out { run: 1, pt: vec![41.5, 20.25], label: "a".into() })?;
    t.finish()?;
    // …and RNTuples: std::int32_t, std::vector<float>, std::string fields.
    let mut n = f.rntuple::<Out>("EventsNT")?;
    n.fill(&Out { run: 1, pt: vec![41.5, 20.25], label: "a".into() })?;
    n.finish()
})?;

// User classes in trees: split branches (event.fRun, event.fHit.fId, …,
// hits.fId[hits_]), read back whole.
#[derive(rhizoid_io::Row)]
struct Rec { #[row(rename = "event.")] event: UserEvent, hits: Vec<Hit> }
for r in tree.rows::<Rec>()? { let r = r?; }

// Adding to a file, all or nothing: subdirectories, and an existing tree
// extended (a new cycle with the new entries after the old ones).
rhizoid_io::update("skim.root", |f| {
    f.mkdir("calib/run2")?.put("h_pt", &h)?;
    let mut t = f.tree::<Out>("Events")?;
    t.fill(&Out { run: 2, pt: vec![], label: "b".into() })?;
    t.finish()
})?;

// Arrow record batches (crate rhizoid-arrow).
use rhizoid_arrow::TreeExt;
for rb in tree.record_batches(["MET_pt", "Muon_pt"], rhizoid_io::tree::BatchSize::Cluster)? {
    let rb: arrow_array::RecordBatch = rb?;
}
```

## Development

```text
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The uproot cross-check runs when `RHIZOID_ORACLE_PYTHON` points at a Python
with `uproot numpy awkward lz4 zstandard xxhash` installed. The corpus
comparison also needs the corpus:

```text
pip install scikit-hep-testdata
python tests/cross/fetch_corpus.py corpus        # ~44 MB
RHIZOID_CORPUS=corpus cargo test -p rhizoid-io --test corpus
RHIZOID_CORPUS=corpus cargo test -p rhizoid-io --features async,http --test asynchronous
```

The WASM tests run under Node with `wasm-bindgen-test-runner` (the
version of `wasm-bindgen` in `Cargo.lock`):

```text
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
  cargo test -p rhizoid-io --target wasm32-unknown-unknown --features async --test wasm
```

On Windows, everything else (ROOT included) lives in WSL:

```text
wsl --install -d Ubuntu-24.04
wsl -d Ubuntu-24.04 -u root -- bash tools/wsl-setup.sh    # Rust, Node, uproot, ROOT 6.42
wsl -d Ubuntu-24.04 -u root -- bash tools/wsl-check.sh    # every check, ROOT included
```

Fuzzing (nightly toolchain and `cargo install cargo-fuzz`): `fuzz/` has a
target per decoder of untrusted bytes (whole files, compressed blocks, keys
and directories, class descriptions, `RNTuple` envelopes and pages, baskets)
and for the writing API (rows and histograms written, then read back
exactly). Inputs that once crashed live in `crates/rhizoid-io/tests/data/fuzz`
and are replayed on every CI run.

```text
cargo +nightly fuzz run open_file -- -max_total_time=300
RUSTFLAGS="--cfg fuzzing" cargo test -p rhizoid-io --test fuzz_regressions
```

The ROOT cross-check (`crates/rhizoid-io/tests/root.rs`) runs against the
ROOT named by `RHIZOID_ROOT` (`tools/wsl-test-root.sh`);
`tools/root-fixture.sh` regenerates the ROOT-written fixture.

## License

MIT ([LICENSE](LICENSE)).
