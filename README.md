# rhizoid

A Rust ecosystem for CERN ROOT files. **`rhizoid-io`**, its first member,
reads and writes ROOT files in pure Rust, without ROOT installed.

> Status: **M1** (objects), **M2** (`TTree` reading), **M3** (remote
> sources), **M4** (`RNTuple` reading), **M5** (async and WASM), **M6**
> (user classes, typed rows, schema evolution), **M7** (flat `TTree`
> writing), **M8** (`RNTuple` writing), **M9** (subdirectories and
> update mode) and **M10** (split objects in trees) done. Of the 1.0
> gate, the coverage against ROOT, the data-driven suite and the crash
> harness pass locally (ROOT 6.42); the nightly ROOT 6.26–6.34 matrix
> and the semver baseline wait for the first CI runs.
> Any object reads through the class descriptions stored in the file;
> histograms (TH1/TH2/TH3 of every cell type), profiles, graphs, `TF1`,
> `TNamed` and `TObjString` read and write as data models, like uproot's
> (checked against ROOT 6.42); user classes read and write with
> `#[derive(RootObject)]`, which ROOT reads with its own dictionary; trees
> and `RNTuple`s read as typed rows (`#[derive(Row)]`), and rows write
> flat trees and `RNTuple`s (numbers, fixed and variable-size arrays,
> strings, compressed, in clusters; `RNTuple` pages compressed in
> parallel) that ROOT 6.42 and uproot read as they read ROOT's own;
> `TTree` branches read typed or untyped, flat or split,
> including `std::vector`, `TClonesArray`s, maps and whole objects, with
> branch projection, entry selection, parallel batches and Arrow output.
> On a corpus of 25 ROOT-written files, 4178 branches match uproot, none
> differ and none are unsupported ([report](tests/cross/corpus-report.md));
> against ROOT 6.42 itself, all 2130 branches of numbers it reads in 58
> files match, with what is not covered named
> ([report](tests/cross/coverage-report.md)).
> `RNTuple` fields read typed or untyped, every column type and encoding
> of format 1.0, including collections, classes, variants and projected
> fields: in 30 `RNTuple`s written by ROOT and uproot, 1938 fields match
> uproot and none differ ([report](tests/cross/rntuple-report.md)).
> Files can be read over HTTP(S) and from object stores (S3, GCS, Azure)
> with bounded range coalescing, synchronously or with `async`/`.await`
> (feature `async`), each column read in one request; the library builds
> for WASM and reads in the browser through `fetch`. Files are updated
> all or nothing (`rhizoid_io::update`): new objects, subdirectories,
> trees extended as a new cycle; a crash harness checks that any crash
> leaves the old file or the new one, which ROOT opens. User classes go
> into trees as split branches, as ROOT splits them, and ROOT reads them
> with its dictionary as its own. See [ARCHITECTURE.md](ARCHITECTURE.md)
> §12.

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
use rhizoid_io::objects::{TAxis, TH1D, Histogram};

// Objects are data models, as uproot's: build one from data (cells with
// under- and overflow, sums of squares, statistics)…
let h = TH1D::new("h_pt", "p_{T}", TAxis::uniform(4, 0.0, 200.0)?)
    .with_values_flow(vec![0.0, 3.0, 9.0, 4.0, 1.0, 2.0])?;
rhizoid_io::create("out.root", |f| f.put("h_pt", &h))?;

// …and read it back, typed or not: TH1/TH2/TH3 of every cell type,
// profiles, graphs, TF1, TNamed, TObjString.
let file = rhizoid_io::File::open("out.root")?;
let h: TH1D = file.get("h_pt")?;
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
    #[root(base)] object: rhizoid_io::objects::TObject,
    id: i32,                                  // int fId
    e: [f32; 3],                              // float fE[3]
    #[root(alias = "fCharge")] q: f64,        // also read from an older name
}
let hit: Hit = file.get("hit")?;

// Typed rows, checked once when opened.
#[derive(rhizoid_io::Row)]
struct Muons { #[row(rename = "nMuon")] n: u32, #[row(rename = "Muon_pt")] pt: Vec<f32> }
for m in tree.rows::<Muons>()? { let m = m?; }
for (met, pt) in tree.rows_of::<(f32, Vec<f32>), 2>(["MET_pt", "Muon_pt"])? .map(Result::unwrap) {}

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
comparison (M2's exit criterion) also needs the corpus:

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

The ROOT cross-check (`crates/rhizoid-io/tests/root.rs`) runs against the
ROOT named by `RHIZOID_ROOT` (`tools/wsl-test-root.sh`);
`tools/root-fixture.sh` regenerates the ROOT-written fixture.

Design documents: [ARCHITECTURE.md](ARCHITECTURE.md),
[ROOT-BEHAVIOUR.md](ROOT-BEHAVIOUR.md), [STYLE.md](STYLE.md),
[TODOS.md](TODOS.md).

## License

MIT ([LICENSE](LICENSE)).
