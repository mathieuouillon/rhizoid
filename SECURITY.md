# Security

rhizoid parses files that may come from anywhere: a file from the web, a
colleague or an experiment's storage must not be able to crash, hang or
exhaust the program reading it.

## What rhizoid promises

For any input bytes, reading with `rhizoid-io`:

- never panics and never aborts;
- uses no `unsafe` code (`#![forbid(unsafe_code)]` in every crate);
- checks every length and count read from a file against `ReadLimits`
  before allocating (`ReadLimits::untrusted()` for files you do not
  trust), and fails with `LimitExceeded` rather than allocating what the
  file claims;
- verifies the checksums ROOT stores (LZ4 blocks, `RNTuple` envelopes and
  pages).

These hold for every decoder, which are fuzzed every night (`fuzz/`); each
input that ever broke one is replayed on every CI run
(`crates/rhizoid-io/tests/fuzz_regressions.rs`).

A crash, hang, abort, unbounded allocation or out-of-bounds read on some
input is a security bug. So is a file written by rhizoid that another
reader (ROOT, uproot) misreads in a way an attacker could steer.

## Reporting a vulnerability

Report it privately through GitHub: **Security → Report a vulnerability**
on the repository (private vulnerability reporting). Please include the
input file, or the code that builds it, and the rhizoid version. Do not
open a public issue for it.

You will get an answer within a week. A fix is released as a patch
version of each supported release line, with an advisory once users have
had time to update.

## Supported versions

The latest minor release of each crate gets security fixes.

| Crate | Supported |
|---|---|
| `rhizoid-io`, `rhizoid-derive`, `rhizoid-source`, `rhizoid-cli` | 1.1.x |
| `rhizoid-arrow`, `rhizoid-object-store` | 0.1.x |
