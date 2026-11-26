"""Write reference files with uproot, for rhizoid's cross-check.

Usage: python uproot_make.py OUTPUT_DIR

Writes one file per compression algorithm, each with histograms, nested
directories, a TTree and a string, and prints a JSON manifest of what each
file contains.
"""

import json
import sys

import numpy as np
import uproot

out = sys.argv[1]
rng = np.random.default_rng(42)
manifest = {}

codecs = {
    "zlib": uproot.ZLIB(1),
    "lz4": uproot.LZ4(4),
    "zstd": uproot.ZSTD(5),
    "lzma": uproot.LZMA(7),
    "none": None,
}

for name, codec in codecs.items():
    path = f"{out}/uproot_{name}.root"
    with uproot.recreate(path, compression=codec) as f:
        f["hist"] = np.histogram(rng.normal(size=100_000), bins=200)
        f["dir/sub/h2"] = np.histogram2d(rng.normal(size=20_000), rng.normal(size=20_000), bins=(40, 40))
        f["dir/note"] = "a TObjString written by uproot"
        f["events"] = {
            "px": rng.normal(size=50_000),
            "n": np.arange(50_000, dtype=np.int32),
        }
    with uproot.open(path) as f:
        manifest[path] = sorted(f.keys(recursive=True))

print(json.dumps(manifest, indent=1))
