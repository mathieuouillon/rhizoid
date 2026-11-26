# Council transcript — `rootfile` architecture review

2026-11-25 · Method: LLM Council (5 advisors → anonymous peer review → chairman)

## Original question

> Review the architecture using [the LLM Council skill]. Also check what ROOT
> writes and reads into a file and how. I want the power of Rust so when
> possible it should fail at compile time.

## Context given to every advisor

- `ARCHITECTURE.md` (the proposal, before this review)
- `ROOT-BEHAVIOUR.md` (ROOT's actual read/write behaviour, checked in ROOT's
  sources the same day)
- `ROOT File Format.md` (the format reference)

## Framed question

A developer is designing a reader AND writer for CERN ROOT files (TFile
container, streamed objects such as histograms and user classes, TTree,
RNTuple) in Rust. Their stated goals: follow the conventions of the Rust
standard library and current best practice, use the best libraries, be "smart
and future proof", and — a new, explicit requirement — use the power of Rust
so that, whenever possible, errors are caught at COMPILE TIME rather than at
run time. The proposed architecture is in ARCHITECTURE.md; ROOT-BEHAVIOUR.md
lists what ROOT actually does on disk, including corner cases the architecture
may not yet handle.

Decision to make: is this architecture the right one to commit to before
writing code, what must change first, and which parts should be redesigned so
that misuse of the API and format-level mistakes become compile-time errors
(type system, typestate, newtypes, traits, const generics, macros), without
making the library unusable for files whose schema is only known at run time?

Stakes: this is the foundation of a long-lived public library. Wrong layering
or public API choices are expensive to undo after release (semver). Output that
ROOT or uproot cannot read makes the writer useless; a reader that panics or
misreads real files makes the library untrustworthy.

---

## Advisor responses

### The Contrarian

Don't commit yet. Several sections contradict ROOT-BEHAVIOUR.md, and the
"compile-time safety" claim rests on one type split (`File`/`WritableFile`).

**Format errors that would corrupt reads or writes**

- `RBuffer` has no origin. Class tags and back-references count from the key
  header (`KeyLen` offset, ROOT-BEHAVIOUR §6). As drawn in §4.1, every `TList`
  back-reference and every basket entry offset resolves to the wrong byte.
  Make origin a required constructor argument.
- "Fail loudly" (§2) breaks real files. ROOT logs a byte-count mismatch and
  repositions; a reader that errors there rejects files ROOT opens.
  `Strictness::Lenient` has to be the default, and §4.1 needs to say so.
- The `Allocator` (§4.1) does not match ROOT: first-fit vs ROOT's best-fit, no
  negative-`i32` gap markers, no initial `[100, 2e9]` free segment. The
  `TFile::Recover` CI check would only catch this at M4.
- The key form is chosen by the wrong rule. `Seek` says the encoder decides
  from the record's version; in fact baskets are always large and `RBlob` is
  always 1004. Encode the form as a type parameter on `KeyHeader<Small|Large>`,
  chosen by record kind.
- Bare `"rntuple"` files cannot pass through `File::open`; NTuple must not
  depend on the TFile facade.

**API holes the type system should close**

- `options().update(true).open()` cannot have one return type. Use separate
  `open` / `create` / `open_update`, each returning a distinct type.
- Child writers don't borrow the file. `create_ntuple` should return
  `NTupleWriter<'f>` borrowing `&mut WritableFile`, so calling `out.close()`
  before `w.finish()` is a compile error. Today it is a silently truncated tree.
- `Drop` doing I/O is dangerous. If a panic happens mid-write, `Drop` writes a
  header that makes a half-written file look valid. `Drop` should do nothing
  (ROOT can recover the file), `close(self)` should consume the writer, and
  the writer should be `#[must_use]`.
- Invalid values can be built: `Compression { pub level: u8 }` allows 250;
  `CLASS_VERSION: i16` disagrees with `class_version: i32`;
  `header_bytes: usize` should be a validated type.

