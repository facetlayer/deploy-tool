# Standing up a deploy-server instance

One instance per host. It owns a deployments directory on disk, a SQLite
database, and the JSON-RPC endpoint the CLI talks to.

The existing droplets (do2, dohl) run the **old** server today: unit `deploy`,
port 4715, deployments at `/root/deploys`, database at
`/root/.local/state/deploy/db.sqlite`. This server opens that same database in
place — the schema is inherited and new columns are added by migration — so
replacing the binary keeps every project, deployment and active-deployment row.
Take a copy of the database before the first swap anyway.

## 1. Build and install the binary

The droplets run Ubuntu 24.04 (glibc 2.39) and have no Rust toolchain, so
cross-compile locally:

```bash
install/build-release.sh            # → target/x86_64-unknown-linux-gnu/release/deploy-server
```

Copy it to the host (`/root/bin/deploy-server` is the convention for binaries
that are not themselves deployed by this tool):

```bash
scp target/x86_64-unknown-linux-gnu/release/deploy-server root@host:/root/bin/deploy-server
ssh root@host 'chmod +x /root/bin/deploy-server'
```

## 2. Deployments directory

Where every uploaded project lands, one subdirectory per project. The path is
stored in the database, not in the environment, and the directory must already
exist:

```bash
mkdir -p /root/deploys
/root/bin/deploy-server set-deployments-dir /root/deploys
```

On do2 and dohl this is already set and should be left alone. The server
refuses to serve deployments until it is configured.

## 3. Database location

`deploy-server` resolves its state directory in this order, and uses
`<state-dir>/db.sqlite`:

1. `DEPLOY_STATE_DIR`, if set and non-empty.
2. `XDG_STATE_HOME`, if set and non-empty → `$XDG_STATE_HOME/deploy`.
3. `$HOME/.local/state/deploy`.

Running as root with none of those set gives
`/root/.local/state/deploy/db.sqlite`, which is where the existing instances'
data already is. The directory is created if missing; the schema is created or
migrated on open (WAL journal, 5 s busy timeout).

`deploy start-services`, which runs on the host, resolves the same three
variables and opens that database **read-only**.

## 4. auth-center configuration

All of these live in the instance's `EnvironmentFile`
(`/root/secrets/deploy.env`, `0600`, root-owned), never in the unit file.
Template: [`install/deploy.env.template`](../install/deploy.env.template).

| Variable | Meaning |
|---|---|
| `DEPLOY_AUTH_URL` | auth-center base URL, e.g. `https://auth.apf1.dev`. Never hardcoded. Unset or empty ⇒ legacy-only: no network calls, and the resource model is not enforced. |
| `DEPLOY_AUTH_KEY` | This instance's own auth-center service key, holding `auth:introspect`. Each instance gets its own so they can be revoked independently. |
| `DEPLOY_ADMIN_RESOURCE` | This instance's administration resource, e.g. `deploy-do2`. Required whenever `DEPLOY_AUTH_URL` is set; it is what `createProject` is checked against. No default and no derivation from the hostname. |
| `DEPLOY_DISABLE_LEGACY_KEYS` | `1` turns off the local `secret_key` table on an instance whose keys have all migrated. |
| `DEPLOY_STATE_DIR` | Optional override of the state directory (above). |

The server refuses to start half-configured — e.g. `DEPLOY_AUTH_URL` with no
`DEPLOY_ADMIN_RESOURCE` — rather than silently falling back to legacy-only. At
startup it prints an authorization summary (auth-center URL, admin resource,
whether legacy keys are enabled, and warnings for the dangerous combinations)
so the journal shows what a running instance is actually doing.

The authorization model itself — the `deploy:<resource>:<action>` scope format,
the action table, caching, and what happens when auth-center is unreachable —
is in [auth-integration.md](auth-integration.md). Do not restate it in an
operational runbook; mint keys against that document.

## 5. Run it

```bash
/root/bin/deploy-server serve --port 4715
```

`--port` is required. Under systemd, use
[`install/deploy-server.service`](../install/deploy-server.service) — `Type=simple`,
`Restart=always`, secrets pulled in with `EnvironmentFile=`:

```bash
scp install/deploy-server.service root@host:/etc/systemd/system/deploy-server.service
ssh root@host 'systemd-analyze verify /etc/systemd/system/deploy-server.service && \
  systemctl daemon-reload && systemctl enable --now deploy-server && \
  systemctl is-active deploy-server'
```

`enable` is what makes it survive a reboot. On a host still running the old
`deploy` unit on 4715, stop and disable that unit first — two processes cannot
hold the port, and both would write the same database.

Note the binary currently binds `0.0.0.0`, inherited from the old server. That
is against the do2 convention of binding `127.0.0.1` behind nginx, and there is
no flag to change it yet; on a droplet with no general firewall the port is
reachable directly. Keep that in mind when picking a port and when deciding
whether the instance can accept legacy keys.

## 6. Mint a first key

Two kinds of key exist during migration.

**A legacy key** — local to the instance, no auth-center involved, grants
everything on it:

```bash
/root/bin/deploy-server create-key      # prints the key once
```

Put it on the client in `~/secrets/deploy.env` as `DEPLOY_API_KEY=…` (or in the
environment variable of the same name). This is the right key for step 1 of the
rollout, and for bootstrapping an instance before auth-center is configured.

**An auth-center key** — minted in the auth-center dashboard, granted the exact
scope strings from [auth-integration.md](auth-integration.md):

- `deploy:deploy-do2:create-project` — to register projects on do2.
- `deploy:<resource>:deploy` — a CI key that ships to one resource.
- `deploy:<resource>:read` — an on-call key that can only look.
- `deploy:<resource>:*` — everything on one resource, including `sql`.

Once a key exists, register the projects on the instance:

```bash
deploy create-project hotlaps-api --resource hotlaps-staging --override-dest https://apf1.dev
```

Projects carried over from the old tool exist with no binding; running
`create-project` against them binds them (reported as `rebound`) and needs no
`--rebind` flag. Repointing an already-bound project does need `--rebind`.

Auditing what is left:

```bash
/root/bin/deploy-server list-legacy-keys
```

Prints id, label, created-at and last-used for each local key, and never the
key text. When it reports none remain, the instance can set
`DEPLOY_DISABLE_LEGACY_KEYS=1`.

## 7. Staged rollout

From [project-goals.md](project-goals.md) and the auth-center requirements,
unchanged:

1. Land the resource model with `DEPLOY_AUTH_URL` unset everywhere. No behavior
   change: the instance is legacy-only and still creates projects implicitly.
2. Register resources and issue keys in auth-center.
3. Enable auth-center on **do2** with the legacy table still active as a
   fallback. Register every project on the instance, then confirm real deploys
   work.
4. Enable on **dohl** only after do2 has run clean for a while.
5. Disable the legacy table per instance (`DEPLOY_DISABLE_LEGACY_KEYS=1`) once
   that instance's keys have migrated.

Do not set `DEPLOY_AUTH_URL` on a host still running the **old** server: its
interim code treats a missing `allowed` as permission granted, which is
strictly worse than legacy-only.

## Verify

```bash
ssh root@host 'systemctl is-active deploy-server'
ssh root@host 'journalctl -u deploy-server -n 50 --no-pager'   # check the auth summary
ssh root@host 'ss -ltnp | grep 4715'
deploy history <some-project>.qc                                # end-to-end read
deploy preview <some-project>.qc                                # before any real deploy
```

A denied call answers HTTP 401 and logs `[deploy auth] denied "<method>": …` on
the server; the reason is deliberately not returned to the client.
