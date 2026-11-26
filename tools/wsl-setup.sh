#!/usr/bin/env bash
# Sets up a Linux (WSL Ubuntu 24.04) machine for rhizoid development:
# compilers, Rust (stable, the MSRV, the WASM target), Node and
# wasm-bindgen-cli for the WASM tests, Python with the uproot oracle, and
# ROOT itself for the cross-checks uproot cannot do.
#
# Run as root (WSL: `wsl -d Ubuntu-24.04 -u root -- bash tools/wsl-setup.sh`).
# Idempotent: steps already done are skipped. Everything but the apt
# packages goes under /opt/rhizoid-tools; `source /opt/rhizoid-tools/env.sh`
# puts it on the PATH.

set -euo pipefail

ROOT_VERSION=6.42.00
NODE_VERSION=v26.13.1
WASM_BINDGEN_VERSION=0.2.130   # must match wasm-bindgen in Cargo.lock
MSRV=1.89
TOOLS=/opt/rhizoid-tools

mkdir -p "$TOOLS"
export DEBIAN_FRONTEND=noninteractive

step() { printf '\n== %s\n' "$*"; }

step "System packages (compilers; ROOT's runtime dependencies)"
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    build-essential binutils cmake dpkg-dev g++ gcc git curl ca-certificates xz-utils \
    pkg-config libssl-dev python3 python3-venv python3-pip python3-dev \
    libx11-dev libxext-dev libxft-dev libxpm-dev libtbb-dev libvdt-dev libgif-dev \
    libjpeg-dev libpng-dev libgsl-dev nlohmann-json3-dev libxml2-dev libfftw3-dev \
    libopenblas-dev libz-dev liblzma-dev libzstd-dev liblz4-dev libxxhash-dev \
    > /dev/null

step "Rust: stable and $MSRV, with clippy, rustfmt and wasm32-unknown-unknown"
export RUSTUP_HOME="$TOOLS/rustup" CARGO_HOME="$TOOLS/cargo"
if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain stable -c clippy,rustfmt
fi
export PATH="$CARGO_HOME/bin:$PATH"
rustup toolchain install stable "$MSRV" --profile minimal -c clippy,rustfmt
rustup target add wasm32-unknown-unknown

step "Node $NODE_VERSION (for the WASM tests)"
if [ ! -x "$TOOLS/node/bin/node" ]; then
    cd /tmp
    f="node-$NODE_VERSION-linux-x64.tar.xz"
    curl -sSfLO "https://nodejs.org/dist/$NODE_VERSION/$f"
    curl -sSfL "https://nodejs.org/dist/$NODE_VERSION/SHASUMS256.txt" | grep " $f\$" | sha256sum -c -
    rm -rf "$TOOLS/node" && mkdir -p "$TOOLS/node"
    tar -xJf "$f" -C "$TOOLS/node" --strip-components=1
    rm -f "$f"
fi

step "wasm-bindgen-cli $WASM_BINDGEN_VERSION"
if ! "$CARGO_HOME/bin/wasm-bindgen-test-runner" --version 2>/dev/null | grep -q "$WASM_BINDGEN_VERSION"; then
    cargo install -q wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked
fi

step "Python with the uproot oracle"
if [ ! -x "$TOOLS/venv/bin/python" ]; then
    python3 -m venv "$TOOLS/venv"
fi
"$TOOLS/venv/bin/pip" install -q --upgrade pip
"$TOOLS/venv/bin/pip" install -q uproot==5.7.8 numpy awkward==2.15.1 lz4 zstandard xxhash scikit-hep-testdata

step "ROOT $ROOT_VERSION"
if [ ! -x "$TOOLS/root/bin/root" ]; then
    cd /tmp
    f="root_v$ROOT_VERSION.Linux-ubuntu24.04-x86_64-gcc13.3.tar.gz"
    curl -sSfLO "https://root.cern/download/$f"
    rm -rf "$TOOLS/root"
    tar -xzf "$f" -C "$TOOLS"
    rm -f "$f"
fi

cat > "$TOOLS/env.sh" <<EOF
# rhizoid development tools (written by tools/wsl-setup.sh)
export RUSTUP_HOME=$TOOLS/rustup
export CARGO_HOME=$TOOLS/cargo
export PATH=$TOOLS/cargo/bin:$TOOLS/node/bin:\$PATH
export RHIZOID_ORACLE_PYTHON=$TOOLS/venv/bin/python
export RHIZOID_ROOT=root
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
source $TOOLS/root/bin/thisroot.sh
EOF

step "Versions"
# shellcheck disable=SC1091
source "$TOOLS/env.sh"
rustc --version
rustc "+$MSRV" --version
node --version
wasm-bindgen-test-runner --version
"$RHIZOID_ORACLE_PYTHON" -c 'import uproot; print("uproot", uproot.__version__)'
root-config --version
root -b -q -e 'printf("ROOT works: %s\n", gROOT->GetVersion());'
