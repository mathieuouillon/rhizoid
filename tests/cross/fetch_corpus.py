"""Fetch the corpus: ROOT-written test files from scikit-hep-testdata.

TTree files (M2) and RNTuple files (M4), about 44 MB, capped at 50 MB.
Usage: python fetch_corpus.py DIR
Then: RHIZOID_CORPUS=DIR RHIZOID_ORACLE_PYTHON=python cargo test --test corpus
(and --test rntuple).
"""

import os
import sys

import skhep_testdata

out = sys.argv[1]
cap = 50 * 1024 * 1024
names = [
    "uproot-HZZ.root",
    "uproot-Zmumu.root",
    "uproot-simple.root",
    "uproot-leaflist.root",
    "uproot-small-flat-tree.root",
    "uproot-sample-6.20.04-zlib.root",
    "uproot-sample-6.20.04-lz4.root",
    "uproot-sample-6.08.04-lzma.root",
    "uproot-sample-5.30.00-zlib.root",
    "uproot-sample-5.23.02-zlib.root",
    "uproot-small-evnt-tree-fullsplit.root",
    "uproot-small-evnt-tree-nosplit.root",
    "uproot-stl_containers.root",
    "uproot-vectorVectorDouble.root",
    "uproot-HZZ-objects.root",
    "uproot-small-dy-withoffsets.root",
    "uproot-small-dy-nooffsets.root",
    "uproot-double32-float16.root",
    "uproot-demo-double32.root",
    "uproot-foriter.root",
    "uproot-nesteddirs.root",
    "uproot-issue21.root",
    "uproot-mc10events.root",
    "uproot-Event.root",
    # RNTuple, written by ROOT (format 1.0.0.0 to 1.0.1.0)
    "Run2012BC_DoubleMuParked_Muons_1000evts_rntuple_v1-0-0-0.root",
    "cmsopendata2015_ttbar_19980_NANOAOD_RNTupleImporter_rntuple_v1-0-0-1.root",
    "ntpl001_staff_rntuple_v1-0-0-0.root",
    "ntpl001_staff_rntuple_v1-0-1-0.root",
    "rntviewer-testfile-multiple-rntuples-v1-0-0-0.root",
    "rntviewer-testfile-uncomp-single-rntuple-v1-0-0-0.root",
    "test_1jag_int_float_rntuple_v1-0-0-0.root",
    "test_atomic_bitset_rntuple_v1-0-0-0.root",
    "test_bit_rntuple_v1-0-0-0.root",
    "test_class_inheritance_rntuple_v1-0-0-1.root",
    "test_emptystruct_invalidvar_rntuple_v1-0-0-0.root",
    "test_extension_columns_rntuple_v1-0-0-0.root",
    "test_float_types_rntuple_v1-0-0-0.root",
    "test_index_multicluster_rntuple_v1-0-0-0.root",
    "test_int_5e4_rntuple_v1-0-0-0.root",
    "test_int_float_rntuple_v1-0-0-0.root",
    "test_int_multicluster_rntuple_v1-0-0-0.root",
    "test_int_vfloat_tlv_vtlv_rntuple_v1-0-0-0.root",
    "test_multiple_cluster_groups_rntuple_v1-0-0-0.root",
    "test_multiple_representations_rntuple_v1-0-0-0.root",
    "test_nested_structs_rntuple_v1-0-0-0.root",
    "test_split_3e4_rntuple_v1-0-0-0.root",
    "test_splitint_rntuple_v1-0-1-0.root",
    "test_stl_containers_rntuple_v1-0-0-0.root",
    "uproot-physlite-rntuple_v1-0-0-0.root",
]
os.makedirs(out, exist_ok=True)
total = 0
for name in names:
    if total >= cap:
        print("cap reached, skipping", name)
        continue
    try:
        path = skhep_testdata.data_path(name, cache_dir=out)
    except Exception as e:  # noqa: BLE001
        print("failed", name, e)
        continue
    size = os.path.getsize(path)
    total += size
    print(f"{size:>10}  {name}")
    if total > cap:
        os.remove(path)
        total -= size
        print("removed (over cap)", name)
print("total", total)
