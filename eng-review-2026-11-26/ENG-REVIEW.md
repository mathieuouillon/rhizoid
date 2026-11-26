# Engineering plan review — `rootfile` architecture

- **Skill:** gstack `plan-eng-review` (garrytan/gstack, `plan-eng-review/SKILL.md` + `sections/review-sections.md`, read 2026-11-26)
- **Target (fixed):** plan = `ARCHITECTURE.md` (revision 5, 2062 lines), with `ROOT-BEHAVIOUR.md` (536) and `STYLE.md` (593) as its details
- **Scope gate:** the user named the target explicitly ("review the architecture and details"), so the menu was skipped → B (plan)
- **Original plan:** unchanged copies in `plan-snapshot/`
- **Report file:** this file. The skill's state directory (`$GSTACK_STATE_ROOT`) does not exist here, and writing a review into the design doc itself would bury the doc, so the report sits next to the snapshot.

## Environment deviations (gstack tooling absent)

The skill's helper commands (`gstack-skill-start/end`, `gstack-brain-cache`,
`gstack-learnings-*`, `gstack-plan-reality`, `gstack-review-log/read`,
`gstack-tasks`, `gstack-decision-log`, `gstack-question-*`) and the Codex CLI
are not installed. In their place:

| Skill step | What was done instead |
|---|---|
| Preamble, telemetry, brain context, prior learnings | Skipped (no state store). Session treated as interactive |
| Design doc check / office-hours offer | The design doc is the target itself, so no offer |
| Retrospective learning (git log) | Not a git repository: n/a |
| Reality rows (`gstack-plan-reality`) | Worked by hand below, same row names |
| Web research (Aside) | Aside absent. This session already read the primary sources (ROOT's C++, uproot, serde, regex, rayon, axum, anyhow, thiserror, parquet); they are cited as `[Layer 1]` evidence |
| Outside voice (Codex) | Codex absent → the skill's native fallback: a `Plan` subagent with the same prompt |
| Review log / dashboard | Recorded in this file's terminal report only |

## Scope record

feature answers: D2=A (TTree flat + split writer in 1.0); D3=A (async API in 1.0); D4=A (full update mode in 1.0, with a crash-consistency design added to the plan); structure: B (6 crates, D1); accepted scope: the full planned feature set (M0–M7, async, full update) in the 6-crate arrangement, plus the update crash-consistency design; pending remedies: none

**Scope Challenge result: scope accepted as-is.** The smaller arrangement keeps the scope, so it is not a reduction.

---

## Step 0 — Scope Challenge

### 0A. What already exists

| Sub-problem | Existing solution | Plan's choice | Receipt |
|---|---|---|---|
| Reading ROOT in Rust | `oxyroot` (reads TTree, partial write); `root-io` (old) | Build new; oxyroot cited as prior art only | ARCHITECTURE.md:65–71 |
| Read/write design reference | go-hep `groot` (Go), uproot (Python) | Used as design references | ARCHITECTURE.md:65–71 |
| Codecs, hashes | `flate2`, `zstd`, `lz4_flex`, `liblzma`, `xxhash-rust` | Reused `[Layer 1]` | ARCHITECTURE.md:460–502 |
| Remote byte ranges | `object_store` (`get_ranges`, coalescing), `reqwest` | Reused, with object_store's defaults | ARCHITECTURE.md:540–548 |
| Columnar output | `arrow-rs` | Reused through an adapter crate | ARCHITECTURE.md:202 |
| Parallelism | `rayon` | Reused, caller's pool | ARCHITECTURE.md:1014–1018 |
| Test corpus | scikit-hep-testdata | Reused | ARCHITECTURE.md:1385 |

**Finding S1** — [INFORMATIONAL] (confidence: 5/10) ARCHITECTURE.md:65 — "Prior
art: `oxyroot` (Rust)". The plan does not say why extending oxyroot was
rejected. *Medium confidence: verify this is actually an issue.* oxyroot's
current capabilities were not read in this session. A one-paragraph
"why not oxyroot" with evidence would close the question for contributors.

### 0B. Minimum change and complexity check

The plan's goal is a 1.0 reader **and** writer. Counted once each:

- **Crates proposed: 14** (`source`, `core`, `compress`, `io`, `streamer`,
  `derive`, `objects`, `columnar`, `tree`, `ntuple`, facade, `arrow`,
  `object-store`, `cli`), plus `xtask` and `fuzz`. Receipt:
  ARCHITECTURE.md:187–204.
- **New public types/traits (major):** about 35 (`File`, `AsyncFile`,
  `LocalAsyncFile`, `WritableFile`, `FileScope`, `Directory`, `DirWriter`,
  `Tree`, `RNTuple`, `RNTupleWriter`, `OwnedRNTupleWriter`,
  `TreeReaderBuilder`, `TreeMetadata`, `FileMetadata`, `Projection`,
  `EntrySelection`, `Row`, `RootObject` + 4 component traits, `DynObject`,
  `Value`, `Error`, `ErrorKind`, `Compression`, `ReadOptions`,
  `WriteOptions`, `ObjectPath`, `Column`, `Jagged`, `Scratch`, `ReadAt`,
  `AsyncReadAt`, `LocalAsyncReadAt`, `WriteAt`, …). These are estimates from
  §4.10–§4.12.
- **Milestones: 8** (M0–M7). Receipt: ARCHITECTURE.md:1475–1482.

This is far past the skill's threshold (8+ files or 2+ new classes), so
**10B applies**: the review stops here for the structure question and for
each proposed cut or deferral.

**Deferrable work, flagged** (each asked separately below):

- the split-object TTree writer (M7);
- the async API;
- full update mode.

The serde bridge, Python bindings and the expert tier are already outside
1.0 or optional, so they are not asked.

### 0C. Reality rows (worked by hand)

**Premise table** (`unverified=3`):

| # | Premise | Status | Receipt |
|---|---|---|---|
| P1 | The on-disk layout is as in the format reference | verified against ROOT's sources | ROOT-BEHAVIOUR.md §1–9 |
| P2 | ROOT reads the TTree subset uproot writes | verified in uproot's writer source; not executed | ROOT-BEHAVIOUR.md §8 |
| P3 | RNTuple 1.x anchor and envelopes are as documented | verified (`RMiniFile.cxx`, spec) | ROOT-BEHAVIOUR.md §9 |
| P4 | Rust ≥ 1.85 is available | verified (rustc 1.99.1 locally) | STYLE.md §1 |
| P5 | The dependencies exist at the cited versions | verified (`cargo search`, 2026-11-25) | ARCHITECTURE.md §11 |
| P6 | Unsplit top-level objects carry a byte-count header | **unverified** (inferred from uproot's reader) | ROOT-BEHAVIOUR.md §8.9 |
| P7 | Porting `GetCheckSum` is compatible with MIT/Apache | **unverified** (legal) | ARCHITECTURE.md:1490 |
| P8 | AutoSave with `overwrite` deletes inside `WriteTObject` | **unverified** | ROOT-BEHAVIOUR.md §8.7 |

```
REALITY: premise-table finding unverified=3 (P6 header, P7 licence, P8 overwrite) ARCHITECTURE.md:1490
REALITY: already-done finding oxyroot exists; no "why not extend it" (S1) ARCHITECTURE.md:65
REALITY: surface-check finding ~35 public types/traits across 5 published crates for 1.0 ARCHITECTURE.md:857
REALITY: binding-decisions pass Rust; std conventions; compile-time checks; reference crates; council verdicts ARCHITECTURE.md:21
REALITY: numbers finding most defaults are sourced (ROOT, object_store, parquet); "15 %" compile budget and the 4 KiB probe are unsourced ARCHITECTURE.md:1421
REALITY: deferred-asks pass no linked issues (not a repository)
REALITY: pr-count finding 8 milestones and no PR/branch plan; M0 alone spans 4 crates + harness ARCHITECTURE.md:1475
```

### 0D. Decision ledger (pending — questions D1–D4)

#### D1 — Structure: 14 crates or 6
- **Finding:** S2 [INFORMATIONAL] (confidence: 8/10) ARCHITECTURE.md:187–204 — 14 crates, 8 of them "internal" yet published (ARCHITECTURE.md:248–270). Those are importable internals (red flag 7) and a lockstep-release burden, before any code exists.
- **Plan baseline:** 14 crates; internals published in lockstep with `=x.y.z`.
- **Runtime evidence:** unknown (no code).
- **Comparison grid:**

| Selectable | Current | A) Original | B) Smaller |
|---|---|---|---|
| Published crates | 14 | 14 | 6: `rootfile-source`, `rootfile` (core, compress, io, streamer, columnar, objects, tree, ntuple as private modules), `rootfile-derive`, `rootfile-arrow`, `rootfile-object-store`, `rootfile-cli` |
| Layering enforced by | the crate graph | the crate graph | module privacy (`pub(crate)`) plus a CI check of the module dependency graph |
| Features, contracts, fixes | as planned | unchanged | unchanged |
| Expert tier (`rootfile-core`) | later crate | unchanged | split out when stable, as regex did with `regex-automata` |