**Layering:** `ReadPlan`/`Column` are "shared" by tree and ntuple, but those
crates must not depend on each other, so no crate holds the shared code. Fix
this, and merge crates before semver locks them in.

### The First Principles Thinker

The goal is framed wrongly. The two failures that would sink this library are
"ROOT can't read what we wrote" and "we misread a real file". The compiler
can't catch either: the input is bytes, and the schema comes from
`TStreamerInfo` at run time. Only the corpus and the ROOT/uproot cross-checks
in §8 can show them. So the compile-time push should target the writer API
and the derive macro, and nothing else.

**Commit to** the layering (sans-IO core, `ReadAt`/`AsyncReadAt`, separate
`File`/`WritableFile`).

**Fix before any code, because the layout is wrong today**

- `RBuffer` has no origin. Tags, back-references and basket entry offsets
  count from `fKeylen` (ROOT-BEHAVIOUR §6, §7).
- `Seek`'s size should come from the record kind, not the record's version:
  baskets and `RBlob` are always large. Put it in
  `trait Record { const KEY_FORM: KeyForm; }`. Do not make it a const-generic
  `Seek<LARGE>`, because crossing `kStartBigFile` is only known at run time.
- Compress only if `ObjLen > 256`, and store raw if any block fails to shrink.
- Byte-count mismatches should be handled leniently by default.
- Readers follow pointers, not class names (`"TDirectory"`, `""`).

**Where the compiler can actually help**

- Writers borrow from the file: `create_ntuple` and `mkdir` return values tied
  to `&mut WritableFile`, so `out.close()` won't compile while a writer is
  alive. `'static` Arc handles are fine for reading; on the write side they
  throw this guarantee away.
- `close(self)` and `finish(self)` take ownership and are `#[must_use]`.
  Delete the best-effort close in `Drop`: it hides data loss.
- `.update(true)` is a run-time flag. Replace it with
  `File::open_update() -> WritableFile`.
- `begin_object` returns a guard that must be consumed by `end_object`.
- The derive rejects at compile time: a `count = "n"` field that is not an
  integer or is declared after the array, Rust types with no ROOT mapping,
  and duplicate member names. It computes the checksum when it builds
  `StreamerInfo`.
- Unify the version types (`i16` vs `i32`).

**Do not commit to writing TTrees:** ROOT-BEHAVIOUR §8 says the `TTree`
member list hasn't been checked yet.

### The Expansionist

Commit. The sans-IO core, the derive/dynamic split (§4.4) and the shared
`ReadPlan` contain something bigger than the document claims: one schema
system that can be checked at compile time. Make that the centre of the design.

1. **"sqlx for ROOT".** `xtask gen-models` (§4.6) already turns streamer
   infos into Rust structs. Release it as a product: a `rootfile-build` crate
   for `build.rs` plus a `root_schema!("ref.root", "Events")` macro. Users get
   typed `Events`/`MyEvent` structs, with compile-time checks of branch names,
   leaf types and the `//[fN]` count member. Run-time schemas still go
   through `DynObject`.
2. **One `RootSchema` trait instead of three schema paths**, with two
   implementors: derived structs (checked at compile time) and `DynSchema`
   (checked at run time). `create_tree::<E>`, `create_ntuple::<E>`,
   `tree.read::<E>()` and the Arrow conversion share the same code. Typed
   projections: `tree.columns::<(Px, JetPt)>()`.
3. **Typestate where the format has rules:** `KeyHeader<K: RecordKind>` with
   `const ALWAYS_LARGE`; `RBuffer` stores its origin at construction;
   `TreeWriter<'f>` borrows `&mut WritableFile`;
   `Compression::new::<ALG, LEVEL>()` with a const assertion.
4. **Bare RNTuple and `PageSink` are a lakehouse format:** a DataFusion
   `TableProvider` and a Polars IO plugin over `object_store`.
5. **Move `rootfile-py` from M6 to M2:** handing Arrow to uproot users through
   the PyCapsule interface is how the library gets adopted.

