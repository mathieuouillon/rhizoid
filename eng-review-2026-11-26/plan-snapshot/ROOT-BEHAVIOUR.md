# What ROOT actually writes and reads

Checked against ROOT's `master` sources on 2026-11-25: `io/io/src/TFile.cxx`,
`TKey.cxx`, `TDirectoryFile.cxx`, `TBufferFile.cxx`, `core/base/inc/TBuffer.h`,
`tree/tree/src/TBasket.cxx`, `TBranch.cxx`, `TTree.cxx`, the `tree/tree/inc`
headers of `TTree`, `TBranch`, `TBranchElement`, `TLeaf*` and `TBasket`,
`tree/ntuple/src/RMiniFile.cxx` and the RNTuple binary specification, plus
uproot's TTree writer for §8.
Items marked *(unverified)* came from a summary of the source, not from the
code itself, and must be checked before implementing.

This complements `ROOT File Format.md` (the layout) with ROOT's *behaviour*:
the order of operations, the validation it does, and the corner cases a
compatible implementation has to reproduce.

---

## 1. Opening a file for reading (`TFile::Init`)

1. Reads the first **300 bytes** (`kBEGIN + 200`) in one go.
2. Checks the magic `"root"`; then reads `fVersion`, `fBEGIN`, and the rest of
   the header in the small or large form (`fVersion >= 1000000` → large).
3. Rejects `fBEGIN < 0 || fBEGIN > fEND`. It does **not** check `fBEGIN == 100`.
4. Reads the root directory record at `fBEGIN + fNbytesName`, from the 300-byte
   buffer when it fits, else with a second read. Rejects
   `fNbytesName < 10 || > 10000`.
5. Compares the file size with `fEND`: smaller → "truncated"; no key list →
   "probably not closed". Either way, with `TFile.Recover` on (default), it
   calls `Recover()`, which walks records from `fBEGIN`.
6. Reads the root key list (`TDirectoryFile::ReadKeys`). Every key's
   `SeekKey` and `SeekPdir` must lie in `[64, file size]`, else it stops with
   "reading illegal key".
7. Reads the **StreamerInfo record eagerly** at open (`fgReadInfo`, default on).
8. Counts keys of class `TProcessID` (it does not read them yet).
9. Reads the free list **only if the file is opened for writing**.

Subdirectories are lazy: a subdirectory's key list is read when the directory
itself is read.

## 2. Creating a file

1. `fVersion = gROOT->GetVersionInt()` (e.g. `63406`), `fUnits = 4`,
   `fBEGIN = 100`.
2. The free list starts as **one segment `[100, 2 000 000 000]`**; when `fEND`
   passes the segment's end, it is extended by 10⁹.
3. The record at byte 100 is a key of class `TFile` named after the **file
   name**, holding `TNamed` (name, title) + the root directory record.
   `fNbytesName = KeyLen + TNamed size`.
4. The header is written immediately, then rewritten at close.

## 3. Closing a file (`TFile::Close`)

Verified order:

1. `WriteStreamerInfo()` — only the classes actually written (tracked in
   `fClassIndex`). The `TList` also carries a `TList` named `listOfRules`
   with the schema-evolution rules *(unverified)*. The old record is freed.
2. `TDirectoryFile::Close` → for each directory: write a new key list (old one
   freed), rewrite the directory record in place at `fSeekDir + fNbytesName`.
3. `WriteFree()` then `WriteHeader()`; the header switches to the large form
   when `fEND > kStartBigFile` (`fVersion += 1000000`, `fUnits = 8`)
   *(unverified detail)*.
4. Flush, close the descriptor.

## 4. Keys (`TKey`)

- Key version `> 1000` ⇔ 8-byte seeks. **Writers** add 1000 when the record's
  position is `> kStartBigFile`; **readers** only test `version > 1000`.
- In the 8-byte form, the stored `SeekPdir` word also carries a *process-ID
  offset* in its high bits (`fPidOffset << kPidOffsetShift`); mask it off.