- **Question D1:** see the AskUserQuestion brief (header "Structure").
- **Pending remedies not decided here:** none.
- **State:** approved · **Actual answer:** B) 6 crates (user, 2026-11-26) · **Accepted scope:** the 6-crate arrangement, with the same features and contracts · **History:** pending → approved

#### D2 — TTree writing in 1.0
- **Finding:** S3 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:1481–1482 — the M6 flat writer is proven by uproot. M7 (`TBranchElement`, types 0–41) is the most complex part of the format and still has unverified premises (P6).
- **Plan baseline:** M6 and M7 both in the roadmap; the writer is unstable until it passes the cross-check.
- **Comparison grid:**

| Option | Flat (M6) | Split objects (M7) | Completeness |
|---|---|---|---|
| A | 1.0 | 1.0 | 10 |
| B | 1.0 | after 1.0 | 8 |
| C | after 1.0 | after 1.0 | 5 |

- **State:** approved · **Actual answer:** A) both in 1.0 (user, 2026-11-26) · **Accepted scope:** M6 and M7 in 1.0

#### D3 — Async API in 1.0
- **Finding:** S4 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:880, 904, 1069 — async adds `AsyncFile`, `LocalAsyncFile`, two async traits, the stream state machine and WASM support. Remote reading itself does not need async: a blocking `HttpSource` with vector reads covers batch jobs.
- **Plan baseline:** sync + async + HTTP + object store in M2.
- **Comparison grid:**

| Option | Sync remote (HTTP, object store) | Async API (`AsyncFile`, streams, WASM) | Completeness |
|---|---|---|---|
| A | 1.0 | 1.0 | 10 |
| B | 1.0 | after 1.0; traits reserved in `rootfile-source` | 8 |
| C | after 1.0 | after 1.0 | 4 |

- **State:** approved · **Actual answer:** A) async in 1.0 (user, 2026-11-26) · **Accepted scope:** as planned

#### D4 — Update mode in 1.0
- **Finding:** S5 [CRITICAL] (confidence: 7/10) ARCHITECTURE.md:998, 1335–1358 — update mode rewrites users' existing files: key lists, the free list, gap reuse and the header. A bug there corrupts data the user already had (the data-destruction class). The plan has the cross-check, but no update-specific safety design (atomicity, crash consistency).
- **Plan baseline:** full update mode in M5 (fill gaps first, replace, delete).
- **Comparison grid:**

| Option | Add new objects/cycles | Fill freed gaps, replace, delete | Completeness |
|---|---|---|---|
| A | 1.0 | 1.0 | 10 |
| B | 1.0, append-only at `fEND`; old key lists freed but gaps never reused | after 1.0 | 7 |
| C | after 1.0 | after 1.0 | 4 |

- **State:** approved · **Actual answer:** A) full update in 1.0 (user, 2026-11-26) · **Accepted scope:** full update mode, plus a crash-consistency design added to the plan before M5

### Scope Challenge findings and dispositions

1. S5 [CRITICAL] (confidence: 7/10) ARCHITECTURE.md:998 — update mode has no crash-consistency design → **accepted** (D4-A). The design is common work under D4 (Section 1, A2).
2. S2 [INFORMATIONAL] (confidence: 8/10) ARCHITECTURE.md:187–204 — 14 crates, internals importable → **accepted** (D1-B).
3. S3 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:1482 — M7 is the riskiest scope → **rejected as a cut** (D2-A). It stays in 1.0.
4. S4 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:904 — async surface → **rejected as a cut** (D3-A). It stays in 1.0.
5. S1 [INFORMATIONAL] (confidence: 5/10) ARCHITECTURE.md:65 — no "why not oxyroot". *Medium confidence: verify this is actually an issue.* → **pending** (D6, item 4).

---

## Section 1 — Architecture review

### Two shapes before one

The plan adds new modules and boundaries, so it needs a second shape.

- **Shape 1 (chosen in the plan):** a layered sans-IO core, then format
  layers, then a facade. Users get handles (`File`, `Tree`, `RNTuple`), a
  reader builder, and derives. Typed and dynamic decoding share one entry
  point.
- **Shape 2 (alternative, uproot-style):** one dynamic object model at the
  centre. Every read returns dynamic values, and typed access is a
  conversion layer on top:

  ```rust
  pub struct File;
  impl File { pub fn get(&self, p: &str) -> Result<DynObject> { unimplemented!() } }
  pub trait FromRoot: Sized { fn from_root(v: &DynObject) -> Result<Self>; }   // typed = conversion
  pub struct Tree;
  impl Tree { pub fn arrays(&self, cols: &[&str]) -> Result<DynColumns> { unimplemented!() } }
  impl DynColumns { pub fn get<T: FromRoot>(&self, name: &str) -> Result<Vec<T>> { unimplemented!() } }
  ```

