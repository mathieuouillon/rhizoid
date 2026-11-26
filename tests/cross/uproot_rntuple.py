"""Writes an RNTuple with uproot: scalars, booleans, jagged and string
fields over several clusters. Values are functions of the entry number.

Usage: python uproot_rntuple.py OUT.root
"""

import sys

import awkward as ak
import numpy as np
import uproot


def chunk(start, stop):
    i = np.arange(start, stop)
    counts = i % 3
    return {
        "i32": (i * 7 - 100).astype(np.int32),
        "u16": (i % 60000).astype(np.uint16),
        "x": i * 0.5,
        "flag": i % 2 == 0,
        "jet_pt": ak.unflatten((np.arange(int(counts.sum())) * 0.25).astype(np.float32), counts),
        "ids": ak.unflatten(np.arange(int(counts.sum()), dtype=np.int64), counts),
        "label": ak.Array([f"entry{k}" for k in i]),
    }


with uproot.recreate(sys.argv[1]) as f:
    f["events"] = chunk(0, 300)
    f["events"].extend(chunk(300, 500))