- Readers reject negative `Nbytes`, `ObjLen`, `KeyLen` and `Nbytes < KeyLen`.
- The class name of a `TDirectoryFile` is stored as **`"TDirectory"`**.
- **Cycles**: a new key under an existing name gets `max cycle + 1` and is
  inserted *before* the older ones. `Get("name")` returns the highest cycle;
  `Get("name;n")` needs an exact match. Negative cycles mean "keep"; ROOT uses
  the absolute value.
- **Compression on write** happens only if the level is > 0 **and
  `ObjLen > 256`**. If *any* block fails to shrink below `ObjLen`, the whole
  object is stored raw.
- **Read**: decompress iff `ObjLen > Nbytes - KeyLen`.
- **Allocation**: best-fitting free segment, else `fEND`. When a record leaves
  a remainder in a gap, ROOT writes a **negative `i32` (−remaining size)** at the
  start of the remainder, so sequential scans can skip it.
- **Deleting** a key frees its bytes (`MakeFree`), but deleting a directory key
  frees nothing.

## 5. Directories (`TDirectoryFile`)

- Record: version, `fDatimeC`, `fDatimeM`, `fNbytesKeys`, `fNbytesName`, three
  seeks (8 bytes if version > 1000), `TUUID`, then **12 zero bytes in the
  small form** (only when the file version ≥ 40000) so the record can be
  rewritten large in place.
- Version % 1000: `< 2` no UUID, `== 2` old UUID layout, `> 2` current.
- Key-list record: a key (name/title/class of the directory: `TFile` for the
  root, `TDirectory` for subdirectories), then `i32 nkeys`, then the key
  headers. ROOT reserves 8 extra bytes in large files that are never written.
- Writers other than ROOT use other class names here (RNTuple's mini writer
  uses `""`), so **readers must not depend on bookkeeping class names**.

## 6. Object streaming (`TBufferFile`)

- **Byte count**: `count | 0x40000000`, the count excluding its own 4 bytes.
  Absence is detected by bit 30 being clear; the reader then rewinds and reads
  a bare 2-byte version (`TObject` and old data).
- **Checksum after the version**: written only for *foreign* classes (no
  `ClassDef`) with version ≤ 1, as `version 0` + `u32 checksum`. A reader reads
  it when `version <= 0` and the byte count is `>= 6`.
- **Member-wise** collections OR `0x4000` (`kStreamedMemberWise`) into the
  version; it is not masked by `ReadVersion`, the caller masks it.
- **Byte-count mismatch**: ROOT logs an error and **repositions to the
  declared end**; it does not fail.
- **Class tags / references**: map offset = position of the byte-count word
  + `kMapOffset` (2). Positions are **relative to the start of the buffer, which
  includes the key header** (`fKeylen`): the reader places the decompressed
  object after a copy of the key, so offsets work out. `kMaxMapCount =
  0x3FFFFFFE`. Null pointer = `0`.
- **Strings**: `TString` and `std::string` = 1-byte length (or `255` + `i32`);
  a null `std::string*` is one `0` byte. `char*` = **`i32` length** + bytes, no
  terminator. `WriteString` (rare) writes the trailing NUL.
- `Long_t` is always 8 bytes on disk; `bool` 1 byte.
- **Arrays sized by another member** (`//[fN]`, type code 40 + n) are
  preceded by a 1-byte flag: 0 means null with no data, 1 means the array
  follows (§8.6).
- **`Float16_t` / `Double32_t`**:
  - with a range `[min, max, nbits]`: `u32(0.5 + factor·(x − min))`, 4 bytes;
  - with `nbits` only: 1-byte exponent + 2-byte mantissa (sign in bit
    `nbits+1`), 3 bytes; `Float16_t` defaults to 12 bits;
  - `Double32_t` without range or bits: a plain `float`.

## 7. TTree baskets (`TBasket`)

- Basket keys are always **large** (ROOT sets `fVersion += 1000` in the
  constructor).
- Key extension: version (2), `fBufferSize` (4), `fNevBufSize` (4; **negative
  means a 1-byte `fIOBits` follows**), `fNevBuf` (4), `fLast` (4), flag (1).
- Flag = 1 (has offsets) or 2 (no offsets), +10 data embedded, +40
  displacement array, +80 offsets must be regenerated (not stored). On-disk
  baskets are normally header-only: flag 0 or 80.
