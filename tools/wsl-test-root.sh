#!/usr/bin/env bash
# Runs the ROOT cross-check (crates/rhizoid-io/tests/root.rs) against the
# ROOT of tools/wsl-setup.sh. Run from the repository root:
#   wsl -d Ubuntu-24.04 -u root -- bash tools/wsl-test-root.sh
set -euo pipefail
# shellcheck disable=SC1091
source /opt/rhizoid-tools/env.sh
export RHIZOID_ROOT=root
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/rhizoid-target}"
cargo test -q -p rhizoid-io --test root -- --nocapture
