#!/usr/bin/env bash
# Regenerates the ROOT-written histogram fixture and what ROOT reads from
# it (tests/data/root_hists.root and root_hists.txt), with the ROOT of
# tools/wsl-setup.sh. Run from the repository root:
#   wsl -d Ubuntu-24.04 -u root -- bash tools/root-fixture.sh

set -euo pipefail
# shellcheck disable=SC1091
source /opt/rhizoid-tools/env.sh

data=crates/rhizoid-io/tests/data
root -b -q -l "tests/cross/root_make.C(\"$data/root_hists.root\")"
root -b -q -l "tests/cross/root_dump.C(\"$data/root_hists.root\")" 2>&1 \
    | grep -v '^Processing' | grep -v '^$' > "$data/root_hists.txt"
if grep -E 'Error in <|Warning in <' "$data/root_hists.txt"; then
    echo "ROOT complained while reading its own file" >&2
    exit 1
fi
echo "wrote $data/root_hists.root and root_hists.txt (ROOT $(root-config --version))"
