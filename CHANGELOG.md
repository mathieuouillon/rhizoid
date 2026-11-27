# Changelog

All notable changes, per release.

## 1.1.0 — unreleased

- **Breaking — Rust-style names for the data models** (released as 1.1.0
  by the owner's choice, though semver calls for 2.0). Rename in your code;
  ROOT's class names in files are unchanged:

  | Before | After |
  |---|---|
  | `TAxis` | `Axis` |
  | `TH1<T>`, `TH2<T>`, `TH3<T>` | `Hist1<T>`, `Hist2<T>`, `Hist3<T>` |
  | `TH1D`, `TH2F`, … (`TH{1,2,3}{C,S,I,L,F,D}`) | removed: `Hist1<f64>`, `Hist2<f32>`, … (`i8`, `i16`, `i32`, `i64`, `f32`, `f64`) |
  | `TProfile`, `TProfile2D`, `TProfile3D` | `Profile1`, `Profile2`, `Profile3` |
  | `TGraph`, `TGraphErrors`, `TGraphAsymmErrors` | `Graph`, `GraphErrors`, `GraphAsymErrors` |
  | `TF1` | `Function1` |
  | `TNamed`, `TObject`, `TObjString` | `Named`, `Object`, `StringObject` |

  A type alias is gone with its type: `Hist1::<f64>::new(…)` replaces
  `TH1D::new(…)`.
- **Added:** `AsyncFile::check`. A `DirId` belongs to the file that made
  it: `put_in` refuses another file's with `InvalidInput` (it wrote into
  whichever directory had the same index). The thread-safety of the API's
  types is asserted by a test.
- **Fixed:** a file holding objects of two versions of one class (a
  `RootObject` at version 3 and at version 4, say) kept only one version's
  description, so the other version's objects could not be read back. Every
  version written is kept now.
- **Fixed:** split members whose `fID` counts members that I/O rules added
  in memory (CMSSW MiniAOD's `reco::ParticleState`: `pdgId_`, `status_`)
  were refused; the member is now found by name. Across CMS NanoAOD
  (Run 1 and 2), MiniAOD and ATLAS PHYSLITE files, every branch ROOT reads
  reads the same in rhizoid.
- **Fixed:** in an object read from a file, a reference to an object it is
  part of (a split collection's members to the collection, by
  `fBranchCount`) was read as null, so a tree copied with `get_dyn` and
  `put` lost it; it is kept now, and ROOT's objects are written again as
  ROOT's bytes.
- **Project:** a README per crate, whose examples run as doctests;
  `SECURITY.md` (what rhizoid promises on untrusted input, private
  reporting) and `CONTRIBUTING.md`; release pull requests by release-plz.
- **Fuzzing:** the corpus each target grows is kept between nightly runs
  (minimized), so the fuzzing time adds up. New targets: update mode
  against a model (`update_ops`), every source reading alike
  (`sources_agree`), split objects and every field type (`write_split`).
  Differential: nightly, ROOT and uproot read the files the `write_rows`
  corpus makes rhizoid write, and must find exactly its rows
  (`fuzz/differential`); on the first 300 files they did.
- **Benchmarks** (`crates/rhizoid-io/benches`): opening, reading flat and
  jagged columns of trees and `RNTuple`s, writing; `compare.py` times
  uproot and ROOT on the same files; nightly, a regression of more than 25%
  against the previous night fails.
- **Documented:** ROOT reads a tree's `String` field (`TLeafC`) up to its
  first NUL byte; rhizoid and uproot read all of it.
- **Fixed:** a forged `RNTuple` (element offsets past the column's pages)
  made a read allocate whatever the file claimed (2^60 bytes); such reads
  are now checked against `ReadLimits` and fail with `LimitExceeded`.
  Found by fuzzing.
- **Fixed:** `Compression::new` accepted LZMA, Zstandard and ROOT's old
  codec, which the writer cannot produce, and writing then failed after
  the data was written; such settings are now refused when made, and by
  `create_with` before the file is touched. Found by fuzzing.
- **Fixed:** `TAxis::uniform` accepted more bins than ROOT can store
  (`fNbins` is an `i32`).
- **Fixed:** a missing key's error names the directory searched ("no key
  `x` in directory `a/b`") instead of repeating the key; the `rhizoid`
  command no longer prints an I/O error's cause twice.
- **Testing:** fuzz targets (`fuzz/`, `cargo fuzz`) for every decoder of
  untrusted bytes and for the writing API, run nightly; their findings
  are replayed on every CI run (`tests/fuzz_regressions.rs`). Contract
  tests of the API (`tests/api.rs`), and unit and property tests for
  selections, `RNTuple` envelopes, the read cache, the object encoder,
  row layouts and the Arrow conversions. Code coverage is measured in CI
  (`cargo llvm-cov`), with a floor; tests for what it showed untested: the
  packing of `Double32_t`/`Float16_t` members when written, the encoder's
  refusals, every async accessor against its sync twin, the byte-source
  traits, and the `rhizoid` command.

## 1.0.0 — 2026-11-27

First release: `rhizoid-io`, `rhizoid-derive`, `rhizoid-source` and the
`rhizoid` command at 1.0.0; `rhizoid-arrow` and `rhizoid-object-store` at
0.1.0 (each makes a breaking release with each `arrow` / `object_store`
major, whose types are in their API).

- **Objects:** any object reads through the class descriptions stored in
  the file. Histograms (TH1/TH2/TH3 of every cell type), profiles, graphs,
  `TF1`, `TNamed` and `TObjString` read and write as data models.
- **User classes:** `#[derive(RootObject)]` gives a struct its own class
  description, checksum included; it is written with it and read back from
  any version of the class, by member name.
- **Trees:** `TTree` branches read typed or untyped, flat or split
  (`std::vector`, `TClonesArray`, maps, whole objects), with projections,
  entry selections, parallel batches and Arrow output. Typed rows
  (`#[derive(Row)]`, tuples) read trees and write them: numbers, fixed and
  variable-size arrays, strings, in clusters; fields of user classes as
  split branches, as ROOT splits them.
- **RNTuple:** every column type and encoding of format 1.0, read typed or
  untyped; written from typed rows, pages compressed in parallel.
- **Sources:** local files, HTTP(S) and object stores (S3, GCS, Azure),
  with bounded range coalescing; synchronous or `async` (feature
  `async`); builds for WASM and reads in the browser through `fetch`.
- **Updates:** subdirectories, and `update` adds objects, directories,
  trees (an existing tree extended as a new cycle) and `RNTuple`s to a
  file, all or nothing; other writers are locked out meanwhile.
- **Compatibility:** ROOT 6.26 and later reads our trees and objects, ROOT
  6.34 and later our `RNTuple`s (CI and nightly against ROOT 6.26, 6.30,
  6.34 and 6.42). `TH1L`/`TH2L`/`TH3L` need ROOT 6.32, the first version
  with those classes.
- **Coverage:** against ROOT 6.42, all 2130 branches of numbers ROOT reads
  in 58 files match (`tests/cross/coverage-report.md`, with what is not
  covered); against uproot, 4178 branches and 1938 `RNTuple` fields match
  (`tests/cross/corpus-report.md`, `tests/cross/rntuple-report.md`).