- Entry offsets are written **after the data at `fLast`**, as a count followed
  by `fNevBuf + 1` `i32`s, **absolute positions counted from the start of the
  key** (entry 0 starts at `fKeylen`). A displacement array may follow.
- Without offsets, entry *i* is at `fKeylen + i · fNevBufSize`.
- Compression follows the key rules (raw if a block does not shrink).

## 8. TTree / TBranch / TLeaf

Verified on 2026-11-25 in `tree/tree/inc/TTree.h`, `TBranch.h`,
`TBranchElement.h`, `TLeaf.h`, `TLeaf{I,D,F,L,B,S,O,C}.h`, `TLeafElement.h`,
`TBasket.h`, `ROOT/TIOFeatures.hxx` and `TTree::Streamer`. Cross-checked
against uproot's TTree writer (`uproot/writing/_cascadetree.py`), whose output
ROOT reads.

### 8.1 Class versions

| Class | Version | Bases |
|---|---|---|
| `TTree` | **20** | `TNamed`, `TAttLine`, `TAttFill`, `TAttMarker` |
| `TBranch` | **13** | `TNamed`, `TAttFill` |
| `TBranchElement` | 10 | `TBranch` |
| `TLeaf` | 2 | `TNamed` |
| `TLeafI/D/F/L/B/S/O/C` | 1 | `TLeaf` |
| `TLeafElement` | 1 | `TLeaf` |
| `TBasket` | 3 | `TKey` |
| `ROOT::TIOFeatures` | **no `ClassDef`**: written as version 0 + checksum (see 8.6) | — |

### 8.2 `TTree` persistent members, in streaming order

After the four bases:

| # | Member | Type | Default (ROOT ctor / uproot) |
|---|---|---|---|
| 1 | `fEntries` | `Long64_t` | — |
| 2 | `fTotBytes` | `Long64_t` | 0 |
| 3 | `fZipBytes` | `Long64_t` | 0 |
| 4 | `fSavedBytes` | `Long64_t` | 0 |
| 5 | `fFlushedBytes` | `Long64_t` | 0 |
| 6 | `fWeight` | `Double_t` | 1.0 |
| 7 | `fTimerInterval` | `Int_t` | 0 |
| 8 | `fScanField` | `Int_t` | 25 |
| 9 | `fUpdate` | `Int_t` | 0 |
| 10 | `fDefaultEntryOffsetLen` | `Int_t` | 1000 |
| 11 | `fNClusterRange` | `Int_t` | 0 |
| 12 | `fMaxEntries` | `Long64_t` | 1 000 000 000 000 |
| 13 | `fMaxEntryLoop` | `Long64_t` | 1 000 000 000 000 |
| 14 | `fMaxVirtualSize` | `Long64_t` | 0 |
| 15 | `fAutoSave` | `Long64_t` | −300 000 000 (bytes) |
| 16 | `fAutoFlush` | `Long64_t` | −30 000 000 (bytes) |
| 17 | `fEstimate` | `Long64_t` | 1 000 000 |
| 18 | `fClusterRangeEnd` | `Long64_t*` `[fNClusterRange]` | inclusive last entry of each range |
| 19 | `fClusterSize` | `Long64_t*` `[fNClusterRange]` | entries per cluster in each range |
| 20 | `fIOFeatures` | `ROOT::TIOFeatures` | one `UChar_t fIOBits` = 0 |
| 21 | `fBranches` | `TObjArray` | the top-level branches |
| 22 | `fLeaves` | `TObjArray` | every leaf, as **references** to the leaves inside `fBranches` |
| 23 | `fAliases` | `TList*` | null |
| 24 | `fIndexValues` | `TArrayD` | empty |
| 25 | `fIndex` | `TArrayI` | empty |
| 26 | `fTreeIndex` | `TVirtualIndex*` | null |
| 27 | `fFriends` | `TList*` | null |
| 28 | `fUserInfo` | `TList*` | null |
| 29 | `fBranchRef` | `TBranchRef*` | null |

Every other member (`fMaxClusterRange`, `fCacheSize`, `fDirectory`, IMT
state, …) is transient.

### 8.3 `TTree::Streamer`

