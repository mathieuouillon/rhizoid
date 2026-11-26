# TODOs

Deferred work recorded by the CEO review (2026-11-26,
[`ceo-review-2026-11-26/CEO-REVIEW.md`](ceo-review-2026-11-26/CEO-REVIEW.md)).
None of it is in the current scope.

## P1 — Name a design partner and adoption measures (before M2)

- **What:** pick one design partner and 1–2 adoption measures. The partner
  could be an experiment analysis group, an Arrow or CERN Open Data user, or
  a Rust physics or ML project. An example measure: "partner X reads dataset
  Y end to end in their pipeline with rhizoid-io 0.2".
- **Why:** the plan has no named user or success measure (outside-voice K6).
  The partner's needs should order M3–M10.
- **Pros:** real feedback before the API freezes; a reason for 0.x releases
  to exist.
- **Cons:** time spent on outreach; risk of over-fitting one user.
- **Context:** CEO review, Section 10 and the 1-year question. Start by
  posting the 0.1 plan on the HSF / Scikit-HEP forums.
- **Effort:** S (human) / S (Claude Code).
- **Depends on:** nothing.

## P1 — Licence review of embedded class descriptions (before 0.1)

- **What:** confirm that the port of `TClass::GetCheckSum`, and embedding
  class descriptions captured from ROOT-written files, can ship under
  MIT/Apache-2.0. Assign an owner and a date.
- **Why:** it blocks M1's release (0.1); ROOT is LGPL-2.1 (ARCHITECTURE §13
  Q2, outside-voice K10).
- **Pros:** removes the only legal blocker.
- **Cons:** may need an external opinion.
- **Context:** precedent: uproot (BSD-3) embeds class descriptions captured
  from ROOT files. Start by reading uproot's licence notes and its
  `class_rawstreamers` provenance.
- **Effort:** S / S.
- **Depends on:** nothing. **Blocks:** 0.1.

## Done — Check M1's histograms with ROOT itself (2026-11-26)

- `tests/root.rs` against ROOT 6.42.00 (ARCHITECTURE §12.6); CI job
  `root`. Still open: the same check against ROOT 6.26 (the oldest version
  targeted), with the ROOT version matrix of the nightly tier.

## P1 — Finish the 1.0 gate (needs the repository and CI)

- **What:** put the project under version control with CI; let the
  nightly run the ROOT 6.26, 6.30 and 6.34 images for the first time; tag
  `v1.0.0` to record the `cargo-semver-checks` baseline (the `semver`
  job then checks every change against it).
- **Why:** everything else of the gate passes locally (ARCHITECTURE
  §12.12); these steps need a remote and CI runners.
- **Effort:** S / S. **Depends on:** the licence review (P1).

## P2 — Grow the corpus with experiment files (CERN Open Data)

- **What:** CMS NanoAOD (Run 1 ~0.93 GB, record 31131) and MiniAOD
  (~90–100 MB, record 24117), ATLAS PHYSLITE (~87 MB, record 80014), named
  in Goal 1 but not yet in the corpus; run `tests/coverage.rs` on them.
- **Why:** the 1.0 measure runs on scikit-hep-testdata and the fixtures
  (2130 of 2130 branches of numbers match ROOT); the experiments' own
  files are larger and use more classes.
- **Effort:** S / S (downloads, then an 18-minute-plus run).

## P2 — Verify what uproot cannot (ROOT is now available: WSL, CI job `root`)

- M2 is done against uproot (`tests/cross/corpus-report.md`). 19 corpus
  branches read without error where uproot cannot read them (`//[fN]`
  arrays and `fBits` in split collections, `bitset`s, an unsplit `TH1F`
  member): compare them with ROOT itself.
- The 10 `bitset` branches of the corpus are empty in every entry, so the
  `bitset` layout is only checked on empty data.
- Compare the whole corpus with ROOT once the ROOT container exists (with
  the M1 item above): uproot is wrong in two known places
  (ROOT-BEHAVIOUR §8.10).

## P3 — Opaque members written back verbatim (after M6)

- **What:** a member kept as `Value::Opaque` (a class this version cannot
  decode) is written back as its original bytes. If those bytes hold a
  reference to a class tag defined earlier in the original record, the
  reference now points at the wrong place.
- **Why:** silent corruption on read-modify-write of exotic objects.
- **Fix:** refuse to write `Opaque` values that contain class references
  (scan them on read), or decode every class (M6 makes that likelier).
- **Effort:** S / S.

## P2 — Multi-file dataset API, like `TChain` (after M2)

- **What:** a dataset handle over 10²–10⁴ files, with shared `FileMetadata`,
  global entry ranges, parallel per-file reading, and Arrow output.
- **Why:** real analyses read many files; this decides whether Arrow and
  DataFusion users adopt the library (outside-voice K11).
- **Pros:** the most-requested shape for analysis work.
- **Cons:** new public API surface.
- **Context:** builds on the reader builder (§4.12.1), shared metadata
  (§4.12.3) and `HttpPool`. Look at uproot's `uproot.iterate` /
  `uproot.dask` for semantics.
- **Effort:** M / S.
- **Depends on:** M2, M3.

## P2 — Bare RNTuple container and streamer fields (after M4)

- **What:** read `RNTuple`s stored without a `TFile` (magic `"rntuple"`,
  ROOT-BEHAVIOUR §9) and fields stored by a ROOT streamer (field role 4).
- **Why:** both are in format 1.0, but no file in the corpus uses them, so
  M4 could not test them (ARCHITECTURE §12.4).
