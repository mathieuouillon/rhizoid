# `rhizoid-io` — architecture

The first member of **rhizoid**, a Rust ecosystem for CERN ROOT.

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
> - a small stable `rhizoid-source` crate for people writing sources;
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
>
> **Revision 6 (2026-11-26).** Engineering review (gstack `plan-eng-review`,
> record in [`eng-review-2026-11-26/ENG-REVIEW.md`](eng-review-2026-11-26/ENG-REVIEW.md)),
> decisions D1–D16:
> - six crates with private modules (§3);
> - one `File<I = Sync>` for sync and async (§4.10);
> - remote retries and response validation, and coalescing bounded to 1.25×
>   over-read (supersedes the 1 MiB default above) (§4.3);
> - one class manifest (§4.6);
> - a release process (§3);
> - a single-commit-point update protocol with deferred frees and free-list
>   validation, replacing an unsound first draft (§7.2);
> - all-or-nothing update errors (§4.10);
> - `WriteAt::sync` (§4.3);
> - `KeyHeader` cycles that allow 0 (§4.1);
> - recover-by-default for unclosed files (§6);
> - a crash harness that reorders and tears writes (§9);
> - the roadmap re-cut to M0–M9 with a 1.0 gate (§12).
>
> **Revision 7 (2026-11-26).** CEO review in HOLD SCOPE mode (record in
> [`ceo-review-2026-11-26/CEO-REVIEW.md`](ceo-review-2026-11-26/CEO-REVIEW.md)).
> The scope is unchanged:
> - Goal 1 is restated as measured coverage against ROOT dumps over real
>   experiment files (CEO-D4);
> - write-compatibility target ROOT ≥ 6.26 / RNTuple ≥ 6.34 (C1);
> - external basket files refused by default (C2);
> - pure-Rust LZMA decoding (C3);
> - writer locking with a fallback (C4, CEO-D6; MSRV 1.89);
> - an explicit `HttpPool`, no global state (C5, CEO-D7);
> - `ReadStats`/`WriteStats` and a troubleshooting guide (C6);
> - 0.x releases per milestone (C7);
> - a commit protocol of two ordered writes (header, then root directory),
>   replacing the single write that could tear (CEO-D1);
> - the roadmap reordered: flat TTree writer before update, RNTuple read
>   before async (CEO-D2);
> - per-PR and nightly CI tiers (CEO-D3);
> - RNTuple extension in update is a 1.0 non-goal (CEO-D5);
> - every capability reachable through `'static` handles and dynamic types
>   (CEO-D8);
> - effort re-baselined.

