"""Print what uproot reads from every histogram and string in a file.

One fact per line: `KEY FIELD VALUE...`, floats as Python `repr` (exact).
Cells are flattened with x fastest, as ROOT stores them.

Usage: python uproot_dump.py FILE
"""

import sys

import numpy as np
import uproot


def floats(xs):
    return " ".join(repr(float(x)) for x in np.ravel(np.asarray(xs), order="F"))


with uproot.open(sys.argv[1]) as f:
    for key in f.keys(cycle=True):
        obj = f[key]
        print(key, "class", obj.classname)
        if obj.classname == "TObjString":
            print(key, "string", str(obj))
            continue
        if not obj.classname.startswith("TH"):
            continue
        print(key, "title", obj.member("fTitle"))
        print(key, "values", floats(obj.values(flow=True)))
        print(key, "sumw2", floats(obj.member("fSumw2")))
        for name in ("fEntries", "fTsumw", "fTsumw2", "fTsumwx", "fTsumwx2"):
            print(key, name, repr(float(obj.member(name))))
        for i, axis in enumerate(obj.axes):
            print(key, f"edges{i}", floats(axis.edges(flow=False)))
            print(key, f"axistitle{i}", axis.member("fTitle"))
