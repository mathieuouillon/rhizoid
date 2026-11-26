# CEO plan review — `rootfile`

- **Skill:** gstack `plan-ceo-review` (`SKILL.md`, `sections/review-sections.md`, `sections/preamble.md`, read 2026-11-26)
- **Plan under review:** `ARCHITECTURE.md` revision 6 (after the engineering review), with `ROOT-BEHAVIOUR.md` and `STYLE.md`
- **Review depth:** implementation-ready (default)
- **Report file:** this file, next to the plan. gstack's state directory is absent.

## Environment deviations

- **Absent tooling:** gstack's helpers (`gstack-*`, the mode-handoff hook, the brain cache, learnings, the review log), Codex and Aside are not installed. Their steps are done by hand or skipped, as below.
- **System audit (git log, diff, stash, TODO grep):** not a git repository, so n/a. The docs were read in full during this session. There is no `TODOS.md` or `CLAUDE.md`.
- **Design doc check:** no `/office-hours` design doc exists, and the `/office-hours` skill is not installed, so the prerequisite offer is moot. `ARCHITECTURE.md` serves as the design doc.
- **Landscape check:** done with WebSearch (Aside absent). The queries carried no user data.
- **Outside voice:** the native fallback (a Claude `Plan` subagent) instead of Codex.

## Landscape (search results; untrusted, cited)

| Fact | Source |
|---|---|
| `oxyroot` (Rust) reads ROOT files and TTrees; it can write TTrees, but writing is **restricted to uncompressed files**. It is a port of groot (Go) and uproot (Python); its latest release is 0.1.25 (Oct 2024) | docs.rs/oxyroot, docs.rs/crate/oxyroot/0.1.23 |
| RNTuple is slated to **replace TTree from Run 4 (2029 onward)**; TTree stays as legacy (functional, no new features); ATLAS stores AOD/DAOD in RNTuple | ATLAS ACAT 2024 proceedings; ATLAS CHEP 2024 slides |
| uproot writes RNTuples (since 5.7.0), but **cannot extend ROOT-written RNTuples that use split encodings** | uproot docs, `WritableNTuple` |
| No 2026-specific announcements were found; the status may have moved since | — |

## Step 0 — Nuclear scope challenge

### 0A. Premise challenge

| Question | Answer from the plan | Challenge |
|---|---|---|
| The real problem | Rust programs cannot read and write ROOT files fully, safely and fast without ROOT (§1) | Real, but the **largest audience is not Rust programmers**: HEP analysis runs in Python (uproot, coffea) and C++ (ROOT). The plan puts Python bindings *after* 1.0 (§12) |
| Target outcome | A complete, verified reader and writer at 1.0 (§12, gate M0–M9) | Users get no stable artefact until the **least strategic** pieces ship: the split-object TTree writer (M9), for a format that is legacy from 2029 |
| Cost of doing nothing | Rust users keep oxyroot (no compressed writing, no RNTuple) or call uproot through pyo3 | A low cost for the few Rust HEP users; a higher one for Arrow/DataFusion users, who have no native path to CERN Open Data |
| Direct or proxy | Direct: a library | — |
| Build vs extend | oxyroot exists (MIT/Apache, about 1 maintainer, last release Oct 2024) | The eng review declined a written "why not oxyroot" note (D6). At CEO level the strategic question remains: **differentiation**. The plan's differentiators are RNTuple, compressed writing, update mode, async/remote, Arrow and compile-time guarantees. None of these exist in oxyroot today |

### 0B. Existing leverage

As in the eng review, step 0A (the reuse table is unchanged). The eng review's reality rows carry over, and the premise table now has two more rows:

| # | Premise | Status |
|---|---|---|
| P9 | Python users benefit from a Rust core (as Polars shows) | **unverified**: no user interviews or uproot-maintainer contact |
| P10 | TTree writing is still wanted after Run 4 | partly verified: TTree stays legacy (supported, frozen); analysis ntuples still use it today |

```
REALITY: premise-table finding unverified=5 (P6, P7, P8 from eng review; P9 audience, P10 TTree demand) ARCHITECTURE.md:85
REALITY: already-done finding oxyroot covers TTree read + uncompressed write; uproot covers RNTuple write (Python) ARCHITECTURE.md:65
REALITY: surface-check finding ~35 public types for 1.0; no user-facing artefact before the M9 gate ARCHITECTURE.md:1630
REALITY: binding-decisions pass eng review D1–D16 recorded eng-review-2026-11-26/ENG-REVIEW.md
REALITY: numbers finding the plan has no adoption or usage target (users, downloads, projects) ARCHITECTURE.md:81
REALITY: deferred-asks pass no linked issues
REALITY: pr-count finding 10 milestones, no release cadence before 1.0 ARCHITECTURE.md:1630
```

### 0C. Dream state

