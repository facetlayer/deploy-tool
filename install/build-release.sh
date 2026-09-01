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

# The dashboard is compiled into deploy-server by rust-embed, so it has to be
# built first. Skipped when only the CLI is being built, and skipped with a
# warning rather than an error when node is absent: a server without the
# dashboard bundle still serves the whole deploy API, which is the part that
# matters. `cargo` will not rebuild the binary on its own if only `dist`
# changed, so touch the source that carries the embed.
if printf '%s\n' "${BINARIES[@]}" | grep -qx deploy-server; then
    if command -v pnpm >/dev/null 2>&1 || command -v npm >/dev/null 2>&1; then
        PKG=$(command -v pnpm >/dev/null 2>&1 && echo pnpm || echo npm)
        echo "Building the dashboard with $PKG..."
        (cd crates/deploy-server/dashboard && "$PKG" install && "$PKG" run build)
        touch crates/deploy-server/src/dashboard/mod.rs
    else
        echo "WARNING: neither pnpm nor npm found; building without the dashboard bundle." >&2
        echo "         The deploy API is unaffected; the dashboard will serve 404s." >&2
    fi
fi

cargo zigbuild --release --target "${TARGET}.${GLIBC_VERSION}" "${BIN_ARGS[@]}"

OUT_DIR="target/${TARGET}/release"
echo
for binary in "${BINARIES[@]}"; do
    echo "  ${OUT_DIR}/${binary}"
done
echo
echo "To upgrade a host that is already set up, prefer:"
echo "  install/deploy-to-hosts.sh both        # backs up, verifies, detached restart"
echo
echo "First install on a new host:"
echo "  scp ${OUT_DIR}/deploy-server root@<host>:/root/bin/deploy-server"
echo "  ssh root@<host> 'chmod +x /root/bin/deploy-server'"