- **Read, version > 4:** `ReadClassBuffer`, the generic path driven by
  `TStreamerInfo`. Then ROOT fixes things up:
  - links branches, `fBranchRef`, friends and the index back to the tree;
  - **drops a non-empty old-style `fIndex`/`fIndexValues`** with a warning;
  - raises `fEstimate ≤ 10000` to 1 000 000;
  - sets `fMaxClusterRange = fNClusterRange`.
- **Read, version ≤ 4** (very old files): a hand-written member-by-member
  layout with `double` counters.
- **Write:** `WriteClassBuffer`, with the `TRefTable` disabled around it.
  Nothing else is set at write time.

### 8.4 `TBranch` persistent members (v13), in streaming order

After `TNamed` and `TAttFill`:

| Member | Type | Notes |
|---|---|---|
| `fCompress` | `Int_t` | `algorithm·100 + level`; uproot writes 0 and compresses with the file setting |
| `fBasketSize` | `Int_t` | 32 000 |
| `fEntryOffsetLen` | `Int_t` | 0 for fixed-size leaves, 1000 when the leaf has a count |
| `fWriteBasket` | `Int_t` | number of baskets written = index of the next one |
| `fEntryNumber` | `Long64_t` | entries filled |
| `fIOFeatures` | `TIOFeatures` | as in `TTree` |
| `fOffset` | `Int_t` | 0 |
| `fMaxBaskets` | `Int_t` | capacity of the three basket arrays |
| `fSplitLevel` | `Int_t` | 0 for simple branches |
| `fEntries` | `Long64_t` | |
| `fFirstEntry` | `Long64_t` | 0 |
| `fTotBytes` | `Long64_t` | sum of `KeyLen + ObjLen` of its baskets |
| `fZipBytes` | `Long64_t` | sum of `Nbytes` of its baskets |
| `fBranches` | `TObjArray` | sub-branches |
| `fLeaves` | `TObjArray` | the leaf objects themselves |
| `fBaskets` | `TObjArray` | baskets kept in memory (embedded); empty when all are on disk |
| `fBasketBytes` | `Int_t*` `[fMaxBaskets]` | |
| `fBasketEntry` | `Long64_t*` `[fMaxBaskets]` | first entry of each basket; **`fBasketEntry[fWriteBasket]` holds the total entry count**, so the capacity must exceed the number of baskets |
| `fBasketSeek` | `Long64_t*` `[fMaxBaskets]` | |
| `fFileName` | `TString` | empty = same file |

The arrays are written with their full `fMaxBaskets` length, zero-padded.

`TBranchElement` (v10) adds `fClassName`, `fParentName`, `fClonesName`,
`fCheckSum`, `fClassVersion`, `fID`, `fType`, `fStreamerType`, `fMaximum`,
`fBranchCount`, `fBranchCount2`. The values of `fType`:

| `fType` | Meaning |
|---|---|
| −1 | unsplit object with a custom streamer |
| 0 | unsplit object, or a simple member of a split object |
| 1 | base class of a split object |
| 2 | class-typed member of a split object |
| 3 | counter of a split `TClonesArray` |
| 31 | member of the content of a split `TClonesArray` |
| 4 | counter of a split STL collection |
| 41 | member of the content of a split STL collection |

### 8.5 `TLeaf` persistent members

| Class | Members |
|---|---|
| `TLeaf` v2 | `fLen` (`Int_t`, fixed element count), `fLenType` (`Int_t`, bytes per element), `fOffset` (`Int_t`), `fIsRange` (`bool`: this leaf is a counter for others), `fIsUnsigned` (`bool`), `fLeafCount` (`TLeaf*`: the counter of a variable-length leaf, written as a reference) |
| `TLeafX` v1 | `fMinimum`, `fMaximum` in the leaf's own type. For a counter leaf, `fMaximum` = the largest count, which ROOT uses to size buffers |
| `TLeafElement` v1 | `fID`, `fType` |

Leaf class letters as uproot writes them:

