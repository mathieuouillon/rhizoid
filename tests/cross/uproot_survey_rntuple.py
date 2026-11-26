"""Survey the RNTuples of ROOT files: one line per field, as uproot sees it.

Columns: file, ntuple, field id, parent id, role, name, type, repetition,
flags, columns (type/bits/repr).

Usage: python uproot_survey_rntuple.py FILE...
"""

import os
import sys

import uproot

for path in sys.argv[1:]:
    with uproot.open(path) as f:
        names = [k.split(";")[0] for k, c in f.classnames().items() if c == "ROOT::RNTuple"]
        for name in sorted(set(names)):
            nt = f[name]
            cols = {}
            for i, cr in enumerate(nt.column_records):
                cols.setdefault(cr.field_id, []).append(f"{cr.type:#04x}/{cr.nbits}/r{cr.repr_idx}")
            for fid, fr in enumerate(nt.field_records):
                print(
                    "\t".join(
                        str(x)
                        for x in (
                            os.path.basename(path),
                            name,
                            fid,
                            fr.parent_field_id,
                            int(fr.struct_role),
                            fr.field_name,
                            fr.type_name,
                            fr.repetition,
                            int(fr.flags),
                            " ".join(cols.get(fid, [])),
                        )
                    )
                )
