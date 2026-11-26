"""Print every top-level field of the given RNTuples as flat scalar
leaves, per entry (the format of uproot_leaves.py).

Lines (tab-separated):
    NTUPLE  FIELD  counts  N0 N1 ...
    NTUPLE  FIELD  leaves  L0 L1 ...
or  NTUPLE  FIELD  error   MESSAGE

Usage: python uproot_leaves_rntuple.py FILE ENTRY_STOP NTUPLE...
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
    for name in sys.argv[3:]:
        nt = f[name]
        for field in nt.fields:
            try:
                entries = field.array(library="ak", entry_stop=stop).to_list()
            except Exception as e:  # noqa: BLE001
                msg = f"{type(e).__name__}: {e}".replace("\t", " ").replace("\n", " ")
                print(name, field.name, "error", msg, sep="\t")
                continue
            per = [list(leaves(e)) for e in entries]
            print(name, field.name, "counts", " ".join(str(len(p)) for p in per), sep="\t")
            print(name, field.name, "leaves", "\t".join(x for p in per for x in p), sep="\t")