### The Outsider

1. **The documents disagree, and the reader can't tell which one wins.**
   - §2 says "fail loudly"; ROOT-BEHAVIOUR §10 says `Lenient` is the default.
     `Strictness` does not appear in ARCHITECTURE.
   - The allocator is first-fit; ROOT uses best-fit and writes gap markers.
   - The cache reads 4 KiB; ROOT reads 300 bytes.
   - Bare `"rntuple"` files, `RBuffer`'s origin and the ≤ 256-byte rule are
     missing.

   Merge §10 into the architecture before writing any code.
2. **One concept has three types.** A class version is `i16` / `i32` / `i32`.
   Offsets are `Seek(u64)` in one place and bare `u64` / `Range<u64>`
   elsewhere. Add newtypes `ClassVersion`, `Cycle`, `FileOffset`, `ByteLen`,
   `KeyRelOffset`.
3. **The compile-time claims are thinner than they look.**
   - `update(true)`: a run-time bool can't pick the return type.
   - `Compression { pub level: u8 }` accepts 250.
   - `#[root(count = "n")]` is an unchecked string.
   - `Drop`'s best-effort close leaves half-written files; use `#[must_use]`
     and make `Drop` write nothing.
   - Whether `out.put()` works while an ntuple writer is alive must be
     encoded in the borrow.
4. **Jargon nobody defines** (fBEGIN, kStartBigFile, cycle, basket, cluster,
   epoch, member-wise), and the `[§n]` references point to a file that isn't
   at hand. Add a glossary.
5. **A string encodes its own format.** `get("dir/hist;2")` is a typo waiting
   to happen. Make `ObjectPath` the main argument type, with `Cycle` as its
   own type.

`DynObject` stays the escape hatch.

### The Executor

Commit after one week of changes on paper. M0 has no writer, so nothing
proves ROOT can read our output until M1.

**Monday, in order:**

1. Settle what the writer claims to be (`fVersion`) and verify the `TTree`
   member list.
2. Write the §10 corner cases into §4.1:
   - `RBuffer` origin
   - always-large record kinds
   - the compression rule
   - the `SeekPdir` PID mask
   - gap markers
   - `Lenient` as a documented exception
   - bare-`rntuple` `Container` enum
3. Publish only the `rootfile` facade; the other crates are `publish = false`
   until their APIs settle.
4. Move the cross-check to M0: write an empty file, then ROOT
   `TFile::Recover` and uproot must open it cleanly.

**Compile-time, cheapest first:**

- `WBuffer::object(version, |w| …)` closure
- `Seek<Small>`/`Seek<Large>` from `KeyKind` markers
- `close(self)` + `#[must_use]`
- a const fn `Compression::new` that rejects bad levels
- the derive checks types at expansion

**Run-time stays run-time:** `DynObject`, `Interpretation`, `get_dyn`. Typed
access is a layer over the dynamic path, not a replacement for it.

---

## Peer review

Each reviewer saw the five responses relabelled A–E in an independently
shuffled order. The mappings were:

| Reviewer | A | B | C | D | E |
|---|---|---|---|---|---|
| 1 | Outsider | Executor | Contrarian | Expansionist | First Principles |
| 2 | Outsider | Executor | Contrarian | Expansionist | First Principles |
| 3 | Contrarian | Expansionist | First Principles | Executor | Outsider |
| 4 | Expansionist | Outsider | First Principles | Executor | Contrarian |
| 5 | Executor | Contrarian | Outsider | First Principles | Expansionist |

Results, de-anonymized:

| Reviewer | Strongest | Biggest blind spot |
|---|---|---|
| 1 | First Principles (Contrarian close second) | Expansionist |
| 2 | First Principles | Expansionist |
| 3 | First Principles | Expansionist |
| 4 | First Principles (Contrarian close second) | Expansionist |
| 5 | First Principles | Expansionist |

### Reviewer 1

- **Strongest: First Principles.** It frames the problem right: the failures
  that matter are run-time facts. It aims the type system at the writer API,
  and catches that the key form can't be a const generic. The Contrarian is a
  close second.
