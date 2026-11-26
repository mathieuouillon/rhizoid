"""Captures the class descriptions (the StreamerInfo record, decompressed)
of a ROOT-written file, for rhizoid to embed as its built-in descriptions.

Prints the record's key length: the origin class tags count from.

Usage: python capture_streamers.py FILE.root OUT.bin
"""

import sys

import uproot

path, out = sys.argv[1], sys.argv[2]
f = uproot.open(path).file
seek_info, nbytes_info = f.fSeekInfo, f.fNbytesInfo
key = uproot.reading.ReadOnlyKey(
    f.chunk(seek_info, seek_info + nbytes_info),
    uproot.source.cursor.Cursor(seek_info),
    {},
    f,
    f,
    read_strings=True,
)
assert key.fClassName == "TList" and key.fName == "StreamerInfo", key
chunk, cursor = key.get_uncompressed_chunk_cursor()
body = chunk.raw_data.tobytes()
assert len(body) == key.fObjlen, (len(body), key.fObjlen)
with open(out, "wb") as o:
    o.write(body)
print(f"wrote {len(body)} bytes; origin (key length) {key.fKeylen}")
