"""Fails when a benchmark got slower than its previous run by more than a
threshold: criterion compares each run with the one before it (kept in
`target/criterion`), and writes the change it measured.

    cargo bench -p rhizoid-io --bench io
    python crates/rhizoid-io/benches/regressions.py [--criterion DIR] [--threshold 0.25]

Shared CI runners are noisy: the default threshold (25 %) only catches real
regressions; the table shows every change.
"""

import argparse
import json
import os
import sys


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--criterion", default=os.path.join(os.environ.get("CARGO_TARGET_DIR", "target"), "criterion"))
    p.add_argument("--threshold", type=float, default=0.25)
    a = p.parse_args()
    rows, slower = [], []
    for root, _, files in os.walk(a.criterion):
        if os.path.basename(root) == "change" and "estimates.json" in files:
            bench = os.path.relpath(os.path.dirname(root), a.criterion).replace(os.sep, "/")
            change = json.load(open(os.path.join(root, "estimates.json")))["mean"]["point_estimate"]
            rows.append((bench, change))
            if change > a.threshold:
                slower.append(bench)
    if not rows:
        print("no previous run to compare with")
        return
    print("| benchmark | change of the mean |\n|---|---|")
    for bench, change in sorted(rows):
        flag = " ⚠" if bench in slower else ""
        print(f"| {bench} | {change:+.1%}{flag} |")
    if slower:
        print(f"\n{len(slower)} benchmark(s) slower by more than {a.threshold:.0%}: {', '.join(slower)}")
        sys.exit(1)


if __name__ == "__main__":
    main()
