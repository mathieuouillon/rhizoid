#!/usr/bin/env bash
# The full check on Linux, with the tools of tools/wsl-setup.sh: format,
# clippy (native and WASM), tests with the uproot oracle and ROOT, the corpus
# tests (CI's `corpus` job; the corpus is RHIZOID_CORPUS, else ./corpus), the
# WASM tests under Node, docs, the MSRV, and a ROOT smoke test (ROOT opens the
# fixtures and a file we write).
#
# The corpus: python tests/cross/fetch_corpus.py corpus  (~44 MB)
#
# Run from the repository root:
#   wsl -d Ubuntu-24.04 -u root -- bash tools/wsl-check.sh
# Builds go to ~/rhizoid-target (Linux disk: much faster than /mnt/c).

set -euo pipefail
# shellcheck disable=SC1091
source /opt/rhizoid-tools/env.sh
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/rhizoid-target}"
export RUSTFLAGS="-D warnings"

step() { printf '\n== %s\n' "$*"; }

step fmt
cargo fmt --all --check
step "clippy (all features, then defaults)"
cargo clippy -q --workspace --all-targets --all-features
cargo clippy -q --workspace --all-targets
step "clippy (wasm32)"
cargo clippy -q -p rhizoid-io --target wasm32-unknown-unknown --features async,http --lib --test wasm
step "tests (with the uproot oracle, ROOT, and the derives' compile-fail cases)"
RHIZOID_UI=1 cargo test -q --workspace --all-features
step "corpus (as CI's corpus job)"
corpus="${RHIZOID_CORPUS:-$PWD/corpus}"
if [ -d "$corpus" ]; then
    (
        export RHIZOID_CORPUS="$corpus"
        cargo test -q -p rhizoid-io --all-features --test corpus --test rntuple --test cases
        cargo test -q -p rhizoid-arrow --test arrow corpus
        cargo test -q -p rhizoid-io --features http --test sources corpus
        cargo test -q --release -p rhizoid-io --features async,http --test asynchronous
    )
    corpus_note=""
else
    corpus_note="SKIPPED: no corpus at $corpus (python tests/cross/fetch_corpus.py corpus)"
    echo "$corpus_note"
fi
step "WASM tests (Node)"
cargo test -p rhizoid-io --target wasm32-unknown-unknown --features async --test wasm  # no -q: the runner refuses --quiet
step "docs"
RUSTDOCFLAGS="-D warnings" cargo doc -q --workspace --no-deps --all-features
step "MSRV"
cargo +1.89 check -q --workspace --all-targets
cargo +1.89 check -q -p rhizoid-io --features async,http

step "ROOT smoke test"
out=$(mktemp -d)
cargo run -q -p rhizoid-cli -- new "$out/ours.root"
for f in crates/rhizoid-io/tests/data/*.root "$out/ours.root"; do
    root -b -q -l "tools/root_smoke.C(\"$f\")"
done
rm -rf "$out"
echo; echo "all checks passed"
if [ -n "$corpus_note" ]; then
    echo "but the corpus tests were $corpus_note"
fi
