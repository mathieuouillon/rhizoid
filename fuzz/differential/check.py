"""The differential check: ROOT and uproot read the files the differential
example wrote from the `write_rows` corpus, and must find exactly the rows
rhizoid wrote (`OUT/i.expected`).

    cargo run --release --manifest-path fuzz/Cargo.toml --example differential -- CORPUS OUT
    python fuzz/differential/check.py OUT [--root root] [--no-uproot]

Prints one line per disagreement and exits 1 if there is any. A file
holding a string with a NUL byte is compared up to it for ROOT's trees: a
`TLeafC` holds a C string (a known limit of the format, not a disagreement).
"""

import argparse
import os
import subprocess
import sys

import numpy as np


def hexs(s):
    if isinstance(s, str):
        s = s.encode("utf-8", "surrogateescape")
    return bytes(s).hex()


def uproot_rows(path, kind):
    import awkward as ak
    import uproot

    obj = uproot.open(path)["t"]
    names = ["flag", "small", "id", "big", "x", "y", "xyz", "hits", "label"]
    arrays = obj.arrays(names, library="ak")
    lines = []
    for i in range(len(arrays)):
        r = arrays[i]
        hits = np.asarray(ak.to_numpy(r["hits"]), dtype=np.float32)
        xyz = [int(v) for v in ak.to_list(r["xyz"])]
        lines += [
            f"{i} flag {int(bool(r['flag']))}",
            f"{i} small {int(r['small'])}",
            f"{i} id {int(r['id'])}",
            f"{i} big {int(r['big'])}",
            f"{i} x {int(np.float64(r['x']).view(np.uint64)):016x}",
            f"{i} y {int(np.float32(r['y']).view(np.uint32)):08x}",
            f"{i} xyz {xyz[0]} {xyz[1]} {xyz[2]}",
            f"{i} hits {len(hits)} " + " ".join(f"{int(v):08x}" for v in hits.view(np.uint32)),
            f"{i} label {hexs(r['label'])}",
        ]
    return lines


def normal(lines):
    return [line.rstrip() for line in lines if line.strip()]


def strip_after_nul(lines):
    """ROOT's view of a TLeafC string: up to its first NUL byte."""
    out = []
    for line in lines:
        head, sep, value = line.partition(" label ")
        if sep:
            raw = bytes.fromhex(value)
            value = raw.split(b"\0")[0].hex()
            line = f"{head} label {value}"
        out.append(line)
    return out


def first_difference(want, got):
    for k, (a, b) in enumerate(zip(want, got)):
        if a != b:
            return f"line {k + 1}: want `{a[:120]}`, got `{b[:120]}`"
    return f"{len(want)} lines wanted, {len(got)} read"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("out")
    p.add_argument("--root", default=None, help="the ROOT executable (skip ROOT if absent)")
    p.add_argument("--no-uproot", action="store_true")
    a = p.parse_args()
    stems = sorted(f[:-5] for f in os.listdir(a.out) if f.endswith(".root"))
    kinds = {s: open(os.path.join(a.out, s + ".kind")).read().strip() for s in stems}
    failures = 0

    if a.root:
        listing = os.path.join(a.out, "list.txt")
        with open(listing, "w") as f:
            for s in stems:
                f.write(f"{os.path.join(a.out, s)} {kinds[s]}\n")
        macro = os.path.join(os.path.dirname(os.path.abspath(__file__)), "read_root.C")
        subprocess.run([a.root, "-b", "-q", "-l", f'{macro}("{listing}")'], check=False,
                       stdout=subprocess.DEVNULL)

    for s in stems:
        want = normal(open(os.path.join(a.out, s + ".expected")).read().splitlines())
        readers = []
        if not a.no_uproot:
            try:
                readers.append(("uproot", normal(uproot_rows(os.path.join(a.out, s + ".root"), kinds[s])), want))
            except Exception as e:  # noqa: BLE001 - any failure is a disagreement
                readers.append(("uproot", [f"ERROR {type(e).__name__}: {e}"], want))
        if a.root:
            path = os.path.join(a.out, s + ".root.txt")
            got = normal(open(path).read().splitlines()) if os.path.exists(path) else ["ERROR no output"]
            expected = normal(strip_after_nul(want)) if kinds[s] == "tree" else want
            readers.append(("ROOT", got, expected))
        for name, got, expected in readers:
            if got != expected:
                failures += 1
                print(f"{s} ({kinds[s]}) {name}: {first_difference(expected, got)}")
    print(f"{len(stems)} files, {failures} disagreements")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