```
CURRENT STATE                    THIS PLAN (1.0)                    12-MONTH IDEAL
─────────────                    ───────────────                    ──────────────
Rust: oxyroot (TTree read,       Pure-Rust read+write: TFile,       Same library, PLUS:
uncompressed write)              objects, TTree, RNTuple, update,   · used by Python users (wheels,
Python: uproot (read+write,      async/remote, Arrow, typed API       Arrow zero-copy) as a fast path
RNTuple write, no split enc.     with compile-time checks           · read CERN Open Data from
extension)                       Gate: all of M0–M9 first             DataFusion/Polars/DuckDB via Arrow
C++: ROOT (reference)            Users: Rust developers             · releases every milestone (0.x),
JS: JSROOT (browser viewer)                                           so users from month 2
                                                                    · cited by uproot/ROOT as a
                                                                      compatible implementation
```

**The gap between the plan and the ideal is distribution and audience, not
format coverage.**

### Decision ledger

| ID and owner | Contract and evidence | Current | Proposed | Status | Exact approval and scope |
|---|---|---|---|---|---|
| CEO-0 mode | skill 0E; rule ">15 planned changed files → SCOPE REDUCTION" | — | recommend SCOPE REDUCTION because the 1.0 gate (ARCHITECTURE.md §12) requires M0–M9, including both TTree writers and full update, before any stable release | approved | user chose HOLD SCOPE (recommendation declined) |
| C1 compat target | Goal 2; §13 Q1 | open | A) ROOT ≥ 6.26 reads our TTree/objects, ≥ 6.34 our RNTuple; CI matrix | approved | user: option A (2026-11-26) |
| C2 external basket files | Goal 4; RB §8.4 fFileName | unspecified | A) refuse by default; opt-in base directory | approved | user: option A (2026-11-26) |
| C3 LZMA backend | Goal 4; §4.2 | liblzma default | A) pure-Rust decoder default; liblzma only for writing | approved | user: option A (2026-11-26) |
| C4 cross-process writers | D11/D13 invariant | none | A) exclusive advisory lock (std `File::try_lock`, Rust ≥ 1.89) | approved | user: option A (2026-11-26) |
| C5 HTTP connections | Appendix C; §4.3 | per-source clients | A) shared client + global cap 64 | approved | user: option A (2026-11-26) |
| C6 observability | Appendix C testability | tracing only | A) `ReadStats`/`WriteStats` + span conventions + troubleshooting guide | approved | user: option A (2026-11-26) |
| C7 release cadence | — | nothing before 1.0 | A) 0.x per milestone | approved | user: option A (2026-11-26) |

**CEO-0 → HOLD SCOPE** (user, 2026-11-26). Mode selection is not approval of any remedy. The SCOPE REDUCTION recommendation was declined; the eng-review scope stands (D1–D16).

### 0G. HOLD SCOPE checks

1. **Complexity:** over the threshold (more than 8 files, more than 2 new
   classes), as the eng review already established. The structure was
   already reduced there (D1: six crates). No further challenge under HOLD.
2. **Minimum changes / deferrable work:** in HOLD SCOPE the deferrable work
   is flagged, not cut:
   - the split-object TTree writer (M9) targets a format that is legacy from
     2029;
   - Python bindings sit after 1.0, although Python is the largest audience.

   Both are recorded in Section 10 and offered as TODOs.
3. **Invariants kept** (repairs that keep them hold are in scope):
   - Goal 1, read every file ROOT and uproot write;
   - Goal 2, ROOT and uproot read what we write;
   - Goal 4, no panics or unbounded allocations on hostile input;
   - D11/D13, update is all-or-nothing;
   - Appendix C, the cost contract.

### 0I. Temporal interrogation (M0, the first milestone)

| Window | Work | Blockers / pending |
|---|---|---|
| Hour 1 | workspace, lints, `rootfile-source` traits (incl. `WriteAt::sync`), `core` newtypes, `RBuffer`/`WBuffer` | none |
| Hours 2–3 | `FileHeader`, `KeyHeader` (raw `i16` cycle), `DirectoryRecord`, `KeyList`, free list: decode + encode + proptest | none |
| Hours 4–5 | `compress` (zlib/LZ4/zstd/LZMA blocks), record walker, empty-file writer | **LZMA backend choice** (C3) |
| Hour 6+ | cross-check harness: ROOT container + uproot in CI, the module-graph check | **feasibility:** the `rootproject/root` image is several GB, so CI time and caching need planning; the corpus download needs caching |

