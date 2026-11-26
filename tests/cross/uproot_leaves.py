"""Print every branch of the given trees as flat scalar leaves, per entry.

For each branch, two tab-separated lines:
    TREE  BRANCH  counts  N0 N1 ...      (leaves per entry)
    TREE  BRANCH  leaves  L0 L1 ...      (all leaves, entries back to back)
or  TREE  BRANCH  error   MESSAGE
Leaves are Python literals; strings are JSON (ASCII-escaped). Objects are
flattened field by field, lists item by item; None is skipped.

Usage: python uproot_leaves.py FILE ENTRY_STOP TREE...
"""

import json
import sys

import uproot


def leaves(x):
    if x is None:
        return
    if isinstance(x, dict):
        for v in x.values():
            yield from leaves(v)
    elif isinstance(x, (list, tuple)):
        for v in x:
            yield from leaves(v)
    elif isinstance(x, str):
        yield json.dumps(x, ensure_ascii=True)
    elif isinstance(x, bytes):
        yield json.dumps(x.decode("utf-8", "replace"), ensure_ascii=True)
    else:
        yield repr(x)


path, stop = sys.argv[1], int(sys.argv[2])
with uproot.open(path) as f:
    for tname in sys.argv[3:]:
        tree = f[tname]
        for b in tree.itervalues(recursive=True):
            try:
                entries = b.array(library="ak", entry_stop=stop).to_list()
            except Exception as e:  # noqa: BLE001
                print(tname, b.name, "error", f"{type(e).__name__}: {e}".replace("\t", " ").replace("\n", " "), sep="\t")
                continue
            per = [list(leaves(e)) for e in entries]
            print(tname, b.name, "counts", " ".join(str(len(p)) for p in per), sep="\t")
            print(tname, b.name, "leaves", "\t".join(x for p in per for x in p), sep="\t")
