"""Writes an uncompressed file with one object of every class M1 supports.

Its StreamerInfo record is the source of the class descriptions embedded in
rhizoid-io (`crates/rhizoid-io/src/streamer/builtin.bin`). Usage:

    python uproot_hists.py OUT.root
"""

import sys

import numpy as np
import uproot

out = sys.argv[1]
edges = np.linspace(0.0, 10.0, 11)
values = np.arange(12, dtype=np.float64)  # with under- and overflow
variances = values * 2.0

with uproot.recreate(out, compression=None) as f:
    for cls, dt in (("TH1D", np.float64), ("TH1F", np.float32), ("TH1I", np.int32)):
        f["h_" + cls[-1].lower()] = uproot.writing.identify.to_TH1x(
            fName="h_" + cls[-1].lower(),
            fTitle="a " + cls,
            data=values.astype(dt),
            fEntries=66.0,
            fTsumw=60.0,
            fTsumw2=120.0,
            fTsumwx=330.0,
            fTsumwx2=2100.0,
            fSumw2=variances,
            fXaxis=uproot.writing.identify.to_TAxis(
                fName="xaxis", fTitle="x", fNbins=10, fXmin=0.0, fXmax=10.0
            ),
        )
    values2 = np.arange(12 * 7, dtype=np.float64)
    for cls, dt in (("TH2D", np.float64), ("TH2F", np.float32)):
        f["h2_" + cls[-1].lower()] = uproot.writing.identify.to_TH2x(
            fName="h2_" + cls[-1].lower(),
            fTitle="a " + cls,
            data=values2.astype(dt),
            fEntries=10.0,
            fTsumw=10.0,
            fTsumw2=10.0,
            fTsumwx=20.0,
            fTsumwx2=50.0,
            fTsumwy=15.0,
            fTsumwy2=30.0,
            fTsumwxy=25.0,
            fSumw2=values2,
            fXaxis=uproot.writing.identify.to_TAxis(
                fName="xaxis", fTitle="", fNbins=10, fXmin=0.0, fXmax=10.0
            ),
            fYaxis=uproot.writing.identify.to_TAxis(
                fName="yaxis",
                fTitle="",
                fNbins=5,
                fXmin=-1.0,
                fXmax=1.0,
                fXbins=np.array([-1.0, -0.5, 0.0, 0.25, 0.5, 1.0]),
            ),
        )
    f["note"] = "hello rhizoid"
