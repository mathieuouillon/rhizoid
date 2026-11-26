"""Writes the ROOT-derived data-driven cases: `tests/cases/root_trees.toml`
and `tests/cases/root_rntuples.toml`.

For each file below, ROOT itself reads every branch (`root_sums.C`) or
RNTuple field (`root_ntuple_sums.C`), whole and over an entry range, and
each reading becomes a case: the number of values and their sum. A branch
whose two ROOT readings (entry by entry, and `TTree::Draw`) disagree is
left out, as are `fBits` (ROOT sets bits in memory): a case states one
answer. Corpus files (in `RHIZOID_CORPUS`) are sampled, a few branches
each; the suite skips their cases where the corpus is absent.

    source /opt/rhizoid-tools/env.sh       # ROOT on PATH
    python3 tests/cross/make_cases.py      # from the repository root
"""

import math
import os
import re
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DATA = os.path.join(REPO, "crates", "rhizoid-io", "tests", "data")
CORPUS = os.environ.get("RHIZOID_CORPUS", os.path.join(REPO, "corpus"))

# (file, entry ranges, at most this many branches per tree; 0 = all)
TREES = [
    ("root_tree.root", [None, (3, 9), (19, 25)], 0),
    ("root_user_tree.root", [None, (1, 4)], 0),
    ("uproot_trees.root", [None, (1, 4)], 0),
    ("uproot-HZZ.root", [None, (500, 1500)], 10),
    ("uproot-Zmumu.root", [None], 8),
    ("uproot-sample-5.23.02-zlib.root", [None, (10, 20)], 8),
    ("uproot-sample-6.08.04-lzma.root", [None], 8),
    ("uproot-sample-6.20.04-lz4.root", [None], 8),
    ("uproot-small-evnt-tree-fullsplit.root", [None, (25, 75)], 12),
    ("uproot-small-dy-withoffsets.root", [None], 8),
    ("uproot-double32-float16.root", [None], 8),
    ("uproot-mc10events.root", [None], 8),
]

# (file, RNTuple, entry ranges, at most this many fields; 0 = all)
RNTUPLES = [
    ("root_ntuple.root", "t", [None, (5, 12)], 0),
    ("uproot_rntuple.root", None, [None], 0),
    ("ntpl001_staff_rntuple_v1-0-0-0.root", "Staff", [None, (100, 2000)], 0),
    ("test_int_vfloat_tlv_vtlv_rntuple_v1-0-0-0.root", "ntuple", [None, (1, 3)], 0),
    ("test_index_multicluster_rntuple_v1-0-0-0.root", "ntuple", [None], 0),
    ("test_multiple_cluster_groups_rntuple_v1-0-0-0.root", "ntuple", [None], 0),
    ("Run2012BC_DoubleMuParked_Muons_1000evts_rntuple_v1-0-0-0.root", "Events", [None, (100, 300)], 0),
]


def find(name):
    for d in (DATA, CORPUS):
        p = os.path.join(d, name)
        if os.path.exists(p):
            return p
    return None


def root(macro, *args):
    quoted = ", ".join(f'"{a}"' if isinstance(a, str) else str(a) for a in args)
    out = subprocess.run(
        ["root", "-b", "-q", "-l", f"{os.path.join(REPO, 'tests', 'cross', macro)}({quoted})"],
        capture_output=True, text=True, check=False,
    )
    return out.stdout.splitlines()


def sample(items, cap):
    if cap == 0 or len(items) <= cap:
        return items
    step = len(items) / cap
    return [items[int(i * step)] for i in range(cap)]


def slug(*parts):
    return re.sub(r"[^a-z0-9]+", "-", "-".join(parts).lower()).strip("-")


def number(x):
    if math.isnan(x):
        return "nan"
    if x == int(x) and abs(x) < 2**53:
        return str(int(x))
    return repr(x)


def case(name, file, kind, obj, member, rng, count, total):
    key, mkey = ("tree", "branch") if kind == "tree" else ("rntuple", "field")
    lines = ["[[case]]", f'name = "{name}"', f'file = "{file}"', f'{key} = "{obj}"', f'{mkey} = "{member}"']
    if rng:
        lines.append(f"range = [{rng[0]}, {rng[1]}]")
    lines += [f"count = {count}", f"sum = {number(total)}", ""]
    return "\n".join(lines)