- **Blind spot: Expansionist.** It adds scope while skipping the verified
  format bugs and the I/O in `Drop`. A macro built from one reference file
  ignores schema evolution.
- **All missed:**
  - Arrow, `object_store` and `bytes` in the public API mean every arrow-rs
    major release forces a semver break.
  - Porting `TClass::GetCheckSum` and copying ROOT's captured streamer infos
    may make the crate a derivative of ROOT's LGPL code.

### Reviewer 2

- **Strongest: First Principles.** It knows where compile-time checking stops
  and rejects a const-generic `Seek<LARGE>` for the right reason. One flaw: a
  "guard that must be consumed" does not exist in Rust, which has no linear
  types.
- **Blind spot: Expansionist.**
  - `Compression::new::<ALG, LEVEL>` needs enum const generics, which are
    unstable.
  - `root_schema!` checks only one reference file.
- **All missed:**
  - `#[must_use]` does not force `close`/`finish` once the value is bound with
    `let`. Closure scoping is the only real enforcement.
  - Borrowing `&mut WritableFile` prevents filling two trees at once, or
    writing histograms or calling `mkdir` while a tree is open. That is ROOT's
    commonest write pattern.

### Reviewer 3

- **Strongest: First Principles.** The key form is per record kind; a pure
  type parameter would encode ordinary keys wrong. It also warns about the
  unverified `TTree` members.
- **Blind spot: Expansionist.** It ignores the Lenient default, the allocator,
  the compression rule and the TTree members. Its const-generic compression
  also blocks the run-time choice that `cp` needs.
- **All missed:**
  - A per-writer `&mut` borrow blocks concurrent writers.
  - `AsyncReadAt` requires `Send` futures, but browser fetch futures are not
    `Send`, which contradicts the WASM goal.

### Reviewer 4

- **Strongest: First Principles** (Contrarian close second).
- **Blind spot: Expansionist.** Commits without fixing the contradictions and
  widens the semver surface. `root_schema!` proves nothing about the files it
  meets at run time.
- **All missed:**
  - Concurrent writers: Events/Runs/Lumi trees filled side by side,
    mid-loop saves, parallel RNTuple writing. Use a scoped API like
    `std::thread::scope`, or a shared sink whose `close` fails while children
    live.
  - `trybuild` compile-fail tests, so the claimed compile errors cannot
    silently regress.

### Reviewer 5

- **Strongest: First Principles.** The large-key switch depends on file
  position, so only baskets and `RBlob` can carry their key form as a type.
- **Blind spot: Expansionist.**
- **All missed:** semver hygiene.
  - `#[non_exhaustive]`
  - sealed traits (`RootClass`, `Codec`, `Record`)
  - `cargo-semver-checks`
  - keeping fast-moving dependencies (`arrow`, `object_store`, `bytes`) out of
    the stable facade

---

## Chairman's synthesis

### Where the Council Agrees

- **The layering is right.** The sans-IO core, the `ReadAt`/`AsyncReadAt`
  split, the `File`/`WritableFile` split and `DynObject` as the run-time path
  stay. The API surface has to change before any semver commitment.
- **ARCHITECTURE.md contradicts ROOT-BEHAVIOUR.md:**
  - The allocator is first-fit, has no gap markers and no initial free
    segment.
  - `RBuffer` has no origin.
  - "Fail loudly" conflicts with ROOT's leniency on byte counts.
  - Always-large keys, `RBlob` 1004, the ≤ 256-byte rule and bare `rntuple`
    files are missing.
- **The compile-time claims are thin today:**
  - `update(true)`
  - `pub level: u8`
  - `i16`/`i32` versions
  - I/O in `Drop`
- **The worst failures are run-time facts.** Only the corpus and the
  ROOT/uproot cross-checks catch them. Compile-time effort goes to the writer
  API and the derive macro.

### Where the Council Clashes