- **Comparison on depth.** Shape 2 has the smaller interface: one decode
  path. But it pays for every typed read with a dynamic decode plus a
  conversion, and it cannot turn misuse into compile errors (Goal 5).
  Shape 1 hides more behind `File` and the builder: the typed fast path, the
  evolution path and the scratch pool are invisible to users.
- **Pick: Shape 1.** The format complexity (keys, streamers, baskets,
  pages) sits behind one handle per format plus one builder.

### Red-flag screen of the chosen shape

| # | Flag | Where | Verdict / fix |
|---|---|---|---|
| 1 | Shallow module | `rootfile-source` (3 traits + 1 newtype), ARCHITECTURE.md:188 | Intentional (axum-core / serde_core pattern for implementers). Keep |
| 2 | Information leakage | `Interpretation` exposes format internals (§4.8) | Acceptable at level 5 (expert); documented as such. Keep |
| 3 | Temporal decomposition | §6 / §7 split by execution order | These are documentation sections, not modules. No action |
| 4 | Pass-through method | `file.keys()` → `root().keys()` (§4.10) | Deliberate convenience for the root directory; one line, no logic. Keep, noted |
| 5 | Split ownership | `FileScope` allocator + children registry | One owner (the scope). Pass |
| 6 | **Two ways to do one task** | async reading: `AsyncFile` "has exactly the read methods of `File`" (ARCHITECTURE.md:904, 2003) **and** one builder generic over input (ARCHITECTURE.md:1069) | **Finding A1** → D5 |
| 7 | Importable internals | published internal crates | Resolved by D1-B |
| 8 | **Hand-synced lists** | built-in `ClassRegistry` (ARCHITECTURE.md:649), generated models (ARCHITECTURE.md:709), captured class descriptions (ARCHITECTURE.md:668) | **Finding A3** → D6, item 2 |

### Findings

1. **A1 [CRITICAL] (confidence: 9/10) ARCHITECTURE.md:904** — "**Async mirrors sync.** `AsyncFile` has exactly the read methods of `File`". This conflicts with ARCHITECTURE.md:1069: "`tree.reader()` and `async_tree.reader()` return the two aliases". Two designs for one task (red flag 6). → D5.
2. **A2 [CRITICAL] (confidence: 8/10) ARCHITECTURE.md:1335–1358** — the write path has no commit protocol. The plan rewrites directory records in place [RB§3] and the header last, but does not say which write makes an update durable, or what may be reused before then. **Accepted under D4-A** (common work), with this design:
   1. **Never reuse space freed in the current session before the commit.** Freed segments go into the in-memory free list only after the header is durable, so the old key lists and old StreamerInfo stay intact until then.
   2. **Commit order:** new records → new key lists → StreamerInfo → free list → `fsync` → rewrite the directory records in place (small, one sector each) → rewrite the header → `fsync`.
   3. **Crash analysis.** A crash before the first `fsync` leaves the old file valid, plus unreferenced bytes past the old end, which ROOT's recovery and our `check` tolerate. A crash between the directory and header rewrites leaves directory records pointing at fully written new key lists, with an old header: the file reads correctly, and the free list is stale but conservative.
   4. **Option `UpdateOptions::copy_on_write(true)`:** copy the file, update the copy, then atomically rename it over the original (`rename` on POSIX, `ReplaceFileW` on Windows). Off by default, documented for irreplaceable files.
3. **A3 [INFORMATIONAL] (confidence: 8/10) ARCHITECTURE.md:649, 668, 709** — three hand-maintained lists of built-in classes: hand-written codecs, generated models, captured class descriptions. They can drift. **Remedy (D6, item 2):** one manifest (`xtask/classes.toml`) from which `xtask` generates the model structs, the registry table and the embedded class descriptions. CI fails if the generated files are stale.
4. **A4 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:503–548** — remote sources have no timeout, retry or response validation. A realistic production failure: an EOS/HTTP server returns 503 or a truncated body mid-cluster, and the read fails or, worse, decodes short data. **Remedy (D6, item 1):**
   - `HttpSource` defaults: 30 s timeout, 3 retries with exponential backoff on 5xx/429/connection resets;
   - every response's `Content-Range` and length are checked against the request;
   - `object_store` sources reuse object_store's own retry configuration.
5. **A5 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:248–270** — distribution: no release process. The crates' publish order, docs.rs configuration and release automation are unstated. **Remedy (D6, item 3):** `release-plz` for changelogs, versions and dependency-ordered publishing, plus `[package.metadata.docs.rs]` with all features and `--cfg docsrs`.
6. **Security** — no issues found. Hostile input is bounded (`ReadLimits`, no panics, fuzzing). Credentials go only through `object_store`/`reqwest` configuration, never through URLs.

**Dispositions:** A1 → pending D5 · A2 → accepted (D4) · A3, A4, A5 → pending D6 · Security → no issues.

---

## Section 2 — Code quality review

1. **C1 [CRITICAL] (confidence: 9/10) ARCHITECTURE.md:289** — "`Cycle` | `NonZeroU16` … an `i16` on disk, never 0". ROOT-BEHAVIOUR.md:301 says basket keys carry "cycle 0, key version 1004". If key headers used `Cycle`, every basket key would be rejected. **Remedy (common work under Goal 1, "read every file ROOT writes"):**
   - `KeyHeader` keeps the raw `i16`;
   - `Cycle` (1..=32767) is only for user-facing names (`"h;2"`);
   - the fix is noted in ROOT-BEHAVIOUR §4.

   **Accepted.**
2. **C2 [INFORMATIONAL] (confidence: 8/10) ARCHITECTURE.md:959** — "If the closure returns `Err`, the scope still closes the file". The plan leaves open what happens when closing itself fails, for example on a full disk: which error is returned, and in what state the file is left. → D7.
3. **C3 [INFORMATIONAL] (confidence: 8/10) ARCHITECTURE.md:1092** — `from_entry_list(sorted: &[u64])` trusts its input. Unsorted, duplicated or out-of-range lists are unspecified. **Remedy (common work under STYLE §17, "validate in the setter"):** `Err(ErrorKind::InvalidSelection)`. **Accepted.**
4. **C4 [INFORMATIONAL] (confidence: 9/10) STYLE.md:282** — "Internal crates are `#[doc(hidden)]` at the facade level" is stale after D1-B. **Factual correction under D1 → accepted.**
5. **C5 [INFORMATIONAL] (confidence: 8/10) ARCHITECTURE.md:215–246** — the dependency-graph diagram will be stale after D1-B. **Accepted under D1** (redrawn as a module graph).
6. **C6 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:2034** — Appendix C promises "never another column's bytes", while §4.3 coalesces any gap up to 1 MiB, which pulls in other branches' baskets (ROOT interleaves the baskets of different branches as they fill). The contract and the design contradict each other. → Section 4, P1 (D8).

**Dispositions:** C1, C3, C4, C5 → accepted under existing approvals · C2 → pending D7 · C6 → pending D8.