def trees():
    out, names = [], set()
    for file, ranges, cap in TREES:
        path = find(file)
        if not path:
            print(f"missing {file}", file=sys.stderr)
            continue
        stem = file.removesuffix(".root")
        for rng in ranges:
            args = (path, 0) if rng is None else (path, 0, rng[0], rng[1])
            readings, entries = {}, {}
            for line in root("root_sums.C", *args):
                w = line.split()
                if len(w) == 3 and w[0] == "TREE":
                    entries[w[1]] = int(w[2])
                if len(w) == 5 and w[0] in ("SUM", "DRAW"):
                    readings.setdefault((w[1], w[2]), {})[w[0]] = (int(w[3]), float(w[4]))
            agreed = []
            for (tree, branch), r in readings.items():
                if branch.endswith("fBits"):
                    continue
                vals = list(r.values())
                if len(vals) == 2 and not (
                    vals[0][0] == vals[1][0]
                    and (vals[0][1] == vals[1][1] or (math.isnan(vals[0][1]) and math.isnan(vals[1][1])))
                ):
                    continue
                agreed.append((tree, branch, r.get("SUM", r.get("DRAW"))))
            by_tree = {}
            for t, b, v in agreed:
                by_tree.setdefault(t, []).append((b, v))
            for tree, items in by_tree.items():
                # A range past the end is an error to readers, not a short read.
                if rng and rng[1] > entries.get(tree, 0):
                    continue
                for branch, (count, total) in sample(items, cap):
                    name = slug(stem, tree, branch, *([f"{rng[0]}-{rng[1]}"] if rng else []))
                    if name in names:
                        continue
                    names.add(name)
                    out.append(case(name, file, "tree", tree, branch, rng, count, total))
    return out


def rntuple_names(path):
    py =os.environ.get("RHIZOID_ORACLE_PYTHON", "python3")
    code = (
        "import sys, uproot\n"
        "f = uproot.open(sys.argv[1])\n"
        "print('\\n'.join(k.split(';')[0] for k, c in f.classnames().items() if c == 'ROOT::RNTuple'))\n"
    )
    out = subprocess.run([py, "-I", "-c", code, path], capture_output=True, text=True, check=False)
    return [n for n in out.stdout.split() if n]


def rntuples():
    out, names = [], set()
    for file, nt, ranges, cap in RNTUPLES:
        path = find(file)
        if not path:
            print(f"missing {file}", file=sys.stderr)
            continue
        stem = file.removesuffix(".root")
        for name in [nt] if nt else rntuple_names(path):
            for rng in ranges:
                args = (path, name) if rng is None else (path, name, rng[0], rng[1])
                fields, entries = [], 0
                for line in root("root_ntuple_sums.C", *args):
                    w = line.split()
                    if len(w) == 3 and w[0] == "NTUPLE":
                        entries = int(w[2])
                    if len(w) == 5 and w[0] == "FIELD":
                        fields.append((w[2], (int(float(w[3])), float(w[4]))))
                if rng and rng[1] > entries:
                    continue
                for field, (count, total) in sample(fields, cap):
                    cname = slug(stem, name, field, *([f"{rng[0]}-{rng[1]}"] if rng else []))
                    if cname in names:
                        continue
                    names.add(cname)
                    out.append(case(cname, file, "rntuple", name, field, rng, count, total))
    return out


HEADER = """# Generated by tests/cross/make_cases.py from ROOT's own reading ({what});
# do not edit by hand. Each case: the number of values and their sum, arrays
# and collections flattened, as ROOT 6.42 reads them. Corpus files' cases
# are skipped where RHIZOID_CORPUS is absent.

"""


def main():
    cases = os.path.join(REPO, "tests", "cases")
    t = trees()
    with open(os.path.join(cases, "root_trees.toml"), "w", encoding="utf-8", newline="\n") as f:
        f.write(HEADER.format(what="root_sums.C, entry by entry and TTree::Draw agreeing") + "\n".join(t))
    r = rntuples()
    with open(os.path.join(cases, "root_rntuples.toml"), "w", encoding="utf-8", newline="\n") as f:
        f.write(HEADER.format(what="root_ntuple_sums.C, through RDataFrame") + "\n".join(r))
    print(f"{len(t)} tree cases, {len(r)} RNTuple cases")


if __name__ == "__main__":
    main()
