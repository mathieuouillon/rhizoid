# Changelog

All notable changes, per release.

## Unreleased

- **Added — XRootD (`root://`)**, feature `xrootd`: `io::XrootdSource`
  and, with `async`, `io::AsyncXrootdSource` (any executor), the protocol
  in pure Rust over TCP, no `libXrdCl` and no new dependency.
  `File::open_url` and every `rhizoid` command take
  `root://host[:port]//path` (and `xroot://`).
  - One multiplexed connection per file: reads from many threads or tasks
    are in flight together. Several ranges go in `kXR_readv` (at most
    1024 elements of at most 2 MiB each); large ranges are read with
    `kXR_read`, 16 MiB at a time.
  - Redirects followed (an EOS manager's to its storage node, with the
    capabilities in the CGI); `kXR_wait` and `kXR_waitresp` honoured.
  - Every answer is checked against its request. Transient failures (a
    lost connection, a timeout, an overloaded or restarted server) are
    retried on a new connection with backoff (`XrootdOptions`); a file
    reopened with another size or modification time is an error.
  - Logins are anonymous, or by the `unix` protocol when the server asks
    for it. GSI, Kerberos, tokens and TLS (`roots://`) are `Unsupported`.
  - Tested against an XRootD server written for the tests (faults
    injected; ROOT's own client reads from it too), against a real xrootd
    (5.6) with `RHIZOID_XROOTD_SERVER`, and with
    `RHIZOID_XROOTD_NETWORK=1` against CERN Open Data on
    `eospublic.cern.ch`. Fuzz target `xrootd_responses`.
  - Fixed during testing against the real server: opening a directory is
    `InvalidInput`, as for the test server (xrootd answers
    `kXR_isDirectory`).
- **Added — writing every codec ROOT writes**: Zstandard
  (`Compression::zstd::<L>()`, ROOT's recommended 505), LZMA
  (`Compression::lzma::<L>()`, ROOT's smallest is 207) and ROOT's old
  algorithm (setting 3xx), besides zlib and LZ4. `Compression::new`
  accepts every algorithm, and no setting is refused when a file is
  created any more.
  - Zstandard: ruzstd (already a dependency), one speed, about zstd's
    level 1, whatever the level asked (ROOT asks zstd for twice the ROOT
    level).
  - LZMA: lzma-rust2 (pure Rust, Apache-2.0; new dependency, with `sha2`),
    an `.xz` stream as ROOT writes it: LZMA2 at the level's preset, a CRC32
    check, the dictionary at most a quarter of the block. lzma-rs, which
    decodes, has an encoder that stores the data uncompressed.
  - ROOT 6.42 and uproot read the files; uproot reads no file of the old
    algorithm, its own or ROOT's.
- **Fixed — ROOT's old algorithm (`CS` blocks) is read**: it is raw deflate
  (RFC 1951), which ROOT 6.42 still writes for settings 3xx. Reading such
  a record failed with `Unsupported`, and the documentation called the
  codec "read only".
- **Fixed:** updating a file whose setting is LZMA, Zstandard or the old
  algorithm compressed new records with zlib 1; they use the file's
  setting now, as ROOT does.
- **Tests:** `tests/data/root_codecs.root`, written by ROOT
  (`tests/cross/root_codecs.C`): an object and a tree per setting (101,
  109, 207, 306, 404, 505, none), read by every test run.

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
- **Project:** a README per crate, whose examples run as doctests;
  `SECURITY.md` (what rhizoid promises on untrusted input, private
  reporting) and `CONTRIBUTING.md`; release pull requests by release-plz.
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