### Decision ledger (questions D5–D8)

#### D5 — One `File` type for sync and async (A1)
- **Plan baseline:** a separate `AsyncFile` mirroring `File`, plus a generic reader builder. **Runtime evidence:** none (no code).

| Selectable | A) One generic type (recommended) | B) Two types |
|---|---|---|
| Handles | `File<I = Sync>`; `pub type AsyncFile = File<Async>` | `File` + `AsyncFile`, separate structs |
| Docs of shared methods | written once | written twice |
| User code (sync) | `File`, unchanged thanks to the default parameter | `File` |

- **State:** approved · **Actual answer:** A (user)

#### D6 — Plan additions (independent; multi-select)
1. Remote timeout, retry and `Content-Range` validation (A4)
2. One class manifest generating the registry, models and captured class descriptions (A3)
3. Release process: `release-plz`, publish order, docs.rs metadata (A5)
4. A "Why not extend oxyroot" paragraph with evidence (S1)
- **State:** approved · **Actual answer:** items 1, 2, 3 (user); item 4 not selected

#### D7 — Error when closing after a failed closure (C2)

| Option | Returned error | File state |
|---|---|---|
| A (recommended) | the closure's error, with the close failure attached as context and `source()` | "not closed" header (recoverable) |
| B | the close error, with the closure's error attached | same |
| C | the closure's error only; the close is skipped | "not closed" |

- **State:** approved · **Actual answer:** A (user)

#### D8 — Coalescing policy for remote reads (C6 / P1)

| Option | Gap rule | Over-read bound | Contract text |
|---|---|---|---|
| A (recommended) | merge when gap ≤ 64 KiB **and** fetched ≤ 1.25 × needed | ≤ 25 % | "reads at most 1.25× the bytes of the selected baskets" |
| B | object_store default: gap ≤ 1 MiB | unbounded for interleaved branches | "may read other columns' bytes to save round trips" |

- **State:** approved (delegated) · **Actual answer:** "No preference" (user); recommended A applied

### Answers D5–D8 (recorded 2026-11-26)

- **D5 → A) one generic `File`.** State: approved. Accepted scope: `File<I = Sync>`, `pub type AsyncFile = File<Async>`. §4.10 and Appendix B #14 are revised to match.
- **D6 → items 1, 2, 3 approved; item 4 not selected.** Accepted scope: remote timeout/retry/`Content-Range` validation; one class manifest; release process. S1 (why not oxyroot) → **rejected** by the user.
- **D7 → A) the closure's error first.** State: approved.
- **D8 → the user answered "No preference".** The recommended option A (bounded merging: gap ≤ 64 KiB and ≤ 1.25× over-read) is applied as **delegated**, and flagged in the completion summary so the user can reverse it.

---

## Section 3 — Test review

**Framework detection.** There is no `CLAUDE.md` and no `Cargo.toml` yet; the
target is a plan. The planned stack (ARCHITECTURE.md §9, STYLE.md §11) is
`cargo test`, `proptest`, `insta`, `trybuild`, `cargo-fuzz`, `divan`, the
data-driven `tests/cases/*.toml` and the ROOT/uproot cross-check. All of
these are proposals.

### Code paths and planned coverage (proposals)

```
READ PATH                                         PLANNED TEST                         MARK
File::open ─ header (small/large form) ────────── corpus + cases + fuzz(header)        [★★★ TESTED]
          ├─ truncated / not closed ─────────────── cases(error=…) + open_recover        [★★ TESTED]
          └─ > 2 GiB file (large keys, 8-byte seeks) ─ (none: corpus has no >2 GiB file)  [GAP]
key list → object record → decompress ──────────── corpus + proptest(codecs) + fuzz      [★★★ TESTED]
  ├─ basket keys with cycle 0 (C1) ──────────────── (none named)                          [GAP]
streamer decode ─ typed fast path ───────────────── cross-check + corpus                  [★★★ TESTED]
                ├─ evolution path (old version) ──── corpus (old ROOT files)              [★★ TESTED]
                └─ byte-count mismatch (Lenient/Strict) ─ cases                           [★★ TESTED]
TTree read ─ flat / jagged / split / embedded ───── corpus vs uproot + cases              [★★★ TESTED]
          └─ EntrySelection sparse (scan_baskets) ── (none named)                         [GAP]
RNTuple read ─ TFile / bare, encodings ──────────── corpus + proptest(encode∘decode)      [★★★ TESTED]
remote ─ multi-range / single-range fallback ────── cases × HTTP mock                     [★★ TESTED]
       ├─ 503 / truncated body / retry (D6-1) ────── (none)                               [GAP]
       └─ over-read bound ≤ 1.25× (D8) ───────────── (none)                               [GAP]
async stream ─ next_cluster, cancellation ───────── (none)                                [GAP]

WRITE PATH
create → put → close (scope) ────────────────────── cross-check M0/M1                     [★★★ TESTED]
  ├─ several writers + threads (FileScope) ──────── (none)                                [GAP]
  ├─ closure Err + close Err (D7) ───────────────── (none)                                [GAP]
  └─ dropped WritableFile → "not closed" ────────── (none)                                [GAP]
TTree writer flat (M6) / split (M7) ─────────────── cross-check                           [★★ TESTED] [→E2E]
RNTuple writer (M4) ─────────────────────────────── cross-check + ROOT verifier            [★★★ TESTED]
UPDATE (M5, full) ─ append, replace, delete, gaps ── cross-check                           [★★ TESTED]
  └─ crash at any write ────────────────────────── (none)                                 [GAP]  ← IRON RULE
compile-time guarantees ─────────────────────────── trybuild ui                           [★★★ TESTED]
WASM build ──────────────────────────────────────── build only                            [★ TESTED]
```

```
COVERAGE: 13/24 paths tested (54%) | Code paths: 11/20 | User flows: 2/4
QUALITY: ★★★:9 ★★:8 ★:1 | GAPS: 11 (E2E: 1, eval: 0)
```

(★★ rows count as tested; the single ★ row, WASM build-only, does not.)

LLM/eval scope: none (no prompts or LLM calls).

### Missing tests

Gaps that prove an approved contract are common work, with no question:
- Goal 1 → C1 and >2 GiB;
- D6-1 → retry;
- D8 → over-read bound;
- D7 → close error;
- STYLE §10 → cancellation safety;
- §4.10 → several writers and the dropped-handle rule.

