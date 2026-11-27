"""rhizoid against uproot and ROOT, on the files `cargo bench --bench io`
generates and the operations it measures: a Markdown table on stdout.

    cargo bench -p rhizoid-io --bench io                     # rhizoid, and the data
    python crates/rhizoid-io/benches/compare.py [--root root] [--data DIR] [--criterion DIR]

rhizoid's times are criterion's mean estimates; uproot's and ROOT's are the
best of five runs, each a cold open and a full read into memory, as the
rhizoid benchmarks do. Same machine, same files: only ratios mean anything.
"""

import argparse
import json
import os
import subprocess
import tempfile
import time

OPS = [
    ("open/tree_zlib", "open/tree_zlib"),
    ("open/rntuple", "open/rntuple"),
    ("read/f32/tree_zlib", "read/f32/tree_zlib"),
    ("read/f32/tree_lz4", "read/f32/tree_lz4"),
    ("read/jagged_f32/tree_zlib", "read/jagged_f32/tree_zlib"),
    ("read/jagged_f32/tree_lz4", "read/jagged_f32/tree_lz4"),
    ("read/f32/rntuple", "read/f32/rntuple"),
    ("read/jagged_f32/rntuple", "read/jagged_f32/rntuple"),
    ("write/tree/zlib", "write/tree/zlib"),
    ("write/tree/lz4", "write/tree/lz4"),
]


def best(f, repeat=5):
    times = []
    for _ in range(repeat):
        t = time.perf_counter()
        f()
        times.append(time.perf_counter() - t)
    return min(times)


def uproot_times(data):
    import awkward as ak
    import numpy as np
    import uproot

    out = {}
    for name in ["tree_zlib", "tree_lz4", "rntuple"]:
        path = os.path.join(data, name + ".root")
        out[f"open/{name}"] = best(lambda: uproot.open(path).keys())
        if name == "rntuple":
            continue
        out[f"read/f32/{name}"] = best(lambda: uproot.open(path)["events"]["x"].array(library="np"))
        out[f"read/jagged_f32/{name}"] = best(lambda: uproot.open(path)["events"]["pt"].array(library="ak"))
    nt = os.path.join(data, "rntuple.root")
    out["read/f32/rntuple"] = best(lambda: uproot.open(nt)["events"]["x"].array(library="np"))
    out["read/jagged_f32/rntuple"] = best(lambda: uproot.open(nt)["events"]["pt"].array(library="ak"))

    n = 100_000
    ids = np.arange(n, dtype=np.int32)
    xs = (np.arange(n) * 0.5).astype(np.float32)
    counts = (np.arange(n) % 9).astype(np.int32)
    pt = ak.unflatten(np.concatenate([np.arange(c, dtype=np.float32) + 0.25 for c in counts]), counts)
    for name, comp in [("zlib", uproot.ZLIB(1)), ("lz4", uproot.LZ4(4))]:
        def write():
            with tempfile.TemporaryDirectory() as d:
                with uproot.recreate(os.path.join(d, "w.root"), compression=comp) as f:
                    f["events"] = {"id": ids, "x": xs, "pt": pt}
        out[f"write/tree/{name}"] = best(write)
    return out


def root_times(root, data):
    macro = os.path.join(os.path.dirname(os.path.abspath(__file__)), "compare_root.C")
    res = subprocess.run([root, "-b", "-q", "-l", f'{macro}("{data}", 5)'],
                         capture_output=True, text=True, check=False)
    out = {}
    for line in res.stdout.splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[0] == "BENCH":
            out[parts[1]] = float(parts[2])
    return out


def rhizoid_times(criterion):
    out = {}
    for op, path in OPS:
        est = os.path.join(criterion, *path.split("/"), "new", "estimates.json")
        if os.path.exists(est):
            out[op] = json.load(open(est))["mean"]["point_estimate"] / 1e9
    return out


def fmt(t):
    return "—" if t is None else (f"{t * 1e3:.1f} ms" if t >= 1e-3 else f"{t * 1e6:.0f} µs")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--data", default=os.environ.get("RHIZOID_BENCH_DATA",
                                                   os.path.join(tempfile.gettempdir(), "rhizoid-bench")))
    p.add_argument("--criterion", default=os.path.join(os.environ.get("CARGO_TARGET_DIR", "target"), "criterion"))
    p.add_argument("--root", default=None)
    a = p.parse_args()
    ours = rhizoid_times(a.criterion)
    up = uproot_times(a.data)
    rt = root_times(a.root, a.data) if a.root else {}
    print("| operation (1M entries; writes 100k) | rhizoid | uproot | ROOT | uproot / rhizoid | ROOT / rhizoid |")
    print("|---|---|---|---|---|---|")
    for op, _ in OPS:
        r, u, o = ours.get(op), up.get(op), rt.get(op)
        ratio = lambda x: f"{x / r:.1f}×" if (x and r) else "—"  # noqa: E731
        print(f"| {op} | {fmt(r)} | {fmt(u)} | {fmt(o)} | {ratio(u)} | {ratio(o)} |")


if __name__ == "__main__":
    main()