- **Key form as a type parameter** (Contrarian, Expansionist, Executor) **vs
  a per-kind constant** (First Principles and three reviewers).
  **Verdict:** `trait Record { const KEY_FORM: KeyForm }` with
  `AlwaysLarge | ByPosition`; `Seek` stays a run-time value.
- **The Expansionist's agenda.** All five reviewers called it the blind spot.
  **Verdict:** reject `root_schema!`, enum const generics, and the
  DataFusion/Polars work for 1.0. A u8 const-generic constructor with an
  inline `const { assert!() }` is stable and may sit beside a run-time
  `Compression::new(alg, level) -> Result`.
- **Writer lifetimes:** "borrow `&mut WritableFile`" vs the reviewers. The
  reviewers win.

### Blind Spots the Council Caught

- Per-writer `&mut` borrows break ROOT's most common write pattern.
- `#[must_use]` does not force `close()`. Rust has no linear types; closure
  scoping is the only real guarantee.
- `AsyncReadAt`'s `Send` futures contradict the WASM goal.
- Semver exposure:
  - `arrow`, `object_store` and `bytes` leak into the public API
  - no sealed traits
  - no `trybuild` tests
- Licensing: porting `GetCheckSum` and copying ROOT's streamer infos (LGPL)
  needs a review before publishing.

### The Recommendation

Commit to the layering; rewrite the API surface first.

**Write API: a scoped closure API plus a shared-sink handle; not per-writer
`&mut`.**

```rust
rootfile::create("out.root", |f| {           // f: &FileScope<'s>
    let mut ev   = f.tree::<Event>("Events")?;  // TreeWriter<'s>
    let mut runs = f.tree_dyn("Runs", schema)?;
    let mut nt   = f.ntuple("Hits", model)?;     // NTupleWriter<'s>
    for e in src { ev.fill(&e)?; if cond { f.put("h_pt", &hist)?; } }
    Ok(())
})?;  // children finished in creation order, then directories, keys, StreamerInfo, header
```

- Children hold `&'s FileScope`, a shared borrow of one sink with interior
  mutability (a `Mutex` around the allocator and file end).
- Any number of trees, ntuples, `put` and `mkdir` calls can coexist, and the
  borrow checker guarantees no handle outlives the file.
- Finalization always runs and returns `Result`.
- The owned `WritableFile::create()` handle is for long-lived structs: its
  children hold an `Arc`, and `close(self)` returns `Err(ChildrenOpen)` while
  a child is alive.
- `Drop` does no I/O.

**Other changes:**

1. Separate constructors.
2. `RBuffer::new(bytes, origin)`.
3. `Record::KEY_FORM`.
4. A single compression-policy function.
5. A `Container` enum.
6. Newtypes with private fields.
7. `Strictness::Lenient` as the default for byte counts.
8. `WBuffer::object(version, |w| …)`.
9. Derive checks at expansion.
10. Sealed traits.
11. A `LocalAsyncReadAt` for WASM.
12. Arrow and object_store in adapter crates.
13. `trybuild` tests.
14. Typed access layered over dynamic access.
15. The TTree writer stays unpublished until its member list is verified.

### The One Thing to Do First

Move the cross-check into M0. Write an empty file and a one-histogram file,
run them through ROOT's `TFile::Recover`, ROOT's `TFile::Open` and uproot, and
gate CI on the result before any other writer code.

---

## Organizer's notes (applied when updating ARCHITECTURE.md)

- The Executor's "publish only the facade, keep the rest `publish = false`"
  cannot work as stated. A crate published on crates.io cannot depend on
  unpublished crates. ARCHITECTURE.md therefore publishes the internal crates
  in lockstep, pinned with `=x.y.z`, and documents them as having no
  stability promise. Only `rootfile` (and the derive and adapter crates)
  carry the semver promise.
- On the reviewers' "already in the plan" point: `#[non_exhaustive]` and
  `cargo-semver-checks` were already in ARCHITECTURE.md §9; sealed traits and
  public-dependency exposure were not.
