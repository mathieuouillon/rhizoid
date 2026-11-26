"""Open a file written by rhizoid with uproot and print what uproot sees.

Usage: python uproot_read.py FILE
"""

import json
import sys

import uproot

with uproot.open(sys.argv[1]) as f:
    info = {
        "keys": f.keys(recursive=True),
        "fVersion": f.file.fVersion,
        "fBEGIN": f.file.fBEGIN,
        "fEND": f.file.fEND,
        "fSeekInfo": f.file.fSeekInfo,
        "fNbytesInfo": f.file.fNbytesInfo,
        "fSeekFree": f.file.fSeekFree,
        "fCompress": f.file.fCompress,
        "streamers": len(f.file.streamers),
        "uuid": str(f.file.uuid),
    }
print(json.dumps(info, indent=1))