| Letter | Leaf class | Type | Unsigned |
|---|---|---|---|
| `O` | `TLeafO` | `bool` | — |
| `B` | `TLeafB` | `i8` | — |
| `b` | `TLeafB` | `u8` | `fIsUnsigned` |
| `S` / `s` | `TLeafS` | `i16` / `u16` | lowercase |
| `I` / `i` | `TLeafI` | `i32` / `u32` | lowercase |
| `L` / `l` | `TLeafL` | `i64` / `u64` | lowercase |
| `F` | `TLeafF` | `f32` | — |
| `D` | `TLeafD` | `f64` | — |
| `C` | `TLeafC` | string; `fLen` = longest string | — |

The branch title follows ROOT's leaf-list syntax, `name[counter]/F`.

### 8.6 Encoding details a writer must reproduce

- **Arrays with a count member** (`Long64_t* fX; //[fN]`, type code 40 + n)
  are preceded by a **1-byte flag**: 0 for a null pointer with nothing after
  it, 1 when the array follows. uproot calls this the "speedbump": empty
  cluster arrays are written as a single `0`, basket arrays as `1` + data.
- **`TIOFeatures`** has no `ClassDef`, so it is a foreign class. It is written
  as byte count `7`, **version 0**, the 4-byte checksum `0x1aa12f10`, then
  `fIOBits`. This confirms the checksum rule in §6.
- **Object bits:** uproot writes `fBits = 0x03000000` on leaves and
  `0x00400000` on branches (`kDoNotUseBufferMap`).
- **`TObjArray`s of branches and leaves** use `WriteObjectAny` class tags.
  `TTree::fLeaves` refers back to the leaf objects already written inside the
  branches, so writing it needs the reference map (§6).
- **Basket keys:** class `TBasket`, name = branch name, title = tree name,
  cycle 0, key version 1004. `fBufferSize` 32 000.
  - Fixed-size data: `fNevBufSize` = item size and flag 0; no offsets.
  - Jagged data: entry offsets (from the start of the key) appended after the
    data, as `i32 count` then `i32`s. `fNevBufSize` = `count + 1`, and `fLast`
    points to the end of the data.

### 8.7 Lifecycle

- `fBasketEntry` is searched by binary search.
- `fClusterRangeEnd[i]` is **inclusive**.
- **ROOT** grows the basket arrays by 1.5× from 10.
- **`TTree::AutoSave`** (verified):
  - with the `flushbaskets` option, it first flushes every branch;
  - it sets `fSavedBytes = GetZipBytes()`;
  - it writes the tree as a **new key**, i.e. a new cycle;
  - only after that write succeeds does it delete the previous key of the
    same name and class. With `overwrite`, the deletion happens inside
    `WriteTObject` instead;
  - it then rewrites the StreamerInfo record every time;
  - with `saveself`, it also rewrites the directory and the header.

  A file killed mid-run therefore holds at most two tree cycles, and the
  highest one is the last complete AutoSave.
- **uproot** reserves basket-array capacity, rewrites the tree metadata **in
  place** while it fits, and otherwise rewrites the whole tree object
  elsewhere and frees the old copy.

### 8.8 Split objects: `TBranchElement`

Verified in `tree/tree/src/TBranchElement.cxx` (all of it read), plus the
type constants in uproot's `const.py`.

**Streamer element type codes.** These are the values found in
`fStreamerType` and in `TStreamerElement::fType`:

| Code | Name | Meaning |
|---|---|---|
| 0 | `kBase` | base class |
| 1–19 | basic types | §7 of the format reference; `kCounter` = 6, `kCharStar` = 7, `kBits` = 15, `kFloat16` = 19 |
| 21–39 | `kOffsetL` + n | fixed array of a basic type |
| 41–59 | `kOffsetP` + n | array whose length is another member (`//[fN]`), with the 1-byte presence flag |
| 61 / 62 | `kObject` / `kAny` | member object (`TObject`-derived / not) |
| 63 / 64 | `kObjectp` / `kObjectP` | pointer to an object (`//->` / not) |
| 65 / 66 / 67 | `kTString` / `kTObject` / `kTNamed` | |
| 68 / 69 / 70 | `kAnyp` / `kAnyP` / `kAnyPnoVT` | pointers to non-`TObject` classes |
| 71 | `kSTLp` | pointer to an STL collection |
| 300 | `kSTL` | STL collection |
| 365 | `kSTLstring` | `std::string` |
| 500 / 501 | `kStreamer` / `kStreamLoop` | custom-streamed member; array of objects with a count |