Effort (from the eng review's tasks), human team vs Claude Code:
- **M0:** about 2–3 weeks for a human team, about 1 day with Claude Code.
- **M0–M9 overall:** about 6–9 months, vs 2–4 weeks.

**Feasibility blocker beyond M0:** P7, the licence review of embedded class
descriptions, must be settled before M1 ships its generated models.

---

## Section 1: Architecture Review

**Current scope:** HOLD SCOPE; ledger rows: eng D1–D16 (approved); CEO-0
(HOLD). No scope dispositions pending.

**System architecture:** the module graph in ARCHITECTURE.md §3 (revision 6)
is current. Data flow, all four paths, for the main read path:

```
 File::open ──► header ──► root dir ──► key list ──► get(path) ──► record ──► decompress ──► decode
   │HAPPY: object returned (typed or DynObject)
   │NIL:   path not found ─────────────────────────► ErrorKind::NotFound (no I/O beyond key lists)
   │EMPTY: directory with 0 keys → empty iterator; tree with 0 entries → reader yields no batch;
   │       branch with 0 baskets → empty Column       ◄── UNSPECIFIED in plan (finding 1-1)
   │ERROR: corrupt bytes → Malformed/UnexpectedEof with context; truncated file → recover (D16)
```

**State machines.**
- **Update commit** (ARCHITECTURE.md §7.2): `Prepare → Synced → Committed`,
  with `Aborted` reachable from `Prepare` and `Synced`. Invalid transitions:
  - committing with open children;
  - reusing same-session frees before `Committed`;
  - a second commit without a new `Prepare`.
- **Writer child:** `Open → Finished`, finished by the scope if not
  explicitly.

**Scaling breakpoints.**
- **×10** (10 GB files, 5 000 branches): basket tables of about 10 MB per
  tree. OK.
- **×100** (1 TB datasets of 10⁴ files): metadata re-reads dominate. The
  shared `FileMetadata` (§4.12.3) covers this.
- **Concurrency of remote sources:** see Section 7, finding 7-1.

**Single points of failure.**
- The one commit write in §7.2 can tear. This is mitigated by
  `copy_on_write`.
- The cross-check CI depends on ROOT's container image.

**Rollback.** Users pin crate versions. Files are the durable artefact: what
we write must stay readable by ROOT forever. → finding 1-2.

**Value test of artefacts:** `ROOT-BEHAVIOUR.md` is used by implementers and
changes format decisions: keep. The council and review files are historical:
retire them to `docs/history/` at M0.

**Findings**
1. **1-1 WARNING (confidence: 8/10) ARCHITECTURE.md §6, §4.8** — the empty paths are unspecified: 0 keys, 0 entries, 0 baskets, 0 clusters, a zero-length jagged entry. **Repair under Goal 1 (in scope, HOLD):** specify each one (an empty iterator, an empty `Column`, no batch) and add `tests/cases/empty.toml`. **Accepted** (common work).
2. **1-2 WARNING (confidence: 8/10) ARCHITECTURE.md §13 Q1** — "Claimed ROOT version … fixed per release" is still open. Which ROOT versions must read our files is a compatibility promise, effectively forever. → **C1**.

---

## Section 2: Error & Rescue Map

| METHOD/CODEPATH | WHAT CAN GO WRONG | ERROR (kind) |
|---|---|---|
| `File::open` | not a ROOT file; truncated; unclosed; I/O | `NotRootFile`, `UnexpectedEof`, (recover → `Diagnostic`), `Io` |
| `get::<T>` | missing path; wrong type; unknown class; corrupt object | `NotFound`, `TypeMismatch`, `UnknownClass`, `Malformed` |
| decompress | unknown tag; corrupt stream; bomb | `UnknownCompression`, `Decompress`, `LimitExceeded` |
| remote read | 5xx, 429, reset, timeout; truncated body | `Io` (after retries) |
| reader builder | bad selection; projection of an unknown column | `InvalidSelection`, `NotFound` |
| `create` scope | closure error; close failure; disk full | the closure's error (+ close as context, D7); `Io` |
| `update` scope | locked by another writer; sink without `sync`; crash | `Unsupported`; (lock → C4); old file intact (D13) |
| `read(..)` | output too large | allocation failure → abort ◄── **GAP** unless `max_output_bytes` is set (D10 chose docs + opt-in) |

| ERROR | RESCUED? | RESCUE ACTION | USER SEES |
|---|---|---|---|
| `Io` (remote transient) | Y | retry with backoff ×3 (D6) | data, or `Io` with the URL and range as context |
| `Malformed`/`UnexpectedEof` | Y | Lenient: reposition + `Diagnostic` where ROOT tolerates it; else re-raise with offset, key and class context | a clear message with a location |
| `LimitExceeded` | Y | re-raise naming the limit and how to raise it | a clear message |
| truncated/unclosed file | Y | recover (D16) + `Diagnostic` | data + a warning |
| update failure/panic | Y | abort, nothing committed (D13) | the error; the old file intact |
| allocation failure in `read(..)` | **N** | process abort (Rust OOM) | crash: **accepted by D10** (documented, opt-in limit) |

- **Catch-alls:** none; `ErrorKind` is exhaustive per cause.
- **Logging of the attempted operation:** see Section 8.
- **No new gaps beyond D10's accepted one.**

---

## Section 3: Security & Threat Model

| Threat | Likelihood | Impact | Mitigated? |
|---|---|---|---|
| Hostile file → panic, OOM, unbounded loop | H | M | Y (`ReadLimits`, no panics, fuzzing, depth limits) |
| **Hostile file references other files** through `TBranch::fFileName` ("name of file where buffers are stored", [RB§8.4]): following it opens arbitrary local paths or URLs named by an untrusted file | M | **H** | **N, not in the plan** → **C2** |
| Supply chain: C codecs (`liblzma` bundles xz, the project of the 2024 backdoor CVE-2024-3094; `zstd-sys`) | L | H | partly (`cargo-deny`) → **C3** |
| Credentials on remote redirects | L | M | Y: credentials only in client config; reqwest drops auth headers on cross-host redirects |
| Untrusted `serde` bridge / `DynObject` from hostile files | M | L | Y (same limits) |
| Derive macro misuse | n/a (compile time) | — | — |

**Findings**
1. **3-1 CRITICAL GAP (confidence: 8/10) ROOT-BEHAVIOUR.md §8.4 (`fFileName`)** — branches can point to external files, and the plan says nothing about following them. **Unrescued, untested and silent** if implemented naïvely. → **C2**.
2. **3-2 WARNING (confidence: 7/10) ARCHITECTURE.md §4.2** — `liblzma` (C, bundled xz) is on by default, so every user ships it. → **C3**.

---

## Section 4: Data Flow & Interaction Edge Cases

```
 INPUT(bytes) ─► VALIDATE(bounds, limits) ─► TRANSFORM(decompress, decode) ─► OUTPUT(Column/object)
   nil/empty: 0-length file ─► NotRootFile ✓   |  stale: file grows while read ─► header snapshot ✓
   too long: lengths > limits ─► LimitExceeded ✓|  partial: truncated ─► recover (D16) ✓
   timeout: remote ─► retry (D6) ✓              |  conflict: TWO WRITERS ON ONE FILE ─► ✗ unhandled
```

- **Async ordering / shared mutable state.**
  - Within one process, `FileScope` serializes allocation behind a `Mutex`
    (an invariant boundary). The crash harness and T6 exercise both
    completion orders.
  - **Across processes** nothing prevents two writers. Two `update`s, or
    ROOT plus us, on the same file corrupt it, because ROOT itself does not
    lock.
- **Interaction edge cases.**

  | Interaction | Edge case | Handled? | How |
  |---|---|---|---|
  | update | a second process updates concurrently | **N** | → **C4** |
  | read during update | the reader opens mid-update | Y | it sees the old header until the commit (§7.2) |
  | batch job retry | the same output path written twice | Y | `create` truncates; `update` adds cycles |
  | huge list | 10⁵ keys in one directory | Y | O(K) key list, one read |

**Finding 4-1 WARNING (confidence: 8/10)** — no cross-process write
exclusion. → **C4**.

---

## Section 5: Code Quality

No new issues beyond the eng review (C1–C6, all dispositioned). One check
holds for the future: `maybe_async!` (D5) and the tuple macro must not branch
more than 5 ways per generated method. STYLE §15 already covers macro
hygiene. **No issues found.**

## Section 6: Test Review

The eng review's coverage diagram, T1–T8 and the test plan apply. Additions
under the CEO lens:

- **The 2am-Friday test:** the ROOT/uproot cross-check on every PR. Present.
- **The hostile-QA test:** fuzz + `ReadLimits`. Present. **Plus:** a fixture
  whose `fFileName` points to `/etc/passwd` and to a URL (C2).
- **The chaos test:** the crash harness (D12) and remote faults (T3).
  Present.
- **Compatibility matrix (new, from C1):** read our files with each ROOT
  version in the compatibility target, and read the corpus from ROOT
  5.34–latest. → C1's remedy includes it.
- **Empty-path cases (1-1):** `tests/cases/empty.toml`. Accepted.

## Section 7: Performance

- **Top slow paths, estimated:**
  - remote `File::open`: 1–2 round trips, p99 ≈ 2 × RTT + server latency;
  - first `get` of a class: the StreamerInfo load, one extra round trip;
  - LZMA-compressed clusters: decompression-bound, roughly 10× slower than
    zstd.

  No benchmarks exist yet; these are estimates, not measurements.
- **Memory:** bounded by Appendix C. **Caching:** `FileMetadata` sharing.
  **Connection pool:** see 7-1.
1. **7-1 WARNING (confidence: 7/10) ARCHITECTURE.md §4.3** — `HttpSource::new(url)` per file, with 10 parallel requests each. A job opening 1 000 files could hold 10 000 connections. Each source has its own client and limits, with no process-wide bound. → **C5**.

## Section 8: Observability

- **Logs:** `tracing` is named (§11), but there are no span or field
  conventions.
- **Metrics:** none. Yet Appendix C's contract (bytes fetched vs needed,
  requests per cluster) needs counters to be testable *and* debuggable in
  production jobs.