- **Pros:** completes the format.
- **Cons:** needs test files written by ROOT, so the ROOT container first.
- **Context:** past the anchor, the bare container reads like the rest;
  streamer fields hold one `TBufferFile` object per entry, which M1's
  decoder reads.
- **Effort:** S / S.
- **Depends on:** a ROOT container in CI, as for the P1 histogram check.

## P2 — Async Arrow batches and shared metadata (after M5)

- **What:** an async `record_batches` stream in `rhizoid-arrow` over
  `BatchStream`; `TreeMetadata`/`FileMetadata` loaded once and reused
  across files and processes (ARCHITECTURE §4.12.3, feature `serde`).
- **Why:** M5 gives async batches as `Batch`es only, and every file of a
  dataset re-reads its class descriptions.
- **Pros:** async pipelines straight into DataFusion/Polars; fewer
  requests on datasets of many files.
- **Cons:** a serialized metadata format to keep stable.
- **Context:** ARCHITECTURE §12.5; the multi-file dataset item below
  would use the shared metadata.
- **Effort:** M / S.
- **Depends on:** M5.

## P2 — Derive: the typed fast path and more member kinds (after M6)

- **What:** generate `Marshal`/`Unmarshal` (reading and writing the bytes
  directly when the file's description is the compiled one, ARCHITECTURE
  §4.4); support `Double32_t`/`Float16_t` members (`#[root(double32(…))]`),
  pointers to objects, maps, and `Vec` of fixed arrays in derived classes.
- **Why:** derived classes read through the evolution path (correct for
  every version); the fast path is for objects read in bulk.
- **Context:** ARCHITECTURE §12.7.
- **Effort:** M / M.
- **Depends on:** M6.

## P2 — Async rows

- **What:** `rows` on async trees and `RNTuple`s (a stream of rows).
  Writing rows to trees (M7) and `RNTuple`s (M8) is done.
- **Effort:** S / S.

## P2 — `RNTuple` writer: zstd, dynamic and nested fields

- **What:** zstd writing (ROOT's default, level 5) once a pure-Rust
  encoder is in; `DynRow` writers (schemas known at run time) and
  `OwnedRNTupleWriter`; nested records and classes (`#[derive(Row)]` with
  struct fields, `Vec<struct>`); `Real32Trunc`/`Real32Quant` and
  `Real16` columns as options; several cluster groups; an async writer.
- **Why:** M8 writes flat fields of the types trees have, as ROOT does.
- **Context:** ARCHITECTURE §12.9.
- **Effort:** M / M.
- **Depends on:** M8.

## P2 — Tree writer: irregular clusters, AutoSave, zstd

- **What:** clusters of different sizes (`fClusterRangeEnd`,
  `fClusterSize`); AutoSave (the tree written again as a new cycle every so
  many bytes, so a killed job leaves a readable tree, ARCHITECTURE §4.8);
  zstd and LZMA writing; an async writer.
- **Why:** M7 writes the tree object once, at finish, with clusters of one
  size; a crash before then loses the tree (the baskets are in the file).
- **Context:** ARCHITECTURE §12.8.
- **Effort:** M / S.
- **Depends on:** M7.

## P2 — Update mode: reuse free space, replace and delete (after M9)

- **What:** allocate new records in free gaps (best fit, ROOT's gap
  markers for what remains) instead of always appending; `put_replace`
  (frees the previous cycle) and `delete`; a test of updates past 2 GB
  (large key and record forms).
- **Why:** M9 appends: a file grows with each update, though ROOT reuses
  what rhizoid frees.
- **Context:** ARCHITECTURE §12.10, §7.2–7.3.
- **Effort:** M / M.
- **Depends on:** M9.

## P2 — Split writer: TClonesArray, richer collections (after M10)

- **What:** `TClonesArray` branches (`fType` 3/31); collections whose
  element class has strings, vectors or embedded objects (members 41 of
  those kinds, `fBranchCount2`); maps; extending split trees in update
  mode; user-class fields in `RNTuple`s; reading whole collections whose
  members are split further.
- **Why:** M10 writes split objects of every member kind the derive has,
  and collections of classes of numbers and arrays, as ROOT does.
- **Context:** ARCHITECTURE §12.11, ROOT-BEHAVIOUR §8.8.1.
- **Effort:** M / M.
- **Depends on:** M10.

## P3 — RNTuple extension in update mode (after 1.0)

- **What:** append clusters to existing RNTuples, including ROOT's split
  encodings, through new cluster groups and a new anchor cycle.
- **Why:** a non-goal for 1.0 (CEO-D5), and the known gap in uproot.
- **Pros:** completes update mode for the future format.
- **Cons:** complexity in the riskiest milestone.
- **Context:** ARCHITECTURE §7.3; RNTuple specification for cluster groups
  and page lists.
- **Effort:** L / M.
- **Depends on:** M8, M9.

## P3 — Python bindings: decide the timing (after 0.2)

- **What:** decide when to start `rhizoid-py` (pyo3 + Arrow PyCapsule),
  using the CEO-D8 rule (every capability reachable through `'static`
  handles and dynamic types).
- **Why:** Python is the largest audience (CEO review, Section 10).
- **Pros:** reach.
- **Cons:** a second API surface to maintain.
- **Context:** decide with the design partner's input. Compare with uproot
  rather than competing with it; a fast-path backend may be the better pitch.
- **Effort:** L / M.
- **Depends on:** 0.2, the design partner.
