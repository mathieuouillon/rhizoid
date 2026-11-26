"""Survey the trees of ROOT files: one line per branch, as uproot sees it.

Columns: file, tree, branch (full name), class, fType, fStreamerType, fID,
fClassName, embedded baskets, uproot interpretation.

Usage: python uproot_survey.py FILE...
"""

import os
import sys

import uproot


def trees(directory, prefix=""):
    for key, cls in directory.classnames(recursive=False).items():
        name = key.split(";")[0]
        if cls in ("TTree", "TNtuple", "TNtupleD"):
            yield prefix + name
        elif cls in ("TDirectory", "TDirectoryFile"):
            yield from trees(directory[name], prefix + name + "/")


for path in sys.argv[1:]:
    with uproot.open(path) as f:
        for tname in sorted(set(trees(f))):
            t = f[tname]
            for b in t.itervalues(recursive=True):
                m = b.all_members
                try:
                    interp = repr(b.interpretation)
                except Exception as e:  # noqa: BLE001
                    interp = f"<{type(e).__name__}>"
                embedded = len(m.get("fBaskets", []) or [])
                print(
                    "\t".join(
                        str(x)
                        for x in (
                            os.path.basename(path),
                            tname,
                            b.name,
                            type(b).__name__.split("_v")[0].replace("Model_", ""),
                            m.get("fType", ""),
                            m.get("fStreamerType", ""),
                            m.get("fID", ""),
                            m.get("fClassName", ""),
                            embedded,
                            interp[:110],
                        )
                    )
                )