> **Name: rhizoid** (chosen 2026-11-26). In botany, a rhizoid is the
> root-like filament of fungi and mosses; Rust is a fungus, so the name reads
> as "Rust's root" without claiming to be CERN's ROOT. **rhizoid** names a
> whole Rust ecosystem for ROOT, and **`rhizoid-io`**, the library this
> document designs, is its first member.
>
> | Crate | Role | Status |
> |---|---|---|
> | `rhizoid-io` | read and write ROOT files (this document) | designed |
> | `rhizoid-source`, `rhizoid-derive`, `rhizoid-arrow`, `rhizoid-object-store` | its companion crates (§3) | designed |
> | `rhizoid-cli` | the `rhizoid` command (`rhizoid ls`, `rhizoid check`, …) | designed |
> | `rhizoid` | umbrella crate re-exporting the family behind features (`rhizoid::io`, …), the way `bevy` re-exports its crates | reserved |
> | `rhizoid-hist`, `rhizoid-fit`, `rhizoid-plot`, `rhizoid-py` | examples of later members: histogramming, fitting, plotting, Python bindings | ideas |
>
> All these names were free on crates.io on 2026-11-26, and `rhizoid` was free
> on PyPI. Names can be taken at any moment until they are published.
>
> Prior art:
> - [`oxyroot`](https://crates.io/crates/oxyroot) (Rust)
> - [go-hep/groot](https://github.com/go-hep/hep/tree/main/groot) (Go, read
>   and write; the closest architectural relative)
> - [uproot](https://github.com/scikit-hep/uproot5) (Python, the reference
>   for reading without ROOT)

---

## 1. Goals and non-goals

**Goals**

1. **Read real ROOT files, with a measured coverage target** (CEO-D4). Files
   come local or remote, with no ROOT installed and no generated code
   (decoding is driven by the file's own class descriptions).
   - **Corpus:** CMS NanoAOD and MiniAOD and ATLAS PHYSLITE from CERN Open
     Data, plus scikit-hep-testdata and files written by ROOT 5.34 to the
     latest version.
   - **Measure:** the share of objects and branches decoded identically to
     **ROOT's own dumps**, reported for every release.
   - **1.0 target:** 100 % of classes with complete class descriptions, and
     a named, published list of what is not covered. Classes with
     hand-written streamers have no complete on-file description, so they
     are covered by built-in codecs, class by class.
2. Write files that ROOT and uproot open without warnings: objects,
   directories, RNTuple, `TTree` (§4.8), update in place.
   - **Compatibility target (C1):** ROOT **≥ 6.26** reads our TTrees and
     objects, and ROOT **≥ 6.34** reads our RNTuples.
   - The header's claimed version and the class versions written are chosen
     to match.
   - Nightly CI reads our output with ROOT 6.26, 6.30, 6.34 and the latest.
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
| **Lazy by default** | Opening reads the header, the root directory and its key list. Class descriptions, objects, baskets and pages load on demand. ROOT reads class descriptions eagerly [RB§1]; `rhizoid check` does too. |
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
| **Every capability reachable without generics or lifetimes** (CEO-D8) | Each capability is reachable through `'static` handles (`File`, `WritableFile`, `Owned*Writer`) plus dynamic types (`DynObject`, `DynRow`, `Value`). That keeps later Python or C bindings cheap. CI has one owned-API example per capability |
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
| serde | Each trait and its derive share a name (`Serialize` / `#[derive(Serialize)]`), and the derive is a feature of the main crate | `RootObject` and `Row` are each both a trait and a derive, re-exported by `rhizoid-io` behind the default `derive` feature (§4.4) |
| serde | Macros expand to paths in a `#[doc(hidden)] pub mod __private`, and `#[serde(crate = "…")]` lets re-exporting crates redirect them | `rhizoid_io::__private`, and `#[root(crate = "…")]` for frameworks that re-export us (STYLE §15) |
| serde | Interoperates through the de facto standard instead of inventing a new one | Feature `serde`: a `DynObject` is a `serde::Deserializer`, so `rhizoid_io::serde::from_dyn::<MyConfig>(&obj)` reads any class into a `#[derive(Deserialize)]` struct, and `Value: Serialize` gives JSON dumps (§4.4) |
| serde | 1.0 kept for years: additions only, never a break | The facade aims for a long-lived 1.x. Every public enum is `#[non_exhaustive]`, trait methods have defaults, and `cargo-semver-checks` runs in CI |
| **regex** (BurntSushi) | A simple facade (`regex`), an expert crate (`regex-automata`) and a parser (`regex-syntax`), each with its own semver | Six crates (§3, revision 6): the layers are private modules; an expert crate (`rhizoid-raw`) is split out of `core` after 1.0, once its API settles, as `regex-automata` was |
| regex | **Documented complexity guarantees** (linear-time matching) and a performance guide | Appendix C: round trips per open, bytes read per query, memory bounds, and no super-linear behaviour on hostile input |
| regex | One data-driven test suite (TOML) run against every engine | `tests/cases/*.toml`, run against **every source** (memory, file, mmap, HTTP mock, async) and every container (§9) |
| regex | Continuous fuzzing in OSS-Fuzz | The fuzz targets are enrolled in OSS-Fuzz once public (§9) |
| regex | `regex-lite`: a smaller build for people who need less | `default-features = false, features = ["pure-rust"]` (§4.10) |
| regex | Crate docs that teach: overview, usage, examples, features, performance, untrusted input, MSRV | The standard crate-doc outline (STYLE §7) |
| **thiserror / anyhow** | Typed errors for libraries, contextual errors for applications | One typed `Error` with a `kind()` and context (§4.1); `anyhow` only in the CLI; user closures may return their own error type (§4.10) |
| **rayon** | Parallelism through iterator traits, running in the caller's pool | `Clusters: IntoParallelIterator`, `tree.par_rows::<T>()` under the `rayon` feature; work runs in the current pool (§4.7, §4.10) |
| rayon | The same code works with parallelism compiled out | Without the feature, `par_*` are absent and everything runs sequentially; the results are identical (tested) |
| **axum** (tokio) | Typed extraction from plain functions and tuples, with no macro needed | `tree.rows_of::<(f32, Vec<f32>)>(["MET_pt", "Muon_pt"])`: tuples of column types implement `Row`, so the derive is optional (§4.8, A.20) |
| axum, serde | `axum-core` exists so library authors implement its traits without depending on `axum`. serde has split the same way: `serde` is now a facade over `serde_core`, which holds the traits | **`rhizoid-source`**: `ReadAt`, `AsyncReadAt`, `WriteAt` and `FileOffset` only. It aims to stay 1.x, and its only dependencies are `bytes` and `futures-core` (§3, §4.3) |
| **parquet** (arrow-rs) | The closest large relative. One reader builder generic over sync or async input, projection and row-selection types, metadata loaded once and shared, dyn-compatible async I/O, and a push decoder (sans-IO) under the async stream | §4.12: `TreeReaderBuilder<I>`, `Projection`, `EntrySelection`, `TreeMetadata`, `next_cluster()`; §4.3 |
| regex-automata | Immutable compiled state behind an `Arc`, mutable per-search `Cache` taken from a pool; `*_with(&mut Cache)` variants skip the pool | §4.12: `Scratch` buffers in a pool, plus `read_with(&mut Scratch)` |
| axum | Good compiler errors are a feature (`#[debug_handler]`) | `#[diagnostic::on_unimplemented]` on every user-facing trait, and `trybuild` snapshots of the messages (§5.5, §5.6) |
| **kurbo** (linebender) | Concrete `f64` types over numeric genericity, for clarity and compile times | Histograms are concrete per ROOT class (`TH1D`, `TH1F`), not `TH1<T>`. `File` is not generic. Generics only where they pay: columns and rows |
| kurbo | A focused scope with explicit non-goals | §1 non-goals; plotting, fitting and formulas stay out |
| **std** | Generic outer functions delegate to a non-generic inner one (`fs::read` → `inner(&Path)`) | Required for every generic public function with a non-trivial body, to limit monomorphization bloat (STYLE §9) |
| std | `Debug` on everything; conversions through `From`/`TryFrom`/`FromStr`; `std`'s naming | STYLE §2–3 |

---

## 3. Workspace layout

**Six crates (revision 6, decision D1).** The layers are private modules of
one `rhizoid-io` crate, so importing internals fails to build, and there is one
version to release. An expert crate is split out only once its API settles,
as regex did with `regex-automata`.

```
rhizoid/
├─ Cargo.toml                 workspace: shared deps, lints, profiles
├─ ARCHITECTURE.md  ROOT-BEHAVIOUR.md  STYLE.md
├─ rustfmt.toml  clippy.toml  deny.toml  release-plz.toml
├─ crates/
│  ├─ rhizoid-source/        STABLE, tiny: ReadAt, AsyncReadAt, WriteAt, FileOffset
│  ├─ rhizoid-io/             the I/O library: public API + private modules
│  │  └─ src/
│  │     ├─ core/             sans-IO wire format: buffers, header, keys,
│  │     │                    directories, allocator, newtypes, errors
│  │     ├─ compress/         9-byte block framing + codecs
│  │     ├─ io/               source implementations, range coalescing, caches
│  │     ├─ streamer/         TStreamerInfo, dynamic decoder, Marshal/Unmarshal,
│  │     │                    class registry, hand-written streamers
│  │     ├─ objects/          typed TH*, TGraph*, TF1, TAxis… (generated)
│  │     ├─ columnar/         ReadPlan, Column, Jagged, NativeType
│  │     ├─ tree/             TTree read/write
│  │     ├─ ntuple/           RNTuple read/write, TFile and bare containers
│  │     └─ lib.rs            the facade: the only public items
│  ├─ rhizoid-derive/        #[derive(RootObject)], #[derive(Row)] (proc-macro)
│  ├─ rhizoid-arrow/         adapter: Column ↔ arrow, versioned with arrow
│  ├─ rhizoid-object-store/  adapter: object_store source, versioned with it
│  └─ rhizoid-cli/           `rhizoid ls | dump | streamers | check | cp`
├─ examples/                  runnable programs, one per Appendix A recipe; built in CI
├─ xtask/                     class manifest codegen, corpus download, cross-check runner
│  └─ classes.toml            the one list of built-in classes (§4.6)
├─ fuzz/                      cargo-fuzz targets, one per decoder (OSS-Fuzz)
└─ tests/
   ├─ cases/                  data-driven test cases (TOML), run against every source
   ├─ corpus/                 reference files (fetched, not committed)
   ├─ cross/                  ROOT macros + uproot scripts for cross-validation
   ├─ crash/                  update crash-consistency harness (§9)
   └─ ui/                     trybuild compile-fail tests (§5.6)
```

### Module graph (arrows point at dependencies)

```
   rhizoid-cli ──► rhizoid-io (lib.rs facade) ◄── rhizoid-arrow, rhizoid-object-store
                        │ (private modules below)
      ┌─────────┬───────┴─────┬───────────┬──────────┐
      ▼         ▼             ▼           ▼          ▼
     tree     ntuple       objects       io     rhizoid-derive (separate proc-macro crate)
      │ │       │ │           │           │
      │ └──┬────┘ │           │           │
      │    ▼      │           │           │
      │ columnar  │           │           │
      ▼           ▼           ▼           │
     streamer ◄───────────────┘           │
         │                                │
         ▼                                │
     compress                             │
         │                                │
         ▼                                ▼
        core ─────────────► rhizoid-source ◄── (io depends on nothing ROOT-specific)
```

Rules:

- **Enforced in CI.** A `tests/architecture.rs` check parses
  `cargo modules dependencies --lib` output and fails on any edge not in this
  graph. The crate graph used to do this; with private modules, CI does it.
- `rhizoid-source` holds only traits and one newtype, so it can stay at
  1.x forever.
- `core` does no I/O, and `io` knows nothing about ROOT.
- `tree` and `ntuple` share code only through `columnar`.
- `ntuple` does not depend on the facade code in `lib.rs`, so bare RNTuple
  files, which have no TFile [RB§9], are first-class.

### Publishing, semver, releases

| Crate | Promise |
|---|---|
| `rhizoid-io`, `rhizoid-derive` | semver, released together; the derive is pinned `=x.y.z` |
| `rhizoid-source` | semver; aims never to leave 1.x, like `futures-core` |
| `rhizoid-arrow`, `rhizoid-object-store` | semver, each with its own version: a breaking release (a 0.x minor, later a major) with each `arrow` / `object_store` major, as their types are in the API; 0.1.0 at rhizoid-io 1.0 |
| `rhizoid-cli` | a binary, no API promise; the installed command is `rhizoid` |
| `rhizoid` | the ecosystem's umbrella crate: re-exports family members behind features (`rhizoid::io` = `rhizoid-io`). Reserved now, published with 0.1 as a thin re-export |
| an expert crate (`rhizoid-raw`), after 1.0 | split out of `core` once its API settles, with its own semver |

- **`release-plz`** (decision D6) writes changelogs from conventional commits
  and bumps versions. It publishes in dependency order: `rhizoid-source` →
  `rhizoid-derive` → `rhizoid-io` → adapters → `rhizoid-cli`.
- **docs.rs** builds with `[package.metadata.docs.rs] all-features = true`
  and `rustdoc-args = ["--cfg", "docsrs"]`.
- **Fast-moving ecosystems** stay out of `rhizoid-io`'s public API. `arrow`,
  `object_store` and `reqwest` types live only in the adapters (or behind the
  `http` feature's own types). `bytes::Bytes` is the one external type in the
  core API: it has been stable at 1.x since 2020 and is the common currency
  of Rust I/O.

---

## 4. Layers

### 4.1 `core` — the wire format, no I/O

**Newtypes.** One concept, one type. The constructors validate, and the
fields are private.

| Type | Wraps | Notes |
|---|---|---|
| `FileOffset` | `u64` | Absolute position in the file. Whether it is written in 4 or 8 bytes is decided by the encoder (below) |
| `ByteLen` | `u32` | A length read from the file. Built only through checks against the remaining buffer and the limits |
| `KeyRelOffset` | `u32` | A position counted from the **start of a key**, as used by class tags, back-references and basket entry offsets [RB§6, RB§7] |
| `ClassVersion` | `i16` | Class versions everywhere, with the member-wise bit `0x4000` split off into a separate flag on decode |
| `Cycle` | `NonZeroU16`, constructed only for `1..=32767` | The `;n` cycle number **of user-facing names** (`"h;2"`). ROOT's negative "keep" cycles are normalized to their absolute value [RB§4]. `KeyHeader` keeps the raw on-disk `i16`, because basket keys carry cycle **0** [RB§8.6] (review finding C1) |
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
- neighbouring gaps merged when space is freed;
- **frees are deferred to the next commit** (§7): space released in the
  current session is never reused before the header that stops referencing
  it is durable;
- **the free-list record's own size is iterated to a fixed point** (at most 2
  passes), since allocating the record can change the number of segments.

**Free lists are not trusted.** Before reusing any gap of an existing file
(update mode), every free segment below `fEND` is checked against the
record walker: it must start with the matching negative gap marker and
overlap no walked record. On any mismatch (files from foreign writers,
recovered files, a stale list) the allocator falls back to **append-only**
for that session and reports a `Diagnostic`.

Its invariants (no overlap, no leak, gap markers present, no reuse of
same-session frees) are property-tested in isolation.

**Record walker.** `walk(&[u8]) -> impl Iterator<Item = Result<RecordOrGap>>`
steps from `fBEGIN` by `Nbytes`, skipping negative gap markers, like ROOT's
recovery [§3]. It is used by `rhizoid check`, by recovery of unclosed files,
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

### 4.2 `compress`

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
| `lzma` (default) | `lzma-rs`: pure-Rust **decoder** (C3) | yes |
| `lzma-write` | `liblzma` (bundled C xz), for **writing** LZMA only; opt-in | no |

### 4.3 `rhizoid-source` and `io` — moving bytes

The traits live in **`rhizoid-source`**, a stable crate kept deliberately
tiny:
- the three traits below and `FileOffset`;
- `bytes` as its only dependency;
- implementations for `std` types.

Anyone writing a source (a storage system, a test double, a custom cache)
depends on it alone, as `tower` services depend on `axum-core` rather than
`axum`. The source implementations and adapters live in `io`.

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
    /// Durability barrier: every earlier write is on stable storage when this
    /// returns (`File::sync_data`). The default returns `ErrorKind::Unsupported`.
    /// Update mode refuses sinks that do not support it, because its commit
    /// protocol (§7) depends on it.
    fn sync(&mut self) -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) }
}
```

| Source | Where | Vector read |
|---|---|---|
| `std::fs::File` | core | one `read_at` per range (the OS page cache does the rest) |
| `memmap2::Mmap` | feature `mmap` | zero-copy slices |
| `Bytes` / `Vec<u8>` | core | zero-copy slices |
| `HttpSource` (`reqwest`) | feature `http` | multipart `Range: bytes=a-b,c-d`; falls back to parallel single ranges when the server answers `200` or with a single part [§12]. **Robustness (D6):** 30 s timeout, 3 retries with exponential backoff on 5xx, 429 and connection resets, and every response's `Content-Range` and length are checked against the request, so a truncated body is never decoded |
| `ObjectStoreSource` | `rhizoid-object-store` | `get_ranges` (S3, GCS, Azure, HTTP); retries through object_store's own `RetryConfig`; response lengths are checked the same way |
| browser `fetch` | feature `wasm` | implements `LocalAsyncReadAt` |
| XRootD | — | through the server's HTTP(S) interface today. A native client is a later `AsyncReadAt`, with no other changes |

Adapters, all composable:

- `Coalesce<S>`, **bounded** (decision D8). ROOT interleaves the baskets of
  different branches, so object_store's Parquet-tuned 1 MiB gap could
  download most of a file to read two branches:
  - two ranges are merged only when the gap is at most **64 KiB** *and* the
    merged request keeps the total fetched at most **1.25×** the bytes
    actually needed;
  - up to **10** requests run in parallel (object_store's number);
  - results come back in the caller's order: sort by start, extend the
    current run while both bounds hold, then split the responses back with
    `partition_point`;
  - both bounds are configurable per source.
- `ReadCache<S>`: caches the first 4 KiB and recently read metadata.
- `BlockingAsync<S>`: presents an async source as `ReadAt` for sync callers.
- Blanket impls: any `AsyncRead + AsyncSeek` type is an `AsyncReadAt` (as
  parquet does for `tokio::fs::File`), and `Box<dyn AsyncReadAt>` is one.

### 4.4 `streamer` — objects as bytes and back

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
let cfg: Config = rhizoid_io::serde::from_dyn(&file.get_dyn("config/MyConfig")?)?;
```

`DynObject` and `Value` also implement `serde::Serialize`, which
`rhizoid dump --json` uses. The bridge is read-only by design: writing still
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

> **Licence.** Porting `TClass::GetCheckSum` and embedding class
> descriptions captured from ROOT-written files, against ROOT's LGPL-2.1:
> decided by the owner on 2026-11-27, MIT (§13 Q2).

### 4.5 `rhizoid-derive`

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

### 4.6 `objects`

Typed models for the classes everyone uses:

- `TAxis`
- `TH1*`, `TH2*`, `TH3*`, `TProfile*`
- `TGraph*`, `TEfficiency`
- `TF1` (data only)

**One class manifest (decision D6).** `xtask/classes.toml` is the single list
of built-in classes: name, versions to embed, and whether the class needs a
hand-written codec. From it, `cargo xtask gen-classes` generates:
- the model structs;
- the `ClassRegistry` table;
- the embedded class descriptions captured from recent ROOT.

The generated files are committed. CI regenerates them and fails on any
difference, so the three can never drift. Older versions are read through the
evolution path.

**The models are data, as uproot's (decision 2026-11-26, at M6):** they
hold what the file stores (cells with under- and overflow, `fSumw2`, the
stored statistics, axes, points, parameters) and give it back; they do not
fill, rebin or compute errors and means. A histogram library (boost-histogram
and the like) converts from and to them in an adapter crate, through the
`Histogram` trait. See §12.7.

### 4.7 `columnar`

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

Arrow conversion lives in `rhizoid-arrow`, over `Column`.

### 4.8 `tree`

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

**Split objects (`TBranchElement`): reading in M2, writing in M10.** The layout is now verified
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
- **New files:** rewrite the tree metadata in place while the reserved
  capacity lasts, and otherwise rewrite the tree and free the old copy, as
  uproot does. The file is not committed yet, so this is safe.
- **Update mode (decision D14):** no in-place rewrite. Appended baskets go to
  new space, and the tree object is written as a **new cycle**, as ROOT's
  AutoSave does [RB§8.7]. The old cycle is freed at the next commit.
- Flush every branch at cluster boundaries and record them in
  `fClusterRangeEnd`/`fClusterSize` (inclusive ends) or in `fAutoFlush`.

The writer stays behind `unstable-tree-writer` until it passes the
cross-check. After that it is stable. RNTuple remains the recommended format
for new data. (It passed in M7 and shipped without the feature, §12.8.)