| # | Test (file) | Asserts | Kind | Value card |
|---|---|---|---|---|
| T1 | `tests/large_file.rs` | a sparse >2 GiB file (written by our writer, then by ROOT in the cross-check job) has large keys, 8-byte seeks and a large header, and reads back | integration + E2E | protects=large-form switch; fails_when=offsets written in 4 bytes past 2e9; why_new=no >2 GiB file in the corpus; seam=none; no_claim=performance |
| T2 | `tests/cases/baskets.toml` | basket keys with cycle 0 decode, and `Cycle` is rejected for 0 in user paths | cases row | protects=C1; fails_when=KeyHeader uses Cycle; why_new=no named case; seam=none; no_claim=- |
| T3 | `tests/remote_faults.rs` | an HTTP mock injecting 503, a truncated body, a wrong `Content-Range` and a reset; reads retry, then fail with `ErrorKind::Io`, and never return short data | integration | protects=D6-1; fails_when=a truncated body is decoded; why_new=new behaviour; seam=none (mock server); no_claim=real CDN behaviour |
| T4 | `tests/cases/remote.toml` + an instrumented source | bytes fetched ≤ 1.25 × needed for sparse projections over interleaved branches | cases row | protects=D8 contract; fails_when=gap rule widened; why_new=contract is new; seam=none; no_claim=latency |
| T5 | `tests/async_cancel.rs` | dropping a stream mid-request and reusing the `File` gives correct results | integration | protects=cancel safety; fails_when=in-flight state leaks; why_new=none exists; seam=none; no_claim=- |
| T6 | `tests/scope.rs` | N writers on M threads, a closure `Err` with an injected close failure (D7), and a dropped `WritableFile` leaving a "not closed" header | integration | protects=§4.10 and D7; fails_when=a child is not finished or the error order is wrong; why_new=new behaviour; seam=a failing `WriteAt` test double (`WriteAt` is public); no_claim=- |
| T7 | `tests/selection.rs` + proptest | `EntrySelection` algebra (`and_then`, `union`) against a `Vec<bool>` model, and `scan_baskets` against a brute force | unit/property | protects=selection correctness; fails_when=off-by-one at basket edges; why_new=new type; seam=none; no_claim=- |
| **T8** | **`tests/update_crash.rs`** | **IRON RULE.** Existing user files are at risk under full update (D4) → **D9** asks how to cover it | — | — |

**Tests made obsolete by this plan:** none (there are no existing tests).

**Dispositions:** T1–T7 → accepted (proof of approved contracts); T8 → pending D9; WASM run-tests → pending D10.

---

## Section 4 — Performance review

1. **P1 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:540–548, 2034** — remote coalescing with 1 MiB gaps causes read amplification on interleaved baskets. Scale: selecting 2 of 1000 branches could fetch most of the file (10–100× the needed bytes). → **resolved by D8** (delegated A).
2. **P2 [INFORMATIONAL] (confidence: 7/10) ARCHITECTURE.md:1586** — `read(..)` materializes a whole column. Scale: 10⁹ entries × 4 B = 4 GB in one `Vec`. A whole-file load is the documented behaviour, but it is easy to call by accident. → **D10**.
3. **P3 [INFORMATIONAL] (confidence: 6/10) §4.8** — `file.tree()` decodes every branch's basket tables. Memory is O(branches × baskets): about 1 500 × 100 × 20 B ≈ 3 MB for NanoAOD. That is inherent (ROOT does the same) → factual addition to Appendix C, **accepted**.
4. **P4** — scratch pool, writer mutex, RNTuple page compression: no issues found at basket or page granularity. Scale is unknown until benchmarks exist; no numbers are invented.
5. **P5 [INFORMATIONAL] (confidence: 8/10)** — `copy_on_write` update (A2) is O(file size) by design → documented. **Accepted.**

**Dispositions:** P1 → D8 · P2 → pending D10 · P3, P5 → accepted (factual) · P4 → no issues.

### Decision ledger (questions D9–D10)

#### D9 — How to cover the update regression risk (IRON RULE; T8)
Behaviour to preserve: after any update, crash or not, ROOT and our reader
open the file and see either the old content or the new content, never a
mix and never a corrupted file.

| Option | How | Coverage |
|---|---|---|
| A (recommended) | fault injection: a `WriteAt` double that fails or tears at write #k, for **every** k of a recorded update; reopen and assert old-or-new content, plus ROOT opening the result in the cross-check job | every crash point, deterministic |
| B | real process kills at random points (`SIGKILL` during an update), repeated N times | realistic, but probabilistic |
| C | A + B | both |

- **State:** approved · **Actual answer:** A) fault injection at every write index (user)

#### D10 — Guard against accidental whole-column loads (P2)

| Option | Behaviour |
|---|---|
| A (recommended) | no limit; `read(..)` docs carry a `# Performance` warning pointing to `reader()` batches; `ReadLimits::max_output_bytes` exists, unset by default |
| B | default cap of 2 GiB per `read`, with a clear error naming `reader()` and the override |

- **State:** approved · **Actual answer:** A) docs + opt-in limit (user)

### Answers D9–D10 (recorded 2026-11-26)

- **D9 → A) fault injection at every write index.** State: approved. *Reopened as D12* after outside-voice finding F3.
- **D10 → A) docs + opt-in `ReadLimits::max_output_bytes`.** State: approved.

---

## Outside voice

Codex CLI not installed → the skill's native fallback: a `Plan` subagent
with the skill's read-only prompt (filesystem boundary, brutally honest
reviewer, severities, a Recommendation line, a VERDICT line).
`outside_status: completed` (native fallback, in-host).

### OUTSIDE VOICE SAYS (plan review — native fallback)

