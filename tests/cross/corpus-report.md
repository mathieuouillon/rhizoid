# Corpus coverage

| file | trees | branches | groups | match | unsupported | differ |
|---|---|---|---|---|---|---|
| uproot-Event.root | 1 | 60 | 5 | 47 | 0 | 0 |
| uproot-HZZ-objects.root | 1 | 22 | 0 | 22 | 0 | 0 |
| uproot-HZZ.root | 1 | 51 | 0 | 51 | 0 | 0 |
| uproot-Zmumu.root | 1 | 20 | 0 | 20 | 0 | 0 |
| uproot-demo-double32.root | 1 | 26 | 1 | 25 | 0 | 0 |
| uproot-double32-float16.root | 1 | 12 | 0 | 12 | 0 | 0 |
| uproot-foriter.root | 1 | 1 | 0 | 1 | 0 | 0 |
| uproot-issue21.root | 1 | 7 | 0 | 7 | 0 | 0 |
| uproot-leaflist.root | 1 | 1 | 0 | 1 | 0 | 0 |
| uproot-mc10events.root | 1 | 687 | 2 | 674 | 0 | 0 |
| uproot-nesteddirs.root | 3 | 66 | 2 | 64 | 0 | 0 |
| uproot-physlite-rntuple_v1-0-0-0.root | 6 | 3783 | 203 | 2971 | 0 | 0 |
| uproot-sample-5.23.02-zlib.root | 1 | 35 | 0 | 35 | 0 | 0 |
| uproot-sample-5.30.00-zlib.root | 1 | 35 | 0 | 35 | 0 | 0 |
| uproot-sample-6.08.04-lzma.root | 1 | 35 | 0 | 35 | 0 | 0 |
| uproot-sample-6.20.04-lz4.root | 1 | 35 | 0 | 35 | 0 | 0 |
| uproot-sample-6.20.04-zlib.root | 1 | 35 | 0 | 35 | 0 | 0 |
| uproot-simple.root | 1 | 3 | 0 | 3 | 0 | 0 |
| uproot-small-dy-nooffsets.root | 1 | 8 | 0 | 8 | 0 | 0 |
| uproot-small-dy-withoffsets.root | 1 | 8 | 0 | 8 | 0 | 0 |
| uproot-small-evnt-tree-fullsplit.root | 1 | 43 | 2 | 41 | 0 | 0 |
| uproot-small-evnt-tree-nosplit.root | 1 | 1 | 0 | 1 | 0 | 0 |
| uproot-small-flat-tree.root | 1 | 20 | 0 | 20 | 0 | 0 |
| uproot-stl_containers.root | 1 | 26 | 0 | 26 | 0 | 0 |
| uproot-vectorVectorDouble.root | 1 | 1 | 0 | 1 | 0 | 0 |

**4178 branches match uproot, 0 not supported yet, 0 differ** (215 groups of sub-branches hold no values of their own; 628 more are read without error where uproot cannot read them).

## Not supported yet


## Known uproot errors (our values checked against uproot's valid part)

- uproot-Event.root `fTracks.fVertex[3]`: uproot reads 3x the values of a Double32_t[3] member of a split TClonesArray, running into the next entries and then uninitialised memory (1e-310); only entry 0's start can be compared, and each of our entries holds 3 values per track (fTracks)

Packed floats (`Float16_t`, `Double32_t`) and floats inside objects are compared to 1e-6 relative: we use ROOT's formulas (`aint / factor + min`), uproot an approximation.
