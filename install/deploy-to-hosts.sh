#!/usr/bin/env bash
# Ships deploy-server to the hosts that run it, from this machine, over ssh.
#
#   install/deploy-to-hosts.sh both
#   install/deploy-to-hosts.sh do2
#   install/deploy-to-hosts.sh dohl --skip-tests
#
# Why this is a script and not a GitHub Actions job:
#
#   1. deploy-server cannot deploy itself through the deploy service.
#      Activating a deployment of deploy-server means replacing and restarting
#      the process serving that very deployment; the connection drops before it
#      can report success.
#   2. Doing it from CI would mean putting a private key with root on both
#      droplets into GitHub secrets — a far larger blast radius than the thing
#      it automates, which is one binary that changes rarely. The ssh
#      credential stays on this laptop, where it already is.
#
# Every other service deploys through GitHub Actions with a scoped auth-center
# key and no ssh access at all. This is the deliberate exception.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

TARGET=x86_64-unknown-linux-gnu
BIN=target/$TARGET/release/deploy-server

WHICH=${1:-}
shift || true
SKIP_TESTS=false
for arg in "$@"; do
    case "$arg" in
        --skip-tests) SKIP_TESTS=true ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

case "$WHICH" in
    both) HOSTS="do2 dohl" ;;
    do2)  HOSTS="do2" ;;
    dohl) HOSTS="dohl" ;;
    *)    echo "usage: $0 {both|do2|dohl} [--skip-tests]" >&2; exit 2 ;;
esac

if [ "$SKIP_TESTS" = false ]; then
    echo ">>> cargo test --workspace"
    cargo test --workspace
fi

# Cross-compiles with the glibc version pinned; see the script's own header.
echo ">>> building"
install/build-release.sh deploy-server
file "$BIN"

for host in $HOSTS; do
    echo
    echo "=================== $host ==================="

    # Upload beside the live binary, never over it: a partial transfer onto the
    # running path would leave the host with no working server at all.
    echo ">>> uploading"
    scp "$BIN" "$host:/root/bin/deploy-server.new"

    echo ">>> installing"
    ssh "$host" 'bash -s' < install/remote-upgrade.sh
done

echo
echo ">>> waiting for the restarts to land"
sleep 8

failed=0
for host in $HOSTS; do
    echo
    echo "=================== $host: verify ==================="
    if ssh "$host" 'set -e
        unit=deploy; systemctl list-unit-files --no-legend deploy.service | grep -q . || unit=deploy-server
        systemctl is-active "$unit"
        /root/bin/deploy-server --version
        journalctl -u "$unit" --since "2 min ago" --no-pager | tail -15'; then
        echo ">>> $host OK"
    else
        echo "!!! $host FAILED verification" >&2
        failed=1
    fi
done

echo
if [ "$failed" -ne 0 ]; then
    echo ">>> one or more hosts failed. The previous binary is in"
    echo "    /root/backups/deploy-server/ on that host."
    exit 1
fi
echo ">>> done"
