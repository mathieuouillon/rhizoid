"""Writes a TTree with every leaf type uproot writes, over several baskets.

Values are deterministic functions of the entry number, so tests can
recompute them. Usage:

    python uproot_trees.py OUT.root
"""

import sys

import awkward as ak
import numpy as np
import uproot


def chunk(start, stop):
    i = np.arange(start, stop)
    counts = i % 4  # 0..3 elements per entry
    jag = ak.unflatten(np.arange(int(counts.sum()), dtype=np.float32) * 0.5, counts)
    jag_i = ak.unflatten(-np.arange(int(counts.sum()), dtype=np.int32), counts)
    return {
        "b": (i % 2 == 0),
        "i8": (i % 100 - 50).astype(np.int8),
        "u8": (i % 200).astype(np.uint8),
        "i16": (i * 3 - 1000).astype(np.int16),
        "u16": (i * 7).astype(np.uint16),
        "i32": (i * 1000 - 77).astype(np.int32),
        "u32": (i * 100000).astype(np.uint32),
        "i64": (i * 10**12 - 5).astype(np.int64),
        "u64": (i * 10**13).astype(np.uint64),
        "f32": (i * 0.25).astype(np.float32),
        "f64": i * 1e-3,
        "xyz": np.stack([i * 1.0, i * 2.0, i * 3.0], axis=1).astype(np.float32),
        "jet_pt": jag,
        "jet_q": jag_i,
    }


out = sys.argv[1]
with uproot.recreate(out) as f:
    first = chunk(0, 600)
    types = {k: (v.type.content if isinstance(v, ak.Array) else v.dtype) for k, v in first.items()}
    types["xyz"] = np.dtype((np.float32, (3,)))
    tree = f.mktree("events", types, title="test events")
    tree.extend(first)
    tree.extend(chunk(600, 1000))
    tree.extend(chunk(1000, 1001))
    f.mktree("empty", {"x": np.float64})
