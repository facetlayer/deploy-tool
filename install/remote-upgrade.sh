#!/usr/bin/env bash
# Runs ON a deploy host, fed over ssh by install/deploy-to-hosts.sh:
#
#   ssh root@host 'bash -s' < install/remote-upgrade.sh
#
# Expects the new binary already uploaded to /root/bin/deploy-server.new.
# Swaps it in and schedules a detached restart. Idempotent.

set -euo pipefail

NEW=/root/bin/deploy-server.new
LIVE=/root/bin/deploy-server
BACKUPS=/root/backups/deploy-server

test -x "$NEW" || { echo "no uploaded binary at $NEW" >&2; exit 1; }

# The unit is named `deploy` on both current hosts — inherited from the old
# tool, which predates this repo. install/deploy-server.service and
# docs/server-setup.md say `deploy-server`, which is what a *fresh* install
# would be called. Rather than pick one and be wrong on half the fleet, ask
# systemd which one is actually there.
UNIT=""
for candidate in deploy deploy-server; do
    if systemctl list-unit-files --no-pager --no-legend "$candidate.service" 2>/dev/null | grep -q .; then
        UNIT="$candidate"
        break
    fi
done
test -n "$UNIT" || { echo "found neither deploy.service nor deploy-server.service" >&2; exit 1; }
echo ">>> unit: $UNIT"

# Refuse to install something that cannot run: if the ELF is truncated, or built
# against a newer glibc than this host has, this is where it fails — while the
# working binary is still in place.
echo ">>> uploaded binary reports: $("$NEW" --version)"

ts=$(date +%Y%m%d-%H%M%S)
mkdir -p "$BACKUPS"
if [ -e "$LIVE" ]; then
    cp "$LIVE" "$BACKUPS/deploy-server.$ts"
    echo ">>> previous binary saved to $BACKUPS/deploy-server.$ts"
fi

# mv within /root/bin is atomic, so there is no instant at which the path is
# missing or half-written.
mv "$NEW" "$LIVE"

# Hand the restart to pid 1 and return immediately, so the ssh connection
# closing cannot interrupt it and the command does not own the restart's
# lifetime.
systemd-run --on-active=2 --unit="deploy-restart-$ts" systemctl restart "$UNIT"
echo ">>> restart of $UNIT scheduled as deploy-restart-$ts"