- **"Bug reported 3 weeks later":** a user can't currently tell whether a
  slow job was I/O- or decompression-bound.
- **Runbooks:** none. Error kinds have no "cause → fix" guide.

**Finding 8-1 WARNING (confidence: 8/10).** → **C6**.

## Section 9: Deployment & Rollout

- **Release channel:** crates.io via `release-plz` (D6).
- **Feature flag:** `unstable-tree-writer` until M8 passes the cross-check.
- **Rollback:** `cargo yank` plus a patch release. For *files* written by a
  buggy version, `rootfile check` must detect them, and the fix ships as a
  patch, with an advisory (RUSTSEC-style) if files are affected.
- **Deploy-time risk window:** users mixing versions read each other's files.
  Every version must read every older version's output (format contract).
- **Post-release checklist:**
  - docs.rs built;
  - the examples run against the published crate;
  - the cross-check run against the published crate.
1. **9-1 WARNING (confidence: 7/10) ARCHITECTURE.md §12** — no release cadence before 1.0. Users get nothing until M9, and feedback on the API arrives only after it is frozen. → **C7** (a rollout decision, not a scope change).

## Section 10: Long-term trajectory

| Decision | Reversibility (1 = one-way, 5 = easy) |
|---|---|
| Six crates (D1) | 4 (crates can be split later) |
| `File<I = Sync>` public API (D5) | 2 after 1.0 |
| Written-file compatibility (C1) | **1**: files outlive code |
| Update commit protocol (D11) | 3: the format stays ROOT-compatible; the protocol is internal |
| TTree writers (D2) | 3: can be frozen like ROOT's TTree |

