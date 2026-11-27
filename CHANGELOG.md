# Changelog

All notable changes, per release (C7: a 0.x release when each milestone is
green; breaking changes allowed until 1.0). Nothing is published yet: the
versions below are the milestones as built (ARCHITECTURE §12.1–12.12).

## 1.0.0 — unreleased (the 1.0 gate)

- **API, before the freeze** (breaking): `RNTupleWriter::push` is now
  `fill`, as `TreeWriter::fill` (and ROOT's `Fill`);
  `RNTuple::written_by` (was `writer`, which read as if it returned a
  writer), on `AsyncRNTuple` too; `objects::Histogram` and
  `rhizoid_arrow::TreeExt` are sealed, so methods can be added to them.
  `ntuple` re-exports the reader types it shares with `tree`
  (`RNTupleReaderBuilder`, `RNTupleReader`, `RNTupleRows`,
  `EntrySelection`, `Projection`).
- **Versions:** `rhizoid-io`, `rhizoid-derive`, `rhizoid-source` and the
  CLI are 1.0.0; `rhizoid-arrow` and `rhizoid-object-store` have their own
  versions (0.1.0), with a breaking release at each `arrow` /
  `object_store` major.
- **Licence:** MIT (was MIT OR Apache-2.0).
- The nightly ROOT matrix (6.26, 6.30, 6.34) runs: the cross-check macros
  work with every targeted ROOT (`ROOT::Experimental` RNTuple classes
  before 6.36); `TH1L`/`TH2L`/`TH3L` need ROOT ≥ 6.32, which first has
  them.
- Goal 1's measure against ROOT itself: every branch of numbers of the
  corpus as ROOT 6.42 reads it (entry by entry, and through
  `TTree::Draw`), compared branch by branch (`tests/coverage.rs`,
  `tests/cross/root_sums.C`), with the report and the list of what is not
  covered (`tests/cross/coverage-report.md`): all 2130 branches ROOT reads
  in 58 files match.
- The data-driven suite (`tests/cases/*.toml`), run against every source:
  memory, file, HTTP with and without multi-range support, async. Its
  tree and RNTuple cases take their expected values from ROOT
  (`tests/cross/make_cases.py`, `root_sums.C`, `root_ntuple_sums.C`),
  whole and over entry ranges.
- CI tiers: a nightly workflow (ROOT 6.26, 6.30, 6.34 and the latest;
  coverage; the crash harness; build timings), `cargo-deny`, and a
  `cargo-semver-checks` job against the `v1.0.0` baseline.

## 0.10.0 — M10: split objects in trees

- `#[derive(Row)]` fields of `#[derive(RootObject)]` classes, and `Vec`s
  of them, write split branches (`TBranchElement`) as ROOT 6.42 splits
  them, and read back whole; ROOT reads them with its dictionary.
- The encoder writes references to objects still being written (a split
  collection's members refer to it).

## 0.9.0 — M9: subdirectories and update mode

- `mkdir`, `DirWriter`; `update`, `update_with`, `WritableFile::update`,
  `update_in`: all or nothing, copy-on-write directories, one commit point;
  trees extended as a new cycle; locking; `copy_on_write`.
- The crash harness.
- Fixed: empty subdirectories (no key list) were refused.

## 0.8.0 — M8: RNTuple writing

- `rntuple`, `RNTupleWriter`: ROOT 6.42's layout, pages compressed in
  parallel with `rayon`.
- Fixed: `check` reported ROOT's page blobs as broken.

## 0.7.0 — M7: flat TTree writing

- `tree`, `TreeWriter`: leaf lists of every type, fixed and jagged arrays,
  strings, clusters.
- Fixed: a compressed record as long as its object read as uncompressed.

## 0.6.0 — M6: user classes, typed rows, schema evolution

- `#[derive(RootObject)]`, `#[derive(Row)]`, tuple rows, `rows`,
  `rows_of`; the class checksum; data models of every histogram, profile,
  graph and `TF1`.

## 0.5.0 — M5: async and WASM

- `AsyncFile`, streams of batches, `next_cluster`, the browser's `fetch`.

## 0.4.0 — M4: RNTuple reading

- Both containers, every column type and encoding of format 1.0.

## 0.3.0 — M3: remote sources

- HTTP(S), object stores, bounded range coalescing, caching.

## 0.2.0 — M2: TTree reading

- Flat, fixed, jagged, strings and split objects of every `fType`; reader
  builder, projections, entry selections; Arrow output.

## 0.1.0 — M1: objects

- Objects through their class descriptions; histograms read and written;
  the scoped `create`.
