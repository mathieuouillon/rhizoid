# Changelog

All notable changes, per release.

## Unreleased

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