> **F1 [Critical]: A2's commit order is unsound.** A2 rewrites directory records in place before the header. The window between them is claimed to be "reads correctly, free list stale but conservative". That is false:
> - The old header still points at the old free list and old fEND, which mark as free exactly the space this session's new records and key lists occupy, because §7 fills gaps first. The next UPDATE open, by ROOT or by us, allocates over live data: silent corruption one session later.
> - The old `fSeekInfo` lacks the class descriptions of newly added classes.
> - Several directory records are rewritten one by one, so a crash gives mixed directories, violating D9.
> - "One sector each" is not controllable: records sit at arbitrary offsets, and sector atomicity is not guaranteed on network or FUSE filesystems (EOS).
> - Uncommitted records written into old gaps overwrite gap markers; ROOT's `Recover()` could resurrect them.
>
> **Fix:** write data → key lists → a superset StreamerInfo → a free list freeing nothing from this session → fsync → **header** → fsync → directory records, root last → fsync. Leak the space freed this session until the next commit. Either accept per-directory atomicity and weaken D9, or design a single commit point.
>
> **F2 [High]:** `WriteAt` has no durability barrier, yet A2 needs fsync, and the trait sits in the crate meant to stay 1.x. Add `sync()` now: no default, or one returning `Unsupported`, with update refusing such sinks. `copy_on_write` also needs a parent-directory fsync on POSIX.
>
> **F3 [High]:** the D9 fault injection (fail or tear at write #k) only models prefix-persisting crashes and cannot see a missing or misplaced fsync. The double must buffer writes between `sync` calls, persist arbitrary subsets, tear at 512 B, and run a **second UPDATE** on each reopened state. As specified, T8 would also fail against A2's own mixed states.
>
> **F4 [High]:** "if the closure returns `Err`, the scope still closes" commits half-done updates in `rootfile::update`, and the "not closed" header / dropped-handle rules would make valid existing files need recovery. Update mode needs its own rule: on `Err`, panic or drop, nothing is committed and the old file is untouched; update never writes a "not closed" marker.
>
> **F5 [High]:** in update mode the StreamerInfo record must be the union of the old list and the new classes, keeping several versions of one class; otherwise old objects and the `Value::Opaque` pass-through become undecodable. Rewriting TTree metadata in place mutates a live record before commit, which A2.1 forbids. Whether update extends existing TTrees is undefined.
>
> **F6 [High]:** the milestones are infeasible:
> - M2 is overloaded (TTree read + Arrow + remote + async + retry + coalescing);
> - M4's `RNTupleWriter<E: Row>` needs `derive(Row)`, which arrives in M5;
> - M5 bundles the update protocol, crash harness, derive and evolution;
> - M1's models need the class manifest, which no milestone owns;
> - there is no 1.0 gate.
>
> **F7 [Medium]:** D5's generic `File` makes `File::open(p)` ambiguous (E0034) if both `impl File<Sync>` and `impl File<Async>` define `open`, because default type parameters do not drive inference in expressions. Docs for methods whose signatures differ cannot be shared without a macro. Keep D5, but specify constructor naming and the macro strategy.
>
> **F8 [Medium]:** ARCHITECTURE.md is byte-identical to the snapshot; no approved decision is in the plan of record yet.
>
> **F9 [Low-Medium]:**
> - the glossary says fBEGIN is "always 100", but ROOT does not check it;
> - §6 makes `File::open` fail on unclosed or truncated files while ROOT recovers by default;
> - deleting a directory key is undefined;
> - the Allocator omits the fixed point needed because the free list must describe its own record.
>
> **Riskiest unstated assumption:** that the on-disk free list is ground truth. Validate every free segment against the record walker (gap marker present, no overlap with a walked record) before reusing it; on mismatch, append only.
>
> Recommendation: rewrite A2 and fold every approved decision into ARCHITECTURE.md before M5 and before `rootfile-source` freezes, because as written the accepted crash design can corrupt users' files and the approved test cannot detect it.
>
> VERDICT: FINDINGS

### Verification of the outside findings (agreement is evidence, not approval)

| # | Verdict | Basis |
|---|---|---|
| F1 | **Confirmed** (confidence 9/10). The reviewer's error was mine: ENG-REVIEW.md A2.2 puts directory records before the header | RB §2–§3: the header owns fEND, fSeekFree and fSeekInfo; the old free list covers the new records' space |
| F2 | Confirmed (9/10) | ARCHITECTURE.md:540–543: `WriteAt` has `write_all_at` and `flush` only |
| F3 | Confirmed (8/10) | The D9 model persists prefixes only |
| F4 | Confirmed (9/10) | ARCHITECTURE.md:959 applies to `update` as well |
| F5 | Confirmed (8/10) | RB §3: the StreamerInfo record is rewritten whole |
| F6 | Confirmed (8/10) | ARCHITECTURE.md:1475–1482 against §4.9/A.8 (`E: Row`) |
| F7 | Confirmed (9/10) | Rust resolves `File::open` with an inference variable; two inherent `open`s → E0034 |
| F8 | Confirmed: by design, the plan is updated after approvals (Required outputs, step 1) | — |
| F9 | Confirmed (8/10) | RB §1.3, §1.5, §4 |

**Common work, accepted without a new question** (each is a necessary part of an already-approved contract):

| Item | Approved contract it serves |
|---|---|
| F2: `WriteAt::sync()`, defaulting to `Unsupported`; update refuses sinks that return it; parent-directory fsync for `copy_on_write` | D4 crash consistency |
| F5a: the StreamerInfo union (keep every on-file version) | D4 + update preserves records it doesn't understand |
| Free-list validation against the record walker before any gap reuse; append-only on mismatch | D4 |
| F7: sync constructors stay `File::open`/`open_with`; async constructors are named `open_async`/`from_source_async` (returning `AsyncFile`); methods with differing signatures are generated by one internal macro from one doc comment | D5 |
| F8: fold every approved decision into ARCHITECTURE.md | Required output, step 1 |
| F9a: glossary "fBEGIN = 100, as written by ROOT; readers take the header's value" | Factual correction |
| F9c: `delete` on a directory key returns `Err(ErrorKind::IsDirectory)` (conservative, non-destructive); recursive deletion is not in 1.0 | D4 (no data loss) |
| F9d: the allocator iterates the free-list record's size to a fixed point (at most 2 passes) | D4 |

### Decision ledger (questions D11–D16; cross-model four-option menus)

#### D11 — Update commit protocol (F1)

| Option | Guarantee | How |
|---|---|---|
| **A) Apply: a single commit point** (recommended) | old-or-new for the **whole file** | header-first order, frees deferred; subdirectory records are written **copy-on-write** as new records, so only the root directory record (at `fBEGIN + fNbytesName`) changes in place; the header and the root directory record go out in one write when they fit in the first 4 KiB, and otherwise the update falls back to `copy_on_write` |
| B) Per-directory atomicity | old-or-new **per directory**; D9 weakened to match | header-first order, frees deferred, directory records rewritten in place, root last |
| C) Investigate first | — | a bounded spike (2 days): prototype A against ROOT 6.34 reading copy-on-write subdirectories; approves no implementation |
| D) Defer | — | 1.0 update is append-only with header-first ordering; gap reuse and replace/delete after 1.0 |

#### D12 — Reopen D9: the fault model (F3)

| Option | Fault model |
|---|---|
| **A) Apply** (recommended) | a double that buffers writes between `sync()` calls; at each crash point it persists every subset (bounded: all subsets up to 8 pending writes, random sampling beyond) and tears at 512 B; each state is reopened, checked against the guarantee, given a **second update**, checked again, and opened by ROOT in the cross-check |
| B) Keep D9 | fail or tear at write #k only |
| C) Investigate | evaluate an existing crash-testing harness (e.g. an ALICE/CrashMonkey-style recorder) first |
| D) Defer | ship D9's model, strengthen after 1.0 |

#### D13 — Update-mode error semantics (F4)

| Option | On `Err`, panic or drop in `rootfile::update` / `WritableFile::open_update` |
|---|---|
| **A) Apply** (recommended) | nothing is committed: the header and directory records stay untouched, so the old file is exactly as before (new bytes past the old fEND are unreferenced); update never writes a "not closed" marker; `create` keeps D7 |
| B) Keep | D7 applies to update too: close and commit what was written |

#### D14 — Extending existing TTrees in update mode (F5b)

| Option | Behaviour |
|---|---|
| **A) Apply: supported via copy-on-write** (recommended) | appended baskets go to new space; the tree object is written as a **new cycle**, as ROOT's AutoSave does; the old cycle is freed at the next commit; no in-place metadata rewrite in update mode |
| B) Keep the plan's in-place metadata rewrite | conflicts with D11 |
| C) Investigate | — |
| D) Defer | update can add new trees and objects but not extend existing trees in 1.0 |