- **Path dependency:** investment in the TTree writer (M8–M9) for a format
  that is legacy from 2029.
- **Knowledge concentration:** a single author; mitigated by
  `ROOT-BEHAVIOUR.md`, `STYLE.md` and the review records.
- **Ecosystem fit:** strong (Arrow, object_store, rayon, serde).
- **The 1-year question:** *what would make this the default way to read
  ROOT outside C++?* The answer is reach. Python wheels with Arrow
  zero-copy (after 1.0 in the plan) and DataFusion/Polars access to CERN
  Open Data. Under HOLD SCOPE these are **TODO candidates**, not scope
  changes.

**Debt items:** the M9 strategic fit (flagged); Python timing (TODO).

---

## Outside voice

The native fallback (a Claude `Plan` subagent) with the skill's read-only
prompt; Codex is not installed. `outside_status: completed` (in-host). The
skill notes that a native result does not count as external coverage.

### OUTSIDE VOICE (Claude subagent)

> **Critical**
> - **K1:** the effort estimate (CEO-REVIEW §0I: "6–9 months / 2–4 weeks") is wrong. Comparable projects (uproot, groot, oxyroot) took years. The pace is set by verification against ROOT (the streamer long tail, the corpus, CI and harness wall time), not by typing. Re-baseline per milestone on its verification cost.
>
> **High**
> - **K2:** the compressed flat TTree writer (M8) is the most valuable writer: it is what oxyroot lacks, and it is readable by ROOT ≥ 6.26. It sits behind update mode (M7). Its only dependencies are M1 and M5. Move it right after M5.
> - **K3:** the single commit write falls back to `copy_on_write` only above 4 KiB, while the harness tears at 512 B and the invariant is "never corrupt". A torn write between 512 B and 4 KiB can leave an old header with a new root directory record: the new key list then references records sitting in space the old free list calls free, beyond the old fEND, and the next write corrupts live data. Candidate fix: two ordered writes with a sync between them, header first, then the root directory record.
> - **K4:** CI as written is infeasible. Every PR would be gated on multi-GB ROOT images × the C1 matrix, a crash harness running ROOT on every subset at every crash point, 7 platforms and a flaky 15 % compile gate. Split per-PR from nightly; use self-hosted runners and caching.
> - **K5:** Goal 1, "every file … without generated code", cannot hold: classes with hand-written streamers have no complete on-file description. The corpus has no real experiment files (xAOD, MiniAOD). "Matches uproot" fails exactly where uproot fails. Restate Goal 1 as a measured percentage over a named corpus of real files, with ROOT dumps as the reference.
> - **K6:** no named user, no design partner, no success measure. 0.x crates rarely enter experiment software stacks. (Information under HOLD.)
>
> **Medium**
> - **K7:** update extends TTrees (legacy) but not RNTuples. uproot's known gap is extending ROOT-written RNTuples that use split encodings. Decide: support it, or state it as a non-goal.
> - **K8:** `File::try_lock` is unreliable on EOS FUSE, NFS and AFS, and cannot exclude ROOT. The fallback is unspecified, and an mmap reader can hit SIGBUS when another process modifies the file.
> - **K9:** C3's feature split must be explicit (`lzma` decode-only, `lzma-write` opt-in), plus a speed gate. The plan text still says liblzma is the default.
> - **K10:** the licence blocker (P7) has no owner or date and now lands in month 1 (C7). uproot (BSD-3) embeds captured class descriptions: useful precedent.
> - **K11:** there is no multi-file dataset API (`TChain`-like); real analyses read 10²–10⁴ files.
> - **K12:** M3 bundles WASM, async and object_store ahead of RNTuple read. Put sync remote first, then RNTuple read, then async and WASM.
>
> **Low**
> - **K13:** C5's global cap is hidden global state. Pass the client and pool explicitly through the read options.
> - **K14:** the plan does not yet include C1–C7, and has stale text (A.17 "until M6"; the feature table; §13 Q1).
> - **K15:** Python bindings later need every capability reachable through `'static` handles and dynamic types. State that rule now.
>
> Recommendation: fix the plan in place before M0 starts, without changing scope:
> - re-baseline the effort (K1);
> - move M8 to right after M5, and RNTuple read ahead of async and WASM (K2, K12);
> - replace the single commit write with ordered header-then-directory writes before any M7 work (K3);
> - split CI into per-PR and nightly tiers (K4);
> - name one design partner and one adoption measure (K6).
>
> This matters because the current order and gating spend most of the effort before any user gets the parts they value most, and the update protocol as written breaks its own invariant.
>
> VERDICT: FINDINGS

### Verification (agreement is evidence, not approval)

