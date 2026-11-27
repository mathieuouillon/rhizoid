# rhizoid-cli

`rhizoid`, a command to inspect and check CERN ROOT files, built on
[`rhizoid-io`](https://crates.io/crates/rhizoid-io). Every command takes a
local path or an `http(s)://` URL.

```text
cargo install rhizoid-cli

rhizoid ls -l -r data.root        # keys, recursively, with sizes and dates
rhizoid ls data.root:dir/sub      # one directory
rhizoid check data.root           # walk and decompress every record
rhizoid header data.root          # the file header
rhizoid dump data.root:dir/h      # an object, member by member
rhizoid streamers data.root       # the class descriptions in a file
rhizoid tree data.root:Events     # branches, types, baskets, clusters
rhizoid rntuple data.root:Events  # RNTuple fields, types, clusters
rhizoid scan data.root:Events -b MET_pt,Muon_pt -n 5
rhizoid new empty.root            # a valid, empty file
```

It exits with status 1 and a one-line message on error. License: MIT.