### 4.9 `ntuple`

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

### 4.10 `rhizoid-io` — the facade

**Five levels, one ladder.** A user climbs only as far as the task needs.
Each level is a thin layer over the one below, and every level shares the
same `File`:

| Level | For | Entry point |
|---|---|---|
| 1. Objects | histograms, graphs, user classes | `file.get::<T>(path)` |
| 2. Columns | one branch or field at a time | `tree.branch::<T>(name)?.read(range)` |
| 3. Rows | typed loops over entries | `tree.rows::<T>()` with `#[derive(Row)]`, or `tree.rows_of::<(A, B)>([…])` with no derive; `par_rows` with rayon |
| 4. Arrow | handing data to other tools | `tree.record_batches(..)` (`rhizoid-arrow`) |
| 5. Dynamic | unknown schemas, tools like `rhizoid dump` | `file.get_dyn(path)`, `Interpretation`, `DynRow` |

`use rhizoid_io::prelude::*;` imports what levels 1–3 need: the derives, the
extension traits, `File`, `Result`, `Jagged`.

**Reading.**

```rust
let file = rhizoid_io::File::open("data.root")?;                    // File: Send + Sync + 'static
let file = rhizoid_io::File::open_with("data.root", ReadOptions::default().strictness(Strictness::Strict))?;
let file = rhizoid_io::File::from_source(source, ReadOptions::default())?;
let file: AsyncFile = rhizoid_io::File::open_async(source).await?;  // AsyncFile = File<Async>

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
- **One `File` type for sync and async (decision D5),** after parquet:

  ```rust
  pub struct File<I = Sync> { inner: Arc<FileInner>, input: I }   // Sync(Arc<dyn ReadAt>) or Async(Arc<dyn AsyncReadAt>)
  pub type AsyncFile = File<Async>;
  impl<I> File<I>    { pub fn keys(&self) -> Keys<'_>; pub fn header(&self) -> &FileHeader; … }  // no I/O: written once
  impl File<Sync>    { pub fn open(…) -> Result<Self>;  pub fn get<T>(…) -> Result<T>; … }
  impl File<Async>   { pub async fn get<T>(…) -> Result<T>; … }
  impl File<Sync>    { pub async fn open_async(src) -> Result<AsyncFile>; … }  // see below
  ```

  - **The source type never appears.** It is erased once, at the I/O
    boundary, where a virtual call costs nothing next to the read. Users
    write `File` (sync by default) or `AsyncFile`, never `File<MmapSource>`.
  - **Constructors have distinct names** (`open`, `open_with`,
    `from_source` / `open_async`, `from_source_async`). Two inherent `open`s
    on `File<Sync>` and `File<Async>` would make `File::open(p)` ambiguous
    (E0034), because default type parameters do not drive inference in
    expressions. All constructors live on `impl File` (= `File<Sync>`), so
    `File::open_async` resolves.
  - **Methods without I/O** (`keys`, `header`, metadata accessors) are
    written once on `impl<I> File<I>`.
  - **Methods with I/O** have a sync and an async signature. Both are
    generated by one internal `maybe_async!` macro from one body and one doc
    comment, so they cannot drift.

**Writing: a scoped API by default.**

```rust
rhizoid_io::create("out.root", |f| {
    let mut events = f.rntuple::<Event>("Events")?;  // RNTupleWriter<'s, Event>
    let mut runs   = f.rntuple::<Run>("Runs")?;      // several writers at once
    let calib      = f.mkdir("calib")?;              // DirWriter<'s>
    for ev in source {
        events.fill(&ev)?;                           // type-checked: only Event fits
        if ev.end_of_run() { runs.push(&ev.run())?; calib.put("gain", &gain)?; }
    }
    f.put("h_pt", &h_pt)?;
    Ok(())
})?;  // the scope finishes every child, then writes key lists, StreamerInfo, free list, header
```

```rust
pub fn create<T, E>(path: impl AsRef<Path>,
                    body: impl for<'s> FnOnce(&'s FileScope<'s>) -> Result<T, E>) -> Result<T, E>
where E: From<rhizoid_io::Error>;

pub fn create_with<T, E>(path: impl AsRef<Path>, opts: WriteOptions,
                         body: impl for<'s> FnOnce(&'s FileScope<'s>) -> Result<T, E>) -> Result<T, E>
where E: From<rhizoid_io::Error>;

pub fn update<T, E>(…) -> Result<T, E>;         // same shape; and update_with
```

- **The error type is the caller's.** The closure may return `anyhow::Error`
  or the application's own error, as long as it implements
  `From<rhizoid_io::Error>`, so `?` works on both kinds of error inside the
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
- **Errors and panics in `create` (decision D7).** If the closure returns
  `Err`, the scope still closes the file, so that what was written is
  readable. It then returns **the closure's error**. If closing fails too,
  that failure is attached to it as context and `source()`, so the root
  cause stays on top. If the closure panics, nothing is written after the
  panic: the header still says "not closed", and ROOT's recovery or
  `rhizoid check --recover` can salvage it.
- **Errors and panics in `update` (decision D13): all or nothing.** On
  `Err`, panic or drop, **nothing is committed**. The header and directory
  records are untouched, so the existing file is exactly as before; only
  unreferenced bytes past the old end of the file remain. Update never
  writes a "not closed" marker into an existing file. The commit protocol is
  in §7.
- **Directories.** `mkdir("a/b")` behaves like `std::fs::create_dir_all`: it
  creates missing levels and returns the existing directory if there is one.
  `put` under an existing name adds a cycle, as ROOT does; `put_replace`
  replaces it. `delete` on a directory key returns
  `Err(ErrorKind::IsDirectory)`: ROOT frees nothing when it deletes a
  directory [RB§4], and recursive deletion is not in 1.0.
- **Writing is blocking.** Writes are local and sequential, so the write API
  is synchronous. From async code, wrap it in `spawn_blocking`.

**Which writing API?** Use `rhizoid_io::create` unless the writer must outlive
a function call, for example stored in a struct or handed to a framework.
Then use `WritableFile`.

**Writing: an owned handle, for writers stored in long-lived structs.**

```rust
let mut out = rhizoid_io::WritableFile::create("out.root", WriteOptions::default())?;
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
| `rhizoid_io::create[_with]` / `rhizoid_io::update[_with]` | scoped |

There is no `update(bool)` option.

**`WriteOptions`:**

| Option | Default |
|---|---|
| `compression` | `Compression::zlib::<1>()`, ROOT's default |
| `reproducible` | `false`. When `true`, uses ROOT's `kReproducible` values (datime 1, zero UUIDs) [RB§2], so outputs compare byte for byte |
| `claimed_root_version` | matches the compatibility target: ROOT ≥ 6.26 (C1) |
| `lock` | `true`. `create` and `update` take an exclusive advisory lock (`File::try_lock`) and fail fast with `ErrorKind::Locked` if another rhizoid-io process holds it (C4). Where the filesystem cannot lock (EOS FUSE, NFS, AFS), they proceed and report a `Diagnostic` (CEO-D6). The lock excludes other rhizoid-io writers only, **never ROOT**; and a concurrent writer can crash `mmap` readers (documented) |

**`ReadOptions` additions.**

| Option | Default |
|---|---|
| `http_pool` | none. An `HttpPool` value (one HTTP client + a cap on in-flight requests, default 64) can be created once and passed to many files with `clone()`. With no pool, each `File` gets its own, shared only by handles derived from it. **No process-global state** (C5, CEO-D7) |
| `external_files` | `None`: branches whose baskets live in another file (`TBranch::fFileName`, [RB§8.4]) fail with `ErrorKind::ExternalFile`, naming the file (C2). `Some(base_dir)` allows relative names resolved inside `base_dir` only; absolute paths, `..` escapes and URLs are always refused |
| `parallel` | `true` (the caller's rayon pool) |

**Statistics and troubleshooting (C6).**
- `file.stats()` returns a `ReadStats` snapshot: bytes needed vs fetched,
  requests, retries, cache hits, bytes decompressed and decompression time
  per codec. Writers return `WriteStats`. The counters are atomics with
  negligible cost, and Appendix C's contract is asserted on them.
- `tracing` spans follow a documented convention: `rhizoid.open`,
  `rhizoid.read{object, branch, cluster}`, `rhizoid.fetch{ranges, bytes}`,
  `rhizoid.commit{phase}`.
- A **troubleshooting guide** (`rhizoid_io::_guide::troubleshooting`) maps each
  `ErrorKind` and `Diagnostic` to its causes and fixes.

**Parallelism is the caller's.** Decompression and decoding run in the
*current* rayon pool, so `pool.install(|| tree.branch(..)?.read(..))` bounds
it. `ReadOptions::parallel(false)` keeps everything on the calling thread.
The library never creates threads or a global pool of its own.

**Features:**

- `default = ["zlib", "lz4", "zstd", "lzma", "objects", "tree", "ntuple", "rayon", "derive"]`.
  The defaults open any file.
- `default-features = false, features = ["pure-rust"]` gives a build with no
  C compiler, for WASM. It reads every codec (zstd through `ruzstd`, LZMA
  through `lzma-rs`) but cannot *write* zstd or LZMA.
- A benchmark tracks `lzma-rs` against `liblzma` decoding speed (nightly
  alert). LZMA files are rare, and safety comes first (C3).
- opt-in: `mmap`, `http`, `async`, `wasm`, `serde` (`unstable-tree-writer`
  was not needed: the flat writer passed its cross-check in M7).

### 4.11 `rhizoid-cli`

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
  https://docs.rs/rhizoid-io/…/columns")]` with a link, as axum's extractor
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
      extern crate rhizoid_io as _rhizoid;         // or `use <path> as _rhizoid;` with #[root(crate = "…")]
      #[automatically_derived]
      impl _rhizoid::RootObject for MyEvent { … }
  };
  ```

  Generated code names `Result`, `Ok`, `Vec`, `Option` through
  `_rhizoid::__private`, a `#[doc(hidden)]` module that re-exports `core`
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
| Filling a tree or ntuple with the wrong row type | `RNTupleWriter<'s, E>::fill(&E)` | compile |
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
| `ReadAt`, `LocalAsyncReadAt`/`AsyncReadAt`, `WriteAt` | open, in the stable `rhizoid-source` | Users bring their own sources without depending on the whole library |
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
- MSRV is **1.89**, pinned in CI: `File::try_lock` for writer locking
  (C4). Earlier needs: `const { }` blocks need Rust ≥ 1.79 and
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

**Empty cases (CEO review 1-1):** a directory with 0 keys gives an empty
iterator; a tree with 0 entries gives a reader that yields no batch; a branch
with 0 baskets gives an empty `Column`; a zero-length jagged entry gives an
empty slice. All are covered by `tests/cases/empty.toml`.

**Unclosed and truncated files (decision D16).** If there is no key list, or
the file was never closed or is truncated, `File::open` does what ROOT does
[RB§1]: it rebuilds the key list with the record walker and reports a
`Diagnostic`. `Strictness::Strict` makes this an error instead. Files from
crashed batch jobs are common, so recovering them is part of Goal 1.

## 7. The write path

### 7.1 New files (`create`)

The steps, in the order ROOT uses [RB§2, RB§3]:

```
create  header placeholder; TFile key at 100 (name = file name, TNamed + directory record
        + UUID + 12 bytes padding); free list = [100, 2e9]
put     serialize (WBuffer::object) → compression policy → Allocator (best fit, gap marker)
        → write key + data → record the key in DirectoryState (cycle = max + 1)
close   finish children (scope) → StreamerInfo list → for each directory: key list,
        directory record → free list → header (large form if fEND > kStartBigFile) → sync
```

A new file has no previous content to protect, so ordering only matters for
ROOT's recovery: until the header is written, the file reads as "not
closed".

### 7.2 Updating existing files (`update`): one commit point (decision D11)

An update either happens entirely or not at all, for the whole file. The
design is the outside-voice correction (review F1) of the first draft, which
was unsound.

```
 PREPARE (nothing referenced changes; a crash here leaves the old file intact)
   1. new objects, baskets, tree cycles      → new space (gaps validated, see below, or past old fEND)
   2. changed subdirectories                 → written COPY-ON-WRITE: a new directory record + key list
                                                at new locations, never in place
   3. new root-directory key list            → new space
   4. StreamerInfo = UNION(old list, new classes; every on-file version kept) → new space
   5. free list = old list minus what this session allocated
                 (nothing freed this session is listed yet)         → new space
   6. sync()                                 ← every byte above is durable
 COMMIT (two ordered writes; CEO-D1 replaced the single write)
   7a. write the HEADER (fEND, fSeekFree, fSeekInfo → the new free list and StreamerInfo)
   7b. sync()      ← a crash after here: old root dir + new header = OLD content, leaked space only
   8a. write the ROOT DIRECTORY RECORD (fSeekKeys → the new root key list), within one 512 B sector
   8b. sync()      ← the update is committed
 AFTER
   9. space freed by this session (old key lists, old StreamerInfo, old cycles)
      enters the free list at the NEXT commit, never earlier
```

- **One commit point, two ordered writes.** Steps 2–3 make every change
  reachable only through the root directory record. The header goes first:
  - it only enlarges `fEND` and points at a free list that frees nothing
    from this session;
  - it points at a StreamerInfo that is a superset of the old one.

  So an old root directory under a new header still reads exactly the old
  content. The root directory record's 8a write must lie within one 512 B
  sector; otherwise the update falls back to `copy_on_write`. The header
  (under 100 bytes, at offset 0) always does.
  - `UpdateOptions::copy_on_write(true)` can also be chosen for irreplaceable
    files. It copies the file, applies the update to the copy, syncs, renames
    the copy over the original (`rename` / `ReplaceFileW`), and syncs the
    parent directory on POSIX.
- **Crash analysis.**
  - **Before 7a:** everything old is untouched; the new bytes are
    unreferenced.
  - **Between 7b and 8a:** a new header with the old root directory. The
    file reads as the old version. The space this session wrote is leaked
    until the next update, never reused while live, because `fEND` covers it
    and the free list does not list it.
  - **After 8b:** the new state is complete and durable.
  - **Torn writes:** each commit write fits in one sector. The crash harness
    tears at 512 B granularity and **asserts the 7a → 7b → 8a order**. On
    filesystems without sector atomicity (FUSE, network), use
    `copy_on_write`, which the docs recommend there.
- **Errors, panics, drop (decision D13):** the scope stops before step 7a.
  Nothing is committed.
- **Sinks without `sync()`** are refused for update (`ErrorKind::Unsupported`).
- **Gaps are reused only after validation** against the record walker
  (§4.1); otherwise the session is append-only.
- **To verify first:** that ROOT reads relocated subdirectory records
  (copy-on-write directories). This is the first task of M9 (update), a prototype
  checked against ROOT 6.34+ in the cross-check job, before the rest of
  update mode is built.

### 7.3 Common rules

- **Cycles:** `put` on an existing name gets the next cycle; `put_replace`
  frees the previous one (deferred to the next commit in update mode).
- **Update** never touches records it does not understand, and copies
  `Value::Opaque` members back as they were.
- **Extending an existing TTree** writes a new tree cycle (decision D14,
  §4.8).
- **Extending an existing RNTuple is a non-goal for 1.0** (CEO-D5).
  Update can add new RNTuples and objects to a file, but it does not append
  clusters to an existing RNTuple; doing so is an error naming the TODO.
  This is tracked in `TODOS.md`.
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
   - **Two tiers (CEO-D3).**
     - **Per PR (target < 20 min):** the latest ROOT, a sampled crash
       harness, Linux, macOS and Windows.
     - **Nightly:** the full ROOT matrix 6.26 / 6.30 / 6.34 / latest (C1),
       the full crash harness, all platforms, fuzzing, and compile-time
       tracking.
     - ROOT images are cached; self-hosted runners are budgeted if hosted
       ones are too slow.
2. **Corpus and coverage (CEO-D4):**
   - [scikit-hep-testdata](https://github.com/scikit-hep/scikit-hep-testdata);
   - real experiment files from CERN Open Data (CMS NanoAOD/MiniAOD, ATLAS
     PHYSLITE);
   - files written by every ROOT version in the matrix.

   **The reference is ROOT's own dump** (`tests/cross/dump.C`), not uproot,
   which fails on some split and member-wise cases. Expected dumps are
   compared through `insta`. Coverage (% of objects and branches matching
   ROOT) is reported per release. **Hostile fixtures** include branches whose
   `fFileName` points to `/etc/passwd` or to a URL (C2).
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
5a. **Update crash consistency** (`tests/crash/`, decisions D9 and D12),
    the regression contract for update mode. The behaviour preserved: after
    any update, crash or not, ROOT and our reader see the old content or the
    new content, never a mix and never a corrupt file.
    - A recording `WriteAt` double **buffers writes between `sync()`
      calls**.
    - At every crash point it persists **every subset** of the unsynced
      writes (all subsets up to 8 pending writes, random samples beyond),
      and **tears** writes at 512 B granularity.
    - Each resulting file is reopened and checked against the old-or-new
      guarantee. It is then given a **second update**, which exercises the
      free list left behind, and checked again.
    - ROOT opens every result in the cross-check job.
5b. **Contract tests named by the review** (eng-review-2026-11-26 T1–T7):
    - a >2 GiB file in the large form;
    - basket keys with cycle 0;
    - remote faults (503, truncated body, wrong `Content-Range`, reset);
    - the ≤ 1.25× over-read bound;
    - async cancellation;
    - several writers on several threads with the D7 error order;
    - `EntrySelection` algebra against a model.
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
   times. Nightly CI records `cargo build --timings` for `rhizoid-io` with
   default features, and **alerts** on a regression above 15 %. It does not
   gate merges (CEO-D3: the timings are too noisy on shared runners).
   - Proc macros stay out of every crate except `rhizoid-derive`.
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
- The semver surface is explicit: internals are private modules, and the
  adapters are versioned with their ecosystems.
- Room is left for Python bindings (`pyo3`, Arrow through PyCapsule), a native
  XRootD client, and DataFusion/Polars providers over `rhizoid-arrow`.

## 11. Libraries

| Concern | Crate | Why |
|---|---|---|
| Byte buffers | `bytes` | Zero-copy, stable 1.x, the common currency of Rust I/O |
| Errors | `thiserror` 2 (libraries), `anyhow` (CLI) | |
| Compression | `flate2` (+`zlib-rs`), `zstd`/`ruzstd`, `lz4_flex`, `lzma-rs` (decode), `liblzma` (opt-in write) | §4.2 |
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

| Milestone | Content | Exit criterion | Release (C7) |
|---|---|---|---|
| **M0** container + harness | `core`, `compress`, `io`, record walker; `ls`, `check`; **minimal writer** (empty file); module-graph check; two-tier CI | Lists every corpus file; our empty file passes the cross-check | — |
| **M1** objects | class manifest + generated models, streamer set, dynamic decoder, built-ins, typed histograms; `put` for histograms; scoped `create` | One-histogram file passes the cross-check; ROOT and uproot read our histograms identically | **0.1** (licence review done first, TODOS.md) |
| **M2** sync TTree read | flat, fixed, jagged, strings, split objects (all `fType`s); reader builder, `Projection`, `EntrySelection`; `rhizoid-arrow`; `ReadStats` | Corpus coverage reported against ROOT dumps | **0.2** |
| **M3** sync remote | `HttpSource` (retry, validation), `HttpPool`, `ObjectStoreSource`, bounded coalescing | Every case passes against every sync source; the over-read bound holds | 0.3 |
| **M4** RNTuple read | both containers, all column types and encodings | ROOT's RNTuple test files and corpus RNTuples read | 0.4 |
| **M5** async + WASM | `AsyncFile`, streams, `next_cluster`, `LocalAsyncReadAt`, WASM build and tests | Every case passes against the async sources | 0.5 |
| **M6** derives + evolution | `#[derive(RootObject)]`, `#[derive(Row)]`, tuple rows, schema evolution | User classes round-trip between ROOT and us (objects) | 0.6 |
| **M7** TTree flat writer | the uproot-proven subset, compressed | Cross-check passes; ROOT ≥ 6.26 reads it | 0.7 |
| **M8** RNTuple write | `RNTupleWriter`, parallel page compression | Cross-check passes; ROOT's RNTuple verifier is clean | 0.8 |
| **M9** update | **first:** prototype copy-on-write subdirectories against ROOT; then the commit protocol (§7.2), free-list validation, the StreamerInfo union, new-cycle tree extension, locking, the crash harness | The crash harness (§9, 5a) passes nightly; ROOT opens every result | 0.9 |
| **M10** split-object writer | `TBranchElement` writing (types 0–41) | User classes in trees round-trip between ROOT and us | 0.10 |
| **1.0** | **M0–M10 green**; the Goal 1 coverage target met on the named corpus; the nightly matrix, crash harness and data-driven suite passing | `cargo-semver-checks` baseline recorded | **1.0** |
| after 1.0 | see `TODOS.md`: dataset (`TChain`-like) API, RNTuple extension in update, Python bindings, analytics providers, native XRootD, the expert crate `rhizoid-raw` | — | — |

- **Ordering.** Decision D15 (eng review) set the dependency order and the
  1.0 gate. CEO-D2 then moved the compressed flat TTree writer ahead of
  update mode, and RNTuple reading ahead of async/WASM. The scope did not
  change.
- **Releases (C7):** a 0.x release when each milestone is green; breaking
  changes are allowed until 1.0, and every release has a changelog.
- **Effort, re-baselined (CEO review, K1):** verification against ROOT, not
  writing code, sets the pace.
  - **M0–M10:** about 18–30 months for a 1–2 person team; about 4–8 months
    with Claude Code. Both are bounded by ROOT CI turnaround, corpus work and
    the crash harness.
  - These are estimates, revised at each release from measured milestone
    times.

### 12.1 M1 as built (2026-11-26)

What shipped, and where it departs from the plan above:

- **Dynamic path first.** The decoder interprets the file's own class
  descriptions into `DynObject`; the encoder writes a `DynObject` from the
  embedded descriptions, matching members by name. A base class missing from
  an object built in code is synthesized and takes its members from the
  derived object, so models stay flat (`fEntries` lands in `TH1`). Members
  of objects built in code that match nothing are an `InvalidInput` error.
- **Typed models over the dynamic path.** `TH1D/F/I`, `TH2D/F`, `TAxis` and
  `TObjString` implement `FromDyn` and a new trait, **`ToDyn`**. `File::get`
  and `put` go through them. The typed fast path (`Marshal`/`Unmarshal` on
  raw bytes) moves to M6 with the derive, which generates it. A model read
  from a file keeps its source object, so members it does not expose (line
  colours, axis labels, fitted functions) are written back unchanged.
- **No class manifest or codegen yet.** The embedded descriptions are one
  blob captured from an uproot-written file
  (`tests/cross/uproot_hists.py`) and decoded once at first use. The
  manifest and `xtask gen-classes` arrive with M6, when there is more than
  one source of descriptions to keep in step.
- **Class tags.** We write class references, as ROOT does; uproot spells
  out every class name. Both read either way.
- **Found while building it:** `//->` pointers (`kObjectp`, 63, such as
  `TH1::fFunctions`) are streamed in place, not through `WriteObjectAny`
  [RB§8.8, corrected]. uproot writes no `TArray*` descriptions; neither do
  we (ROOT reads those classes with hand-written streamers).
- **Exit criterion:** met for uproot, in both directions
  (`tests/objects.rs`, `uproot_cross_check_objects`), and for ROOT 6.42
  (`tests/root.rs`, §12.6).

### 12.2 M2 progress (2026-11-26)

**Done: the `TBranch`/`TLeaf` subset**, the layout uproot writes and most
flat ntuples use:

- the tree is decoded by M1's dynamic decoder from the file's class
  descriptions (no tree-specific streamer code); `Tree` builds branches,
  leaves, basket tables and clusters from it;
- baskets: key extension with `fIOBits`, decompression, entry offsets
  (`fLast` border, the unreliable last offset replaced, as uproot does);
- interpretations: primitives of every leaf type, fixed arrays, jagged
  arrays through a counter leaf, strings (`TLeafC`);
- typed reads `tree.branch::<T>(name)?.read(range)` for `T` = a
  `NativeType`, `[T; N]`, `Jagged<T>` or `String`, checked once at open
  (`TypeMismatch`); untyped `tree.read(name, range)` → `Column`;
- one `read_ranges` call per branch read; `ReadStats`; clusters from
  `fClusterRangeEnd`/`fClusterSize`, `fAutoFlush`, or shared basket
  boundaries;
- `rhizoid tree` and `rhizoid scan`;
- every branch cross-checked against uproot (`tests/trees.rs`), and 200
  corrupted files read without a panic.

**Then: split objects and the corpus.** On 24 ROOT-written files
(scikit-hep-testdata, ~42 MB, ROOT 5.23–6.20), **1206 branches match
uproot, 0 differ**, 20 are not supported yet and 12 are groups
(`tests/cross/corpus-report.md`; CI job `corpus`):

- `TBranchElement`: interpretations come from the member's
  `StreamerElement` (`fClassName`, `fClassVersion`, `fID`), so ranges,
  array lengths and counts come from the class description; counters
  (3, 4) read as `i32`; members of split collections (31, 41) as jagged;
  `vector<T>` as jagged after its 10-byte header;
- every other entry kind is decoded per entry by the dynamic decoder into
  `Column::Objects` (`Interpretation::Objects`): whole objects, `TVector3`,
  `vector<string>`, nested vectors, sets, maps, leaf lists;
- embedded baskets, offset-less jagged baskets, `TLeafF16`/`TLeafD32`;
- groups (split nodes without values) say so: `Branch::is_group`.

**Then the rest of M2 (done 2026-11-26):**

- every branch kind in the corpus reads: `char*` members, `//[fN]` arrays
  in split collections (counts from the sibling branch), `std::bitset`,
  `TObject::fBits` with process IDs; the corpus now reads in full
  (**1207 match, 0 differ, 0 unsupported**, 19 more read where uproot
  cannot);
- reads take `impl Into<EntrySelection>` (`..`, ranges, entry lists, and
  `union`/`intersect`/`and_then`), and fetch only the baskets a selection
  touches;
- `tree.reader()` → `TreeReaderBuilder`: projection (a group stands for
  its sub-branches) and selection checked when set, `offset`, `limit`,
  `BatchSize::Cluster` (default) or `entries(n)`; `build()` is an
  `ExactSizeIterator` of `Batch`es;
- feature `rayon`: `par_batches()` (indexed, ordered, on the caller's
  pool); `tree.clusters()` is a slice, so `.par_iter()` comes for free;
- `rhizoid-arrow` (arrow 60): `to_arrow`, `to_record_batch`,
  `TreeExt::record_batches`; objects become `Struct`/`List` arrays.

**Deferred, as designed:** the builder is sync; its async twin
(`TreeReaderBuilder<I>`, §4.12.1) arrives with M5, as do `TreeMetadata`
reuse and the scratch pool (§4.12.3, §4.12.5). The ROOT half of the exit
criterion waits for the ROOT container (TODOS P1).

### 12.3 M3 as built (2026-11-26)

- `rhizoid_io::io`: the source traits, and adapters for any `ReadAt`:
  `Coalesce` (gap ≤ 64 KiB **and** span ≤ 1.25× the bytes needed, per
  group, so per request; property-tested), `ReadCache` (first 4 KiB and
  the last 32 single reads, ≤ 16 MiB), `Counted` (I/O accounting).
- Feature `http`: `HttpSource` (reqwest blocking, rustls) and `HttpPool`
  (one client, a cap on requests in flight, cloned explicitly; no global
  state). Multi-range requests parsed by each part's `Content-Range`;
  a `200` answer is dropped unread and the source falls back to parallel
  single ranges (≤ 10). Every response checked (status 206, each
  `Content-Range`, total length, body length); 3 retries with backoff on
  5xx, 429 (`Retry-After`), connection and body errors; the version read
  is pinned with `If-Match` / `If-Unmodified-Since` (412 = "changed").
  `HttpSource::open` and `File::open_url` give
  `ReadCache<Coalesce<HttpSource>>`; the CLI accepts URLs.
- `rhizoid-object-store` (object_store 0.14; features `aws`, `gcp`,
  `azure`, `http`): `ObjectStoreSource` implements `ReadAt` (on a runtime
  of its own, refusing to block inside one) **and `AsyncReadAt`** (for M5).
  It does not use object_store's `get_ranges`, which merges across 1 MiB
  gaps; ranges are fetched 10 at a time and merged by `Coalesce` instead.
  Requests carry `if_match`; lengths are checked.
- **Exit criterion:** `tests/sources.rs` reads every fixture through every
  sync source (local, coalesced, cached, HTTP raw, HTTP coalesced+cached,
  a single-range-only server, a server refusing `HEAD`) and compares
  everything with the local file; the whole corpus reads identically over
  HTTP; faults (503s, truncation, a wrong `Content-Range`, a changed ETag,
  a server ignoring ranges, 404) are retried or reported; the over-read
  bound holds end to end (fetched ≤ 1.25× needed, and the server sent
  exactly what was fetched). `ObjectStoreSource` passes the same checks
  through the local and HTTP backends. Checked once against a real CDN
  (GitHub raw) as well; S3/GCS/Azure themselves are object_store's.
- Not done, by design: async reading of trees (M5).

### 12.4 M4 as built (2026-11-26)

- `rhizoid_io::ntuple`: `File::rntuple(path)` (and `Directory::rntuple`)
  → `RNTuple`. The metadata is parsed sans-IO (`ntuple/format.rs`) and
  loaded once at open: the anchor and every envelope are checksummed;
  frames are skipped by their size, so records from later minor versions
  read; unknown feature flags are refused; header, footer, schema
  extension, cluster groups and page lists.
- Pages (`ntuple/pages.rs`): every column type of format 1.0
  (`0x00`–`0x1D`): split byte planes, zigzag, per-page delta of split
  index columns, bit-packed booleans, `Real16`, `Real32Trunc`,
  `Real32Quant`, switch columns. Page checksums are verified; pages
  decompress through the same codecs as baskets.
- Fields: leaves, strings, `std::array` and `std::bitset` (repetition),
  collections (vectors, `RVec`, sets, maps, `std::optional`,
  `std::unique_ptr`), records (classes with their bases, pairs, tuples),
  variants, `std::atomic` and other transparent wrappers, cardinality
  fields, projected fields (alias columns), several column
  representations (the one not suppressed in each cluster), columns added
  by schema extension (zero before their first element), several cluster
  groups.
- API: `nt.fields()` is the field tree (`Field`: name, type, description,
  sub-fields, interpretation); `nt.field::<T>(path)?.read(sel)` reads
  typed with the `ColumnType`s trees use, checked at open;
  `nt.read(path, sel)` → `Column`; `nt.clusters()`. Reads take an
  `EntrySelection` and fetch only the pages of the clusters it touches,
  in one vector read per field.
- `rhizoid rntuple FILE:name` describes one; `rhizoid scan` prints trees
  and `RNTuple`s. `rhizoid-arrow` converts every field; variants become
  dense unions, tuples and pairs structs of `_0`, `_1`, ….
- **Exit criterion: met.** The 24 ROOT-written `RNTuple` files of
  scikit-hep-testdata (format 1.0.0.0 to 1.0.1.0, ROOT's own test files
  and two CMS Open Data conversions) plus an uproot-written fixture:
  **30 `RNTuple`s, 1938 fields match uproot, 0 differ, 0 unsupported**
  (`tests/rntuple.rs`, `tests/cross/rntuple-report.md`, CI job
  `corpus`). 200 corrupted files read without a panic.
- Not done:
  - the bare container (an `RNTuple` without a `TFile`): no file written
    that way was found; past the anchor it reads like the rest (TODOS);
  - fields stored by a ROOT streamer (role 4): none in the corpus; they
    are reported `Unsupported`;
  - the batch reader (`reader()`: projection, batches, rayon) is
    tree-only. `RNTuple` gets it with M5's shared, sync-or-async builder
    (§4.12.1) — done in M5 (§12.5).

### 12.5 M5 as built (2026-11-26)

- **How async reads work (the sans-IO core of §2, §4.12.4).** The
  decoders are unchanged: every async read runs the sync decoding code
  against the bytes fetched so far (`Prefetched`, a `ReadAt` over the
  fetched ranges). A request it cannot serve fails and is remembered (the
  first one of a run only, since later ones may come from an error path);
  the run's result is discarded, the missing ranges are fetched with one
  `read_ranges` call, and the run starts again. Reads whose ranges follow
  from metadata (a branch, a field, a batch) are planned first
  (`Tree::plan`, `RNTuple::plan`) and fetched in one request, then decode
  in one run; metadata takes one request per dependent read, as in the
  sync API. Handles share their metadata and point at another source with
  `with_source`, a cheap clone.
- **Departure from §4.10 (D5):** async handles are their own types
  (`AsyncFile<S>`, `AsyncDirectory<S>`, `AsyncTree<S>`, `AsyncRNTuple<S>`)
  rather than `File<Async>`, and no `maybe_async!` macro is needed: the
  async methods are thin drivers over the one sync body, so the two cannot
  drift. `AsyncFile` (default `S = Arc<dyn AsyncReadAt>`) has `Send`
  futures; `LocalAsyncFile` (`Rc<dyn LocalAsyncReadAt>`) is for the browser.
  `File::open_async`, `open_async_with`, `open_local_async`,
  `open_local_async_with`. `File::check` has no async twin (it walks every
  record, one dependent read at a time).
- **One reader builder (§4.12.1):** `ReaderBuilder<H>` serves `Tree`,
  `RNTuple` (new: `RNTuple::reader`) and their async handles; the setters
  exist once. `build()` gives a `Reader` (an `ExactSizeIterator`, with
  `par_batches` under rayon) for a sync handle and a `BatchStream` (a
  `futures_core::Stream`) for an async one. `next_cluster()` returns a
  `ClusterReader` holding a batch's bytes, `Send`, decoded with
  `decode()` on any thread while the next batch is fetched.
- **Sources:** `AsyncReadAt` and `LocalAsyncReadAt` for `Bytes`,
  `Vec<u8>`, `Arc`, `Box` (and `Rc` for the local trait); async twins of
  `Coalesce` and `Counted`; `io::Inline` (a sync source read inside the
  future); `io::AsyncHttpSource` (features `http` + `async`: the blocking
  source's requests, checks, retries and validators, sharing their code;
  `AsyncReadAt` on tokio natively, `LocalAsyncReadAt` through `fetch` on
  WASM); `ObjectStoreSource::new_async`/`open_async`/`from_url_async`.
- **Found while building it:**
  - `ObjectStoreSource` gave each source its own runtime. With two live
    sources on one store, a request could pick up a connection pooled by
    the other source's idle runtime and stall until the 30 s timeout (seen
    as a 211 s test). The sync methods now share one process-wide runtime
    whose workers keep pooled connections alive; a store should not be
    shared between sync and async use (documented).
  - `rhizoid-source` read and wrote `std::fs::File` only on Unix and
    Windows; WASI and `wasm32-unknown-unknown` now seek and read under a
    lock.
- **Exit criterion: met.** `tests/asynchronous.rs` runs every fixture and,
  in CI, the whole corpus (49 files) through the async API over in-memory,
  `Coalesce<Inline<File>>`, local (`!Send`), HTTP (multi-range, single
  range, `HEAD` refused, coalesced) sources: objects, every branch and
  field, batch streams and `next_cluster` give exactly what the sync API
  gives; each column read takes one request; faults are retried or
  reported; the over-read bound holds; damaged files fail as in the sync
  API (proptest). `ObjectStoreSource` passes the same comparison through
  its local, in-memory and HTTP backends. On `wasm32-unknown-unknown`
  under Node, the fixtures decode to the digests recorded natively
  (`tests/data/fingerprints.txt`), sync and async (CI job `wasm`).
- Not done: `TreeMetadata` serialization and reuse across processes
  (§4.12.3, feature `serde`); an async `rhizoid-arrow` stream of record
  batches; async cancellation tests (dropping a read mid-fetch is safe by
  construction: nothing is kept from an unfinished read).

### 12.6 Cross-checked with ROOT (2026-11-26)

- **Tools:** `tools/wsl-setup.sh` installs, in WSL Ubuntu 24.04 (or any
  Ubuntu 24.04), Rust stable and the MSRV with the WASM target, Node and
  `wasm-bindgen-cli`, Python with the uproot oracle, and the ROOT 6.42.00
  binary release. `tools/wsl-check.sh` runs every check there, ending with
  `tools/root_smoke.C` (ROOT opens every fixture and a file we write, and
  reads each object, tree entry and `RNTuple` entry).
- **The histogram cross-check (`tests/root.rs`, M1's ROOT half):**
  `tests/cross/root_dump.C` prints what ROOT reads (every cell with flow,
  errors, `fSumw2`, `fEntries`, `GetStats`, edges, axis titles, means and
  standard deviations, fitted functions); floats are compared exactly.
  - ROOT reads what we write (`TH1D/F/I`, `TH2D/F`, `TObjString`) as we do,
    without a warning, and does not need to recover the file.
  - We read what ROOT writes (`tests/cross/root_make.C`) as ROOT does: a
    fixture written by ROOT 6.42 is committed with ROOT's reading of it,
    so this half runs without ROOT; with `RHIZOID_ROOT`, a fresh file of
    the ROOT at hand is checked too.
  - ROOT's objects, read by us and written back, read in ROOT exactly as
    before, the fitted `TF1` included.
- **Found by it:**
  - Writing back an object with a class the crate does not describe
    itself (`TF1`, `TFormula`, `TF1Parameters` in `fFunctions`) failed. An
    object now carries the descriptions of the file it was read from; the
    writer encodes it with them and stores them in the new file.
  - The writer could not write STL members; it now writes them object-wise
    as ROOT does (header version 10, size, elements; maps key, value, …),
    and the reader decodes object-wise maps (`TFormula::fParams`), which it
    had kept as bytes.
- In CI: job `root` (ROOT 6.42.00 binary release, cached).

### 12.7 M6 as built (2026-11-26)

- **Objects are data models** (the decision of §4.6, at the user's
  request): `TH1<T>`, `TH2<T>`, `TH3<T>` for each cell type (`TH1D` =
  `TH1<f64>` …, `C S I L F D`), `TProfile`, `TProfile2D`, `TProfile3D`,
  `TGraph`, `TGraphErrors`, `TGraphAsymmErrors`, `TF1` (read and written
  back), `TAxis` (with labels), `TObject`, `TNamed`, `TObjString`. They
  hold what the file stores and do not fill, rebin or compute; built from
  data (`with_values_flow`, `with_sumw2`, `stats_mut`). `Histogram` is
  what histogram libraries convert through. Axis edges are computed as
  ROOT computes them (`xmin + i·width`, not `xmax` for the last).
- **Embedded descriptions captured from ROOT 6.42** (`root_make.C`,
  `capture_streamers.py`), replacing uproot's. Every model is
  cross-checked against ROOT in both directions and on rewrite
  (`tests/root.rs`; §12.6).
- **`TClass` checksum ported** (`StreamerInfo::compute_checksum`, the
  latest `TStreamerInfo::GetCheckSum`): it matches every checksum ROOT 6
  stored in the fixtures and corpus (about 650), but for documented cases
  that never arise for user classes (collections, `unique_ptr` members,
  abstract bases, typedef'd templates) (`tests/checksum.rs`).
- **`#[derive(RootObject)]`** (`rhizoid-derive`, re-exported under the
  default feature `derive`): bases (`TObject`, `TNamed`, other derived
  classes), scalars, fixed arrays, `//[fN]` arrays (`count`, the counter
  written from the array's length), `TString` and `std::string`,
  `std::vector` of numbers, strings and classes, embedded classes. The
  generated description equals the one ROOT writes from its dictionary for
  the same class, checksum included; ROOT, with that dictionary
  (`tests/cross/user_classes.C`, ACLiC), reads our objects as its own,
  without a warning; we read ROOT's. **Exit criterion: met.**
- **Schema evolution**: members matched by name and `alias`, absent ones
  zero or empty, unknown ones skipped, numbers converted (narrowing
  checked); `skip` for members not stored.
- **Rows**: `Row` (derive or tuples of up to 16 `RowField`s), `tree.rows`,
  `rows_of`, the same on `RNTuple` and on the reader builder (with
  selections); fields checked against their columns once, at open.
- **Compile-time checks** (§5.5), each with a `trybuild` case
  (`tests/ui`, run with `RHIZOID_UI=1` on stable): a bad version, unknown
  attributes, a `count` that is not an earlier `i32`, a member type with
  no ROOT mapping, duplicate names, generics, a bad row field.
- **Found while building it:** `std::string` members of classes are framed
  (byte count, version 10) and were written bare; a `//[fN]` count in a
  base of an object read from a file was not found.
- **Not done:** the typed fast path (`Marshal`/`Unmarshal` on bytes, §4.4)
  is not generated: derived classes read through the evolution path,
  correct for every version and fast enough for objects; writing rows
  (M7, M8); async rows; `Double32_t`/`Float16_t` members, pointers and
  maps in derived classes (TODOS).

### 12.8 M7 as built (2026-11-26)

- **API:** `f.tree::<R>(name)` for a `#[derive(Row)]` type,
  `tree_with(name, TreeOptions)`, `tree_of::<(…), N>(name, names)` for
  tuples, on `FileScope` and `WritableFile`; `TreeWriter::fill`,
  `extend`, `finish`. A writer borrows its file: it cannot escape
  `create`'s scope, cannot be used after `finish`, and takes only its own
  row type (`trybuild` cases). One dropped unfinished is finished when the
  file closes, where errors are reported.
- **What it writes** is what ROOT 6.42 writes for a leaf-list tree,
  checked member by member against a tree ROOT wrote with the same branches
  (`tests/cross/root_tree.C`, fixture `root_tree.root`): `TTree` v20,
  `TBranch` v13, `TLeaf{O,B,S,I,L,F,D,C}` (unsigned types flagged),
  fixed arrays `x[3]/F`, jagged arrays behind a counter branch (`n_x/I` by
  default, `#[row(count = "nMuon")]` to name or share one; shared lengths
  checked per row, a refused row is not written), strings `x/C`. Only the
  basket positions and compressed sizes differ (another zlib); the
  uncompressed sizes are equal, basket by basket.
- **Clusters:** rows are buffered and written a cluster at a time, one
  basket per branch, so every branch has the same basket boundaries. The
  first cluster ends at a byte budget (30 MB uncompressed by default,
  `cluster_bytes`) or at `cluster_entries`; every later one holds as many
  entries, recorded in `fAutoFlush`, as ROOT does. ROOT's cluster iterator
  walks them as we wrote them.
- **Encoder:** leaves are written once and referred to afterwards
  (`TTree::fLeaves`, `TLeaf::fLeafCount`), as ROOT shares them by address;
  `ROOT::TIOFeatures` is written as version 0 + checksum (foreign class).
- **Exit criterion, cross-check:** met. ROOT 6.42 reads our trees (one
  cluster, three clusters, LZ4, and 30 000 rows in byte-budget clusters)
  entry by entry exactly as it reads its own, without a warning; uproot
  reads them as it reads ROOT's; we read ROOT's as ours. **ROOT ≥ 6.26:**
  the files claim 62600 and use only class versions ROOT 6.26 has
  (`TTree` v20 and `TBranch` v13 since 6.12, `TIOFeatures` since 6.14), but
  only ROOT 6.42 is installed: an older release is not tested here.
- **Found while building it:** a compressed record could come out exactly
  as long as its object; readers (ROOT's rule `fNbytes - fKeylen <
  fObjLen`) then take it for uncompressed. The writer now stores raw unless
  the compressed form is strictly smaller, headers included (a proptest).
  Baskets are compressed whatever their size, as `TBasket::WriteBuffer`
  does, not only above 256 bytes as keys are.
- **Not done:** in-place metadata rewrites and AutoSave cycles (the tree is
  written once, at finish); `fClusterRangeEnd` for irregular clusters;
  split objects (`TBranchElement`, M10); zstd and LZMA writing.

### 12.9 M8 as built (2026-11-26)

- **API:** `f.rntuple::<R>(name)` for a `#[derive(Row)]` type,
  `rntuple_with(name, RNTupleOptions)`, `rntuple_of::<(…), N>(name, names)`
  for tuples, on `FileScope` and `WritableFile`; `RNTupleWriter::fill`,
  `extend`, `finish`. The same row types as the tree writer: a type writes
  to either. A writer cannot escape `create`'s scope, cannot be used after
  `finish`, and takes only its own row type (`trybuild` cases); one dropped
  unfinished is finished when the file closes.
- **What it writes** is what ROOT 6.42's `RNTupleWriter` writes for the
  same fields, record for record (`tests/cross/root_ntuple.C`, fixture
  `root_ntuple.root`; the unit test `metadata_matches_roots` compares the
  anchor version, every field and column of the header, the footer and
  the clusters' pages): format 1.1.0.2; fields `bool`, `std::(u)intN_t`,
  `float`, `double` (column types `Bit`, `(U)Int8`, `SplitInt*`,
  `SplitUInt*`, `SplitReal*`), `std::array<T,N>` (repetition N, child
  `_0`), `std::vector<T>` (collection, `SplitIndex64` offsets, child `_0`),
  `std::string` (`SplitIndex64` + `Char`); the header first, then per
  cluster one `RBlob` with the cluster's pages (each compressed on its own
  when that makes it smaller, then its XXH3 checksum), then the page list
  and the footer (compressed envelopes) and the anchor, the only key in the
  directory. The writer string is `rhizoid-io <version>`.
- **Clusters and pages:** entries are buffered and written a cluster at a
  time (64 MiB uncompressed by default, `cluster_bytes`), pages up to 1 MiB
  (`page_bytes`); with `rayon`, a cluster's pages are compressed in
  parallel. One cluster group.
- **Exit criterion:** the cross-check passes: ROOT 6.42 reads our
  `RNTuple`s (one cluster; small clusters and pages; 30 000 entries in many
  clusters, compressed in parallel; empty; long strings; next to a tree)
  entry by entry exactly as it reads its own, with page checksums checked,
  through `RNTupleReader` and `RNTupleInspector`, without a warning; uproot
  reads them as it reads ROOT's; we read ROOT's as ours. **ROOT's RNTuple
  verifier:** ROOT 6.42 ships no separate verifier; its checks are the
  reader's (envelope and page checksums, which it verifies by default) and
  the descriptor's, which the inspector walks. Those run clean.
- **Found while building it:** `rhizoid check` reported an error on every
  `RNTuple` ROOT ≥ 6.34 writes: it decompressed each `RBlob` as one object,
  but a cluster's blob holds pages compressed one by one, with `ObjLen` their
  uncompressed total. It now checks that blobs are there and leaves their
  content to the `RNTuple` reader.
- **Departures from §4.9:** the default compression is the file's (zlib
  level 1), not zstd level 5, because the crate writes zstd only once a pure
  Rust encoder is in (TODOS); clusters are cut by uncompressed size.
  `DynRow` writers, `OwnedRNTupleWriter`, nested records and classes,
  `Real32Trunc`/`Real32Quant` columns, and several cluster groups are not
  done (TODOS).

### 12.10 M9 as built (2026-11-26)

- **Subdirectories** in new files: `FileScope::mkdir("a/b")` (as
  `create_dir_all`, finding what exists) gives a `DirWriter` with `put`,
  `mkdir`, `tree`, `rntuple`; `WritableFile::mkdir` and `put_in`. The
  layout is ROOT 6.42's (`tests/cross/root_dirs.C`): a `TDirectory` key
  holding the 60-byte record (its `fSeekDir` the key's place,
  `fNbytesName` the key length), its key list a `TDirectory` record, none
  for an empty directory.
- **Update mode** (§7.2): `rhizoid_io::update(path, |f| …)`,
  `update_with(…, UpdateOptions)`, `WritableFile::update`, and
  `WritableFile::update_in(source, sink, …)` for any bytes. The prototype
  §7.2 asked for first passed: **ROOT 6.42 reads copy-on-write
  subdirectories** (a new key holding the moved record, the parent's list
  pointing at it), on files it wrote and on ours, **and updates them
  itself** afterwards (writing into them, reusing the space we freed);
  children whose parent moved keep an old `fSeekParent`, which ROOT does
  not use. Then: new records past the old end; changed directories moved
  copy-on-write up to the root; the StreamerInfo the old list with the new
  descriptions appended (its other entries, such as schema-evolution
  rules, kept byte for byte); the free list the old one checked against
  the records (segments over live records dropped), freeing nothing of the
  session; sync; header; sync; root directory record (in place, inside
  one 512-byte sector, else `Unsupported` without `copy_on_write`); sync.
  What the session made garbage (old key lists, moved directory keys, the
  old StreamerInfo and free list, replaced tree cycles) is then marked with
  ROOT's gap markers and listed in a second free list, made current by a
  second header write: the same guarantee as "freed at the next commit",
  within one session.
- **Trees extended as a new cycle** (D14): `f.tree::<R>("t")` on a tree of
  the same branches continues it (its baskets kept, its entries first);
  its clusters become an explicit range (`fClusterRangeEnd`,
  `fClusterSize`), the new ones follow as `fAutoFlush`; the old cycle is
  freed. ROOT reads the extended tree and walks its clusters as written.
  `RNTuple`s are added, not extended (CEO-D5: `Unsupported`, TODOS).
- **All or nothing** (D13): an error or a panic in the body, or a dropped
  `WritableFile`, commits nothing. Sinks that cannot sync are refused. A
  file not closed cleanly is refused (recover it first).
- **Locking:** the file is locked (`File::try_lock`) for the session;
  another writer gets an `Io` error saying so. The lock is released
  explicitly at the end: on Unix a lock goes with the open file, and a
  child process forked at that moment by another thread holds it until it
  execs (found by the tests).
- **`copy_on_write`:** the update is applied to a copy, synced, renamed
  over the original, the directory synced (POSIX).
- **Crash harness** (§9 5a, `tests/crash.rs`): a recording sink logs every
  write and sync of an update of every kind (objects, a moved nested
  directory, a new one, a new class, an extended tree); at each crash
  point every subset of the pending writes (all up to 8, 256 samples and
  every prefix beyond) and every pending write torn at a sector boundary
  give a file: 359 (352 distinct) over 6 crash points. Each opens cleanly, holds the old content
  or the new, never a mix, and takes a second update correctly; the commit
  order (data synced, header alone, synced, root record alone, synced) is
  asserted; ROOT opens 60 of them without recovery and reads every object.
  Two mutations (the root record written early; the sync before the header
  removed) are caught. **Exit criterion: met** (it runs in every check,
  not only nightly: it takes about 2 seconds).
- **Found while building it:** our reader refused an empty subdirectory
  (`fSeekKeys` 0, which ROOT writes); Windows' file locks are mandatory,
  so reading the file being updated goes through the locked handle.
- **Not done:** reusing free space (updates append: the file grows; ROOT
  reuses what we free); `put_replace` and `delete`; files over 2 GB are
  handled (large key and record forms) but not tested here; zstd for
  what an update writes (the file's own compression is kept when it is
  zlib or LZ4, else zlib).

### 12.11 M10 as built (2026-11-26)

- **API:** a `#[derive(Row)]` field of a `#[derive(RootObject)]` class is a
  split object branch, named after the field with a trailing dot (as ROOT
  recommends, so that two objects' members cannot clash; `rename` sets
  it); a `Vec` of such a class is a split `std::vector` branch. The
  derive implements `RowField` for the class and `RowItem` (what a `Vec`
  field holds) for it. Reading: a typed row's class fields read their
  branches whole, assembled from the sub-branches (`@objects:` columns
  inside the reader; the default interpretation of every branch is
  unchanged, so nothing else reads differently).
- **What it writes** is what ROOT 6.42 writes (`tests/cross/user_classes.C`,
  `user_tree_write`: a leaf-list `run`, a `UserEvent` `event.` with a
  `TNamed` base, every member kind and an embedded `Hit`, a `Hit` `hit.`,
  a `std::vector<Hit>` `hits`), checked member by member over the whole
  tree object and basket by basket: container branches for the object
  (`fType` 0, `fID` −2), its bases (1) and embedded objects (2); a branch
  per member (0) holding it as it is streamed in the object; for the
  collection a counter (4) and a branch per member of the element class,
  bases flattened (41), holding the member of every element back to
  back. Names, titles, split levels, entry offsets, bits, leaves,
  checksums (a collection's is the hash of its name) as ROOT's.
- **Exit criterion: met.** ROOT 6.42, with the classes' dictionary, reads
  our tree (in one cluster and in several) entry by entry exactly as it
  reads its own, without a warning; we read ROOT's into the Rust twins;
  uproot reads every branch of ours as it reads ROOT's (it cannot group
  nested bases whole, in ROOT's file as in ours).
- **Found by ROOT:** a branch of a split collection refers to its counter,
  its own parent, by reference (`fBranchCount`); ROOT crashes without it.
  The encoder now registers shared objects (leaves and branches) when it
  starts writing them, as ROOT maps them, and writes such references; our
  reader reads a reference to an object still being read as null, which
  hid it. A branch counting a `//[fN]` array is shared the same way.
  ROOT also keeps a null `fBaskets` slot per basket and one more.
- **Not done:** `TClonesArray` branches (3/31); collections whose element
  class has strings, vectors or embedded objects; extending a split tree
  in update mode; class fields in `RNTuple`s (TODOS).

### 12.12 The 1.0 gate as built (2026-11-26)

- **Goal 1's measure (`tests/coverage.rs`):** every branch of numbers of
  58 files (the 49 of the scikit-hep-testdata corpus, ROOT 5.23 to 6.20
  trees and ROOT's RNTuple test files, and the 9 fixtures, written by ROOT
  6.42 and uproot), as
  ROOT 6.42 reads it and as we read it: the number of values and their
  sum, arrays and collections flattened. ROOT reads each branch two ways
  (`tests/cross/root_sums.C`): entry by entry (`GetEntry`, then the
  leaf's values) and through `TTree::Draw`; a match with either is a
  match, as each has blind spots, documented in the macro (Draw misreads
  baskets without entry offsets, `small-dy-nooffsets`, and reads `char`
  arrays as strings; the leaf API misreads STL members of classes without
  a dictionary). The report, with the list of what is not covered, is
  `tests/cross/coverage-report.md`, regenerated for each release.
  - **Result: met.** All 2130 branches ROOT reads match (100 %); none
    differ, none are unsupported. 17 minutes in WSL, four ROOT processes
    at a time.
  - **Not covered, named** (in the report, branch by branch or by type):
    2083 branches ROOT does not read as numbers: strings (`string`,
    `TString`, `vector<string>` and nested, 841), base-class containers
    (203), and objects or containers of them (`ElementLink`, xAOD
    metadata, `map`, `set`, `TRef`, …); 200 branches of
    `uproot-mc10events.root` on which ROOT itself crashes (classes
    without a dictionary); 2 `TBits::fAllBits` arrays of
    `uproot-Event.root` that ROOT cannot read without the dictionary
    (its leaf API gives a length of −8, Draw a string per entry; ROOT's
    own `fNbytes` counts the 11318 values we read, and uproot reads the
    same values). The strings and objects are compared with uproot
    instead (`tests/cross/corpus-report.md`: 4178 branches, all match).
  - **Found by it:** nothing in the reader; the oracle itself needed the
    second ROOT reading (above).
- **The data-driven suite (`tests/cases`):** 284 cases (270 of them from ROOT), each run
  through memory, a file, HTTP (multi-range and single-range servers)
  and the async API, which must all agree. The tree and RNTuple cases
  take their expected values from ROOT (`tests/cross/make_cases.py`
  runs `root_sums.C` and `root_ntuple_sums.C`, whole and over entry
  ranges; a branch whose two ROOT readings disagree is left out).
- **CI:** `ci.yml` (fmt, clippy native and WASM, tests on Linux, macOS
  and Windows, MSRV, docs, the ROOT job with every ROOT cross-check,
  `cargo-deny`, `cargo-semver-checks`); `nightly.yml` (ROOT 6.26, 6.30,
  6.34 and the latest read what we write; the coverage and the cases
  against the corpus; the crash harness; build timings). **Not yet run:**
  the project has no repository or CI yet, so only what runs locally is
  checked (`tools/wsl-check.sh`, ROOT 6.42, all passing); the 6.26, 6.30
  and 6.34 images run for the first time with the first nightly.
- **Semver baseline:** the `semver` job checks against the `v1.0.0` tag
  once it exists; before it, it is skipped. The baseline is recorded by
  tagging the release.
- **Licence:** MIT, the owner's decision (2026-11-27, §13 Q2); the
  repository is public.
- **Not done by the gate, the owner's to decide:** the design partner
  (TODOS P1), and corpus files beyond
  scikit-hep-testdata (CMS NanoAOD and MiniAOD, ATLAS PHYSLITE from CERN
  Open Data, about 1–1.6 GB).

## 13. Open questions

1. ~~Claimed ROOT version~~: **resolved by C1** (CEO review). The target
   is ROOT ≥ 6.26 for TTrees and objects, and ≥ 6.34 for RNTuple.
2. ~~Licensing~~: **decided by the owner, 2026-11-27: MIT.** The question
   was whether a clean-room port of `TClass::GetCheckSum`, plus class
   descriptions captured from ROOT-written files, can ship under a
   permissive licence given ROOT's LGPL-2.1; precedent: uproot (BSD-3)
   embeds class descriptions captured from ROOT files. No outside legal
   review was made.
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
| **fBEGIN** | Offset of the first record: 100 as ROOT writes it. Readers take the header's value; ROOT does not check it [RB§1] |
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
design's, not a released crate's. Every example returns `rhizoid_io::Result`, so
`?` works throughout.

### A.1 Inspecting a file

```rust
use rhizoid_io::prelude::*;

fn main() -> rhizoid_io::Result<()> {
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
use rhizoid_io::objects::{TH1D, TH2F};

let h: TH1D = file.get("analysis/h_pt")?;             // typed
let h: TH1D = file.get(path!("analysis/h_pt"))?;      // same, with the path checked at compile time
println!("{} entries, Σw {}", h.entries(), h.stats().tsumw);   // as stored
for (i, v) in h.values().iter().enumerate() {
    println!("[{:>6.1}, {:>6.1})  {v}", h.edges()[i], h.edges()[i + 1]);
}

let map: TH2F = file.get("calib/map;3")?;      // an explicit cycle

// A class with no Rust model: decoded from the file's own class description.
let cfg = file.get_dyn("config/MyConfig")?;
let threshold: f64 = cfg.member("fThreshold")?.try_into()?;
println!("{cfg:#?}");                                 // the whole object, member by member
```

### A.3 Reading `TTree` columns

```rust
use rhizoid_io::columnar::Jagged;

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
    .map(|range| -> rhizoid_io::Result<f64> {
        let pt = tree.branch::<Jagged<f32>>("Muon_pt")?.read(range.clone())?;
        Ok(pt.values().iter().map(|&x| f64::from(x)).sum())
    })
    .sum::<rhizoid_io::Result<f64>>()?;
```

### A.4 Typed rows

```rust
#[derive(rhizoid_io::Row)]
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
use rhizoid_arrow::TreeExt;                          // adapter crate, versioned with arrow

for batch in tree.record_batches(&["MET_pt", "Muon_pt"], 65_536)? {   // cluster-aligned
    let batch: arrow_array::RecordBatch = batch?;
    // Hand it to DataFusion, Polars, DuckDB, Parquet writers, PyArrow …
}
```

### A.6 Remote files, async

```rust
use rhizoid_io::{AsyncFile, io::HttpSource, columnar::Jagged, path};

let source = HttpSource::new("https://example.org/data/nano.root")?
    .coalesce();                                      // gaps ≤ 64 KiB, ≤ 1.25× over-read, 10 requests in parallel
let file = File::open_async(source).await?;           // AsyncFile; header + root directory: 1 request
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

let bare = rhizoid_io::RNTuple::open("events.rntuple")?;      // a bare RNTuple, no TFile
```

### A.8 Writing histograms and an ntuple

```rust
use rhizoid_io::{WriteOptions, Compression, objects::{TH1D, TObjString}};

#[derive(rhizoid_io::Row)]
struct Event { run: u32, pt: f32, jets: Vec<f32> }

let mut h_pt = Hist::new(100, 0.0, 200.0);    // a histogram library's type

rhizoid_io::create_with("out.root", WriteOptions::default().compression(Compression::zstd::<5>()), |f| {
    let mut events = f.rntuple::<Event>("Events")?;
    for ev in source() {
        h_pt.fill(f64::from(ev.pt));
        events.fill(&ev)?;                     // only an `Event` is accepted
    }
    f.put("h_pt", &TH1D::from(&h_pt))?;        // converted by an adapter crate; while the ntuple writer is still open
    f.mkdir("meta")?.put("version", &TObjString::new("v1.2"))?;
    Ok(())
})?;                                           // every writer finished, keys, StreamerInfo, header: done
```

### A.9 Several writers, several threads

```rust
rhizoid_io::create("split.root", |f| {
    let mut signal = f.rntuple::<Event>("Signal")?;
    let mut background = f.rntuple::<Event>("Background")?;

    std::thread::scope(|s| -> rhizoid_io::Result<()> {
        let a = s.spawn(|| fill(&mut signal, signal_source()));
        let b = s.spawn(|| fill(&mut background, background_source()));
        a.join().expect("signal filler panicked")?;
        b.join().expect("background filler panicked")?;
        Ok(())
    })?;
    Ok(())
})?;

fn fill(w: &mut RNTupleWriter<'_, Event>, src: impl Iterator<Item = Event>) -> rhizoid_io::Result<()> {
    for ev in src { w.fill(&ev)?; }
    Ok(())
}
```

### A.10 A user class

```rust
use rhizoid_io::{RootObject, objects::TNamed};

#[derive(RootObject, Debug, Clone, PartialEq)]
#[root(class = "Calibration", version = 2)]
struct Calibration {
    #[root(base)] named: TNamed,
    gain: f64,
    n: i32,
    #[root(count = n)] pedestals: Vec<f32>,         // float* fPedestals //[n]
    #[root(double32(range = 0.0..10.0, bits = 12))] noise: f64,
}

rhizoid_io::create("calib.root", |f| f.put("calib", &calib))?;

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
use rhizoid_io::{Schema, DType, Value};

// e.g. converting a CSV whose columns come from its header line
let schema = Schema::builder()
    .field("run", DType::U32)
    .field("pt", DType::jagged(DType::F32))
    .build()?;

rhizoid_io::create("converted.root", |f| {
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
    fn new(path: &Path) -> rhizoid_io::Result<Self> {
        let file = WritableFile::create(path, WriteOptions::default())?;
        let events = file.rntuple::<Event>("Events")?;
        Ok(Self { file, events })
    }
    fn record(&mut self, ev: &Event) -> rhizoid_io::Result<()> { self.events.fill(ev) }
    fn finish(self) -> rhizoid_io::Result<()> {
        self.events.finish()?;                       // consumes the writer
        self.file.close()                            // consumes the file; Err(ChildrenOpen) if a child is alive
    }
}
```

### A.13 Updating an existing file

```rust
rhizoid_io::update("data.root", |f| {
    f.put("h_pt", &h_pt)?;              // a new cycle: h_pt;2 next to h_pt;1
    f.put_replace("h_eta", &h_eta)?;    // replaces, and frees the old record's space
    f.delete("scratch/tmp")?;    // its space becomes a free gap
    Ok(())
})?;
// Records this library does not understand are left untouched.
```

### A.14 Run-time configuration

```rust
// `rhizoid cp --compression 505 in.root out.root`
let compression = Compression::try_from(args.setting)?;   // 505 → Zstd level 5; 250 → Err
let opts = WriteOptions::default()
    .compression(compression)
    .reproducible(true);                // datime 1, zero UUIDs: byte-identical output
rhizoid_io::create_with(&args.output, opts, |f| f.copy_from(&input, "/"))?;
```

### A.15 Untrusted input, diagnostics, errors

```rust
use rhizoid_io::{ReadOptions, ReadLimits, Strictness, ErrorKind};

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
use rhizoid_io::io::{ReadAt, FileOffset};

struct TapeArchive { /* … */ }

impl ReadAt for TapeArchive {
    fn len(&self) -> std::io::Result<u64> { /* … */ }
    fn read_at(&self, r: Range<FileOffset>) -> std::io::Result<bytes::Bytes> { /* … */ }
    // `read_ranges` has a default; override it if the store supports batched reads.
}

let file = File::from_source(TapeArchive::connect("…")?, ReadOptions::default())?;
```

### A.17 Writing a `TTree` (M7)

```rust
#[derive(rhizoid_io::Row)]
struct Hit {
    run: i32,                       // Int_t,  leaf "run/I"
    energy: f64,                    // Double_t, "energy/D"
    xyz: [f32; 3],                  // "xyz[3]/F"
    xs: Vec<f32>,                   // counter leaf "n_xs/I" + "xs[n_xs]/F"
    label: String,                  // "label/C"
}

rhizoid_io::create("hits.root", |f| {
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

let w = rhizoid_io::create("o.root", |f| Ok(f.rntuple::<Event>("E")?))?;
// error: lifetime may not live long enough: the writer cannot escape the scope

let mut w = out.rntuple::<Event>("E")?;     // out: WritableFile
w.finish()?;
w.fill(&ev)?;
// error[E0382]: borrow of moved value: `w`

events.fill(&run)?;            // `events: RNTupleWriter<'_, Event>`, `run: Run`
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

let info: RunInfo = rhizoid_io::serde::from_dyn(&file.get_dyn("meta/RunInfo")?)?;

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
    .map(|range| -> rhizoid_io::Result<usize> {
        let pt = tree.branch::<Jagged<f32>>("Muon_pt")?.read(range)?;
        Ok(pt.iter().filter(|m| m.len() >= 2).count())
    })
    .sum::<rhizoid_io::Result<usize>>()?;

// Rows in parallel, one task per cluster:
let total: f64 = tree.par_rows::<Muons>()?.map(|r| r.map(|m| f64::from(m.met))).sum::<rhizoid_io::Result<f64>>()?;

// Bounded parallelism: everything runs in the pool the caller chose.
let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
let met = pool.install(|| tree.branch::<f32>("MET_pt")?.read(..))?;
```

### A.22 The reader builder: projection, selection, sync and async

```rust
use rhizoid_io::{Projection, EntrySelection, BatchSize};

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
let meta = rhizoid_io::FileMetadata::load(&first_file)?;
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
| 4 | `rhizoid_io::create(path, WriteOptions::default(), …)` on every call | No boilerplate for defaults (`File::create` vs `OpenOptions`) | `create`/`update` without options; `create_with`/`update_with`; `File::open_with` |
| 5 | The scoped closure had to return `rhizoid_io::Result`, so `?` on the application's own errors did not compile inside it | Composability; `std::thread::scope` is generic in its result | Generic `Result<T, E>` with `E: From<rhizoid_io::Error>` |
| 6 | `ErrorKind` carried payloads, so matching needed `matches!(…(_))` and every new detail was a semver break | Follow `io::ErrorKind`; stable API | Fieldless `Copy` `ErrorKind`, details behind accessors and `Display`, `Error` one pointer wide |
| 7 | The `NTuple` name did not match what physicists search for (`RNTuple`) | Discoverability; STYLE §2.2 | `RNTuple`, `RNTupleWriter`, `file.rntuple(…)` |
| 8 | Rayon's global pool was used implicitly | Explicit over implicit; no hidden global state | Work runs in the caller's current pool; `ReadOptions::parallel(false)` |
| 9 | Two write APIs with no guidance on choosing | Learnability | A one-line rule: scoped unless the writer must outlive a call (§4.10) |
| 10 | Five ways to read data were presented as unrelated APIs | Progressive disclosure | The level ladder in §4.10, plus `rhizoid_io::prelude` |
| 11 | `mkdir` behaviour on an existing directory was unspecified | Least surprise | `create_dir_all` semantics; `put` adds a cycle, `put_replace` replaces |
| 12 | Async writing was undefined | Explicit contracts | Writing is blocking by design; documented, with `spawn_blocking` |
| 13 | `Cycle` was `NonZeroU16`, which admits values ROOT cannot store | Make invalid states unrepresentable | Constructed only for `1..=32767` |
| 14 | Whether `File` is generic over its source was not stated | Friendly types | Not generic over the source (type-erased at the I/O boundary). Since revision 6 it is generic only over sync/async, with `File` defaulting to sync (§4.10, D5) |
| 15 | The default features needed a C compiler, with no alternative | Pay for what you use | `pure-rust` feature set documented |

Checked and kept as they were:

- ~~Separate `File` and `AsyncFile`~~ (superseded in revision 6 by D5: one
  `File<I = Sync>` with `AsyncFile = File<Async>`, as parquet's builder
  does, with I/O methods generated by one internal `maybe_async!` macro).
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
| `tree.branch(name)?.read(range)` | reads the *B* baskets that overlap `range`; with coalescing, at most **1.25×** their bytes (gaps ≤ 64 KiB), never more (D8) | **1 per cluster** (vector read), or 1 per basket on servers without multi-range support |
| decoding anything | O(*n*) time; nesting depth, collection lengths and allocation bounded by `ReadLimits` | — |
| reading a column | peak memory O(output + one decompressed cluster per worker) | — |
| writing | O(data); peak memory O(one basket per branch, or one page per column, plus one cluster when flushing) | — |
| `File` clone, `Tree` clone | O(1), no I/O | 0 |
| `file.tree(name)` | decodes every branch's basket tables: memory O(branches × baskets), about 3 MB for a NanoAOD tree; inherent, as in ROOT | 1 |
| `read(..)` on a whole column | materializes the column: O(entries × item size). Prefer `reader()` batches; `ReadLimits::max_output_bytes` caps it on request (D10) | per cluster |
| update with `copy_on_write` | O(file size) copy, by design | — |

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