| # | Verdict | Disposition route |
|---|---|---|
| K1 | **Confirmed.** My estimate counted writing, not verification | Factual correction, below (no question) |
| K2, K12 | Confirmed: dependencies check out (M8 needs M1 + M5; RNTuple read needs no async) | **CEO-D2** (reopens eng D15, roadmap order only) |
| K3 | **Confirmed** (9/10): the 4 KiB fallback threshold vs the 512 B tear model; old header + new root directory = the F1 failure again | **CEO-D1** (reopens eng D11) |
| K4 | Confirmed | **CEO-D3** |
| K5 | Confirmed: §1 Goal 1 overclaims; the corpus has no real experiment files | **CEO-D4** |
| K6 | Information (HOLD) | TODO question |
| K7 | Confirmed: §7.3 covers TTrees only | **CEO-D5** |
| K8 | Confirmed: C4 leaves the unsupported-lock case open | **CEO-D6** |
| K9 | Confirmed; it is the literal content of C3 | Common work under C3 |
| K10 | Confirmed | TODO question |
| K11 | Confirmed: no `TChain`-like API | TODO question |
| K13 | Confirmed: C5 "process-wide" conflicts with §2.1 "no global state" | **CEO-D7** (reopens C5's shape) |
| K14 | Confirmed: the plan is updated at the end of this review | Common work |
| K15 | Confirmed: a cheap rule now, expensive later | **CEO-D8** |

**K1 correction (re-baselined effort, an estimate, not a measurement):**
verification dominates. Each milestone's exit criterion is a ROOT
cross-check, and real-file coverage needs iteration on the streamer long
tail. Revised estimate:
- **human team of 1–2:** about 18–30 months for M0–M9;
- **with Claude Code:** about 4–8 months, bounded by ROOT CI turnaround,
  corpus work and the crash harness, not by writing code;
- **M0:** 1–2 weeks with Claude Code, including the CI harness.

The 0I table above is superseded.

### Decisions after the outside voice (recorded 2026-11-26)

| ID and owner | Contract and evidence | Current | Proposed | Status | Exact approval and scope |
|---|---|---|---|---|---|
| CEO-D1 commit writes | D11 / D9 invariant; K3 | single write ≤ 4 KiB | header, sync, root dir ≤ 512 B, sync | approved (reopens eng D11) | user: A |
| CEO-D2 roadmap order | D15; K2, K12 | M0–M9 | flat TTree writer after derives; RNTuple read before async/WASM (M0–M10) | approved (reopens eng D15, order only) | user: both items |
| CEO-D3 CI tiers | K4 | everything per PR | per-PR + nightly tiers; compile time alerts, does not gate | approved | user: A |
| CEO-D4 Goal 1 | K5 | "every file" | measured coverage vs ROOT dumps over real experiment files | approved | user: A |
| CEO-D5 RNTuple extend | K7 | unspecified | non-goal for 1.0 + TODO | approved | user: A |
| CEO-D6 lock fallback | C4; K8 | unspecified | proceed + `Diagnostic`; documented limits | approved | user: A |
| CEO-D7 HTTP pool | C5; K13 | process-wide client | explicit `HttpPool` in `ReadOptions` | approved (reopens C5's shape) | user: A |
| CEO-D8 binding rule | K15 | none | every capability reachable through `'static` handles + dynamic types | approved | user: A |
| TODOs | K6, K10, K11, CEO-D5, Section 10 | — | 5 items in `TODOS.md` | approved | user: all four groups |

**Common work (no question):** K1 effort re-baseline (factual correction);
K9 feature split under C3; K14 plan updated; 1-1 empty paths under Goal 1.

## Approval readiness

Every remedy applied to ARCHITECTURE.md revision 7 cites a user answer:
- CEO-0, C1–C7, CEO-D1–D8, TODOs;

or common work under an approved contract:
- 1-1 (Goal 1), K9 (C3), K14 (plan sync), K1 (factual).

**Approval readiness: PASS** (checked: CEO-0, C1–C7, CEO-D1–D8, TODOs).

---

## Required outputs

### Review facts

| Fact | Value |
|---|---|
| Mode | HOLD SCOPE |
| Findings | 1-1, 1-2, 3-1, 3-2, 4-1, 7-1, 8-1, 9-1, plus 15 outside (K1–K15) = 23 |
| Unresolved | 0 |
| Critical gaps | 2 found: 3-1 external files, and K3, a torn commit. **0 remaining** |
| Scope dispositions | none changed (HOLD); 1 explicit non-goal (CEO-D5) |
| Outside coverage | native fallback completed (does not count as external coverage) |
| **Status** | **clean** (0 unresolved, 0 open critical gaps) |

### Reality rows

The 0B lines above, with P9 and P10 added. `PLAN_REALITY`: done by hand
(the helper is not installed). unverified=5.

### NOT in scope

| Item | Disposition |
|---|---|
| Scope reduction (minimum viable 1.0) | declined: user chose HOLD SCOPE |
| RNTuple extension in update mode | non-goal for 1.0 (CEO-D5) → TODOS.md P3 |
| Dataset (`TChain`-like) API | TODOS.md P2 |
| Python bindings | TODOS.md P3 (timing decision after 0.2) |
| Analytics providers, native XRootD, `rootfile-raw` | after 1.0 (unchanged) |

### What already exists

oxyroot (Rust: TTree read, uncompressed write); uproot (Python: read/write,
RNTuple write without split-encoding extension); ROOT (C++, the reference);
JSROOT (browser). Reused libraries: as in the eng review.

### Dream state delta

The plan now covers the format and its safety. It reaches the 12-month
ideal's *distribution* only partly:
- releases come per milestone (C7);
- a design partner and adoption measure are TODO P1;
- Python and Arrow/DataFusion reach remains after 1.0.

The gap is now named, owned and scheduled, not closed.

### Error & Rescue Registry

Section 2's tables, with these updates:
- `ExternalFile` (C2) → refused with the file named;
- `Locked` (C4) → fail fast, or `Diagnostic` where locking is unsupported;
- the `read(..)` out-of-memory case stays accepted (D10).

### Failure Modes Registry

| CODEPATH | FAILURE MODE | RESCUED? | TEST? | USER SEES? | LOGGED? |
|---|---|---|---|---|---|
| open | truncated or unclosed file | Y (recover) | Y | data + Diagnostic | Y |
| read branch | `fFileName` points outside | Y (refuse) | Y (hostile fixtures) | `ExternalFile` error | Y |
| remote | 503 / truncated body | Y (retry, validate) | Y (T3) | data or `Io` | Y (stats) |
| remote, many files | connection explosion | Y (`HttpPool` cap) | Y (case) | — | Y (stats) |
| update | crash between commit writes | Y (ordered writes) | Y (crash harness) | old file | Y |
| update | concurrent rootfile writer | Y (lock) | Y | `Locked` | Y |
| update | concurrent ROOT writer / no locking on FUSE | **N** (impossible to exclude ROOT) | N | Diagnostic (where detectable) | Y |
| `read(..)` | out of memory on a huge column | N (D10 accepted) | N | crash | N |

**No row has RESCUED=N, TEST=N and USER SEES=Silent at once.** The two
unrescued rows are documented and visible, so there is **no CRITICAL GAP**.

### Diagrams

| # | Diagram | Where |
|---|---|---|
| 1 | System architecture | ARCHITECTURE.md §3 (module graph) |
| 2 | Data flow with shadow paths | Sections 1 and 4 above |
| 3 | State machine | §7.2 commit protocol; Section 1 (prepare / synced / committed / aborted) |
| 4 | Error flow | Section 2 tables |
| 5 | Deployment sequence | release-plz order (§3): `rootfile-source` → `rootfile-derive` → `rootfile` → adapters → cli |
| 6 | Rollback flowchart | Section 9: yank, then patch release, then advisory (if files are affected), then `rootfile check` detects affected files |

### Stale Diagram Audit

- §3 module graph: current (rev 6).
- §7.2 protocol diagram: updated in rev 7.
- §12 roadmap table: updated in rev 7.
- **Stale and historical (not updated by design):**
  - `eng-review-2026-11-26/ENG-REVIEW.md`, its single-write A2 and its
    M0–M9 numbering, superseded by CEO-D1 and CEO-D2;
  - `council-*` files.

### Scope Expansion Decisions

n/a (HOLD SCOPE).

## Implementation Tasks

- [ ] **T1 (P1, human: ~1 day / CC: ~15min)** — tree — Refuse external basket files by default; add `ReadOptions::external_files(base_dir)`; hostile fixtures
  - Surfaced by: 3-1, C2 · Files: `tree/branch.rs`, `lib.rs` (options) · Verify: the `/etc/passwd` and URL fixtures fail with `ExternalFile`; relative names inside `base_dir` work
- [ ] **T2 (P1, human: ~1 day / CC: ~15min)** — update — Commit as header → sync → root directory (≤ 1 sector) → sync; harness asserts the order
  - Surfaced by: K3, CEO-D1 · Files: `update/commit.rs`, `tests/crash/` · Verify: the harness tears at 512 B between and within the writes; every state reads as old or new
- [ ] **T3 (P1, human: ~2 days / CC: ~30min)** — ci — Two-tier CI: per PR (latest ROOT, sampled harness, 3 OS) and nightly (matrix 6.26/6.30/6.34/latest, full harness, fuzz, timings alert)
  - Surfaced by: K4, CEO-D3, C1 · Files: `.github/workflows/` · Verify: a PR run takes under 20 min; a nightly run covers the matrix
- [ ] **T4 (P1, human: ~3 days / CC: ~1h)** — tests — Real-file corpus (CMS NanoAOD/MiniAOD, ATLAS PHYSLITE Open Data) + ROOT dump reference + coverage report
  - Surfaced by: K5, CEO-D4 · Files: `xtask/corpus.rs`, `tests/cross/dump.C` · Verify: a coverage % is printed per release; mismatches are listed by class/branch
- [ ] **T5 (P1, human: ~2h / CC: ~10min)** — compress — Features `lzma` (lzma-rs decode, default) and `lzma-write` (liblzma, opt-in); nightly speed benchmark
  - Surfaced by: 3-2, C3, K9 · Files: `compress/lzma.rs`, `Cargo.toml` · Verify: `cargo tree` of a default build contains no `liblzma-sys`
- [ ] **T6 (P2, human: ~4h / CC: ~15min)** — update — `File::try_lock` on create/update; `Locked` error; fallback `Diagnostic`; MSRV 1.89
  - Surfaced by: 4-1, C4, CEO-D6 · Files: `lib.rs`, `update/`, `Cargo.toml` · Verify: two processes → the second fails fast; an unsupported filesystem → `Diagnostic`
- [ ] **T7 (P2, human: ~4h / CC: ~15min)** — io — `HttpPool` (client + cap 64) in `ReadOptions`; no global state
  - Surfaced by: 7-1, C5, CEO-D7 · Files: `io/http.rs` · Verify: 1 000 files over one pool never exceed 64 in-flight requests
- [ ] **T8 (P2, human: ~1 day / CC: ~20min)** — facade — `ReadStats`/`WriteStats`, tracing span conventions, troubleshooting guide
  - Surfaced by: 8-1, C6 · Files: `lib.rs`, `docs/troubleshooting.md` · Verify: the Appendix C over-read assertion uses `ReadStats`
- [ ] **T9 (P2, human: ~2h / CC: ~10min)** — tests — `tests/cases/empty.toml` (0 keys, 0 entries, 0 baskets, empty jagged entry)
  - Surfaced by: 1-1 · Files: `tests/cases/` · Verify: every source passes
- [ ] **T10 (P2, human: ~4h / CC: ~15min)** — release — `release-plz` configured for 0.x per milestone; changelog policy
  - Surfaced by: 9-1, C7 · Files: `release-plz.toml` · Verify: a dry-run 0.1 release
- [ ] **T11 (P3, human: ~4h / CC: ~15min)** — ci — An owned-API example per capability (`'static` handles + dynamic types)
  - Surfaced by: K15, CEO-D8 · Files: `examples/owned_*.rs` · Verify: the examples compile without generics or lifetimes in signatures

_No new tasks from Sections 5 and 6 beyond T4 and T9._

## Completion Summary

```
+--------------------------------------------------------------------+
| CEO REVIEW — rootfile (ARCHITECTURE.md rev 6 → rev 7)               |
+--------------------------------------------------------------------+
| Mode selected        | HOLD SCOPE (recommended REDUCTION declined)  |
| System Audit         | n/a: not a git repo; docs read in full       |
| Step 0               | premise challenged; dream state drawn; HOLD  |
| Section 1            | 2 issues (empty paths, compat target C1)     |
| Section 2            | 8 error paths, 1 GAP (OOM: accepted, D10)    |
| Section 3            | 2 issues, 1 High (external files C2)         |
| Section 4            | 4 edge cases, 1 unhandled → C4               |
| Section 5            | 0 issues                                     |
| Section 6            | diagram (eng review), 2 gaps → T4, T9        |
| Section 7            | 1 issue (connections → C5/CEO-D7)            |
| Section 8            | 1 issue (stats → C6)                         |
| Section 9            | 1 risk (no releases → C7)                    |
| Section 10           | Reversibility 1–4/5; 2 debt items            |
| Section 11           | SKIPPED (no UI scope)                        |
| NOT in scope         | written (5 items)                            |
| What already exists  | written                                      |
| Dream state delta    | written                                      |
| Error/rescue registry| written (Section 2 + updates)                |
| Failure modes        | 8 rows, 0 CRITICAL GAPS                      |
| TODOS.md updates     | 5 items added                                |
| Scope proposals      | 0 (HOLD)                                     |
| CEO plan             | n/a (HOLD: not written)                      |
| Outside voice        | native fallback, completed, 15 findings      |
| Lake Score           | 15/15                                        |
| Diagrams produced    | 6 (mapped above)                             |
| Stale diagrams       | 2 historical files, superseded by design     |
| Unresolved decisions | 0                                            |
| Reality rows         | 7 rows, unverified=5                         |
+--------------------------------------------------------------------+
```

The Lake Score counts C1–C7 and CEO-D1–D8: every answer picked the complete
(recommended) option. The mode and TODO choices are not scored.

## Unresolved Decisions

None.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|---|---|---|---|---|---|
| CEO Review | /plan-ceo-review | Scope & strategy | 1 | CLEAR | mode HOLD SCOPE; 0 proposals, 0 accepted, 1 non-goal recorded; 23 findings, 0 unresolved |
| Outside Review | native fallback (Claude Plan subagent), CEO + Eng phases | Independent 2nd opinion | 2 | CLEAR | 15 findings (CEO), 9 (Eng); all dispositioned |
| Eng Review | /plan-eng-review | Architecture & tests (required) | 1 | CLEAR, **stale** | 28 issues, 0 critical gaps; plan revised after it (CEO-D1 reopened D11, CEO-D2 reopened D15, Goal 1 restated) |
| Design Review | /plan-design-review | UI/UX gaps | 0 | not run (no UI) | — |
| DX Review | /plan-devex-review | Developer experience gaps | 0 | not run | — |

OUTSIDE COVERAGE: native fallback only (Codex CLI not installed). Completed in both phases; per the skill, native results do not count as external coverage.
VERDICT: CEO CLEARED; eng review required (the eng review is stale after plan revision 7)

NO UNRESOLVED DECISIONS
