# `rootfile` — architecture

A pure-Rust reader **and** writer for CERN ROOT files: the classic `TFile`
container, streamed objects (histograms, graphs, user classes), `TTree` and
RNTuple. This document is the design of the library: crates, layers, public
API, data flow, and the decisions behind them.

Companion documents:

- `ROOT File Format.md` — the on-disk layout. `[§n]` refers to its sections.
- [`ROOT-BEHAVIOUR.md`](ROOT-BEHAVIOUR.md) — what ROOT itself does when it
  reads and writes, checked in ROOT's sources. `[RB§n]` refers to its sections.
  Where this document and ROOT disagree, ROOT wins.
- [`council-transcript-2026-11-25.md`](council-transcript-2026-11-25.md) —
  the review that produced revision 2.
- [Appendix A](#appendix-a-api-by-example) — the public API by example, with
  what fails to compile.
- [`STYLE.md`](STYLE.md) — the coding style, which follows the Rust standard
  library and the Rust API Guidelines. Every PR is reviewed against it.
- [Appendix C](#appendix-c-performance-and-complexity-contract) — what the
  library promises about cost, and the performance guide.

> **Revision 2 (2026-11-25).** Changes from the review: ROOT's actual
> behaviour folded in, a scoped write API, compile-time guarantees made
> explicit (§5) and tested, semver exposure reduced, and the TTree writer
> deferred until its layout is verified.
>
> **Revision 3 (2026-11-25).** Changes from a review against Rust's core
> principles and API usability ([Appendix B](#appendix-b-review-against-rust-principles-and-usability)):
> - the API levels laid out as a ladder (§4.10), with a prelude;
> - string paths accepted everywhere, `path!` kept as an opt-in check;
> - `create`/`create_with` without boilerplate, and closures that return the
>   caller's own error type;
> - an `io::Error`-style fieldless `ErrorKind`;
> - `rows`/`num_entries` instead of an overloaded `entries`;
> - `Row` instead of `Entry`, which clashed with `std`;
> - ROOT's `RNTuple` naming;
> - parallelism that runs in the caller's rayon pool.
>
> The `TTree` and `TBranchElement` layouts are now fully verified [RB§8].
>
> **Revision 4 (2026-11-26).** Practices adopted from serde, regex,
> thiserror/anyhow, rayon, axum, kurbo and `std` (§2.2):
> - a small stable `rootfile-source` crate for people writing sources;
> - a three-tier publishing policy;
> - same-name trait and derive pairs, and a `serde` bridge for dynamic
>   objects;
> - typed rows from tuples, with no derive needed;
> - `rayon` parallel-iterator support;
> - a written performance and complexity contract (Appendix C);
> - a data-driven test suite run against every source, OSS-Fuzz, and
>   `examples/` built in CI;
> - a compile-time budget.
>
> **Revision 5 (2026-11-26).** From reading the reference crates' source
> (§4.12):
> - one reader builder for sync and async (parquet);
> - `Projection` and run-length `EntrySelection` (parquet, regex `Input`);
> - metadata loaded once and shared;
> - a sans-IO decoder under the async stream, and `next_cluster` to overlap
>   I/O with decoding;
> - `BoxFuture` async traits;
> - per-thread `Scratch` pools (regex-automata);
> - rayon support by delegation;
> - `all_the_tuples!` with `do_not_recommend` (axum);
> - serde_derive's `Ctxt` error accumulation and dummy-const wrapper;
> - opaque, one-word errors with `#[cold]` constructors, `Error::custom`,
>   lazy context and backtraces (thiserror, anyhow, regex);
> - object_store's coalescing defaults: 1 MiB gaps, 10 requests in parallel.

> Working name: `rootfile`. Check its availability on crates.io before
> publishing. Prior art:
> - [`oxyroot`](https://crates.io/crates/oxyroot) (Rust)
> - [go-hep/groot](https://github.com/go-hep/hep/tree/main/groot) (Go, read
>   and write; the closest architectural relative)
> - [uproot](https://github.com/scikit-hep/uproot5) (Python, the reference
>   for reading without ROOT)

---

## 1. Goals and non-goals

**Goals**

1. Read every file that ROOT ≥ 5.34 and uproot write, local or remote, without
   ROOT installed and without generated code (decoding driven by the file's
   own class descriptions).
2. Write files that ROOT and uproot open without warnings: objects,
   directories, RNTuple, `TTree` (§4.8), update in place.
3. Columnar reads as fast as uproot and ROOT: parallel decompression, vector
   reads, no redundant copies, output to Apache Arrow.
4. Survive hostile input: no panics, no unbounded allocations, no `unsafe`.
5. **Turn misuse into compile errors wherever the information exists at
   compile time** (§5), and catch everything else with tests against ROOT
   itself.
6. Be future proof: new class versions, new RNTuple minor versions, new
   codecs and new transports are additions, not rewrites.
7. **Be easy to use.** The common case (open, get a histogram, read a
   column, write an ntuple) fits in a few lines with no lifetimes, no generic
   parameters and no options. Everything else is reachable from there
   without switching libraries (§4.10).

**Non-goals (for 1.0):**

- A drawing or fitting engine.
- `TFormula` evaluation. `TF1` is read as data, from its sampled `fSave`
  table.
- `TRef` resolution across files.
- Build-time schema generation from a reference file. It would only check
  against one file, not the files met at run time.
- DataFusion and Polars integrations. Arrow output makes them easy later.

---

## 2. Guiding principles

| Principle | What it means here |
|---|---|
| **Sans-IO core** | Every parser and serializer works on bytes in memory and never touches a file. The format says how many bytes the next step needs [§4], so the core returns *byte ranges* to fetch and decodes the bytes it is given. Sync, async, mmap, HTTP, S3 and WASM drivers are thin shells over the same code. |
| **Positioned, shared reads** | Sources are read with `&self` at an offset, never with `Read + Seek` and `&mut self`, so one open file can be read from many threads. This is the model of `FileExt::read_at`. |
| **Batch ranges, not bytes** | Remote reads cost round trips [§12]. Every columnar read builds a plan of ranges, coalesces it, then issues one vector read. |
| **Lazy by default** | Opening reads the header, the root directory and its key list. Class descriptions, objects, baskets and pages load on demand. ROOT reads class descriptions eagerly [RB§1]; `rootfile check` does too. |
| **The file describes itself** | Class layouts come from the file's `TStreamerInfo` [§7]. Typed models are a fast path, used only when the on-file layout matches. |
| **Two-way by construction** | Every on-disk structure has `decode` and `encode` side by side, plus a round-trip property test. |
| **Compile time where the information exists; tests where it doesn't** | The API, the derive macro and the format rules that depend only on the *kind* of record are enforced by types (§5). Whether ROOT reads our output, and whether we read real files correctly, are run-time facts, gated by the ROOT/uproot cross-check from the first milestone (§9). |
| **Be as tolerant as ROOT, no more** | *Unknown* things (type codes, compression tags, RNTuple feature flags, column types) are typed errors. *Irregular but known* things that ROOT tolerates, such as byte-count mismatches, are tolerated the same way by default (`Strictness::Lenient`) and reported. `Strictness::Strict` turns them into errors. |
| **Follow pointers, not names** | Bookkeeping records are found through the offsets that point at them, never through their class names. Writers disagree on those names [RB§5]. |
| **Open for extension, sealed where it matters** | Public enums are `#[non_exhaustive]`. Codecs, sources and class codecs are pluggable. Traits that encode format invariants are sealed (§5.4). |

### 2.1 Rust's core principles, and where each one lives

| Principle | How the design honours it | Where |
|---|---|---|
| **Memory safety without a GC** | `#![forbid(unsafe_code)]`; buffers are borrowed slices or `Bytes`; no raw pointers, even for ROOT's pointer-heavy object graphs (back-references become indices into a per-buffer map) | §4.1, §4.4 |
| **Ownership expresses resource lifetime** | A writer's life is a value's life: `finish(self)`/`close(self)` consume, and children borrow their scope (`'s`), so use-after-close cannot be written | §4.10, §5.1 |
| **RAII, with honest errors** | `Drop` cannot return an error, so the design does not hide fallible I/O in it. The scoped API gives RAII's guarantee (cleanup always runs) *and* returns the `Result`. `std::thread::scope` makes the same trade | §4.10 |
| **Borrowing over copying** | Decoders take `&[u8]`, read handles share one `Arc`'d file, and columns are converted in bulk into one allocation | §4.1, §8 |
| **Fearless concurrency** | `File: Send + Sync` with `&self` reads; `FileScope: Sync` and writers `Send`; the compiler, not documentation, rejects unsound sharing | §4.3, §4.10 |
| **Make invalid states unrepresentable** | Newtypes, sealed `Record::KEY_FORM`, private-field `Compression`, typed writers, `ObjectPath`, separate read and write types | §5 |
| **Zero-cost abstractions** | Typed columns and rows are monomorphized, and schema checks run once per open, not per entry. The single dynamic dispatch sits at the I/O boundary, where it is free in comparison | §4.7, §4.10 |
| **Pay only for what you use** | Codecs, async, HTTP, Arrow and the derive are features or adapter crates; reading a histogram never touches the tree code | §3, §4.10 |
| **Explicit over implicit** | Lenient reading is a named default (`Strictness`) and reports every irregularity as a `Diagnostic`; there are no global state, global thread pools or environment variables | §4.1, §4.10 |
| **Errors are values** | Everything fallible returns `Result`; no panics on input; errors carry a `kind()` to match on and context to read | §4.1 |
| **Composition through traits** | Sources (`ReadAt`), codecs (`Codec`), classes (`Marshal`/`Unmarshal`) and rows (`Row`) are extension points; `std` traits (`From`, `FromStr`, `Display`, `Iterator`, `RangeBounds`) are used wherever they fit | §4, STYLE §3 |

### 2.2 Lessons from reference crates

The crates the Rust community holds up as best written, and what this design
takes from each. Where a lesson changed the design, the last column says
where.

| Crate | Practice | Applied here |
|---|---|---|
| **serde** (dtolnay) | The data model is separate from the formats: one `Serialize` trait, many `Serializer`s | The `Row` trait is separate from storage. One `#[derive(Row)]` type writes to **TTree or RNTuple** and reads from either (§4.8, §4.9) |
| serde | Each trait and its derive share a name (`Serialize` / `#[derive(Serialize)]`), and the derive is a feature of the main crate | `RootObject` and `Row` are each both a trait and a derive, re-exported by `rootfile` behind the default `derive` feature (§4.4) |
| serde | Macros expand to paths in a `#[doc(hidden)] pub mod __private`, and `#[serde(crate = "…")]` lets re-exporting crates redirect them | `rootfile::__private`, and `#[root(crate = "…")]` for frameworks that re-export us (STYLE §15) |
| serde | Interoperates through the de facto standard instead of inventing a new one | Feature `serde`: a `DynObject` is a `serde::Deserializer`, so `rootfile::serde::from_dyn::<MyConfig>(&obj)` reads any class into a `#[derive(Deserialize)]` struct, and `Value: Serialize` gives JSON dumps (§4.4) |
| serde | 1.0 kept for years: additions only, never a break | The facade aims for a long-lived 1.x. Every public enum is `#[non_exhaustive]`, trait methods have defaults, and `cargo-semver-checks` runs in CI |
| **regex** (BurntSushi) | A simple facade (`regex`), an expert crate (`regex-automata`) and a parser (`regex-syntax`), each with its own semver | Three publishing tiers (§3): stable (`rootfile`, `rootfile-derive`, `rootfile-source`, adapters), expert later (`rootfile-core` once its API settles), internal (the rest) |
| regex | **Documented complexity guarantees** (linear-time matching) and a performance guide | Appendix C: round trips per open, bytes read per query, memory bounds, and no super-linear behaviour on hostile input |
| regex | One data-driven test suite (TOML) run against every engine | `tests/cases/*.toml`, run against **every source** (memory, file, mmap, HTTP mock, async) and every container (§9) |
| regex | Continuous fuzzing in OSS-Fuzz | The fuzz targets are enrolled in OSS-Fuzz once public (§9) |
| regex | `regex-lite`: a smaller build for people who need less | `default-features = false, features = ["pure-rust"]` (§4.10) |
| regex | Crate docs that teach: overview, usage, examples, features, performance, untrusted input, MSRV | The standard crate-doc outline (STYLE §7) |
| **thiserror / anyhow** | Typed errors for libraries, contextual errors for applications | One typed `Error` with a `kind()` and context (§4.1); `anyhow` only in the CLI; user closures may return their own error type (§4.10) |
| **rayon** | Parallelism through iterator traits, running in the caller's pool | `Clusters: IntoParallelIterator`, `tree.par_rows::<T>()` under the `rayon` feature; work runs in the current pool (§4.7, §4.10) |
| rayon | The same code works with parallelism compiled out | Without the feature, `par_*` are absent and everything runs sequentially; the results are identical (tested) |
| **axum** (tokio) | Typed extraction from plain functions and tuples, with no macro needed | `tree.rows_of::<(f32, Vec<f32>)>(["MET_pt", "Muon_pt"])`: tuples of column types implement `Row`, so the derive is optional (§4.8, A.20) |
| axum, serde | `axum-core` exists so library authors implement its traits without depending on `axum`. serde has split the same way: `serde` is now a facade over `serde_core`, which holds the traits | **`rootfile-source`**: `ReadAt`, `AsyncReadAt`, `WriteAt` and `FileOffset` only. It aims to stay 1.x, and its only dependencies are `bytes` and `futures-core` (§3, §4.3) |
| **parquet** (arrow-rs) | The closest large relative. One reader builder generic over sync or async input, projection and row-selection types, metadata loaded once and shared, dyn-compatible async I/O, and a push decoder (sans-IO) under the async stream | §4.12: `TreeReaderBuilder<I>`, `Projection`, `EntrySelection`, `TreeMetadata`, `next_cluster()`; §4.3 |
| regex-automata | Immutable compiled state behind an `Arc`, mutable per-search `Cache` taken from a pool; `*_with(&mut Cache)` variants skip the pool | §4.12: `Scratch` buffers in a pool, plus `read_with(&mut Scratch)` |
| axum | Good compiler errors are a feature (`#[debug_handler]`) | `#[diagnostic::on_unimplemented]` on every user-facing trait, and `trybuild` snapshots of the messages (§5.5, §5.6) |
| **kurbo** (linebender) | Concrete `f64` types over numeric genericity, for clarity and compile times | Histograms are concrete per ROOT class (`TH1D`, `TH1F`), not `TH1<T>`. `File` is not generic. Generics only where they pay: columns and rows |
| kurbo | A focused scope with explicit non-goals | §1 non-goals; plotting, fitting and formulas stay out |
| **std** | Generic outer functions delegate to a non-generic inner one (`fs::read` → `inner(&Path)`) | Required for every generic public function with a non-trivial body, to limit monomorphization bloat (STYLE §9) |
| std | `Debug` on everything; conversions through `From`/`TryFrom`/`FromStr`; `std`'s naming | STYLE §2–3 |

---

## 3. Workspace layout

```
rootfile/
├─ Cargo.toml                 workspace: shared deps, lints, profiles
├─ ARCHITECTURE.md  ROOT-BEHAVIOUR.md  STYLE.md
├─ rustfmt.toml  clippy.toml  deny.toml
├─ crates/
│  ├─ rootfile-source/        STABLE, tiny: ReadAt, AsyncReadAt, WriteAt, FileOffset
│  ├─ rootfile-core/          sans-IO wire format: buffers, header, keys,
│  │                          directories, allocator, newtypes, errors
│  ├─ rootfile-compress/      9-byte block framing + codecs
│  ├─ rootfile-io/            source implementations, range coalescing, caches
│  ├─ rootfile-streamer/      TStreamerInfo, dynamic decoder, Marshal/Unmarshal,
│  │                          class registry, hand-written streamers
│  ├─ rootfile-derive/        #[derive(RootObject)], #[derive(Row)]
│  ├─ rootfile-objects/       typed TH*, TGraph*, TF1, TAxis… (generated)
│  ├─ rootfile-columnar/      ReadPlan, Column, Jagged, NativeType: shared by
│  │                          tree and ntuple
│  ├─ rootfile-tree/          TTree read (+ writer behind an unstable feature)
│  ├─ rootfile-ntuple/        RNTuple read/write, TFile and bare containers
│  ├─ rootfile/               facade: the only API with a semver promise
│  ├─ rootfile-arrow/         adapter: Column ↔ arrow, versioned with arrow
│  ├─ rootfile-object-store/  adapter: object_store source, versioned with it
│  └─ rootfile-cli/           `rootfile ls | dump | streamers | check | cp`
├─ examples/                  runnable programs, one per Appendix A recipe; built in CI
├─ xtask/                     model codegen, corpus download, cross-check runner
├─ fuzz/                      cargo-fuzz targets, one per decoder (OSS-Fuzz)
└─ tests/
   ├─ cases/                  data-driven test cases (TOML), run against every source
   ├─ corpus/                 reference files (fetched, not committed)
   ├─ cross/                  ROOT macros + uproot scripts for cross-validation
   └─ ui/                     trybuild compile-fail tests (§5.6)
```

### Dependency graph (arrows point at dependencies)

```
   rootfile-cli ──► rootfile (facade) ◄── rootfile-arrow, rootfile-object-store
                        │
      ┌──────────┬──────┴─────┬──────────────┬─────────────┐
      ▼          ▼            ▼              ▼             ▼
 rootfile-tree rootfile-ntuple rootfile-objects rootfile-io  rootfile-derive
      │   │        │   │          │                          (proc-macro)
      │   └───┬────┘   │          │
      │       ▼        │          │
      │ rootfile-columnar          │
      ▼                ▼          ▼
     rootfile-streamer ◄──────────┘
             │
             ▼
     rootfile-compress
             │
             ▼
       rootfile-core  ◄── (rootfile-io depends on nothing ROOT-specific)
```

Below everything sits `rootfile-source`. Both `rootfile-core` (for
`FileOffset`) and `rootfile-io` depend on it, and nothing else.

Rules:

- `source` holds only traits and one newtype, so it can stay at 1.x forever.
- `core` does no I/O, and `io` knows nothing about ROOT.
- `tree` and `ntuple` share code only through `columnar`.
- `ntuple` does not depend on the facade, so bare RNTuple files, which have
  no TFile [RB§9], are first-class.

### Publishing and semver

Three tiers, after `regex` / `regex-automata` / `regex-syntax`:

| Tier | Crates | Promise |
|---|---|---|
| **Stable** | `rootfile`, `rootfile-derive`, `rootfile-source`; the adapters `rootfile-arrow` and `rootfile-object-store` | semver; `rootfile-source` aims never to leave 1.x, like `futures-core` or `axum-core` |
| **Expert** (after 1.0) | `rootfile-core` | its own semver once the wire-format API settles, for tools that work at the record level, the way `regex-automata` serves people building engines |
| **Internal** | everything else | none |

A crate on crates.io cannot depend on unpublished crates, so the internal
crates are published, but:

- they are versioned in **lockstep** with the facade, which depends on them
  with exact `=x.y.z` requirements;
- their docs say *"implementation detail of `rootfile`; no stability
  guarantee"*;
- the facade re-exports item by item, never `pub use rootfile_core::*`, so the
  promised surface is explicit and checked by `cargo-semver-checks`.

Fast-moving ecosystems stay out of the facade's public API. `arrow`,
`object_store` and `reqwest` live in **adapter crates** whose major version
follows theirs, so an Arrow major release never forces a `rootfile` major
release. `bytes::Bytes` is the one external type in the core API: it has
been stable at 1.x since 2020 and is the common currency of Rust I/O.

---

## 4. Layers

### 4.1 `rootfile-core` — the wire format, no I/O

**Newtypes.** One concept, one type. The constructors validate, and the
fields are private.

| Type | Wraps | Notes |
|---|---|---|
| `FileOffset` | `u64` | Absolute position in the file. Whether it is written in 4 or 8 bytes is decided by the encoder (below) |
| `ByteLen` | `u32` | A length read from the file. Built only through checks against the remaining buffer and the limits |
| `KeyRelOffset` | `u32` | A position counted from the **start of a key**, as used by class tags, back-references and basket entry offsets [RB§6, RB§7] |
| `ClassVersion` | `i16` | Class versions everywhere, with the member-wise bit `0x4000` split off into a separate flag on decode |
| `Cycle` | `NonZeroU16`, constructed only for `1..=32767` | The `;n` cycle number: an `i16` on disk, never 0. ROOT's negative "keep" cycles are normalized to their absolute value [RB§4] |
| `Checksum` | `u32` | A class-layout checksum |
| `Datime`, `Uuid` | — | Packed date; the 18-byte `TUUID` |

**Buffers.**

- `RBuffer<'a>` is a **big-endian** cursor. Its constructor *requires* the
  origin:

  ```rust
  RBuffer::new(data: &'a [u8], origin: KeyRelOffset)
  ```

  `data` starts at `origin` within the key. This makes class tags and
  back-references resolve correctly [RB§6]; there is no constructor without
  an origin. Reads are bounds-checked and return `Result`: `read_string`,
  `read_char_star` (`i32` length) [RB§6], `read_version()` and
  `read_bulk::<T>(n)`.
- `read_version()` follows ROOT's rules [RB§6]:
  - no byte count when bit 30 is clear;
  - a checksum when the version ≤ 0 and the byte count ≥ 6;
  - the member-wise bit is split off.
- `LeBuffer<'a>` is a separate **little-endian** cursor for RNTuple payloads.
  Endianness is a type, not a flag, so RNTuple bytes cannot be read with the
  classic reader by accident.
- `WBuffer` writes objects through a closure:

  ```rust
  w.object(ClassVersion::new(3), |w| { w.write_i32(run)?; … Ok(()) })?;
  ```

  The byte count is reserved before the closure runs and back-patched after
  it, so it cannot be forgotten or patched twice (§5.2).
- `RefMap` / `WriteRefMap` hold the class-tag and back-reference maps of
  `WriteObjectAny` [§7.3], keyed by `KeyRelOffset`.

**Records and key forms.** Every kind of record is a type implementing a
sealed trait:

```rust
pub trait Record: sealed::Sealed {
    const KEY_FORM: KeyForm;          // AlwaysLarge | ByPosition
}
pub enum KeyForm { AlwaysLarge, ByPosition }

// Basket, RBlob                                 → AlwaysLarge   [RB§7, RB§9]
// Object<T>, KeyList, FreeList, StreamerInfo, Directory → ByPosition
```

`Key<R: Record>` encodes its header from `R::KEY_FORM` and the position: the
large form is used when the kind requires it or the position is
`> kStartBigFile` [RB§4]. A basket key with 4-byte offsets cannot be written.
The size switch itself stays a run-time value, because only the position
decides it.

**Physical layout.** Each structure has `decode(&[u8])` and
`encode(&mut WBuffer)`:

| Type | Notes |
|---|---|
| `FileHeader` | Small and large forms. Validation as ROOT does it: magic, `fBEGIN ≤ fEND`, `fNbytesName ∈ [10, 10000]` [RB§1] |
| `KeyHeader` | Rejects negative lengths and `Nbytes < KeyLen`. Masks the process-ID offset out of the high bits of the 8-byte `SeekPdir` [RB§4] |
| `DirectoryRecord` | Small form padded with 12 zero bytes; UUID layout chosen by `version % 1000` [RB§5] |
| `KeyList` | `nkeys` + headers. Each seek is checked to lie in `[64, file size]` [RB§1] |
| `FreeSegments` | Versioned entries |
| `ClassName` | Normalization, e.g. on-disk `"TDirectory"` ↔ `TDirectoryFile` [RB§4] |

**Navigation plans.** The core never reads; it says what to read:

- `FileHeader::root_directory_range()`
- `DirectoryRecord::key_list_range()`
- `KeyHeader::record_range()`

The first read is 4 KiB, a superset of ROOT's 300-byte read [RB§1] that also
covers the root directory record and, in small files, the root key list.

**`Allocator`** (write side, pure). It reproduces ROOT [RB§2, RB§4]:

- the free list starts as one segment `[100, 2 000 000 000]` and is extended
  by 10⁹ as the file grows;
- **best fit**;
- a negative `i32` gap marker at the start of every leftover gap;
- neighbouring gaps merged when space is freed.

Its invariants (no overlap, no leak, gap markers present) are
property-tested in isolation.

**Record walker.** `walk(&[u8]) -> impl Iterator<Item = Result<RecordOrGap>>`
steps from `fBEGIN` by `Nbytes`, skipping negative gap markers, like ROOT's
recovery [§3]. It is used by `rootfile check`, by recovery of unclosed files,
and by the CI cross-check.

**Errors.** One error type shared by the internal crates, in the style of
`std::io::Error`:

```rust
/// One pointer wide: `Result<T, Error>` stays small on the happy path.
pub struct Error(Box<Inner>);
struct Inner { kind: ErrorKind, detail: Detail, context: Vec<Context>,
               source: Option<Box<dyn std::error::Error + Send + Sync>> }

/// What went wrong, for matching. Fieldless and `Copy`, like `io::ErrorKind`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    Io, UnexpectedEof, NotRootFile, Malformed, UnsupportedVersion,
    UnknownCompression, UnknownFeatureFlag, Decompress, UnknownClass,
    ByteCountMismatch, Checksum, LimitExceeded, NotFound, InvalidPath,
    TypeMismatch, ChildrenOpen, Unsupported,
}

impl Error {
    pub fn kind(&self) -> ErrorKind;
    pub fn offset(&self) -> Option<FileOffset>;      // where in the file, when known
    pub fn object_path(&self) -> Option<&str>;       // which key, when known
    pub fn context(&self) -> impl Iterator<Item = &Context>;
}
impl From<Error> for std::io::Error { … }           // kind Io keeps its io::ErrorKind
```

- **Matching** reads like `std`: `if e.kind() == ErrorKind::NotFound { … }`.
- **The specifics** (needed/available bytes, the class name, the unknown tag)
  live in the private `Detail`. They reach users through `Display`, and through
  typed accessors where a program needs them. They can grow without a semver
  break.
- **Context** is gathered as the error bubbles up, so the message reads:
  *"unexpected end of buffer (needed 8, had 3) at offset 0x1f3a0 in class TH1D
  in key `dir/hist;2`"*.
- Libraries use `thiserror` for the `Display`/`source` plumbing; only the CLI
  uses `anyhow`.
- **Opaque by design.** regex's author calls its public error enum a
  "rookie mistake" in a code comment, and thiserror's README shows the fix: a
  public struct over a private representation, with accessors for what is
  meant to be public. That is this `Error`.
- **One pointer wide**, as anyhow is. anyhow gets there with a hand-made
  vtable and `unsafe`; a safe `Box<Inner>` is enough here, because there is a
  single concrete inner type.
- **`#[cold]` constructors.** Every internal constructor is `#[cold]`, as
  serde's `de::Error` defaults are, so error paths stay out of the hot
  decoding loops.
- **`Error::custom(msg: impl Display)`** for user code: hand-written
  `Marshal`/`Unmarshal` impls report their own failures through it. It plays
  the role of serde's `de::Error::custom`, the only required method of that
  trait.
- **Lazy context.** The internal `ResultExt` trait is sealed, as anyhow's
  `Context` is. It has `.context(Context::Key(..))` and
  `.with_context(|| …)`; the closure runs only on the error path.
- **Backtraces** are captured with `std::backtrace::Backtrace::capture()`,
  which costs nothing unless `RUST_BACKTRACE`/`RUST_LIB_BACKTRACE` is set,
  following anyhow's policy. Adding context captures no second backtrace.
- **`Debug` is written for `unwrap()`**, as regex's is. It prints the message,
  the context chain and the file offset on separate lines, instead of the
  struct dump.

**Read options.**

```rust
ReadOptions {
    strictness: Strictness::Lenient,  // or Strict
    limits: ReadLimits {
        max_object_bytes, max_decompressed_bytes,
        max_nesting_depth, max_collection_len,
    },
}
```

`Lenient` reproduces ROOT: on a byte-count mismatch it repositions to the
declared end and emits a `tracing` warning plus a `Diagnostic` the caller can
collect. Every length read from the file is checked against the remaining
buffer and the limits **before** allocating (`Vec::try_reserve`).

### 4.2 `rootfile-compress`

```rust
#[non_exhaustive]
pub enum Algorithm { Zlib, Lzma, Lz4, Zstd, OldRoot /* read only */ }

/// Fields private: an invalid setting cannot be constructed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Compression { algorithm: Algorithm, level: u8 }

impl Compression {
    pub const NONE: Self;

    // Compile-time checked (§5.3). Each exists only if its feature is enabled.
    #[cfg(feature = "zlib")] pub const fn zlib<const L: u8>() -> Self { const { assert!(L >= 1 && L <= 9) }; … }
    #[cfg(feature = "lzma")] pub const fn lzma<const L: u8>() -> Self { … }
    #[cfg(feature = "lz4")]  pub const fn lz4<const L: u8>()  -> Self { … }
    #[cfg(feature = "zstd")] pub const fn zstd<const L: u8>() -> Self { const { assert!(L >= 1 && L <= 22) }; … }

    // Run-time checked, for settings from configuration or a CLI (`cp`).
    pub fn new(algorithm: Algorithm, level: u8) -> Result<Self, InvalidCompression>;
}
impl TryFrom<i32> for Compression { … }   // ROOT's algorithm*100 + level
```

- **Write policy** reproduces ROOT exactly [RB§4], in one function:
  - no compression when the level is 0 or `ObjLen ≤ 256`;
  - otherwise blocks of ≤ 16 MiB, and the **whole object is stored raw** if
    any block fails to shrink.
- **Reading** walks the 9-byte block headers [§6] and checks LZ4's XXH64.
  Totals are checked against `ObjLen`. The algorithm is a fact of the file,
  so an unsupported codec is a run-time `UnknownCompression` error.
- `CodecRegistry` maps tags to `Codec` implementations. The trait is open, so
  a new codec can be registered without forking.

| Feature | Backend | Pure Rust |
|---|---|---|
| `zlib` (default) | `flate2` (miniz_oxide; `zlib-rs` for speed) | yes |
| `lz4` (default) | `lz4_flex` (block API) + `xxhash-rust` | yes |
| `zstd` (default) | `zstd` (libzstd) | no |
| `zstd-pure` | `ruzstd` (decoder) | yes |
| `lzma` (default) | `liblzma` (the maintained successor of `xz2`) | no |

### 4.3 `rootfile-source` and `rootfile-io` — moving bytes

The traits live in **`rootfile-source`**, a stable crate kept deliberately
tiny:
- the three traits below and `FileOffset`;
- `bytes` as its only dependency;
- implementations for `std` types.

Anyone writing a source (a storage system, a test double, a custom cache)
depends on it alone, as `tower` services depend on `axum-core` rather than
`axum`. The source implementations and adapters live in `rootfile-io`.

```rust
/// Synchronous positioned reads: std::fs::File, Bytes, Vec<u8>, &[u8], Mmap (feature), Arc<T>.
pub trait ReadAt: Send + Sync {
    fn len(&self) -> io::Result<u64>;
    fn read_at(&self, range: Range<FileOffset>) -> io::Result<Bytes>;
    fn read_ranges(&self, ranges: &[Range<FileOffset>]) -> io::Result<Vec<Bytes>> { /* loop */ }
}

/// Asynchronous reads. Boxed futures, as parquet's `AsyncFileReader` uses, so
/// the trait is dyn-compatible (`Arc<dyn AsyncReadAt>`, exactly like the sync
/// side) and a stream can store an in-flight request in a struct field and
/// poll it by hand. One allocation per request is noise next to a network
/// round trip.
pub trait AsyncReadAt: Send + Sync {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>>;
    fn read_at(&self, range: Range<FileOffset>) -> BoxFuture<'_, io::Result<Bytes>>;
    /// The default issues the reads one after another, like parquet's
    /// `get_byte_ranges`. Remote sources override it with a batched request.
    fn read_ranges(&self, ranges: Vec<Range<FileOffset>>) -> BoxFuture<'_, io::Result<Vec<Bytes>>> { … }
}

/// The same with `LocalBoxFuture`, for browser `fetch` (whose futures are not `Send`).
pub trait LocalAsyncReadAt { /* same three methods */ }

/// Positioned writes: std::fs::File, Vec<u8>.
pub trait WriteAt: Send {
    fn write_all_at(&mut self, offset: FileOffset, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}
```

| Source | Where | Vector read |
|---|---|---|
| `std::fs::File` | core | one `read_at` per range (the OS page cache does the rest) |
| `memmap2::Mmap` | feature `mmap` | zero-copy slices |
| `Bytes` / `Vec<u8>` | core | zero-copy slices |
| `HttpSource` (`reqwest`) | feature `http` | multipart `Range: bytes=a-b,c-d`; falls back to parallel single ranges when the server answers `200` or with a single part [§12] |
| `ObjectStoreSource` | `rootfile-object-store` | `get_ranges` (S3, GCS, Azure, HTTP) |
| browser `fetch` | feature `wasm` | implements `LocalAsyncReadAt` |
| XRootD | — | through the server's HTTP(S) interface today. A native client is a later `AsyncReadAt`, with no other changes |

Adapters, all composable:

- `Coalesce<S>`, with object_store's tested defaults:
  - ranges whose gap is at most **1 MiB** are merged;
  - up to **10** merged requests run in parallel;
  - results come back in the caller's order. Sort by start, extend the
    current run while `next.start − end ≤ gap`, then split the responses
    back with `partition_point`.
- `ReadCache<S>`: caches the first 4 KiB and recently read metadata.
- `BlockingAsync<S>`: presents an async source as `ReadAt` for sync callers.
- Blanket impls: any `AsyncRead + AsyncSeek` type is an `AsyncReadAt` (as
  parquet does for `tokio::fs::File`), and `Box<dyn AsyncReadAt>` is one.

### 4.4 `rootfile-streamer` — objects as bytes and back

**The model of the class dictionary** [§7.1]:

- `StreamerInfo { name, class_version: ClassVersion, checksum: Checksum, elements }`
- `StreamerElement { name, type_name, kind: ElementKind, array_dims }`
- `ElementKind` is `#[non_exhaustive]`:
  - `Base`
  - `Basic(BasicType)`
  - `FixedArray`
  - `VarArray { count }`, which reads and writes the 1-byte presence flag
    in front of the array [RB§6]
  - `Object`, `ObjectAny`, `ObjectPointer`
  - `TString`, `StdString`, `CharStar`
  - `Stl { container, value_type }`, recognized by the element class
    `TStreamerSTL` and `fSTLtype`, because files carry code 300 or 500
    [RB§8.8]
  - `Float16 { range }`, `Double32 { range }`

`Float16_t` and `Double32_t` carry their range or bit count, decoded with
ROOT's three storage modes [RB§6]. `StreamerInfo` is itself decoded by a
hand-written bootstrap decoder.

**Traits:**

```rust
pub trait RootClass  { const CLASS_NAME: &'static str; const CLASS_VERSION: ClassVersion;
                       fn streamer_info() -> &'static StreamerInfo; }
pub trait Unmarshal: Sized { fn unmarshal(r: &mut RBuffer<'_>, cx: &mut ReadCtx<'_>) -> Result<Self>; }
pub trait Marshal          { fn marshal(&self, w: &mut WBuffer, cx: &mut WriteCtx<'_>) -> Result<()>; }
pub trait FromDyn: Sized   { fn from_dyn(obj: &DynObject) -> Result<Self>; }

/// What users name in bounds, and what `#[derive(RootObject)]` implements:
/// the trait and the derive share one name, as in serde.
pub trait RootObject: RootClass + Marshal + Unmarshal + FromDyn {}
```

- **Open traits.** They stay open: classes with hand-written streamers need
  manual implementations.
- **Derive first.** The derive is the normal way to implement them, and new
  methods are only ever added with default bodies.
- **One bound for users.** User code writes `T: RootObject`. The four
  component traits exist for the rare type that only reads (`Unmarshal +
  FromDyn`) or only writes.

**serde bridge** (feature `serde`). A `&DynObject` implements
`serde::Deserializer`, mapping:
- members to struct fields;
- `Value::Array` to sequences;
- bases to fields named after the base class, which `#[serde(flatten)]` can
  merge.

So any class ROOT wrote can be read into a plain
`#[derive(serde::Deserialize)]` struct, without a ROOT-specific derive:

```rust
#[derive(serde::Deserialize)]
struct Config { #[serde(rename = "fThreshold")] threshold: f64, #[serde(rename = "fLabels")] labels: Vec<String> }
let cfg: Config = rootfile::serde::from_dyn(&file.get_dyn("config/MyConfig")?)?;
```

`DynObject` and `Value` also implement `serde::Serialize`, which
`rootfile dump --json` uses. The bridge is read-only by design: writing still
needs `RootObject`, because ROOT needs a class description that serde's data
model cannot express (class versions, `Double32_t` ranges, `//[fN]` counts).

**Three decode paths, one entry point.** `get::<T>()` picks between the first
two:

1. **Typed fast path.** When the on-file `(class, version, checksum)` equals
   the compiled one, `Unmarshal` runs directly on the bytes.
2. **Evolution path.** Otherwise the dynamic decoder interprets the on-file
   `StreamerInfo` into a `DynObject`, and `FromDyn` maps it to `T` by member
   name.
3. **No model.** `get_dyn()` returns the `DynObject` itself.

`DynObject` and `Value` are as in revision 1. `Value::Opaque` keeps
undecodable members as raw bytes, skipped by their byte count, so update mode
can write them back unchanged.

**`ClassRegistry`** holds hand-written codecs for the classes whose streamers
are not described by their `StreamerInfo`:

- `TObject`, `TNamed`, `TString`
- `TList`, `THashList`, `TMap`, `TObjArray`, `TClonesArray`
- the `TArray*` family
- `TRef`, `TRefArray`, `TObjString`, `TDatime`

The registry is built explicitly. There is no link-time registration, so it
works on WASM.

**Writing class descriptions.** The writer collects the `StreamerInfo` of
every class it writes, plus their dependencies, into the `StreamerInfo` list
[RB§3].

- For built-in classes it uses the descriptions ROOT writes, as uproot does.
- For user classes it uses the ones the derive macro generates.

> **Licence check.** Porting `TClass::GetCheckSum` and embedding class
> descriptions captured from ROOT-written files must be reviewed against
> ROOT's LGPL-2.1 before the first release (§13).

### 4.5 `rootfile-derive`

```rust
#[derive(RootObject)]
#[root(class = "MyEvent", version = 3)]
struct MyEvent {
    #[root(base)] object: TObject,
    run: i32,
    e: [f64; 4],
    n: i32,
    #[root(count = n)] pt: Vec<f32>,    // float* fPt //[fN] : a path, not a string
    tag: RString,                       // TString
    ids: Vec<i32>,                      // std::vector<int>
    #[root(double32(range = 0.0..100.0, bits = 16))] energy: f64,
}
```

The macro generates `RootClass` (with a `StreamerInfo` and its checksum
computed at expansion), `Marshal`, `Unmarshal` and `FromDyn`. It rejects these
at compile time (§5.5):

- a `count` that is not an earlier field of an integer type;
- a field type with no ROOT mapping;
- duplicate on-disk member names;
- a `version` outside `1..=0x3FFF`;
- a range or bit count invalid for `Double32_t`/`Float16_t`.

`#[derive(Row)]` does the same for a row type of a `TTree` or an RNTuple.

### 4.6 `rootfile-objects`

Typed models for the classes everyone uses:

- `TAxis`
- `TH1*`, `TH2*`, `TH3*`, `TProfile*`
- `TGraph*`, `TEfficiency`
- `TF1` (data only)

They are generated by `cargo xtask gen-models` from class descriptions
captured from recent ROOT, and committed. Older versions are read through the
evolution path.

Ergonomic accessors keep the raw members intact: `values()`, `values_flow()`,
`edges()`, `errors()` and the stored statistics. Builders such as
`TH1D::builder(…).bins(100, 0.0, 200.0).build()` keep the fill statistics
consistent [§8].

### 4.7 `rootfile-columnar`

The code shared by `tree` and `ntuple`:

- `ReadPlan`: per cluster, the units to fetch and their ranges.
- Range coalescing.
- `Column`: native vectors plus `offsets` for jagged data.
- The sealed `NativeType` trait, implemented for the primitive types a column
  can hold (`f32`, `f64`, `i8`…`u64`, `bool`).
- `Jagged<T>`.
- Bulk byte-order conversion.
- **Parallel iteration, the rayon way** (feature `rayon`):
  - `Clusters` implements `IntoParallelIterator`, so `tree.clusters()` takes
    `.into_par_iter()` like any collection;
  - `tree.par_rows::<T>()` yields rows in parallel, one task per cluster.

  Order is preserved where rayon preserves it (`collect` into a `Vec`).
  Without the feature, these methods do not exist and nothing else changes.
  A test checks that parallel and sequential results are identical.

Arrow conversion lives in `rootfile-arrow`, over `Column`.

### 4.8 `rootfile-tree`

**Reading.**

- `Tree` holds the branch and leaf tree, the basket tables and the cluster
  layout. `fClusterRangeEnd` is inclusive [RB§8]. Embedded baskets are
  supported.
- An `Interpretation` (`#[non_exhaustive]`: `Primitive`, `FixedArray`,
  `Jagged`, `Strings`, `Objects`) is inferred from the leaf and can be
  overridden.
- The basket decoder implements ROOT's rules [RB§7]:
  - key extension with `fIOBits` (signalled by a negative `fNevBufSize`);
  - flag arithmetic (+10, +40, +80);
  - entry offsets after `fLast`, counted from the start of the key, so it
    uses an `RBuffer` with `origin = KeyLen`;
  - regenerated offsets (flag ≥ 80).

The read pipeline is unchanged from revision 1:

```
plan (pure) → read_ranges (one vector read per cluster)
            → par_iter: decompress → split data/offsets → decode
            → Column
```

**Typed access.**

- `tree.branch::<f64>("px")?` and `tree.branch::<Jagged<f32>>("jet_pt")?`:
  the Rust type must be a `NativeType`, checked at compile time. Whether it
  matches the leaf is checked once, when the branch is opened (§5.7).
- `tree.rows::<Event>()?` for a `#[derive(Row)]` type: the schema is
  checked once at open, and the loop that follows has no per-entry checks.
- `tree.rows_of::<(f32, Vec<f32>)>(["MET_pt", "Muon_pt"])?` for a quick
  typed loop **without a derive**, after axum's tuple extractors. Tuples of up
  to 16 column types implement `Row` when paired with an array of names. The
  checks and the speed are the same as with the derive.
- A `#[derive(Row)]` type is not tied to a format: the same `Event` reads
  from a `TTree` or an RNTuple and writes to either. That is serde's
  separation of data model and format.

**Writer.** The layout is now verified [RB§8]: `TTree` v20, `TBranch` v13,
`TLeaf` v2, the `TLeafX` classes v1 and `TBasket` v3, with every persistent
member in order. The first writer targets **the subset uproot writes and
ROOT already reads**:

- one `TBranch` per column, with `TLeaf{O,B,S,I,L,F,D,C}`;
- fixed-size arrays (`x[3]/F`);
- jagged arrays through a **counter leaf** (`n/I` with `fIsRange`, and
  `x[n]/F` with `fLeafCount` referring to it);
- strings (`TLeafC`).

**Split objects (`TBranchElement`), M7.** The layout is now verified
[RB§8.8]. Reading maps each `fType` to an `Interpretation`:

| `fType` | Interpretation |
|---|---|
| −1, and 0 with `fID = −1` | `Objects(streamer)`: the whole object per entry, decoded by the dynamic decoder |
| 0 with `fID = −2`, 1 and 2 when split | no column: a **group** whose children are columns, presented as a struct |
| 0, 1 and 2 leaves with `fID ≥ 0` | `Primitive`, `FixedArray`, `Jagged` (`//[fN]`, using `fBranchCount2`) or `Strings`, from `fStreamerType` |
| 3 and 4 (counters) | `Primitive(i32)`, turned into **shared offsets** |
| 31 and 41 (members) | `Jagged` over the counter's offsets, members stored back to back |

A split `std::vector<Jet>` therefore reads as one Arrow `List<Struct>`
without decoding any object. Writing mirrors this, as `#[derive(Row)]` with
nested structs and `Vec<struct>`:

- the writer emits the counter and member branches;
- it emits no baskets for container nodes;
- it forces the class descriptions of the content classes into the
  StreamerInfo record, as ROOT does.

What the writer must get right, each with its own test:

- Serialize the tree through one `WBuffer`, so `TTree::fLeaves` can refer
  back to the leaves inside `fBranches`.
- Write the 1-byte presence flag before every `//[fN]` array.
- Write `TIOFeatures` as version 0 + checksum.
- Keep basket capacity above the basket count, because
  `fBasketEntry[fWriteBasket]` holds the total entry count.
- Rewrite the tree metadata in place while the reserved capacity lasts, and
  otherwise rewrite the tree and free the old copy, as uproot does.
- Flush every branch at cluster boundaries and record them in
  `fClusterRangeEnd`/`fClusterSize` (inclusive ends) or in `fAutoFlush`.

The writer stays behind `unstable-tree-writer` until it passes the
cross-check. After that it is stable. RNTuple remains the recommended format
for new data.

### 4.9 `rootfile-ntuple`

- **Containers** [RB§9]:

  ```rust
  enum Container { TFile(Key<Anchor>), Bare }
  ```

  `RNTuple::open(source)` detects which one from the magic (`"root"` or
  `"rntuple"`), and the facade's `file.rntuple(name)` uses the TFile form.
- **The anchor** is a 70-byte big-endian object followed by an XXH3-64
  checksum of bytes `[6, len − 8)` [RB§9]. Its key's `ObjLen` is ≥ 78, and the
  anchor is found by matching the class name, then the object name.
- **`RBlob`** is `Record` with `KEY_FORM = AlwaysLarge`, written as key
  version 1004. Payloads larger than `MaxKeySize` are chunked with the
  trailing `u64` offset table [RB§9].
- **Envelopes and frames** are skipped by their declared size. A different
  *epoch* is refused, newer major/minor versions are accepted, and **unknown
  feature-flag bits are refused**, as the specification requires.
- **Encodings:** `decompress → unsplit → undelta/unzigzag →
  untruncate/dequantize`. Each step is a pure function paired with its
  inverse, and property-tested as a pair.
- **Writer:** `RNTupleWriter<'s, E: Row>` or `RNTupleWriter<'s, DynRow>`
  (§4.10). Pages are 1 MiB, clusters ~128 MiB, Zstd level 5, split encodings.
  The header, then the pages, then the page lists, the footer, and finally the
  anchor are written at `finish`.
- **Storage** stays behind a `PageSource`/`PageSink` pair, so object-store
  RNTuple is a later addition.

### 4.10 `rootfile` — the facade

**Five levels, one ladder.** A user climbs only as far as the task needs.
Each level is a thin layer over the one below, and every level shares the
same `File`:

| Level | For | Entry point |
|---|---|---|
| 1. Objects | histograms, graphs, user classes | `file.get::<T>(path)` |
| 2. Columns | one branch or field at a time | `tree.branch::<T>(name)?.read(range)` |
| 3. Rows | typed loops over entries | `tree.rows::<T>()` with `#[derive(Row)]`, or `tree.rows_of::<(A, B)>([…])` with no derive; `par_rows` with rayon |
| 4. Arrow | handing data to other tools | `tree.record_batches(..)` (`rootfile-arrow`) |
| 5. Dynamic | unknown schemas, tools like `rootfile dump` | `file.get_dyn(path)`, `Interpretation`, `DynRow` |

`use rootfile::prelude::*;` imports what levels 1–3 need: the derives, the
extension traits, `File`, `Result`, `Jagged`.

**Reading.**

```rust
let file = rootfile::File::open("data.root")?;                    // File: Send + Sync + 'static
let file = rootfile::File::open_with("data.root", ReadOptions::default().strictness(Strictness::Strict))?;
let file = rootfile::File::from_source(source, ReadOptions::default())?;
let file = rootfile::AsyncFile::open(source).await?;              // also LocalAsyncFile on wasm

for key in file.keys() { println!("{key}"); }                      // root directory: name;cycle  class  title

let h: TH1D        = file.get("dir/hist")?;                       // &str: parsed at run time
let h: TH1D        = file.get("dir/hist;2")?;                     // explicit cycle
let h: TH1D        = file.get(path!("dir/hist"))?;                // literal checked at compile time
let obj: DynObject = file.get_dyn("dir/my_event")?;
let tree           = file.tree("events")?;
let nt             = file.rntuple("Events")?;
```

- **Paths.** `get`, `tree`, `rntuple` and `get_dyn` take `impl AsObjectPath`,
  a sealed trait implemented for `&str`, `String` and `ObjectPath`.
  - A plain string is the friendly default; a malformed one is an
    `ErrorKind::InvalidPath`.
  - `path!()` is the opt-in compile-time check.
  - An `ObjectPath` value is already validated, so nothing is checked twice.
- **Handles.** Read handles (`File`, `Directory`, `Tree`, `RNTuple`) are cheap
  `Arc` clones, `Send + Sync + 'static`, usable from threads and async tasks
  without lifetimes in user code.
- **No generic parameter on `File`.** The source is type-erased once
  (`Arc<dyn ReadAt>`), at the I/O boundary, where a virtual call costs nothing
  next to the read itself. Users write `File`, never `File<MmapSource>`.
- **Async mirrors sync.** `AsyncFile` has exactly the read methods of `File`,
  with the same names and arguments, plus `.await`. There is no separate
  vocabulary to learn.

**Writing: a scoped API by default.**

```rust
rootfile::create("out.root", |f| {
    let mut events = f.rntuple::<Event>("Events")?;  // RNTupleWriter<'s, Event>
    let mut runs   = f.rntuple::<Run>("Runs")?;      // several writers at once
    let calib      = f.mkdir("calib")?;              // DirWriter<'s>
    for ev in source {
        events.push(&ev)?;                           // type-checked: only Event fits
        if ev.end_of_run() { runs.push(&ev.run())?; calib.put("gain", &gain)?; }
    }
    f.put("h_pt", &h_pt)?;
    Ok(())
})?;  // the scope finishes every child, then writes key lists, StreamerInfo, free list, header
```

```rust
pub fn create<T, E>(path: impl AsRef<Path>,
                    body: impl for<'s> FnOnce(&'s FileScope<'s>) -> Result<T, E>) -> Result<T, E>
where E: From<rootfile::Error>;

pub fn create_with<T, E>(path: impl AsRef<Path>, opts: WriteOptions,
                         body: impl for<'s> FnOnce(&'s FileScope<'s>) -> Result<T, E>) -> Result<T, E>
where E: From<rootfile::Error>;

pub fn update<T, E>(…) -> Result<T, E>;         // same shape; and update_with
```

- **The error type is the caller's.** The closure may return `anyhow::Error`
  or the application's own error, as long as it implements
  `From<rootfile::Error>`, so `?` works on both kinds of error inside the
  closure. `std::thread::scope` is equally generic in what it returns.
- **Options are optional.** `create` uses `WriteOptions::default()`, and
  `create_with` takes them, the way `File::create` and `OpenOptions` split in
  `std`.

This is the model of `std::thread::scope`:

- **Several writers at once.** Children (`RNTupleWriter<'s, E>`,
  `DirWriter<'s>`) hold a **shared** `&'s FileScope`. The allocator and file
  end sit behind a `Mutex`, so any number of trees, ntuples, `put`s and
  `mkdir`s coexist, which is ROOT's usual pattern. `FileScope: Sync` and
  children are `Send`, so they can be moved into `std::thread::scope` threads.
- **Nothing outlives the file.** `'s` is higher-ranked, so a child cannot
  escape the closure. Using one after the file is closed is a compile error.
- **Finalization cannot be forgotten or skipped.** Each child's state lives
  in the scope's own registry; the handle is only a view onto it. When the
  closure returns, the scope finishes every child still open, in creation
  order, then closes the file, and returns `Result`. Forgetting `finish()`,
  `mem::forget` on a handle, or an early `?` all still produce a complete
  file.
- **Errors and panics.** If the closure returns `Err`, the scope still
  closes the file so that what was written is readable, then returns the
  error. If it panics, nothing is written after the panic: the header still
  says "not closed", and ROOT's recovery or `rootfile check --recover` can
  salvage it.
- **Directories.** `mkdir("a/b")` behaves like `std::fs::create_dir_all`: it
  creates missing levels and returns the existing directory if there is one.
  `put` under an existing name adds a cycle, as ROOT does; `put_replace`
  replaces it.
- **Writing is blocking.** Writes are local and sequential, so the write API
  is synchronous. From async code, wrap it in `spawn_blocking`.

**Which writing API?** Use `rootfile::create` unless the writer must outlive
a function call, for example stored in a struct or handed to a framework.
Then use `WritableFile`.

**Writing: an owned handle, for writers stored in long-lived structs.**

```rust
let mut out = rootfile::WritableFile::create("out.root", WriteOptions::default())?;
let mut ev  = out.rntuple::<Event>("Events")?;     // OwnedRNTupleWriter<Event>: 'static, holds an Arc of the sink
ev.push(&e)?;
ev.finish()?;                                    // finish(self): consumes
out.close()?;                                    // close(self): consumes; Err(ChildrenOpen) if a child is alive
```

- `close(self)` and `finish(self)` consume, so the handle cannot be used
  after them.
- **`Drop` performs no I/O.** A dropped, unclosed file keeps a header that
  marks it as not closed, so ROOT and our `check` detect it. It never looks
  valid while half-written. A debug-mode `tracing` warning reports the
  forgotten close.

**Constructors are distinct functions with distinct return types:**

| Call | Returns |
|---|---|
| `File::open`, `File::open_with` | `File` (read only) |
| `WritableFile::create`, `WritableFile::create_with` | `WritableFile` |
| `WritableFile::open_update` | `WritableFile` (existing file) |
| `rootfile::create[_with]` / `rootfile::update[_with]` | scoped |

There is no `update(bool)` option.

**`WriteOptions`:**

| Option | Default |
|---|---|
| `compression` | `Compression::zlib::<1>()`, ROOT's default |
| `reproducible` | `false`. When `true`, uses ROOT's `kReproducible` values (datime 1, zero UUIDs) [RB§2], so outputs compare byte for byte |
| `claimed_root_version` | fixed per release (§13) |

**Parallelism is the caller's.** Decompression and decoding run in the
*current* rayon pool, so `pool.install(|| tree.branch(..)?.read(..))` bounds
it. `ReadOptions::parallel(false)` keeps everything on the calling thread.
The library never creates threads or a global pool of its own.

**Features:**

- `default = ["zlib", "lz4", "zstd", "lzma", "objects", "tree", "ntuple", "rayon", "derive"]`.
  The defaults open any file.
- `default-features = false, features = ["pure-rust"]` gives a build with no
  C compiler, for WASM, at the cost of a zstd decoder only and no LZMA.
- opt-in: `mmap`, `http`, `async`, `wasm`, `serde`, `unstable-tree-writer`.

### 4.11 `rootfile-cli`

`clap` 4 (derive) and `anyhow`. Subcommands:

| Command | What it does |
|---|---|
| `ls [-l]` | List keys |
| `dump [--json]` | Print an object |
| `streamers` | Print class descriptions |
| `tree` | Branches, types, sizes, compression ratio |
| `check [--recover]` | The record walker plus every index, checksum and decompression |
| `cp` | Re-compress, or copy objects between files; compression via `Compression::new` at run time |

### 4.12 Patterns taken from the reference crates' source

Each subsection names the code it follows. The shapes are the reference
crates'; the details are ours.

#### 4.12.1 One reader builder for sync and async (parquet `ArrowReaderBuilder`)

parquet has a single `ArrowReaderBuilder<T>`, and its sync and async readers
are type aliases over newtypes of the input. We do the same, instead of two
parallel APIs that drift apart:

```rust
pub struct TreeReaderBuilder<I> {
    input: I,                          // SyncInput(Arc<dyn ReadAt>) or AsyncInput(Arc<dyn AsyncReadAt>)
    metadata: Arc<TreeMetadata>,       // loaded once, shareable (below)
    projection: Projection,
    selection: EntrySelection,
    batch_size: BatchSize,
    limit: Option<u64>, offset: Option<u64>,
    options: ReadOptions,
}
pub type TreeReaderBuilderSync  = TreeReaderBuilder<SyncInput>;   // build() -> TreeReader: Iterator<Item = Result<Batch>>
pub type TreeReaderBuilderAsync = TreeReaderBuilder<AsyncInput>;  // build() -> TreeStream: Stream<Item = Result<Batch>>

impl<I> TreeReaderBuilder<I> {
    pub fn projection(self, p: impl Into<Projection>) -> Self;
    pub fn selection(self, s: impl Into<EntrySelection>) -> Self;
    pub fn batch_size(self, n: BatchSize) -> Self;   // default: one cluster per batch
    pub fn limit(self, n: u64) -> Self;  pub fn offset(self, n: u64) -> Self;  // applied after selection
}
```

`tree.reader()` and `async_tree.reader()` return the two aliases. The
setters, their docs and their validation exist once. `branch().read()`,
`rows()` and `record_batches()` are shortcuts over it, so there is still one
obvious way for simple cases. The same holds for RNTuple
(`RNTupleReaderBuilder<I>`).

#### 4.12.2 Projection and entry selection (parquet `ProjectionMask`, `RowSelection`; regex `Input`)

```rust
pub struct Projection { leaves: Option<Vec<bool>> }      // None = all
impl Projection {
    pub fn all() -> Self;
    pub fn names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self;  // "jets.e" selects a split member
    pub fn roots(…) -> Self; pub fn leaves(…) -> Self;      // by index, like parquet
    pub fn union(&mut self, other: &Self); pub fn intersect(&mut self, other: &Self);
}
impl<const N: usize> From<[&str; N]> for Projection { … }

pub struct EntrySelection { runs: Vec<Run> }             // alternating select/skip, never zero-length
pub struct Run { pub entries: u64, pub skip: bool }
impl EntrySelection {
    pub fn all() -> Self;
    pub fn from_ranges(r: impl IntoIterator<Item = Range<u64>>, total: u64) -> Self;
    pub fn from_entry_list(sorted: &[u64]) -> Self;     // ROOT's TEntryList, or a cut's output
    pub fn and_then(&self, other: &Self) -> Self; pub fn union(&self, other: &Self) -> Self;
    pub fn selected(&self) -> u64;
}
impl From<Range<u64>> for EntrySelection { … }
impl From<RangeFull> for EntrySelection { … }
impl From<RangeInclusive<u64>> for EntrySelection { … }   // and the other RangeBounds forms
```

- **Everything converts.** Like regex's search methods, which accept
  `impl Into<Input>`, every read accepts `impl Into<EntrySelection>`. So
  `read(..)`, `read(0..1000)` and `read(&cut)` all work. One parameter type
  stands in for what would otherwise be a combinatorial set of methods
  (regex's stated reason).
- **Validation in the setter, not at use.** An out-of-range selection fails
  when it is set, against `num_entries`, as `Input::set_span` checks its
  bounds.
- **The bridge to I/O** is `selection.scan_baskets(&branch) -> ReadPlan`: the
  baskets overlapping selected runs. This mirrors parquet's
  `RowSelection::scan_ranges`. It leaves coalescing to the I/O layer, as
  parquet does.
- **Sparse selections skip baskets without decompressing them.** Selected
  entries inside a fetched basket are extracted with its offset array.

#### 4.12.3 Metadata loaded once and shared (parquet `ArrowReaderMetadata`)

```rust
let meta: Arc<TreeMetadata> = TreeMetadata::load(&file, "Events")?;   // branches, basket tables, clusters
// One reader per cluster range, all sharing the metadata, read concurrently:
let readers = clusters.iter().map(|c| file.tree_reader_with(meta.clone()).selection(c.clone()).build());
```

- `FileMetadata` (header, directories, class descriptions) and `TreeMetadata`
  can be **serialized and reused across processes**, behind the `serde`
  feature. A job over 10 000 files of the same dataset reads the class
  descriptions once, which is what parquet's `get_metadata` hook allows with
  "cached metadata or pre-fetched metadata".
- `File::from_source_with_metadata(src, meta)` skips re-reading it.

#### 4.12.4 Separating I/O from decoding (parquet `next_row_group`, push decoder)

- **The async stream sits on a sans-IO decoder,** as parquet's sits on its
  push decoder. The decoder returns `NeedData(ReadPlan)` or a batch, and the
  stream turns each plan into one `read_ranges` call. This is the sans-IO
  core of §2 made concrete.
- **`stream.next_cluster().await`** returns a *sync* `ClusterReader` holding
  all the bytes it needs. The caller can decode it on rayon while the stream
  already fetches the next cluster. This is how parquet's docs recommend
  prefetching row groups.
- **In-flight state is an enum,** `enum RequestState { Idle { input }, InFlight { future: BoxFuture<'static, …> }, Done }`.
  The input moves into the future and comes back with the bytes. This is why
  the async trait returns `BoxFuture` (§4.3).

#### 4.12.5 Shared immutable state, per-thread scratch (regex-automata `meta::Regex` + `Pool`)

- **What is shared:** `File`, `Tree` and `TreeMetadata` are immutable behind
  an `Arc`, and cheap to clone.
- **What is scratch:** the mutable buffers decoding needs (decompression
  output, unsplit buffers, offset arrays) form a `Scratch`.
- **Where scratch comes from:** a `Pool<Scratch>` of **8 mutex-protected
  stacks, chosen by thread id**, each padded to a 64-byte cache line against
  false sharing. These are regex's numbers.
- **No unsafe.** regex adds an unsafe fast path for the owner thread; we
  don't (`#![forbid(unsafe_code)]`). Uncontended `std` mutexes are fast
  enough at basket granularity.
- **Expert API.** As regex offers `search_with(&mut Cache, …)`, we offer
  `branch.read_with(&mut scratch, selection)`, which skips the pool entirely
  for hot loops that own their buffers.

#### 4.12.6 Parallel iteration (rayon `range.rs`, plumbing README)

- **Delegate instead of implementing a Producer.** `Clusters` holds a
  `Vec<Range<u64>>`. `impl<'a> IntoParallelIterator for &'a Clusters`
  delegates to the slice's parallel iterator, so rayon's blanket impl gives
  `clusters.par_iter()` for free. The result is indexed (`zip`, `enumerate`
  and ordered `collect` work) and needs no custom `Producer`.
- **Indices are `usize`.** rayon's docs note that `zip` is not available on
  `u64` ranges, so per-index parallel APIs use `usize`, never entry numbers.
- **Never `Box<dyn ParallelIterator>`.** rayon's traits are not
  dyn-compatible, so `par_rows` returns `impl IndexedParallelIterator`.

#### 4.12.7 Rows from tuples (axum `all_the_tuples!`)

```rust
macro_rules! all_the_tuples {                       // one line per arity, #[rustfmt::skip]
    ($m:ident) => { $m!(T1); $m!(T1, T2); /* … */ $m!(T1, T2, /* … */ T16); };
}
macro_rules! impl_row_for_tuple {
    ($($T:ident),+) => {
        #[diagnostic::do_not_recommend]
        impl<$($T: Column),+> Row for ($($T,)+) { type Names = [&'static str; count!($($T)+)]; … }
    };
}
all_the_tuples!(impl_row_for_tuple);
```

- `#[diagnostic::do_not_recommend]` keeps the 16 tuple impls out of error
  messages, as axum does for its handler impls.
- `Column` and `Row` carry `#[diagnostic::on_unimplemented(note = "… see
  https://docs.rs/rootfile/…/columns")]` with a link, as axum's extractor
  traits do.
- No marker type parameter is needed: tuples and user structs never overlap.
  axum needs `M` only because one blanket impl would cover another trait's
  implementors; we avoid such blanket impls.

#### 4.12.8 Derive internals (serde_derive)

- **Error accumulation.** A `Ctxt` holding `RefCell<Option<Vec<syn::Error>>>`
  collects every problem. `check()` merges them with `syn::Error::combine`.
  Its `Drop` panics with "forgot to check for errors" if `check` was never
  called, guarded by `!thread::panicking()`. One build reports every bad
  attribute.
- **Attribute slots.** An `Attr<T>` slot per option reports duplicates at the
  second occurrence's span. Unknown keys are reported, and parsing goes on.
- **The wrapper:**

  ```rust
  #[doc(hidden)]
  #[allow(non_upper_case_globals, unused_attributes, unused_qualifications)]
  const _: () = {
      #[allow(unused_extern_crates)]
      extern crate rootfile as _rootfile;         // or `use <path> as _rootfile;` with #[root(crate = "…")]
      #[automatically_derived]
      impl _rootfile::RootObject for MyEvent { … }
  };
  ```

  Generated code names `Result`, `Ok`, `Vec`, `Option` through
  `_rootfile::__private`, a `#[doc(hidden)]` module that re-exports `core`
  and `alloc` items. A user's own `type Result<T> = …` therefore cannot break
  the expansion.
- **No versioned private module.** serde versions its `__private` module by
  patch number (`__private229`) because `serde_derive` may differ from
  `serde`. Ours is pinned with `=x.y.z` by the facade (§3), so a plain
  `__private` is enough.

#### 4.12.9 Trait evolution (serde `Deserializer`, `Visitor`)

- **Defaults for additions.** Methods added after 1.0 get a default body. It
  either forwards to an existing method (as `visit_i8` forwards to
  `visit_i64`) or returns `ErrorKind::Unsupported`, as `deserialize_i128`'s
  default returns "i128 is not supported". Implementors never break.
- **`#[inline]` on forwarding defaults; `#[cold]` on error defaults.**

---

## 5. Compile-time guarantees

What the compiler enforces, how, and what stays a run-time check. Every row
marked *compile* has a `trybuild` test in `tests/ui/` proving the misuse does
not compile (§5.6).

### 5.1 API misuse

| Misuse | Prevented by | Kind |
|---|---|---|
| Writing to a file opened for reading | `File` has no write methods; writing needs `WritableFile` or `FileScope` | compile |
| A writer outliving its file | Children borrow `&'s FileScope` with a higher-ranked `'s` | compile |
| Using a file or writer after close or finish | `close(self)`, `finish(self)` consume | compile |
| Forgetting to finish a writer | The scope finishes children; it does not depend on the user | by construction |
| Filling a tree or ntuple with the wrong row type | `RNTupleWriter<'s, E>::push(&E)` | compile |
| Reading a branch as a type no column can hold | `branch::<T: NativeType>()` (sealed) | compile |
| Reading a branch as the wrong *numeric* type | Checked once against the leaf when opened | run time (schema is in the file) |
| An invalid literal path such as `"a//b;0"` | `path!()` validates in a `const` block | compile |
| A path from user input, or any `&str` | Parsed on use → `ErrorKind::InvalidPath` | run time |
| Sending a non-thread-safe handle to a thread | `Send`/`Sync` bounds | compile |

### 5.2 Format invariants

| Invariant | Enforced by | Kind |
|---|---|---|
| Every object's byte count is patched once | `WBuffer::object(version, closure)` | compile (the only way to write an object) |
| Reference offsets count from the key start | `RBuffer::new` requires a `KeyRelOffset` origin | compile |
| Baskets and `RBlob` always use large keys | `Record::KEY_FORM = AlwaysLarge` on a sealed trait | compile |
| Other keys switch to large past 2 GB | `KeyForm::ByPosition` + the position | run time (depends on where the record lands) |
| RNTuple payloads are read little-endian | `LeBuffer` and `RBuffer` are different types | compile |
| Offsets, lengths, versions and cycles never mix | Newtypes `FileOffset`, `ByteLen`, `KeyRelOffset`, `ClassVersion`, `Cycle` | compile |
| Compression policy (≤ 256 bytes raw, all-or-nothing) | One private function; no public way to compress a record differently | by construction |

### 5.3 Configuration

| Misuse | Prevented by | Kind |
|---|---|---|
| `zstd` level 250 | `Compression::zstd::<L>()` with `const { assert!(..) }` | compile |
| Using a codec whose feature is disabled | The constructor does not exist without the feature | compile |
| Building an invalid `Compression` | Private fields | compile |
| A level from a config file or CLI | `Compression::new` → `Result` | run time |

### 5.4 Sealed and open traits

| Trait | Status | Why |
|---|---|---|
| `Record`, `NativeType` | sealed | They encode format facts that users must not extend |
| `ReadAt`, `LocalAsyncReadAt`/`AsyncReadAt`, `WriteAt` | open, in the stable `rootfile-source` | Users bring their own sources without depending on the whole library |
| `Codec` | open | Plug-in algorithms |
| `RootObject` (= `RootClass + Marshal + Unmarshal + FromDyn`), `Row` | open, derive-first; each trait shares its derive's name | Hand-written streamers need manual implementations; methods are only added with defaults |

### 5.5 Derive macro

| Mistake | Diagnostic |
|---|---|
| `#[root(count = x)]` where `x` is missing, later, or not an integer | Expansion error pointing at the attribute; the integer check uses a `CountType` bound with `#[diagnostic::on_unimplemented]` |
| A field type with no ROOT mapping | A `RootType` bound with `#[diagnostic::on_unimplemented(message = "`{Self}` has no ROOT representation; use …")]` |
| Duplicate on-disk names (after `rename`) | Expansion error |
| `version` out of range, invalid `Double32` range | Expansion error |

The checksum and the `StreamerInfo` are computed at expansion, so they cannot
drift from the struct.

### 5.6 Keeping the guarantees true

- `tests/ui/*.rs` holds one `trybuild` compile-fail case per *compile* row
  above, with the expected compiler message, so a guarantee cannot silently
  disappear.
- MSRV is pinned in CI, because `const { }` blocks need Rust ≥ 1.79 and
  `#[diagnostic::on_unimplemented]` needs Rust ≥ 1.78.

### 5.7 What cannot be compile-time, and how it is caught instead

| Fact | Why it is run time | Caught by |
|---|---|---|
| ROOT and uproot can read what we wrote | Depends on ROOT's reader | Cross-check CI from M0 (§9) |
| We read real files correctly | Depends on the bytes | Corpus dumps vs uproot (§9) |
| A file's schema matches a typed model | The schema is in the file | One check at open, then typed loops; `DynObject` otherwise |
| Hostile input | Bytes | `ReadLimits`, fuzzing |

---

## 6. The read path

| Step | Range (core) | Decoder |
|---|---|---|
| 1. Header | `0..4096`, clamped to the file length | `FileHeader::decode` + ROOT's checks |
| 2. Root directory | `root_directory_range()`, usually inside step 1 | `DirectoryRecord::decode` |
| 3. Key list | `key_list_range()` | `KeyList::decode`, seeks checked |
| 4–5. Subdirectories | `record_range()`, one read per level | `DirectoryRecord` + `KeyList` |
| 6. Object | `record_range()` | `KeyHeader::decode` |
| 7. Decode | — | `decompress` → `RBuffer::new(data, KeyLen)` → `StreamerSet` (loaded once, `OnceLock`) → typed or dynamic |

If there is no key list, or the file is truncated, `File::open` returns a
typed error. `File::open_recover` rebuilds the key list with the record
walker, like ROOT [RB§1].

## 7. The write path

The steps, in the order ROOT uses [RB§2, RB§3]:

```
create  header placeholder; TFile key at 100 (name = file name, TNamed + directory record
        + UUID + 12 bytes padding); free list = [100, 2e9]
put     serialize (WBuffer::object) → compression policy → Allocator (best fit, gap marker)
        → write key + data → record the key in DirectoryState (cycle = max + 1)
close   finish children (scope) → StreamerInfo list (old one freed)
        → for each dirty directory: new key list (old one freed), directory record rewritten in place
        → free list → header (large form if fEND > kStartBigFile) → flush
```

- **Cycles:** `put` on an existing name gets the next cycle; `put_replace`
  frees the previous one.
- **Update mode:**
  - reads the header, directories, class descriptions and free list;
  - fills gaps first;
  - never touches records it does not understand;
  - copies `Value::Opaque` members back as they were.
- **Reproducible mode** makes writer tests golden-file comparisons, including
  against files written by ROOT in its own `kReproducible` mode.

## 8. Concurrency and performance

- `File` is `Send + Sync`; reads take `&self`; metadata is cached in
  `OnceLock` and `RwLock` maps.
- Decompression and decoding run on `rayon`, one task per basket or page
  within a cluster.
- Byte-order conversion works on whole slices (`chunks_exact` +
  `from_be_bytes`, which vectorizes). RNTuple pages are reinterpreted in place
  on little-endian hosts (`bytemuck`, alignment checked).
- On the write side, RNTuple page compression runs in parallel. The scope's
  `Mutex` protects only allocation and the file end, so it is held for
  microseconds.
- Benchmarks (`divan`) run in CI against the previous commit, and
  periodically against uproot and ROOT.

## 9. Testing

1. **Cross-check harness, from M0.** `cargo xtask cross` runs the
   `rootproject/root` container and uproot.
   - Files we write must open in ROOT without triggering recovery, read fully
     in ROOT and in uproot, and match as JSON dumps.
   - Files ROOT writes (`tests/cross/*.C`) must match our dumps.
   - CI is gated on it.
2. **Corpus:** the files of
   [scikit-hep-testdata](https://github.com/scikit-hep/scikit-hep-testdata),
   with expected dumps compared through `insta`.
3. **Data-driven cases, after regex's test suite.** `tests/cases/*.toml`
   describes each check as data:

   ```toml
   [[case]]
   name   = "nanoaod-muon-pt"
   file   = "nanoAOD_2015_CMS_Open_Data_ttbar.root"
   object = "Events"
   branch = "Muon_pt"
   range  = [0, 1000]
   sum    = 52341.25          # or `dump = "snapshot-name"`, or `error = "Malformed"`
   ```

   One harness runs every case against every source (`Vec<u8>`,
   `std::fs::File`, mmap, a local HTTP server with and without multipart
   support, `AsyncFile`) and with `rayon` on and off. A bug in one source or
   path therefore shows up as a diff from the others. New cases need no Rust
   code.
4. **Property tests** (`proptest`) for every `decode`/`encode` pair, the
   allocator, the codecs and the RNTuple encodings.
5. **Compile-fail tests** (`trybuild`, §5.6).
6. **Fuzzing** (`cargo-fuzz`): one target per decoder plus whole-file open.
   - Invariant: no panic, nothing allocated beyond `ReadLimits`, and time
     linear in the input (Appendix C).
   - Run nightly, and continuously in **OSS-Fuzz** once the repository is
     public, as regex does.
   - **Examples are tests.** Every program in `examples/` is built in CI and
     run against the corpus, so the Appendix A recipes can never rot.
     Doc-tests cover every public item.
7. **Platforms:** Linux, macOS and Windows, big-endian `s390x`,
   `wasm32-unknown-unknown` (core with the pure-Rust codecs and
   `LocalAsyncReadAt`), and the MSRV.
8. **Supply chain and API:** `cargo-semver-checks` on the facade and adapter
   crates; `cargo-deny` for licences and advisories.
9. **Compile-time budget**, after kurbo's and dtolnay's attention to build
   times. CI records `cargo build --timings` for the facade with default
   features and fails on a regression above 15 %.
   - Proc macros stay out of every crate except `rootfile-derive`.
   - `syn` is used with only the features the derive needs.
   - Generic public functions follow the "generic outer, non-generic inner"
     pattern (STYLE §9).

## 10. Future proofing

- Versions are read from the file and dispatched; nothing assumes a version.
- `#[non_exhaustive]` on public enums and on structs that mirror on-disk
  records; builders instead of struct literals.
- Codecs, sources and class codecs are pluggable; format-invariant traits are
  sealed.
- RNTuple: frames are skipped by size; the epoch and feature flags are checked
  as the specification requires.
- Unknown members and records pass through opaque.
- The sans-IO core lets new transports arrive without touching format code.
- Arrow is the interchange format, isolated in an adapter crate.
- Models are generated from class descriptions, so a new ROOT version means
  regenerating them.
- The semver surface is explicit, with lockstep internal crates and adapters
  versioned with their ecosystems.
- Room is left for Python bindings (`pyo3`, Arrow through PyCapsule), a native
  XRootD client, and DataFusion/Polars providers over `rootfile-arrow`.

## 11. Libraries

| Concern | Crate | Why |
|---|---|---|
| Byte buffers | `bytes` | Zero-copy, stable 1.x, the common currency of Rust I/O |
| Errors | `thiserror` 2 (libraries), `anyhow` (CLI) | |
| Compression | `flate2` (+`zlib-rs`), `zstd`/`ruzstd`, `lz4_flex`, `liblzma` | §4.2 |
| Checksums | `xxhash-rust` (`xxh64`, `xxh3`) | Pure Rust |
| Async traits | `futures-core` / `futures-util` (`BoxFuture`, `Stream`) | Dyn-compatible async traits and batch streams, as parquet does |
| mmap | `memmap2` | |
| HTTP | `reqwest` (feature `http`) | Control over multipart ranges |
| Object stores | `object_store` (adapter crate) | |
| Arrow | `arrow-array`, `arrow-buffer`, `arrow-schema` (adapter crate) | |
| Parallelism | `rayon` | |
| Ordered maps | `indexmap` | Member order matters |
| UUID | `uuid` | |
| Logging | `tracing` | |
| Macros | `syn`, `quote`, `proc-macro2` | |
| CLI | `clap` 4 | |
| Tests | `proptest`, `insta`, `trybuild`, `cargo-fuzz`, `divan` | |
| Supply chain | `cargo-deny`, `cargo-semver-checks` | |

`#![forbid(unsafe_code)]` applies across the workspace. The only `unsafe` is
in dependencies (mmap, libzstd, liblzma), all behind features.

## 12. Roadmap

| Milestone | Content | Exit criterion |
|---|---|---|
| **M0** container + harness | core, compress, io, record walker; `ls`, `check`; **minimal writer** (empty file) | Lists every corpus file; our empty file passes the ROOT and uproot cross-check in CI |
| **M1** objects | streamer set, dynamic decoder, built-ins, typed histograms; `put` for histograms; scoped API | A one-histogram file passes the cross-check (first M1 task); ROOT and uproot read our histograms identically |
| **M2** TTree read | flat, fixed, jagged, strings, split objects; `rootfile-arrow`; HTTP + object store | Corpus TTree dumps match uproot |
| **M3** RNTuple read | both containers, all column types and encodings | ROOT's RNTuple test files read |
| **M4** RNTuple write | `RNTupleWriter`, parallel page compression | Cross-check passes; ROOT's RNTuple verifier is clean |
| **M5** update + derive | `open_update`, cycles, `#[derive(RootObject)]`, `#[derive(Row)]`, evolution | User classes round-trip between ROOT and us |
| **M6** TTree writer | The uproot-proven subset: flat, fixed arrays, jagged through counter leaves, strings; in-place metadata updates | Cross-check passes; leaves `unstable-tree-writer` |
| **M7** split-object writer | `TBranchElement` writing (types 0–41) for `#[derive(Row)]` with nested structs and `Vec<struct>`; reading already lands in M2 | User classes in trees round-trip between ROOT and us |
| later | Python bindings, analytics providers, native XRootD | — |

## 13. Open questions

1. **Claimed ROOT version** in `fVersion`, which affects the class versions
   readers expect. Proposal: a recent 6.x, fixed per `rootfile` release,
   overridable.
2. **Licensing:** is a clean-room port of `TClass::GetCheckSum`, plus
   embedding class descriptions captured from ROOT-written files, compatible
   with publishing under MIT/Apache-2.0? Get a review before M1 ships.
3. ~~`TTree` and `TBranchElement` layout~~ — **resolved 2026-11-25**: see
   [RB§8]. The last three details were checked on the same day [RB§8.9]:
   - `fID = −2` is set in `TTree::BronchExec`;
   - `kSplitCollectionOfPointers` is 100;
   - members of unsplit objects carry their own headers.

   One small point rests on uproot rather than on ROOT's writer code: the
   header of the top-level object itself. The M2 corpus tests confirm it on
   real files.
4. **`serde` bridge for `DynObject`:** opt-in, useful for JSON dumps, lossy
   for references.

## 14. Glossary

| Term | Meaning |
|---|---|
| **Key** (`TKey`) | The header in front of every record: size, class, name, cycle, position |
| **Record** | A key plus its (possibly compressed) data |
| **fBEGIN** | Offset of the first record, always 100 |
| **kStartBigFile** | 2 000 000 000 bytes: past this, offsets are written in 8 bytes |
| **Cycle** | Version number of an object under one name: `hist;1`, `hist;2`; a plain name means the highest |
| **Streamer / `TStreamerInfo`** | The routine that turns an object into bytes, and the record that describes its layout member by member |
| **Byte count** | The 4-byte length (bit 30 set) in front of a streamed object |
| **Class tag** | How a polymorphic pointer names its class: once per buffer, then by back-reference |
| **Member-wise** | Writing a collection member by member (all `x`, then all `y`) instead of object by object |
| **Basket** | A record holding one `TTree` branch's values for a range of entries |
| **Cluster** | A range of entries at which every branch or column is cut, the unit of parallel reading |
| **Page** | RNTuple's unit of storage and compression: a run of one column's values |
| **Anchor** | The small object pointing to an RNTuple's header and footer |
| **Envelope / frame** | RNTuple metadata containers; frames carry their own size so unknown fields can be skipped |
| **Epoch** | The RNTuple format version component that must match exactly |
| **Bare RNTuple** | An RNTuple stored directly, without a `TFile` around it |
| **Sans-IO** | A design in which parsing code never performs I/O itself |

---

## Appendix A. API by example

Illustrative code for the API this document specifies. The names are the
design's, not a released crate's. Every example returns `rootfile::Result`, so
`?` works throughout.

### A.1 Inspecting a file

```rust
use rootfile::prelude::*;

fn main() -> rootfile::Result<()> {
    let file = File::open("data.root")?;
    println!("{}", file.header());              // format version, size, compression, …

    for key in file.keys() {                    // root directory; highest cycle of each name
        println!("{:<24} {:<12} {}", key.name_cycle(), key.class_name(), key.title());
    }

    for key in file.root().walk() {             // recursive and lazy: one read per directory
        let key = key?;
        if !key.is_directory() {
            println!("{}", key.path());         // e.g. "calib/gain;2"
        }
    }
    Ok(())
}
```

### A.2 Histograms and other objects

```rust
use rootfile::objects::{TH1D, TH2F};

let h: TH1D = file.get("analysis/h_pt")?;             // typed
let h: TH1D = file.get(path!("analysis/h_pt"))?;      // same, with the path checked at compile time
println!("{} entries, mean {:.2}, rms {:.2}", h.entries(), h.mean(), h.std_dev());
for (i, (v, e)) in h.values().iter().zip(h.errors()).enumerate() {
    println!("[{:>6.1}, {:>6.1})  {v} ± {e}", h.edges()[i], h.edges()[i + 1]);
}

let map: TH2F = file.get("calib/map;3")?;      // an explicit cycle

// A class with no Rust model: decoded from the file's own class description.
let cfg = file.get_dyn("config/MyConfig")?;
let threshold: f64 = cfg.member("fThreshold")?.try_into()?;
println!("{cfg:#?}");                                 // the whole object, member by member
```

### A.3 Reading `TTree` columns

```rust
use rootfile::columnar::Jagged;

let tree = file.tree("Events")?;
println!("{} entries in {} clusters", tree.num_entries(), tree.clusters().len());

let met: Vec<f32> = tree.branch::<f32>("MET_pt")?.read(..)?;              // all entries
let mu_pt = tree.branch::<Jagged<f32>>("Muon_pt")?.read(0..100_000)?;     // JaggedVec<f32>
for (event, muons) in mu_pt.iter().enumerate() {                          // muons: &[f32]
    if muons.len() >= 2 { /* … */ }
}

// One task per cluster: each reads only its own baskets, in one vector read.
use rayon::prelude::*;
let sum_pt: f64 = tree
    .clusters()
    .par_iter()
    .map(|range| -> rootfile::Result<f64> {
        let pt = tree.branch::<Jagged<f32>>("Muon_pt")?.read(range.clone())?;
        Ok(pt.values().iter().map(|&x| f64::from(x)).sum())
    })
    .sum::<rootfile::Result<f64>>()?;
```

### A.4 Typed rows

```rust
#[derive(rootfile::Row)]
struct Muons {
    #[root(name = "Muon_pt")]     pt: Vec<f32>,
    #[root(name = "Muon_charge")] charge: Vec<i32>,
    #[root(name = "MET_pt")]      met: f32,
}

// The schema is checked against the tree once, here. The loop then has no
// per-entry type checks.
for row in tree.rows::<Muons>()? {
    let m = row?;
    let q: i32 = m.charge.iter().sum();
}
```

### A.5 Arrow

```rust
use rootfile_arrow::TreeExt;                          // adapter crate, versioned with arrow

for batch in tree.record_batches(&["MET_pt", "Muon_pt"], 65_536)? {   // cluster-aligned
    let batch: arrow_array::RecordBatch = batch?;
    // Hand it to DataFusion, Polars, DuckDB, Parquet writers, PyArrow …
}
```

### A.6 Remote files, async

```rust
use rootfile::{AsyncFile, io::HttpSource, columnar::Jagged, path};

let source = HttpSource::new("https://example.org/data/nano.root")?
    .coalesce_default();                              // merge gaps up to 1 MiB, 10 requests in parallel
let file = AsyncFile::open(source).await?;            // header + root directory: 1 request
let tree = file.tree("Events").await?;
let pt = tree.branch::<Jagged<f32>>("Muon_pt").await?
    .read(..).await?;                                 // one multi-range request per cluster
```

### A.7 RNTuple

```rust
let nt = file.rntuple("Events")?;
for field in nt.fields() {
    println!("{}: {}", field.name(), field.type_name());   // e.g. "jets: std::vector<Jet>"
}
let pt: Vec<f32> = nt.field::<f32>("pt")?.read(..)?;
let jet_e = nt.field::<Jagged<f32>>("jets.e")?.read(..)?;

let bare = rootfile::RNTuple::open("events.rntuple")?;      // a bare RNTuple, no TFile
```

### A.8 Writing histograms and an ntuple

```rust
use rootfile::{WriteOptions, Compression, objects::{TH1D, TObjString}};

#[derive(rootfile::Row)]
struct Event { run: u32, pt: f32, jets: Vec<f32> }

let mut h_pt = TH1D::builder("h_pt", "p_{T};p_{T} [GeV];events")
    .bins(100, 0.0, 200.0)
    .build()?;

rootfile::create_with("out.root", WriteOptions::default().compression(Compression::zstd::<5>()), |f| {
    let mut events = f.rntuple::<Event>("Events")?;
    for ev in source() {
        h_pt.fill(f64::from(ev.pt));
        events.push(&ev)?;                     // only an `Event` is accepted
    }
    f.put("h_pt", &h_pt)?;                     // while the ntuple writer is still open
    f.mkdir("meta")?.put("version", &TObjString::new("v1.2"))?;
    Ok(())
})?;                                           // every writer finished, keys, StreamerInfo, header: done
```

### A.9 Several writers, several threads

```rust
rootfile::create("split.root", |f| {
    let mut signal = f.rntuple::<Event>("Signal")?;
    let mut background = f.rntuple::<Event>("Background")?;

    std::thread::scope(|s| -> rootfile::Result<()> {
        let a = s.spawn(|| fill(&mut signal, signal_source()));
        let b = s.spawn(|| fill(&mut background, background_source()));
        a.join().expect("signal filler panicked")?;
        b.join().expect("background filler panicked")?;
        Ok(())
    })?;
    Ok(())
})?;

fn fill(w: &mut RNTupleWriter<'_, Event>, src: impl Iterator<Item = Event>) -> rootfile::Result<()> {
    for ev in src { w.push(&ev)?; }
    Ok(())
}
```

### A.10 A user class

```rust
use rootfile::{RootObject, objects::TNamed};

#[derive(RootObject, Debug, Clone, PartialEq)]
#[root(class = "Calibration", version = 2)]
struct Calibration {
    #[root(base)] named: TNamed,
    gain: f64,
    n: i32,
    #[root(count = n)] pedestals: Vec<f32>,         // float* fPedestals //[n]
    #[root(double32(range = 0.0..10.0, bits = 12))] noise: f64,
}

rootfile::create("calib.root", |f| f.put("calib", &calib))?;

let file = File::open("calib.root")?;
let back: Calibration = file.get("calib")?;   // fast path: same version and checksum
assert_eq!(back, calib);
// A file written by version 1 of the class reads through the evolution path:
// members are matched by name, new ones take their default.
```

ROOT, which has never seen `Calibration`, reads the file through the class
description the derive generated.

### A.11 A schema known only at run time

```rust
use rootfile::{Schema, DType, Value};

// e.g. converting a CSV whose columns come from its header line
let schema = Schema::builder()
    .field("run", DType::U32)
    .field("pt", DType::jagged(DType::F32))
    .build()?;

rootfile::create("converted.root", |f| {
    let mut w = f.rntuple_dyn("Events", &schema)?;           // RNTupleWriter<'_, DynRow>
    w.push_row(&[Value::U32(1), Value::from(vec![12.5_f32, 40.1])])?;  // checked at run time
    Ok(())
})?;
```

### A.12 A writer stored in a struct

```rust
struct Recorder {
    file: WritableFile,
    events: OwnedRNTupleWriter<Event>,                // 'static: holds an Arc of the sink
}

impl Recorder {
    fn new(path: &Path) -> rootfile::Result<Self> {
        let file = WritableFile::create(path, WriteOptions::default())?;
        let events = file.rntuple::<Event>("Events")?;
        Ok(Self { file, events })
    }
    fn record(&mut self, ev: &Event) -> rootfile::Result<()> { self.events.push(ev) }
    fn finish(self) -> rootfile::Result<()> {
        self.events.finish()?;                       // consumes the writer
        self.file.close()                            // consumes the file; Err(ChildrenOpen) if a child is alive
    }
}
```

### A.13 Updating an existing file

```rust
rootfile::update("data.root", |f| {
    f.put("h_pt", &h_pt)?;              // a new cycle: h_pt;2 next to h_pt;1
    f.put_replace("h_eta", &h_eta)?;    // replaces, and frees the old record's space
    f.delete("scratch/tmp")?;    // its space becomes a free gap
    Ok(())
})?;
// Records this library does not understand are left untouched.
```

### A.14 Run-time configuration

```rust
// `rootfile cp --compression 505 in.root out.root`
let compression = Compression::try_from(args.setting)?;   // 505 → Zstd level 5; 250 → Err
let opts = WriteOptions::default()
    .compression(compression)
    .reproducible(true);                // datime 1, zero UUIDs: byte-identical output
rootfile::create_with(&args.output, opts, |f| f.copy_from(&input, "/"))?;
```

### A.15 Untrusted input, diagnostics, errors

```rust
use rootfile::{ReadOptions, ReadLimits, Strictness, ErrorKind};

let opts = ReadOptions::default()
    .strictness(Strictness::Strict)                 // byte-count mismatches become errors
    .limits(ReadLimits::untrusted());               // tight allocation and nesting caps
let file = File::from_source(uploaded_bytes, opts)?;

match file.get::<TH1D>("h") {
    Ok(h) => { /* … */ }
    Err(e) if e.kind() == ErrorKind::NotFound => { /* absent */ }
    Err(e) => return Err(e),    // "byte count mismatch (expected 812, read 808) in class TH1D in key `h;1`"
}

// With the default `Lenient`, the same file opens, and the irregularities are reported:
for d in file.diagnostics() { tracing::warn!(%d); }
```

### A.16 A custom source

```rust
use rootfile::io::{ReadAt, FileOffset};

struct TapeArchive { /* … */ }

impl ReadAt for TapeArchive {
    fn len(&self) -> std::io::Result<u64> { /* … */ }
    fn read_at(&self, r: Range<FileOffset>) -> std::io::Result<bytes::Bytes> { /* … */ }
    // `read_ranges` has a default; override it if the store supports batched reads.
}

let file = File::from_source(TapeArchive::connect("…")?, ReadOptions::default())?;
```

### A.17 Writing a `TTree` (feature `unstable-tree-writer` until M6)

```rust
#[derive(rootfile::Row)]
struct Hit {
    run: i32,                       // Int_t,  leaf "run/I"
    energy: f64,                    // Double_t, "energy/D"
    xyz: [f32; 3],                  // "xyz[3]/F"
    xs: Vec<f32>,                   // counter leaf "n_xs/I" + "xs[n_xs]/F"
    label: String,                  // "label/C"
}

rootfile::create("hits.root", |f| {
    let mut t = f.tree::<Hit>("hits")?;      // TTree v20, one TBranch + TLeafX per member
    for h in hits() { t.fill(&h)?; }
    Ok(())
})?;
```

### A.18 What does not compile

Each case below is a `trybuild` test in `tests/ui/`.

```rust
let file = File::open("a.root")?;
file.put("h", &h)?;
// error[E0599]: no method named `put` found for struct `File`

let w = rootfile::create("o.root", |f| Ok(f.rntuple::<Event>("E")?))?;
// error: lifetime may not live long enough: the writer cannot escape the scope

let mut w = out.rntuple::<Event>("E")?;     // out: WritableFile
w.finish()?;
w.push(&ev)?;
// error[E0382]: borrow of moved value: `w`

events.push(&run)?;            // `events: RNTupleWriter<'_, Event>`, `run: Run`
// error[E0308]: mismatched types: expected `&Event`, found `&Run`

let c = Compression::zstd::<30>();
// error[E0080]: evaluation of constant value failed: zstd level must be in 1..=22

let c = Compression::lzma::<7>();          // built without the `lzma` feature
// error[E0599]: no function or associated item named `lzma` found for struct `Compression`

let h: TH1D = file.get(path!("dir//h;0"))?;
// error[E0080]: evaluation of constant value failed: invalid object path: empty component

let s = tree.branch::<String>("px")?;
// error[E0277]: `String` is not a column type; use `Jagged<u8>` or read strings with `branch_strings()`

#[derive(RootObject)]
struct Bad { #[root(count = n)] xs: Vec<f32>, n: i32 }
// error: count member `n` must be declared before `xs`

#[derive(RootObject)]
struct Bad2 { when: std::time::SystemTime }
// error[E0277]: `SystemTime` has no ROOT representation; use `Datime` or an integer
```

### A.19 Any class into a plain serde struct

```rust
// Feature `serde`. No ROOT-specific derive: serde's own does the work.
#[derive(serde::Deserialize, Debug)]
struct RunInfo {
    #[serde(rename = "fRun")]   run: u32,
    #[serde(rename = "fBeam")]  beam_energy: f64,
    #[serde(rename = "fTags")]  tags: Vec<String>,
}

let info: RunInfo = rootfile::serde::from_dyn(&file.get_dyn("meta/RunInfo")?)?;

// And the other way, for tools: any object as JSON.
let json = serde_json::to_string_pretty(&file.get_dyn("meta/RunInfo")?)?;
```

### A.20 Typed rows without a derive

```rust
// Tuples of column types are rows, as axum's handlers take tuples of extractors.
for row in tree.rows_of::<(f32, Vec<f32>, i32)>(["MET_pt", "Muon_pt", "nJet"])? {
    let (met, muon_pt, n_jet) = row?;
    // …
}
// Asking for a type the branch does not hold fails once, before the loop:
// "branch `nJet` holds i32 (TLeafI); requested f32 at tuple position 2"
```

### A.21 Parallel, the rayon way

```rust
use rayon::prelude::*;

// Clusters are a parallel iterator, like any collection.
let n_dimuon: usize = tree
    .clusters()
    .into_par_iter()
    .map(|range| -> rootfile::Result<usize> {
        let pt = tree.branch::<Jagged<f32>>("Muon_pt")?.read(range)?;
        Ok(pt.iter().filter(|m| m.len() >= 2).count())
    })
    .sum::<rootfile::Result<usize>>()?;

// Rows in parallel, one task per cluster:
let total: f64 = tree.par_rows::<Muons>()?.map(|r| r.map(|m| f64::from(m.met))).sum::<rootfile::Result<f64>>()?;

// Bounded parallelism: everything runs in the pool the caller chose.
let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
let met = pool.install(|| tree.branch::<f32>("MET_pt")?.read(..))?;
```

### A.22 The reader builder: projection, selection, sync and async

```rust
use rootfile::{Projection, EntrySelection, BatchSize};

// Sync: an Iterator of batches.
let reader = tree.reader()
    .projection(["Muon_pt", "Muon_eta", "MET_pt"])
    .selection(&passing_entries)                  // a sorted &[u64], e.g. from a first pass
    .batch_size(BatchSize::Entries(65_536))
    .build()?;
for batch in reader { let batch = batch?; /* batch.column::<Jagged<f32>>("Muon_pt")? … */ }

// Async: the same builder, a Stream of batches.
let mut stream = async_tree.reader().projection(["MET_pt"]).build()?;
while let Some(batch) = stream.try_next().await? { /* … */ }

// Async I/O overlapped with parallel decoding: fetch cluster n+1 while decoding cluster n.
let mut pending = Vec::new();
while let Some(cluster) = stream.next_cluster().await? {          // a sync ClusterReader, bytes in hand
    pending.push(tokio::task::spawn_blocking(move || cluster.decode()));  // decoding runs on rayon inside
}
for handle in pending { let batches = handle.await.expect("decoder panicked")?; /* … */ }

// Many files of one dataset: read the metadata once, reuse it.
let meta = rootfile::FileMetadata::load(&first_file)?;
for path in paths {
    let f = File::from_source_with_metadata(std::fs::File::open(path)?, meta.clone())?;
    // …
}
```

---

## Appendix B. Review against Rust principles and usability

Revision 3, 2026-11-25. Each finding names the problem, the principle at
stake, and the change made.

| # | Finding | Principle | Resolution |
|---|---|---|---|
| 1 | `tree.entries()` returned a count in one place and a typed iterator (`entries::<T>()`) in another | One name, one meaning (C-WORD-ORDER, least surprise) | `num_entries()` for the count, `rows::<T>()` for typed rows |
| 2 | The row trait and derive were named `Entry`, which collides with `std::collections::hash_map::Entry` in user imports | Friendly imports | Renamed `Row` / `DynRow` |
| 3 | Every lookup needed `path!(…)` or `ObjectPath::parse(…)?` | The common case must be short | Lookups take `impl AsObjectPath` (`&str`, `String`, `ObjectPath`); `path!` stays as an opt-in compile-time check |
| 4 | `rootfile::create(path, WriteOptions::default(), …)` on every call | No boilerplate for defaults (`File::create` vs `OpenOptions`) | `create`/`update` without options; `create_with`/`update_with`; `File::open_with` |
| 5 | The scoped closure had to return `rootfile::Result`, so `?` on the application's own errors did not compile inside it | Composability; `std::thread::scope` is generic in its result | Generic `Result<T, E>` with `E: From<rootfile::Error>` |
| 6 | `ErrorKind` carried payloads, so matching needed `matches!(…(_))` and every new detail was a semver break | Follow `io::ErrorKind`; stable API | Fieldless `Copy` `ErrorKind`, details behind accessors and `Display`, `Error` one pointer wide |
| 7 | The `NTuple` name did not match what physicists search for (`RNTuple`) | Discoverability; STYLE §2.2 | `RNTuple`, `RNTupleWriter`, `file.rntuple(…)` |
| 8 | Rayon's global pool was used implicitly | Explicit over implicit; no hidden global state | Work runs in the caller's current pool; `ReadOptions::parallel(false)` |
| 9 | Two write APIs with no guidance on choosing | Learnability | A one-line rule: scoped unless the writer must outlive a call (§4.10) |
| 10 | Five ways to read data were presented as unrelated APIs | Progressive disclosure | The level ladder in §4.10, plus `rootfile::prelude` |
| 11 | `mkdir` behaviour on an existing directory was unspecified | Least surprise | `create_dir_all` semantics; `put` adds a cycle, `put_replace` replaces |
| 12 | Async writing was undefined | Explicit contracts | Writing is blocking by design; documented, with `spawn_blocking` |
| 13 | `Cycle` was `NonZeroU16`, which admits values ROOT cannot store | Make invalid states unrepresentable | Constructed only for `1..=32767` |
| 14 | Whether `File` is generic over its source was not stated | Friendly types | Not generic: the source is type-erased once at the I/O boundary |
| 15 | The default features needed a C compiler, with no alternative | Pay for what you use | `pure-rust` feature set documented |

Checked and kept as they were:

- **Separate `File` and `AsyncFile`**, as in the `parquet` crate. A
  `maybe_async` macro or a generic executor would make every signature
  harder to read; the two mirror each other name for name instead.
- **Lenient reading by default.** It matches ROOT, and it is explicit: the
  value is named and every irregularity is reported.
- **Two derives (`RootObject`, `Row`).** They answer different questions: how
  an object is *streamed*, and how a row maps to *columns*. A struct can
  derive both.
- **Consuming `close(self)` without I/O in `Drop`.** This departs from
  `BufWriter`, which flushes in `Drop` and swallows errors, deliberately: a
  silently truncated physics file is worse than a loud, recoverable one.


---

## Appendix C. Performance and complexity contract

Promises the library makes, and tests, about cost. Like regex's linear-time
guarantee, they hold for **any input**, including hostile files. Notation:

| Symbol | Meaning |
|---|---|
| *K* | number of keys in a directory |
| *D* | directory depth |
| *B* | baskets or pages overlapping the requested range, for the requested columns |
| *n* | bytes of input decoded |

| Operation | Cost | Remote round trips |
|---|---|---|
| `File::open` | O(*K*) bytes for the root key list; no object, tree or class description is read | **1**, plus 1 if the root key list does not fit in the first 4 KiB |
| `file.get(path)` | O(*D*) directory reads, then the object's record; class descriptions once per file | at most *D* + 2 |
| `tree.branch(name)?.read(range)` | reads exactly the *B* baskets that overlap `range`; never another column's bytes | **1 per cluster** (vector read), or 1 per basket on servers without multi-range support |
| decoding anything | O(*n*) time; nesting depth, collection lengths and allocation bounded by `ReadLimits` | — |
| reading a column | peak memory O(output + one decompressed cluster per worker) | — |
| writing | O(data); peak memory O(one basket per branch, or one page per column, plus one cluster when flushing) | — |
| `File` clone, `Tree` clone | O(1), no I/O | 0 |

- **No super-linear behaviour on any input.** Back-reference maps are hash
  maps keyed by offset. Every recursion carries a depth counter. A cycle of
  references in a hostile file ends at the depth limit, not in a stack
  overflow. A fuzz target checks time per input byte.
- **The ranges are tested.** The data-driven suite (§9) asserts the number
  of reads and bytes per case on an instrumented source, so a change that
  reads more fails CI.

**Performance guide** (in the crate docs, after regex's):

1. **Open once, share.** `File` is `Send + Sync`; clone the handle across
   threads instead of reopening.
2. **Read columns, not entries.** Prefer `branch().read(range)` or
   `record_batches` over per-entry access. A loop calling `get` per entry
   re-decodes baskets.
3. **Read by cluster.** Ranges aligned to `tree.clusters()` never decompress
   a basket twice.
4. **Ask only for the columns you use.** Each extra column costs its own
   baskets; the others are never touched.
5. **Remote:** wrap the source in `Coalesce`, and use the async API to
   overlap requests.
6. **Writing:** use one `put` per object, and let the scope finish writers.
   Increase the basket or page size for very wide or very narrow rows.