#### D15 — Milestone re-cut (F6)

| Option | Roadmap |
|---|---|
| **A) Apply** (recommended) | M0 container + harness · M1 objects + class manifest · M2 sync TTree read + Arrow · M3 remote (retry, bounded coalescing) + async · M4 RNTuple read · M5 `derive(Row)`/`derive(RootObject)` + evolution · M6 RNTuple write · M7 update (commit protocol + crash harness) · M8 TTree flat writer · M9 split writer · **1.0 = M0–M9 all passing the cross-check** |
| B) Keep the current M0–M7 |

#### D16 — Opening unclosed or truncated files (F9b)

| Option | Behaviour |
|---|---|
| **A) Apply: recover by default** (recommended) | like ROOT: rebuild the key list with the record walker and report a `Diagnostic`; `Strictness::Strict` makes it an error |
| B) Keep | `File::open` errors; `File::open_recover` is opt-in |

### Answers D11–D16 (recorded 2026-11-26)

| ID | Answer | State | Accepted scope |
|---|---|---|---|
| D11 | A) single commit point | approved | header-first, deferred frees, copy-on-write subdirectories, one commit write, `copy_on_write` fallback; prototype against ROOT first (M7) |
| D12 | A) reorder + tear model (reopens D9) | approved | D9 history: A (prefix faults) → superseded by D12-A |
| D13 | A) abort, commit nothing | approved | update is all-or-nothing; `create` keeps D7 |
| D14 | A) new cycle, copy-on-write | approved | update extends existing trees through new cycles |
| D15 | A) re-cut to M0–M9 | approved | the roadmap in ARCHITECTURE.md §12, with a 1.0 gate |
| D16 | A) recover by default | approved | `File::open` recovers with a `Diagnostic`; Strict makes it an error |

## Approval readiness

Every accepted remedy cites its own answer or an approved contract:
- **User answers:** D1–D16 (D8 delegated: the user answered "No preference", and the recommended option was applied).
- **Common work under approved contracts:**
  - C1, C3, C4, C5, P3, P5;
  - T1–T7;
  - F2, F5a, F7, F8, F9a, F9c, F9d;
  - free-list validation.

**Approval readiness: PASS** (checked: D1–D16, plus the common-work list
above).

---

## Review body

### Working plan (as revised)

ARCHITECTURE.md revision 6, which folds in D1–D16 and the common work (see
its revision note). The original plan is in `plan-snapshot/`.

### NOT in scope

- **Python bindings, DataFusion/Polars providers, a native XRootD client:**
  after 1.0 (ARCHITECTURE.md §12), unchanged by this review.
- **The expert crate `rootfile-raw`:** split out of `core` only after its API
  settles (D1).
- **Recursive directory deletion:** not in 1.0; `delete` on a directory
  returns `IsDirectory` (F9c).
- **"Why not oxyroot" paragraph (S1):** declined by the user (D6).

### What already exists

See Step 0A. Reused `[Layer 1]`: the codecs, `object_store`, `reqwest`,
`arrow-rs`, `rayon`, scikit-hep-testdata. Design references: uproot, groot,
parquet, regex-automata. No shared-code extraction was proposed: the target
has no code yet.

### Diagrams

- The update commit protocol: ARCHITECTURE.md §7.2 (new).
- The module graph: ARCHITECTURE.md §3 (redrawn for six crates).
- The coverage diagram: Section 3 above.
- **Files needing inline diagrams when implemented:** `core/allocator.rs`
  (free-segment state transitions, deferred frees), `update/commit.rs` (the
  prepare → commit → after state machine), `io/coalesce.rs` (the merge rule
  with both bounds).

### Failure modes

| New path | Realistic production failure | Test / handling | User sees |
|---|---|---|---|
| update commit | crash or power loss mid-update | §7.2 protocol + crash harness (5a) | old or new file, never corrupt |
| update on a foreign or stale free list | gap reuse overwrites live data | free-list validation → append-only + `Diagnostic` | a `Diagnostic`; a correct file |
| remote read | 503 or truncated body from EOS/HTTP | retries + `Content-Range`/length checks (T3) | an `Io` error after retries, never short data |
| sparse remote projection | read amplification | bounded coalescing (T4) | ≤ 1.25× bytes |
| open of a crashed job's file | missing key list | recover by default (D16) | data + `Diagnostic` |
| `create` closure fails, then the close fails | root cause hidden | D7 error order (T6) | the closure's error, with the close failure as context |
| async stream dropped mid-request | in-flight state leaks | explicit state enum + T5 | nothing (reuse works) |
| file > 2 GiB | 4-byte offsets written | large-form switch + T1 | a correct file |

**Critical gaps (no test, no handling, silent):** 2 found during the review
(update crash; truncated remote body decoded). **0 remain**.

### Worktree parallelization strategy

| Step | Modules touched | Depends on |
|---|---|---|
| M0 container + harness | `core`, `compress`, `io`, `tests/cross`, CI | — |
| Class manifest + codegen (M1 prep) | `xtask` | — |
| Release + docs.rs config | workspace config, CI | — |
| `rootfile-source` traits (incl. `WriteAt::sync`) | `rootfile-source` | — |
| M7 prototype: ROOT reads copy-on-write subdirectories | `tests/cross` spike | M0 |
| M1 objects | `streamer`, `objects` | M0, manifest |

- **Lanes:** A = M0 → M1; B = manifest + codegen; C = release/CI config;
  D = the `rootfile-source` traits.
- **Order:** launch A, B, C, D in parallel. B must finish before M1
  starts. Run the M7 prototype spike as soon as M0's writer exists: it
  de-risks D11 early.
- **Conflicts:** A and C both touch the CI workflow. Give C ownership of
  `.github/workflows`; A adds only its own job file.

## Implementation Tasks

- [ ] **T1 (P1, human: ~2h / CC: ~10min)** — rootfile-source — Add `WriteAt::sync()` with an `Unsupported` default before freezing the crate
  - Surfaced by: F2 · Files: `crates/rootfile-source/src/lib.rs` · Verify: update refuses a sink without `sync` (test)
- [ ] **T2 (P1, human: ~2h / CC: ~10min)** — core — `KeyHeader` keeps the raw `i16` cycle; `Cycle` only for user names
  - Surfaced by: C1 · Files: `core/key.rs` · Verify: `tests/cases/baskets.toml` (cycle 0)
- [ ] **T3 (P1, human: ~3 days / CC: ~1h)** — update — Prototype copy-on-write subdirectories and check that ROOT 6.34+ reads them
  - Surfaced by: D11 · Files: `tests/cross/cow_dirs.C`, spike code · Verify: ROOT lists and reads every object; `TFile::Recover` is not triggered
- [ ] **T4 (P1, human: ~1 week / CC: ~2h)** — update — Implement the §7.2 commit protocol (deferred frees, StreamerInfo union, fixed-point free list, single commit write, `copy_on_write` fallback)
  - Surfaced by: F1, F5a, F9d, D11 · Files: `core/allocator.rs`, `update/commit.rs` · Verify: the crash harness (T5)
