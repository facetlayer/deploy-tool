# Server setup

A deploy-server instance owns a SQLite database, a deployments directory, and
one JSON-RPC listener.

## Build and install

For the Ubuntu 24.04 deployment hosts, build the Linux release locally:

```bash
install/build-release.sh
scp target/x86_64-unknown-linux-gnu/release/deploy-server \
  root@host:/root/bin/deploy-server
ssh root@host 'chmod +x /root/bin/deploy-server'
```

For later upgrades, use `install/deploy-to-hosts.sh`. It tests and builds,
uploads beside the live binary, validates the upload, backs up the installed
binary, swaps it atomically, schedules a restart, and verifies the unit.

## Storage

Create the payload directory, then record it in the database:

```bash
mkdir -p /root/deploys
/root/bin/deploy-server set-deployments-dir /root/deploys
```

The database is `<state-dir>/db.sqlite`. State-directory precedence is:

1. `DEPLOY_STATE_DIR`
2. `$XDG_STATE_HOME/deploy`
3. `$HOME/.local/state/deploy`

The directory and schema are created on open. The connection uses WAL mode and
a five-second busy timeout.

When pointed at a database from the previous service, startup preserves its
project and deployment state, creates the binding tables, and adds missing
deployment-attribution columns. Back up the database before the first start.

## Authorization configuration

Create `/root/secrets/deploy.env` as a root-owned `0600` file using
`install/deploy.env.template`. Set:

```text
DEPLOY_AUTH_URL=https://auth.example.com
DEPLOY_AUTH_KEY=<service key with auth:introspect>
DEPLOY_ADMIN_RESOURCE=host-deploy
```

All three values are required. The server exits if one is absent. Do not use
`--disable-api-key-check` outside local development.

## systemd

Install `install/deploy-server.service`, then verify and enable it:

```bash
scp install/deploy-server.service root@host:/etc/systemd/system/deploy-server.service
ssh root@host 'systemd-analyze verify /etc/systemd/system/deploy-server.service && \
  systemctl daemon-reload && \
  systemctl enable --now deploy-server && \
  systemctl is-active deploy-server'
```

The supplied unit listens on port 4715. The server currently binds
`0.0.0.0`, so restrict direct access with the host firewall or network policy
when it should only be reachable through a reverse proxy.

An older installation may use the unit name `deploy` on the same port. Stop
and disable it before enabling `deploy-server`; the upgrade script recognizes
either unit name.

## Register projects

Create the instance administration key with
`<DEPLOY_ADMIN_RESOURCE>:create-project`. For each application, generate its
scopes and register its binding:

```bash
deploy auth-scopes app.qc --resource app-staging
# then register app -> app-staging on this instance, with auth-setup
```

Distribute project keys to the clients or CI jobs that need them. Reads and
deploys remain denied until the project has a binding and the caller has the
matching action scope.

## Verify

```bash
ssh root@host 'systemctl is-active deploy-server'
ssh root@host 'journalctl -u deploy-server -n 50 --no-pager'
ssh root@host 'ss -ltnp | grep 4715'
deploy history app.qc
deploy preview app.qc
```

The startup log shows the auth-center URL and administration resource. Full
authorization denial reasons are written to the journal.

## Recovery notes

- Restore a previous binary from `/root/backups/deploy-server/` and restart
  the unit to roll back a server upgrade.
- Preserve the SQLite database during upgrades; it identifies active
  deployments and powers static serving, service startup, and history.
- Because authorization fails closed, maintain a tested recovery path for an
  auth-center outage, especially if auth-center is itself deployed through
  this service.
