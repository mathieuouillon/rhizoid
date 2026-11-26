# `rootfile` coding style

**Rule zero: when in doubt, do what the Rust standard library does.** If
`std` has a convention for something (naming, conversions, errors,
iterators, builders, documentation), we follow it, so that a Rust user can
guess our API without reading our docs.

This guide applies, in order of precedence:

1. The [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/).
   Rule IDs such as **C-CASE** refer to its checklist.
2. The [Rust Style Guide](https://doc.rust-lang.org/style-guide/), enforced
   by `rustfmt`.
3. Clippy, configured as in §12.
4. The project-specific rules below. They exist because we parse untrusted
   binary data and mirror a C++ format.

Tooling enforces every rule it can. Reviewers enforce the rest, citing the
section number.

---

## 1. Toolchain and formatting

- **Edition 2024**, `rust-version = "1.85"` in the workspace `Cargo.toml`.
  - The MSRV may rise in a minor release, never in a patch, and only to a
    toolchain at least six months old.
  - It never drops below what the design needs: `const { }` blocks (1.79),
    `#[diagnostic::on_unimplemented]` (1.78), edition 2024 (1.85).
- **`rustfmt` with default settings**, plus a minimal `rustfmt.toml` using
  stable options only:

  ```toml
  style_edition = "2024"
  newline_style = "Unix"
  use_field_init_shorthand = true
  use_try_shorthand = true
  ```

- CI runs `cargo fmt --all --check`. No `#[rustfmt::skip]` except on tables
  of byte constants, with a comment saying why.
- Imports come in three groups separated by a blank line: `std`/`core`/
  `alloc`, then external crates, then `crate::`/`super::`. One `use` per
  crate root, nested with braces.
- **No glob imports**, except a `prelude` module and `use super::*;` in
  `#[cfg(test)]` modules.

## 2. Naming

### 2.1 Case and words (C-CASE, RFC 430)

| Item | Convention | Example |
|---|---|---|
| Types, traits, enum variants | `UpperCamelCase`; acronyms are one word | `KeyHeader`, `Uuid`, `HttpSource`, `ObjectPath` |
| Functions, methods, modules, locals | `snake_case` | `read_ranges`, `key_list_range` |
| Constants, statics | `SCREAMING_SNAKE_CASE`, no `K_` prefix | `HEADER_PROBE_LEN`, `START_BIG_FILE` |
| Type parameters | short `UpperCamelCase` | `T`, `S: ReadAt`, `E: Row` |
| Lifetimes | short and meaningful | `'a`, `'s` (a write scope), `'f` (a file) |
| Features | plain nouns, no `use-`/`with-` prefixes (C-FEATURE) | `zstd`, `mmap`, `http`, `unstable-tree-writer` |
| Crates | `rootfile-*`, never `-rs` | `rootfile-core` |

### 2.2 Mirroring ROOT names

ROOT names follow C++ conventions: Hungarian `f`/`k` prefixes and
abbreviations. We translate them, and keep the original findable:

- **Types that model a ROOT class users work with keep ROOT's name**, because
  that is what users search for: `TH1D`, `TGraph`, `TAxis`, `TObjString`, `RNTuple` (and its `RNTupleWriter`).
  Types for our own concepts follow §2.1, even when ROOT has a class for the
  same idea: `KeyHeader`, not `TKey`.
- **Members drop the prefix and become `snake_case`.** The ROOT spelling goes
  in a doc alias, so rustdoc search finds it:

  ```rust
  /// Size of the key plus the name and title of the file, in bytes.
  #[doc(alias = "fNbytesName")]
  pub fn name_len(&self) -> ByteLen { … }
  ```

- ROOT constants become associated constants without the `k`:
  `kStartBigFile` → `FileOffset::START_BIG_FILE`, with
  `#[doc(alias = "kStartBigFile")]`.
- Where a ROOT class name collides with our own concept, our concept takes the
  clear name and the ROOT class gets an alias: the dictionary model is
  `StreamerInfo` with `#[doc(alias = "TStreamerInfo")]`.

### 2.3 Method names (as `std` does)

| Pattern | Meaning | Example |
|---|---|---|
| `new`, `with_*` | constructors (C-CTOR) | `RBuffer::new(data, origin)` |
| `open`, `create` | constructors that do I/O, as in `std::fs::File` | `File::open`, `WritableFile::create` |
| `from_*` | conversion constructors not covered by `From` | `File::from_source` |
| `as_*` | free, borrowed → borrowed (C-CONV) | `ObjectPath::as_str()` |
| `to_*` | expensive, or borrowed → owned | `to_dyn()` |
| `into_*` | consuming | `into_inner()`, `into_columns()` |
| no `get_` prefix | getters (C-GETTER) | `key.cycle()`, not `key.get_cycle()` |
| `get` | keyed lookup, as in `HashMap::get`; or the inner value of a newtype, as in `NonZeroU64::get` | `file.get::<TH1D>(path)`, `offset.get()` |
| `is_*`, `has_*` | `bool` queries | `is_large()`, `has_offsets()` |
| `try_*` | fallible variant of an infallible method | `try_reserve` |
| `iter`, `iter_mut`, `into_iter` | iteration (C-ITER) | `columns.iter()` |
| a plural noun | returns an iterator of that noun; the iterator type is named after it (C-ITER-TY) | `keys()` → `Keys<'_>`, `fields()` → `Fields<'_>` |
| `len`, `is_empty` | sizes, always together | |
| `finish`, `close` | consuming end of life, returns `Result` | `writer.finish()?` |

## 3. Types

- **Implement common traits eagerly** (C-COMMON-TRAITS):
  - `Debug` on *every* public type (C-DEBUG); never an empty `Debug`
    (C-DEBUG-NONEMPTY);
  - `Clone`, and `Copy` for small value types (`FileOffset`, `Compression`,
    `Cycle`);
  - `PartialEq`/`Eq`/`Hash` when equality is meaningful, `PartialOrd`/`Ord`
    when ordering is;
  - `Default` only when there is one obvious default (`ReadOptions`,
    `WriteOptions`);
  - `Display` for anything a user prints: paths, keys, errors, versions.
- **Conversions use the standard traits** (C-CONV-TRAITS):
  - `From`/`TryFrom` for type conversions, never ad-hoc `to_x`/`from_x` pairs;
  - `FromStr` for anything parsed from text (`ObjectPath`, `Compression` from
    `"zstd:5"`);
  - `AsRef<[u8]>`/`AsRef<Path>` on inputs.
- **Newtypes for every unit** (C-NEWTYPE): offsets, lengths, versions,
  cycles, checksums. A bare `u64` in a public signature needs a reason.
- **Private fields** (C-STRUCT-PRIVATE). A public struct with public fields is
  allowed only for plain data with no invariant, such as a returned summary.
  Structs that mirror on-disk records are `#[non_exhaustive]`.
- **Enums:** `#[non_exhaustive]` on every public enum that the format or the
  library may extend. Use an enum, never a `bool`, for options with two named
  states (`Strictness::Lenient`, not `strict: false`).
- **Builders** (C-BUILDER):
  - option structs (`ReadOptions`, `WriteOptions`) use consuming setters
    (`fn compression(self, c: Compression) -> Self`) and are `Clone`;
  - constructors with validation end in `build() -> Result<T>`;
  - every builder method is `#[must_use]`.
- **Sealed traits** (C-SEALED) for traits that encode format facts (`Record`,
  `NativeType`):

  ```rust
  mod sealed { pub trait Sealed {} }
  pub trait Record: sealed::Sealed { const KEY_FORM: KeyForm; }
  ```

- **Generic inputs, concrete outputs** (C-GENERIC):
  - accept `impl AsRef<Path>`, `impl Into<String>`, `impl IntoIterator`
    where `std` would;
  - return concrete types or `impl Iterator`, never `Box<dyn …>` unless the
    type must be object-safe.
- **`Send` and `Sync`** (C-SEND-SYNC): public types are `Send + Sync`
  whenever possible. A test asserts it, so a regression fails to compile:

  ```rust
  #[allow(dead_code)]
  fn assert_thread_safe() {
      fn is<T: Send + Sync>() {}
      is::<File>(); is::<Tree>(); is::<RNTuple>(); is::<FileScope<'_>>();
  }
  ```

- **`#[must_use]`** on:
  - writers and handles that must be finished;
  - pure functions whose result is the point (`Compression::new`,
    `ObjectPath::parse`);
  - `Result`-like custom types.

## 4. Errors and panics

- **Library errors** (C-GOOD-ERR):
  - error types implement `std::error::Error + Send + Sync + 'static`, with
    `source()` chaining to the cause;
  - they are defined with `thiserror`;
  - the public API never uses `Box<dyn Error>` or `anyhow`. Those are for the
    CLI.
- **Messages follow `std`'s style:**
  - lowercase, no trailing punctuation, concise;
  - they state what happened, not what to do:
    `unexpected end of buffer (needed 8, had 3)`.

  Context (offset, key, class) is added by the `Context` chain, not by
  formatting it into the message.
- **`pub type Result<T, E = Error> = std::result::Result<T, E>;`** in each
  crate, re-exported by the facade.
- **The library does not panic on input.** Malformed bytes, hostile lengths,
  unknown classes and I/O failures all produce `Err`.
  - `unwrap()` is denied in library code.
  - `expect()` is allowed only for internal invariants. Its message states
    the invariant, not the failure:
    `expect("key list is built from validated headers")`.
  - `panic!`, `unreachable!`, `todo!` and `unimplemented!` are denied in
    library code; `unreachable!` is allowed only with a comment proving it.
- A public function that can panic on *caller* misuse documents it under
  `# Panics`. Prefer making the misuse unrepresentable (ARCHITECTURE §5).

## 5. Untrusted binary data (project-specific)

Every byte comes from a file we did not write.

- **No slice indexing in parsers.** Use `RBuffer`/`LeBuffer`, or
  `slice.get(range)`. `clippy::indexing_slicing` is denied in
  `rootfile-core`, `rootfile-compress`, `rootfile-streamer`,
  `rootfile-columnar`, `rootfile-tree` and `rootfile-ntuple`.
- **No `as` casts on numbers.** Use `From` for widening, `TryFrom` for
  narrowing and sign changes, and map the error to `ErrorKind::Malformed`.
  `clippy::cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`
  and `cast_lossless` are denied.
- **Checked arithmetic on values from the file**: `checked_add`, `checked_mul`
  and `checked_sub`, never `+` on two untrusted numbers.
  `clippy::arithmetic_side_effects` warns in the parsing crates.
- **Allocate after validating.** A length read from the file is checked
  against the remaining buffer and `ReadLimits` *before* `Vec::try_reserve`.
  Never `Vec::with_capacity(untrusted)`.
- **Bounded recursion.** Every recursive decode takes a depth counter checked
  against `max_nesting_depth`.
- **Endianness is explicit.** Use `u32::from_be_bytes` and
  `u32::from_le_bytes`, never `from_ne_bytes`, and never transmute.
- **Cite the source of format facts** in a comment next to the code:
  `// [RB§6] checksum present iff version <= 0 and bytecount >= 6`. A rule
  without a source is a bug report waiting to happen.

## 6. `unsafe`

`#![forbid(unsafe_code)]` workspace-wide. The only exception path is a
dependency (`memmap2`, `zstd`, `liblzma`) behind a feature. If a measured
bottleneck ever seems to need `unsafe`:

1. use a safe crate (`bytemuck`, `zerocopy`) first;
2. otherwise get a design review, add a `// SAFETY:` comment on every block,
   and test it under Miri.

## 7. Documentation

- **Every public item is documented** (`missing_docs` warns; CI turns warnings
  into errors).
  - Each module explains its role in the architecture.
- **Crate-level docs teach**, in the outline BurntSushi uses for `regex`
  (C-CRATE-DOC). In this order:
  1. **Overview:** what the crate does, in three sentences.
  2. **Usage:** the `Cargo.toml` line.
  3. **Examples:** the five most common tasks, each runnable. For the facade,
     these are the ladder levels of ARCHITECTURE §4.10.
  4. **Crate features:** every feature, what it enables, and what it costs
     (dependencies, C compiler, compile time).
  5. **Untrusted input:** what is guaranteed on hostile files (`ReadLimits`,
     no panics).
  6. **Performance:** the contract and guide from ARCHITECTURE Appendix C.
  7. **Minimum supported Rust version** and its policy.

  Long-form guides (schema evolution, writing TTrees, remote reading) are
  `#[doc = include_str!("../docs/….md")]` modules under
  `rootfile::_guide`, the pattern `clap::_derive::_tutorial` uses, so they
  are versioned and tested with the code.
- **`examples/` mirrors the docs.** Every recipe in the crate docs and in
  ARCHITECTURE Appendix A is a program in `examples/`, built and run in CI
  (serde, regex and axum all ship runnable examples).
- The **first line is one sentence**, in the third person, ending with a
  period: `/// Reads the key list of this directory.`
- **Standard sections, in this order:** `# Examples`, `# Errors` (C-FAILURE:
  every `Result`-returning function), `# Panics`, `# Performance` (for
  anything that does I/O or is O(n) where O(1) might be assumed).
- **Examples use `?`, not `unwrap()`** (C-QUESTION-MARK). They hide the
  `fn main() -> rootfile::Result<()>` boilerplate with `#`, and compile as
  doc-tests. Examples that need a file use `rootfile::testing::sample()`.
- **Intra-doc links** for every type and method mentioned (C-LINK):
  ``[`File::open`]``.
- **Feature-gated items** carry
  `#[cfg_attr(docsrs, doc(cfg(feature = "zstd")))]`, and docs.rs builds with
  `--cfg docsrs` and all features.
- **ROOT names** are reachable through `#[doc(alias = "…")]` (§2.2). The docs
  of a format type link to the section of `ROOT File Format.md` or
  `ROOT-BEHAVIOUR.md` it implements.
- Comments explain *why*. Code that needs a comment to say *what* it does is
  rewritten instead.

## 8. Modules, visibility, re-exports

- **`foo.rs` + `foo/`**, not `foo/mod.rs` (the 2018+ layout).
- **The narrowest visibility works:** private, then `pub(crate)`, then `pub`.
  `unreachable_pub` warns, so items that are not reachable from the crate
  root are not marked `pub`.
- **Re-exports are explicit and item by item.**
  - The facade never re-exports an internal crate wholesale.
  - Internal crates are `#[doc(hidden)]` at the facade level and say "no
    stability guarantee" in their crate docs (ARCHITECTURE §3).
- **One concept per module.** A module named after a ROOT structure contains
  its `decode`, `encode`, and their round-trip test.
- **No public dependency types in the facade** except `bytes::Bytes`. Types
  from `arrow` and `object_store` live in their adapter crates (C-STABLE).

## 9. Performance idioms

- **Borrow in, own out:** take `&[u8]`/`&str`/`&T`; return owned values or
  `Cow` when a copy is sometimes avoidable.
- **Iterators over index loops; bulk over per-element.**
  `chunks_exact(4).map(u32::from_be_bytes…)` vectorizes; a loop of
  `RBuffer::read_u32` calls does not.
- **No allocation in hot loops.** Reuse buffers (`clear()` + `extend`) per
  worker.
- **`#[inline]`** only on small public functions called across crates in hot
  paths, and only with a benchmark showing the gain. Never `#[inline(always)]`
  without one.
- **No `Rc`, `RefCell` or `Mutex` in public signatures.** Interior
  mutability stays an implementation detail (the `FileScope` sink).
- **Every performance claim in a PR comes with a `divan` benchmark result.**
- **Generic outer, non-generic inner** (as `std::fs::read` does). A generic
  public function with a non-trivial body converts its arguments and calls a
  private non-generic function, so the body is compiled once, not once per
  caller type:

  ```rust
  pub fn open(path: impl AsRef<Path>) -> Result<File> {
      fn inner(path: &Path) -> Result<File> { /* the real work */ }
      inner(path.as_ref())
  }
  ```

- **Generics only where they pay** (kurbo's lesson). Concrete types for
  concrete ROOT classes (`TH1D`, not `TH1<f64>`), and no type parameter on
  `File`. Generics are for what users choose per call site: column types,
  row types, sources at construction.

## 10. Async

- **Runtime-agnostic:** library code never names `tokio`, except inside the
  `http` feature's implementation.
- **Public async traits return `BoxFuture`** (and `LocalBoxFuture` in the
  `Local*` variant), as parquet's `AsyncFileReader` does. They stay
  dyn-compatible, and their futures can be stored in a struct and polled by
  hand. `async fn` in traits is for private traits only.
- **Async functions never block.** CPU-bound work goes to `rayon`, or to the
  caller's `spawn_blocking`.
- **Streams carry an explicit state enum** (`Idle { input } | InFlight { future } | Done`),
  in which the input moves into the request future and comes back with the
  bytes, as parquet's `RequestState` does. No hidden `Mutex` around the
  source.
- **Cancellation safety:** dropping a future of ours never leaves shared
  state inconsistent. The docs of each async method say so, under
  `# Cancel safety`, as `tokio` documents it.

## 11. Tests

- **Unit tests** go in the same file (`#[cfg(test)] mod tests`). **Integration
  tests** go in `tests/` and use only the public API.
- **Names describe behaviour**, not methods:
  `decodes_large_key_with_pid_offset`, not `test_key_2`.
- **Every `decode` has a `proptest` round trip with its `encode`.** Every bug
  fix starts with a failing test, often a fuzz crash reproducer.
- **Snapshots** (`insta`) for dumps of corpus files; review snapshot changes
  like code.
- **Compile-time guarantees** get a `trybuild` case in `tests/ui/`
  (ARCHITECTURE §5.6).
- **No network in tests.** The corpus is fetched by `cargo xtask corpus`.
  Tests skip, with a message, when it is absent, and CI always has it.

## 12. Lints

In the workspace `Cargo.toml`. Crates opt in with `[lints] workspace = true`.

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"
missing_debug_implementations = "warn"
unreachable_pub = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }
unused_qualifications = "warn"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
# panics and shortcuts in library code
unwrap_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
dbg_macro = "deny"
print_stdout = "deny"
print_stderr = "deny"
# numeric safety on untrusted data
cast_possible_truncation = "deny"
cast_sign_loss = "deny"
cast_possible_wrap = "deny"
cast_lossless = "deny"
# pedantic lints we deliberately relax
must_use_candidate = "allow"      # §3 lists where #[must_use] goes
module_name_repetitions = "allow" # `key::KeyHeader` reads better than `key::Header`
```

- The parsing crates add `#![deny(clippy::indexing_slicing)]` and
  `#![warn(clippy::arithmetic_side_effects)]` at their roots.
- The CLI and `xtask` relax `print_stdout`, `print_stderr` and `unwrap_used`.
- CI runs `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- A local `#[allow(…)]` needs a comment with the reason, or
  `#[expect(…, reason = "…")]` (Rust 1.81+), which also fails when the lint
  no longer fires.

## 13. Dependencies and API evolution

- **Dependencies are declared once** in `[workspace.dependencies]`, with
  `default-features = false` and only the features used.
- **A new dependency needs a line in the PR** saying why `std` or an existing
  dependency doesn't do. `cargo-deny` checks licences, duplicates and
  advisories.
- **Public API changes:**
  - are recorded in `CHANGELOG.md` ("Keep a Changelog" format);
  - are checked by `cargo-semver-checks`;
  - removals go through `#[deprecated(since = "x.y.z", note = "use … instead")]`
    for at least one minor release.
- **New trait methods ship with a default body.** New enum variants are
  covered by `#[non_exhaustive]`.

## 14. A type done right

```rust
use core::fmt;

/// A position in a ROOT file, in bytes from the start.
///
/// Whether it is stored in 4 or 8 bytes is decided when a record is encoded;
/// see [`KeyForm`].
#[doc(alias = "fSeekKey")]
#[doc(alias = "Long64_t")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)] // no Default: no offset is "obvious"
pub struct FileOffset(u64);

impl FileOffset {
    /// Offset of the first record of every ROOT file.
    #[doc(alias = "kBEGIN")]
    pub const BEGIN: Self = Self(100);

    /// Past this offset, records use 8-byte offsets ([RB§4]).
    #[doc(alias = "kStartBigFile")]
    pub const START_BIG_FILE: Self = Self(2_000_000_000);

    /// Creates an offset.
    #[must_use]
    pub const fn new(bytes: u64) -> Self { Self(bytes) }

    /// Returns the offset as a number of bytes.
    #[must_use]
    pub const fn get(self) -> u64 { self.0 }

    /// Returns `self + len`, or `None` on overflow.
    #[must_use]
    #[expect(clippy::cast_lossless, reason = "u32 → u64 is lossless; `From` is not const")]
    pub const fn checked_add(self, len: ByteLen) -> Option<Self> {
        match self.0.checked_add(len.get() as u64) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Returns `true` if a record here must use the 8-byte form.
    #[must_use]
    pub const fn is_large(self) -> bool { self.0 > Self::START_BIG_FILE.0 }
}

impl fmt::Display for FileOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl From<FileOffset> for u64 {
    fn from(o: FileOffset) -> u64 { o.0 }
}
```

- `ByteLen` has a `u32` inside. The `as u64` in `checked_add` is allowed only
  because `From` is not `const`, and the `#[expect]` records why. If the cast
  ever stops being lossless, the build breaks.
- The type is `Copy`, ordered, hashable and printable.
- It has no public field, uses `get()` as `NonZeroU64` does, carries the ROOT
  aliases, and cites its source.

## 15. Procedural macros (after serde and thiserror)

`rootfile-derive` holds every proc macro. They follow dtolnay's practices:

- **Same name as the trait.** `#[derive(RootObject)]` implements
  `RootObject`, and `#[derive(Row)]` implements `Row`. The facade re-exports
  each derive next to its trait behind the default `derive` feature, so one
  `use rootfile::Row;` brings both.
- **One attribute namespace:** `#[root(…)]` on containers and fields, with
  serde's vocabulary where it fits (`rename`, `skip`, `default`, `flatten`).
  Users who know serde guess right.
- **Generated code names nothing the user did not import.** Paths are
  absolute and go through `::rootfile::__private` (a `#[doc(hidden)] pub mod`
  re-exporting what the expansion needs), never through the internal crates
  directly. `#[root(crate = "path::to::rootfile")]` redirects them for
  frameworks that re-export us, as `#[serde(crate = "…")]` does.
- **Errors point at the user's code.** Diagnostics are `syn::Error`s spanned
  on the offending field or attribute, and every error is collected before
  failing (`syn::Error::combine`), so one build shows them all. No `panic!`
  in a macro.
- **Trait-bound errors are written for humans:**
  `#[diagnostic::on_unimplemented]` on `RootType`, `CountType`, `NativeType`
  and `Row`.
- **Expansions are tested** with `trybuild`: one `pass` and one `fail` case
  per attribute, with `.stderr` snapshots of the messages.
- **Minimal dependencies:** `syn` with only the features the derive uses,
  `quote`, `proc-macro2`. Nothing else, to keep the first build fast.
- **The derive is optional.** Every derived trait can be implemented by hand,
  and the docs show how. Tuples implement `Row` without any macro (axum's
  "no macro magic" principle).

## 16. Reading list

When a design question is not answered here, look at how these answer it,
in this order:

| Question | Reference |
|---|---|
| Naming, ownership, trait conventions | `std`; the Rust API Guidelines |
| Trait design, derives, data model vs format | `serde`, `serde_derive` |
| Crate splitting, docs, test suites, performance contracts | `regex`, `regex-automata`, `regex-syntax` (BurntSushi) |
| Error types | `thiserror` (libraries), `anyhow` (applications) |
| Parallel APIs | `rayon` |
| Ergonomic typed extraction; a stable core crate for implementers | `axum`, `axum-core` |
| Scope discipline; concrete over generic; compile times | `kurbo` |
| Binary formats in Rust | `arrow-rs`, `parquet` (sync/async reader split), `object_store` |

## 17. Code-level rules taken from the reference crates' source

These come from reading the code of serde, regex, regex-automata, rayon, axum,
anyhow, thiserror and parquet (ARCHITECTURE §4.12).

**Builders and options**
- **Setters use bare names**, as `std`'s `OpenOptions`, `Command` and
  `thread::Builder` do: `.projection(…)`, `.batch_size(…)`, not
  `.with_projection(…)`. The `with_` prefix is for constructors
  (`Vec::with_capacity`, `File::from_source_with_metadata`).
- **Every setter's doc states its default** in one sentence, "The default is
  …", as every regex builder method does.
- **One builder, many inputs.** When an API exists for sync and async (or
  several sources), write one builder generic over a private input newtype,
  and expose type aliases. Never two copies of the setters (parquet).
- **Accept `impl Into<Config>`** for per-call configuration, as regex accepts
  `impl Into<Input>`. Plain values (`0..100`, `..`, `&[u64]`, `["a", "b"]`)
  work without ceremony. Validate in the setter or the `From` impl, never
  deep inside the read.

**State**
- **Immutable shared state behind `Arc`; mutable scratch separate.**
  - Scratch comes from a `Pool`.
  - An explicit-scratch variant is named `*_with(&mut Scratch, …)`, after
    `search_with(&mut Cache, …)`.
  - Cloning a handle clones the `Arc`, never the data.

**Errors**
- Error constructors are `#[cold]`, and forwarding defaults are `#[inline]`.
- **The public error is opaque** (a struct over a private representation)
  with a `Copy` `kind()`; details come through accessors and `Display`.
- **`Debug` on the error is hand-written** to read well in `unwrap()`
  output: message, context lines, offset. No derived struct dump.
- **`Error::custom(impl Display)`** is the one way for user trait impls to
  fail.

**Traits**
- **Methods added later** get a default that forwards or returns
  `ErrorKind::Unsupported`.
- **Public async traits return `BoxFuture`** (§10).
- **Diagnostics:**
  - every trait users implement or use as a bound carries
    `#[diagnostic::on_unimplemented(note = "… see <docs URL>")]`;
  - every blanket or macro-generated impl carries
    `#[diagnostic::do_not_recommend]`.
- **Tuple impls** go through one `all_the_tuples!` macro, with one line per
  arity under `#[rustfmt::skip]`, up to 16.
- **Parallel iterators by delegation.** Implement
  `IntoParallelIterator for &T` by delegating to a slice or a `usize` range;
  write a `Producer` only with a benchmark showing the need. Index
  parallel work with `usize`. Never return `Box<dyn ParallelIterator>`.

**Macros** (in addition to §15)
- **Collect errors in a `Ctxt`** whose `Drop` panics if `check()` was never
  called, except while already panicking.
- **One `Attr<T>` slot per option**, reporting duplicates at the second
  span.
- **Wrap output in `const _: () = { extern crate rootfile as _rootfile; … };`**
  with `#[automatically_derived]` on impls. Generated code reaches `Result`,
  `Option` and `Vec` through `_rootfile::__private`, so user shadowing cannot
  break it.

**Docs and tests**
- **The README is a doc-test:**
  `#[cfg(doctest)] #[doc = include_str!("../README.md")] struct ReadmeDoctests;`.
- **Doc examples end with a hidden `# Ok::<(), rootfile::Error>(())`**, so `?`
  works without a visible `main`.
- **Crate-level `#![allow(clippy::…)]` lists are grouped under a comment
  giving the reason**, as serde's are. A single item uses
  `#[expect(…, reason = "…")]`.
- **No `build.rs`.** serde needs one to probe compiler versions; we declare
  `rust-version` instead and avoid the build-time cost.
