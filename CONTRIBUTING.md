# Contributing

Thank you for helping. rhizoid reads files other programs wrote and writes
files other programs read, so a change is done when ROOT and uproot agree
with it, not only when it compiles.

## Before a pull request

```text
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI also runs, on every pull request: the tests on Linux, macOS and
Windows, the uproot and ROOT cross-checks, WASM, the minimum Rust version
(1.89), licences and advisories (`cargo-deny`), the public API against the
latest release (`cargo-semver-checks`), the fuzz findings, and a line
coverage floor (`cargo llvm-cov`). Nightly, it adds the ROOT 6.26 to
latest matrix, the coverage against ROOT, the crash harness, fuzzing and
benchmarks.

### The cross-checks

- **uproot:** set `RHIZOID_ORACLE_PYTHON` to a Python with
  `uproot numpy awkward lz4 zstandard xxhash`.
- **ROOT:** set `RHIZOID_ROOT=root` with ROOT 6.26 or later on the `PATH`.
- **The corpus:** `python tests/cross/fetch_corpus.py corpus` (~44 MB), then
  `RHIZOID_CORPUS=corpus`.

On Windows, `tools/wsl-setup.sh` installs everything (ROOT included) in
WSL, and `tools/wsl-check.sh` runs every check there, the corpus tests
included when the corpus is in `./corpus` (or `RHIZOID_CORPUS`); without
it, the check says what it skipped. Run them before changing a reader: the
corpus holds encodings the fixtures do not.

### Fuzzing

With a nightly toolchain and `cargo install cargo-fuzz`:

```text
cargo +nightly fuzz run open_file -- -max_total_time=300
```

An input that crashes a target goes to `crates/rhizoid-io/tests/data/fuzz/`,
named after what it hit, with the fix; it is then replayed on every CI run.

### Mutation testing

`cargo install cargo-mutants`. CI mutates the lines a pull request
changes, and fails on a mutant no test catches. Locally, for one file:

```text
cargo mutants -p rhizoid-io -f crates/rhizoid-io/src/tree/write.rs
```

A surviving mutant needs a test that catches it, unless it cannot change
what the library does (a `Debug` impl, a fast path writing the same bytes
as the general one): those are excluded in `.cargo/mutants.toml`, each
with why.

## Style

- **Rust names, not ROOT's.** Types, fields and constants follow the
  standard library's conventions: `Axis`, `Hist1<f64>`, `START_BIG_FILE`;
  never ROOT's `T`, `f` or `k` prefixes. ROOT's names appear only as the
  file-format data they are: class names and member names in strings
  (`"TH1D"`, `"fNbins"`). Docs may say which ROOT class a type models.
- **No panics on input.** No `unwrap`, no indexing that can go out of
  bounds, no unchecked arithmetic on lengths from a file: return an
  `Error` with the right `ErrorKind`. Clippy enforces most of it.
- **Tests with the change:** a test that fails without it. A bug found on
  some input gets that input as a test.
- **Comments say why**, in full sentences; the code says what.

## Compatibility

The 1.x crates follow semver: no breaking change in a minor release
(`cargo-semver-checks` guards it). Add, deprecate, then remove at the next
major. `rhizoid-arrow` and `rhizoid-object-store` follow their dependency's
majors. Note user-visible changes in `CHANGELOG.md` under the next version.

## Releases

Releases are prepared by release-plz (`.github/workflows/release-plz.yml`):
it opens a pull request that bumps versions from the changes since the last
tag; merging it tags the release, and publishes to crates.io once the
repository has a `CARGO_REGISTRY_TOKEN` secret.

By contributing you agree that your contribution is licensed under the
project's MIT license.
