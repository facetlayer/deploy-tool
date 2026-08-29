#!/usr/bin/env bash
# Cross-compiles the release binaries for the droplets.
#
# The droplets run Ubuntu 24.04 (glibc 2.39) and have NO Rust toolchain, so
# everything is built on a laptop and shipped as an ELF. The glibc version is
# pinned in the target triple: an unpinned build links against the local glibc
# and will not start on the server.
#
# Requires: rustup target add x86_64-unknown-linux-gnu
#           cargo install --locked cargo-zigbuild
#           brew install zig            (cargo-zigbuild uses zig as the linker)
#
# Usage:
#   install/build-release.sh                 # both binaries
#   install/build-release.sh deploy-server   # just the server
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$REPO_ROOT"

TARGET=x86_64-unknown-linux-gnu
GLIBC_VERSION=2.39

if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    echo "cargo-zigbuild is not installed: cargo install --locked cargo-zigbuild" >&2
    exit 1
fi

if [ "$#" -gt 0 ]; then
    BINARIES=("$@")
else
    BINARIES=(deploy-server deploy)
fi

BIN_ARGS=()
for binary in "${BINARIES[@]}"; do
    BIN_ARGS+=(--bin "$binary")
done

cargo zigbuild --release --target "${TARGET}.${GLIBC_VERSION}" "${BIN_ARGS[@]}"

OUT_DIR="target/${TARGET}/release"
echo
for binary in "${BINARIES[@]}"; do
    echo "  ${OUT_DIR}/${binary}"
done
echo
echo "Install the server with:"
echo "  scp ${OUT_DIR}/deploy-server root@<host>:/root/bin/deploy-server"
echo "  ssh root@<host> 'chmod +x /root/bin/deploy-server && systemctl restart deploy-server'"
