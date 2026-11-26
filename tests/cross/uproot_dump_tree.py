"""Print every top-level branch of a tree as uproot reads it.

Lines: `BRANCH counts N...` for jagged branches, then
`BRANCH values V...` with the flattened values as Python literals.

Usage: python uproot_dump_tree.py FILE TREE [leaves]

With `leaves`, every branch that holds data (sub-branches of split
objects included) instead of the top-level ones; a branch uproot cannot
read prints `BRANCH error TYPE`.
"""

import sys

import awkward as ak
import uproot

leaves = len(sys.argv) > 3 and sys.argv[3] == "leaves"

with uproot.open(sys.argv[1]) as f:
    tree = f[sys.argv[2]]
    if leaves:
        pick = lambda b: len(b.branches) == 0 or b.member("fType", none_if_missing=True) == 4
    else:
        pick = lambda b: b.top_level
    for name, branch in tree.items(filter_branch=pick):
        if leaves:
            try:
                arr = branch.array(library="ak")
            except Exception as e:  # noqa: BLE001 - reported, compared
                print(name, "error", type(e).__name__)
                continue
        else:
            arr = branch.array(library="ak")
        if arr.ndim > 1 and "var" in str(arr.type):
            print(name, "counts", *ak.num(arr).tolist())
        flat = ak.flatten(arr, axis=None)
        print(name, "values", *[repr(x) for x in ak.to_list(flat)])