> The format reference lists `TStreamerSTL` as 500. ROOT's constants give
> `kSTL = 300`, and 500 is `kStreamer`. Readers should recognize an STL
> element by its **element class (`TStreamerSTL`) and `fSTLtype`**, not by the
> numeric code alone.

**How a branch is built** (`Init`, `Unroll`):

| `fType` | Built when | Basket? |
|---|---|---|
| −1 | top-level object whose class has a custom `Streamer` | yes |
| 0, `fID = −1` | top-level object, not split (`fStreamerType = −1`) | yes |
| 0, `fID = −2` | top-level object, split: a pure container node | **no** |
| 0, `fID ≥ 0` | a member that is not split further (`fStreamerType = element type`) | yes |
| 1 | a base class of a split object | only if not split further |
| 2 | a class-typed member (`kObject`/`kAny`) that is split | only if not split further |
| 3 | a `TClonesArray`, split: the **counter** | yes: one `Int_t` per entry |
| 31 | a member of the `TClonesArray`'s content | yes |
| 4 | an STL collection, split: the **counter** | yes: one `Int_t` per entry |
| 41 | a member of the collection's content | yes |

- Split nodes store no data; ROOT says so in `SetActionSequence`, and
  `FillImpl` only counts entries for them.
- `fID = −2` is never *assigned* in `TBranchElement.cxx`; it comes from
  `TTree::BronchExec` (§8.9).
- **Counter leaves:** the counter's `TLeafElement` is named `name_`, with any
  trailing dot removed, and has `fIsRange` set. The children of a 3 or 4
  counter get titles like `fTracks.fPx[fTracks_]` and `fBranchCount` pointing
  at the counter. `fBranchCount2` is used for a variable array
  (`kOffsetP` + n) inside a collection element.
- **`fMaximum`** starts at 0 and is raised on fill to the largest count. On
  read, a count `< 0` or `> fMaximum` is treated as a missing collection,
  when the entry offsets allow it, and otherwise logged as an error.
- **`fEntryOffsetLen`** is set to the tree default (1000), making entries
  variable-size, when any of these hold:
  - the branch is inside a collection (`btype ≠ 0`);
  - `fStreamerType ≤ kBase`;
  - the type is `kCharStar` or `kBits`;
  - the type is `> kFloat16`, i.e. arrays, objects, strings, STL.

  Plain numeric members keep 0 and fixed-size entries.
- **Splitting stops** at:
  - abstract classes;
  - members whose `CannotSplit()` is true, as with `//||` and custom
    streamers;
  - `TString` / `std::string` collections;
  - collections of collections;
  - collections of pointers below split level 100
    (`kSplitCollectionOfPointers = 100`, §8.9; the code uses
    `splitlevel % kSplitCollectionOfPointers`).

  The `TObject` base is skipped when the class ignores the `TObject`
  streamer, and empty bases get no branch.
- **`TBranchElement::Streamer`** is generic (`ReadClassBuffer` /
  `WriteClassBuffer`), but:
  - on write, a negative `fClassVersion` is written as positive;
  - the class descriptions of the branch's class, of the `TClonesArray`
    content (type 3) and of the collection's value class (type 4) are
    **forced into the StreamerInfo record**;
  - on read, a type-0 branch without leaves gets a `TLeafElement` created.

**What one entry looks like in a basket** (`FillLeaves*` / `ReadLeaves*`):

| `fType` | Writer | Entry bytes |
|---|---|---|
| −1 | `FillLeavesCustomStreamer` | the class's own `Streamer` output for the whole object |
| 0, `fID = −1` | `FillLeavesMember` | the object's members through its action sequence, i.e. the layout its `StreamerInfo` describes |
| 0 / 1 / 2 leaf, `fID ≥ 0` | `FillLeavesMember` (or `…Counter`, `…BranchCount`) | one member's value. For a `//[fN]` member, the count lives in another branch |
| 3 | `FillLeavesClones` | `Int_t n` (the element count) |
| 31 | `FillLeavesClonesMember` | this member for elements `0..n`, back to back; no count, no header |
| 4 | `FillLeavesCollection` | `Int_t n` (the collection size) |
| 41 | `FillLeavesCollectionMember` (+ pointer and associative variants) | this member for elements `0..n`, back to back; no count, no header |