- [ ] **T5 (P1, human: ~1 week / CC: ~2h)** — tests — Crash harness: buffered writes between syncs, every subset, 512 B tears, a second update, ROOT opening every result
  - Surfaced by: D12 · Files: `tests/crash/` · Verify: it detects a deliberately removed `sync()` (mutation check)
- [ ] **T6 (P1, human: ~1 day / CC: ~20min)** — core — Free-list validation against the record walker; append-only fallback
  - Surfaced by: the riskiest assumption · Files: `core/allocator.rs`, `core/walk.rs` · Verify: a fixture with a stale free list is updated append-only, with a `Diagnostic`
- [ ] **T7 (P1, human: ~1 day / CC: ~20min)** — update — All-or-nothing abort on `Err`, panic or drop
  - Surfaced by: D13, F4 · Files: `lib.rs` (scope), `update/` · Verify: a panic inside an update leaves a byte-identical old file below the old fEND
- [ ] **T8 (P1, human: ~2 days / CC: ~30min)** — io — `HttpSource` retry/backoff/timeout and response validation
  - Surfaced by: A4, D6 · Files: `io/http.rs` · Verify: T3 remote-fault tests
- [ ] **T9 (P1, human: ~1 day / CC: ~20min)** — io — Bounded coalescing (64 KiB, 1.25×)
  - Surfaced by: C6, P1, D8 · Files: `io/coalesce.rs` · Verify: T4 over-read test on interleaved branches
- [ ] **T10 (P2, human: ~2 days / CC: ~30min)** — facade — `File<I = Sync>`, `AsyncFile` alias, distinct constructor names, `maybe_async!` macro
  - Surfaced by: A1, D5, F7 · Files: `lib.rs`, `file.rs` · Verify: `File::open` and `File::open_async` both compile (trybuild pass case); the docs show one body per method
- [ ] **T11 (P2, human: ~2 days / CC: ~30min)** — xtask — Class manifest generating the models, the registry and the embedded class descriptions; CI staleness check
  - Surfaced by: A3, D6 · Files: `xtask/classes.toml`, `xtask/src/gen.rs` · Verify: CI fails when a generated file is edited by hand
- [ ] **T12 (P2, human: ~4h / CC: ~15min)** — release — `release-plz`, publish order, docs.rs metadata
  - Surfaced by: A5, D6 · Files: `release-plz.toml`, crate `Cargo.toml`s · Verify: a dry-run release in CI
- [ ] **T13 (P2, human: ~4h / CC: ~15min)** — ci — Module-graph check (`tests/architecture.rs`)
  - Surfaced by: D1 · Files: `tests/architecture.rs` · Verify: an added forbidden `use` fails CI
- [ ] **T14 (P2, human: ~1 day / CC: ~20min)** — tree — Update-mode tree extension through new cycles
  - Surfaced by: D14, F5b · Files: `tree/write.rs` · Verify: ROOT reads the extended tree; the old cycle is freed at the next commit
- [ ] **T15 (P2, human: ~4h / CC: ~15min)** — facade — Recover by default on unclosed or truncated files
  - Surfaced by: D16, F9b · Files: `lib.rs`, `core/walk.rs` · Verify: a crashed-job fixture opens with a `Diagnostic`; Strict errors
- [ ] **T16 (P2, human: ~4h / CC: ~15min)** — facade — D7 error order; `delete` on a directory → `IsDirectory`
  - Surfaced by: C2, D7, F9c · Files: `lib.rs` (scope) · Verify: T6 scope tests
- [ ] **T17 (P2, human: ~1 day / CC: ~20min)** — columnar — `EntrySelection` validation and property tests
  - Surfaced by: C3 · Files: `columnar/selection.rs` · Verify: T7
- [ ] **T18 (P2, human: ~2 days / CC: ~30min)** — tests — >2 GiB large-file test, async cancellation test, multi-thread scope test
  - Surfaced by: Section 3 gaps · Files: `tests/large_file.rs`, `tests/async_cancel.rs`, `tests/scope.rs` · Verify: each fails against a deliberately broken build
- [ ] **T19 (P3, human: ~1h / CC: ~5min)** — docs — `# Performance` warning on `read(..)` and opt-in `ReadLimits::max_output_bytes`
  - Surfaced by: P2, D10 · Files: `columnar/`, docs · Verify: doc-test

_No new tasks from Section 4 beyond T9 and T19._

## Unresolved decisions

None. D8 was answered "No preference" and resolved by delegation to the
recommended option, flagged here so it can be reopened.

## Suppressed findings

None (no finding scored below 5/10).

## Completion summary

- **Step 0:** scope accepted as-is (6-crate arrangement, D1; no feature cuts).
- **Reality rows:** 7 worked by hand; unverified premises = 3 (P6 header, P7 licence, P8 overwrite).
- **Architecture:** 5 issues (A1–A5), plus 9 outside-voice findings. 1 critical design flaw, A2/F1, replaced by D11.
- **Code quality:** 6 issues (C1–C6). 1 critical: C1, basket cycle 0.
- **Test review:** coverage diagram produced; 11 gaps → T1–T8 specified; the IRON RULE regression contract was approved (D9 → D12).
- **Performance:** 3 issues (P1 → D8, P2 → D10, P3 documented).
- **NOT in scope** written · **What already exists** written.
- **TODOS.md:** no file exists; nothing was deferred that needs one (all deferrals are recorded in the plan's roadmap).
- **Failure modes:** 2 critical gaps found, 0 remaining.
- **Unresolved decisions:** 0.
- **Outside voice:** Claude `Plan` subagent (native fallback; Codex not installed). Completed, VERDICT: FINDINGS (9), all resolved (D11–D16 + common work).
- **Parallelization:** 4 lanes (A–D), partly parallel.
- **Lake Score:** 3/3. D2, D3 and D4 were the decisions scored for completeness, and each picked the 10/10 option.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|---|---|---|---|---|---|
| CEO Review | /plan-ceo-review | Scope & strategy | 0 | not run | — |
| Outside Review | native fallback (Claude Plan subagent) | Independent 2nd opinion | 1 | CLEAR | 9 findings, 9/9 resolved |
| Eng Review | /plan-eng-review | Architecture & tests (required) | 1 | CLEAR | 28 issues, 0 critical gaps |
| Design Review | /plan-design-review | UI/UX gaps | 0 | not run (no UI) | — |
| DX Review | /plan-devex-review | Developer experience gaps | 0 | not run | — |

OUTSIDE COVERAGE: native fallback completed (Codex CLI not installed); 9 findings, all dispositioned.
CROSS-MODEL: main and outside reviews used the same model family (Claude), so independence is limited.
VERDICT: ENG CLEARED — ready to implement (gstack's local review log is unavailable here, so this report is the only record)

NO UNRESOLVED DECISIONS
