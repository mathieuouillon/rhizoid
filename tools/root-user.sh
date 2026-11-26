#!/usr/bin/env bash
# Runs tests/cross/user_classes.C (ROOT with the user classes' dictionary):
#   root-user.sh write FILE.root       ROOT writes three UserEvents
#   root-user.sh dump  FILE.root       what ROOT reads from a file of them
#   root-user.sh tree_write FILE.root  ROOT writes a split tree of them
#   root-user.sh tree_dump  FILE.root  what ROOT reads from such a tree
# Run from the repository root, with the ROOT of tools/wsl-setup.sh (or any
# ROOT on the PATH).
set -euo pipefail
if [ -f /opt/rhizoid-tools/env.sh ]; then
    # shellcheck disable=SC1091
    source /opt/rhizoid-tools/env.sh
fi
action=$1 file=$2
mkdir -p /tmp/rhizoid-aclic
root -b -q -l -e 'gSystem->SetBuildDir("/tmp/rhizoid-aclic", true);' \
    -e '.L tests/cross/user_classes.C+' \
    -e "user_${action}(\"${file}\");" 2>&1 | grep -v -E '^(Processing|Info in <TUnixSystem::ACLiC>)' || true