- The member-wise flag `0x4000` does **not** appear in split baskets. A split
  collection is member-wise by construction, one branch per member.
- When reading, the counter (3/4) is read first, and the members (31/41) use
  `fBranchCount->GetNdata()`. Associative containers read their members from
  inside the counter's read.
- Headers inside unsplit objects: see §8.9.

### 8.9 How a top-level branch is chosen, and headers inside unsplit objects

Verified in `TTree.h`, `TTree::BronchExec`, `TBranchElement.cxx`,
`io/io/src/TStreamerInfoWriteBuffer.cxx` and uproot's readers.

**`kSplitCollectionOfPointers`** is an anonymous enum in `TTree`, under the
comment "Split level modifier": `kSplitCollectionOfPointers = 100`.

**`TTree::BronchExec`** decides the top-level branch in this order:

1. A `TClonesArray`:
   - split level > 0: a `TBranchElement` that is a split clones branch
     (type 3);
   - otherwise: a `TBranchObject`.
2. A class with a collection proxy (an STL collection): a `TBranchElement`
   through the collection `Init`, with `fID = −1`. It is split (type 4) when
   the split rules allow.
3. `splitlevel < 0`, or split level 0 for a `TObject` with a custom streamer:
   a **`TBranchObject`**.
4. If the class cannot be split, the split level is forced to 0.
5. Otherwise:

   ```cpp
   Int_t id = -1;
   if (splitlevel > 0) { id = -2; }
   TBranchElement* branch = new TBranchElement(this, name, sinfo, id, objptr, bufsize, splitlevel);
   ```

   The branch is unrolled into sub-branches only when
   `splitlevel % kSplitCollectionOfPointers > 0`.

So `fID = −2` means a split top-level object: a container node with no
baskets. `fID = −1` means the whole object is stored in each entry.

**Headers inside an unsplit object**, from `WriteBufferAux`:

| Member kind | What is written |
|---|---|
| basic types, fixed arrays of them | raw values, no header |
| `kSTL` (300) | `WriteVersion(…, kTRUE)` — a **byte count + version** — then the collection, then `SetByteCount`. Member-wise: `WriteVersionMemberWise` + byte count, then the **element class's version**, then the members |
| `kSTLp` | byte count + version around `cl->Streamer(…)` |
| `kStreamer` (500) | byte count + version around the custom streamer |
| arrays of `TString` (`kOffsetL + kTString`) | byte count + version around the array; a single `TString` has no header |
| `kTObject`, `kTNamed` | their own `Streamer`: `TObject` has no byte count [§7], `TNamed` has one |
| `kObject`/`kAny` (61/62) | `WriteFastArray(obj, cl, n, streamer)`, which streams each object through its class's `Streamer`. For a `TStreamerInfo`-described class, that is `WriteClassBuffer`, with a byte count + version as in [§7] *(the `WriteFastArray` → `WriteClassBuffer` step comes from the `TBufferFile` summary, §6)* |
| `kBase` (0) | `TStreamerBase::WriteBuffer`, i.e. the base class's own streamer, with a byte count + version as in [§7] |

- The version written for `kSTL`, `kSTLp`, `kStreamer` and `TString` arrays
  is that of the **`TStreamerInfo` class** (`this->IsA()`), not of the
  member's type. Readers skip it by its byte count and must not interpret it
  as the collection's version.
- **The top-level object itself** (type 0, `fID = −1`) is written by
  `FillLeavesMember` through the class's action sequence. uproot reads these
  entries with `outer_header=True`, i.e. a **byte count + version** before
  each entry, and for a top-level `std::vector` the element count follows. The
  action-sequence code (`TStreamerInfoActions.cxx`) was not read; the M2
  corpus tests pin this down on real files.

## 9. RNTuple inside a ROOT file (`RMiniFile`)

- Two writers: through a real `TFile`, or a stand-alone "simple" writer that
  hand-serializes the TFile records. Order in the simple writer: header → TFile
  key (+ directory + UUID + 12 bytes padding) → `RBlob`s → anchor key → key list
  → StreamerInfo → free list → header rewrite.
- A third container exists: **bare RNTuple files** (magic `"rntuple"`, no TFile
  at all, blobs without keys).
- `RBlob` keys: class `"RBlob"`, empty name and title, **large form (version
  1004)**, `SeekPdir = 100`, never listed in a key list, no key-level
  compression (pages compress themselves).
- Payloads larger than `MaxKeySize` are split: the first blob holds the data
  head plus, at its tail, a table of `u64` offsets of the other chunks.
- **Anchor**: 70 bytes, all big-endian: byte count `0x40000000 | 66`, class
  version **2**, epoch/major/minor/patch (`u16` each), seek/nbytes/len of
  header and footer (`u64` each), `MaxKeySize` (`u64`); followed by an
  **XXH3-64 checksum (big-endian) of bytes 6..70**, i.e. everything after the
  byte count and class version. The anchor key's `ObjLen` is 78 and its
  payload may be compressed.
- The reader finds the anchor by matching **class name then object name** in
  the key list (first match, cycles ignored), and verifies the checksum over
  `[6, len − 8)` so future appended fields are covered.
- Epoch and feature flags are checked by the descriptor layer, not by the
  anchor reader. Readers must refuse unknown feature-flag bits.

## 10. Consequences for the Rust design

| Fact | Design consequence |
|---|---|
| Reference offsets include the key header | `RBuffer` carries an `origin` (= `KeyLen`) so class tags and back-references resolve; baskets use the same origin for entry offsets |
| No compression for `ObjLen ≤ 256`, raw if any block fails to shrink | The writer's compression policy reproduces this exactly, for byte-identical output |
| ROOT tolerates byte-count mismatches | Reader has a `Strictness` setting: `Strict` (error) or `Lenient` (warn + reposition, ROOT's behaviour, the default) |
| `SeekPdir` high bits hold a PID offset | `KeyHeader::decode` masks it into a separate field |
| Directory keys are stored as `"TDirectory"` | Class-name normalization table in core |
| Baskets are always large keys | Key encoding is chosen per record kind, not only from the offset |
| Gap markers are negative `i32`s | The record walker (`check`, recovery) handles them |
| Bookkeeping records have inconsistent class names | Readers follow pointers, never class names, for key lists, free lists, StreamerInfo |
| StreamerInfo read eagerly by ROOT | We stay lazy for speed, but `check` validates it eagerly |
| RNTuple bare files exist | `rootfile-ntuple` reads both containers behind a `Container` enum |
| ROOT has a `kReproducible` mode (datime = 1, zero UUID) | Our deterministic mode uses the same values, so outputs can be compared byte for byte with ROOT's |
| `//[fN]` arrays carry a 1-byte presence flag | The streamer element kind `VarArray` reads and writes the flag; it is not part of the array |
| Classes without `ClassDef` (`TIOFeatures`) are written as version 0 + checksum | `ClassVersion` decoding handles 0 + checksum; built-in models carry the checksum |
| `fBasketEntry[fWriteBasket]` holds the total entry count | The tree writer keeps basket capacity > number of baskets |
| `TTree::fLeaves` refers to leaves written inside the branches | The tree writer serializes the tree through one `WBuffer` with a shared reference map |
| uproot writes only `TBranch` + `TLeaf{O,B,S,I,L,F,D,C}` and ROOT reads it | Our first TTree writer uses the same proven subset; `TBranchElement` comes later |
| Split container nodes (`fID = −2`, split types 1/2) have no baskets | The reader builds them as groups of child columns; the writer emits no baskets for them |
| Split collections: counter branch (3/4) = one `Int_t` per entry, members (31/41) = values back to back | Read as `Jagged` columns sharing the counter's offsets; written the same way |
| STL element codes differ between sources (300 vs 500) | Recognize `TStreamerSTL` by its class and `fSTLtype` |
| AutoSave writes a new cycle, then deletes the old one | Readers take the highest cycle; our writer's optional autosave does the same |
