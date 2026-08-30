# Standing up a deploy-server instance

One instance per host. It owns a deployments directory on disk, a SQLite
database, and the JSON-RPC endpoint the CLI talks to.

The existing droplets (do2, dohl) run the **old** server today: unit `deploy`,
port 4715, deployments at `/root/deploys`, database at
`/root/.local/state/deploy/db.sqlite`. This server does **not** inherit that
database's schema. Compatibility with it is not a requirement, the local
`secret_key` table is gone, and the resource binding lives in tables the old
schema does not have — so cutting an instance over means rebuilding or
importing the database, not swapping the binary in place. That is
[§7](#7-cutover) and it is the part to plan.

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

The server refuses to serve deployments until it is configured. Rebuilding the
database clears this setting too, so it is part of the cutover checklist.

## 3. Database location

`deploy-server` resolves its state directory in this order, and uses
`<state-dir>/db.sqlite`:

1. `DEPLOY_STATE_DIR`, if set and non-empty.
2. `XDG_STATE_HOME`, if set and non-empty → `$XDG_STATE_HOME/deploy`.
3. `$HOME/.local/state/deploy`.

Running as root with none of those set gives
`/root/.local/state/deploy/db.sqlite`, which is where the existing instances'
data already is. The directory is created if missing; the schema is created on
open (WAL journal, 5 s busy timeout). There are no in-place migrations from the
old schema.

`deploy start-services`, which runs on the host, resolves the same three
variables and opens that database **read-only**.

## 4. auth-center configuration

Three variables, all required. There is no local key table any more, so an
instance missing any of them cannot authenticate anyone; the server prints
which one is missing and exits rather than starting half-configured.

They live in the instance's `EnvironmentFile` (`/root/secrets/deploy.env`,
`0600`, root-owned), never in the unit file. Template:
[`install/deploy.env.template`](../install/deploy.env.template).

| Variable | Meaning |
|---|---|
| `DEPLOY_AUTH_URL` | auth-center base URL, `https://$AUTH_HOST`. Never hardcoded, and deliberately not written down here — the hostname is a deployment detail; each instance gets its own setting. |
| `DEPLOY_AUTH_KEY` | This instance's own auth-center service key, holding `auth:introspect`. Each instance gets its own so they can be revoked independently. |
| `DEPLOY_ADMIN_RESOURCE` | This instance's administration resource: `do2-deploy` on do2, `dohl-deploy` on dohl. It is what `createProject` is checked against, as `<this value>:create-project`. No default and no derivation from the hostname. The requirements' config table omits this variable; it is a deliberate addition, because `createProject` resolves to no project and so has nothing else to be authorized against. |

`DEPLOY_STATE_DIR` is the one optional variable (§3).

At startup the server prints an authorization summary — auth-center URL, admin
resource, and a boxed warning if `--disable-api-key-check` is set — so the
journal shows what a running instance is actually doing.

`--disable-api-key-check` accepts every call from anyone with no key at all. It
exists for local development and the test suite. It is not a migration path and
must never be set on do2 or dohl.

The authorization model itself — the `<resource>:<action>` scope format, the
action table, caching, and what happens when auth-center is unreachable — is in
[auth-integration.md](auth-integration.md). Do not restate it in an operational
runbook; mint keys against that document.

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

**Both current hosts (do2 and dohl) run the unit under the name `deploy`, not
`deploy-server`** — inherited from the old tool, which predates this repo. The
name above is what a fresh install gets; the upgrade tooling detects which of
the two is present rather than assuming either.

With `Restart=always`, a missing environment variable shows up as a unit that
restarts in a loop; `journalctl -u deploy-server` names the variable.

## 5a. Upgrading an existing host

Once a host is set up, new builds go out with:

```bash
install/deploy-to-hosts.sh both          # or: do2 | dohl
install/deploy-to-hosts.sh do2 --skip-tests
```

It runs the workspace tests, cross-compiles, uploads *beside* the live binary
(never over it — a partial transfer onto the running path would leave the host
with no working server), then hands off to
[`install/remote-upgrade.sh`](../install/remote-upgrade.sh), which:

- works out whether the unit is `deploy` or `deploy-server`,
- runs `--version` on the uploaded binary **before** installing it, so a
  truncated file or a glibc mismatch fails while the working binary is still in
  place,
- backs the current binary up to `/root/backups/deploy-server/`,
- swaps it in with an atomic `mv`,
- and schedules the restart with `systemd-run --on-active=2`, so the ssh
  connection closing cannot interrupt it.

The script then verifies each host is `active` and prints the last of its
journal. Rolling back is copying a file out of `/root/backups/deploy-server/`
and restarting.

This is the one service that is **not** deployed by GitHub Actions. It cannot
deploy itself through the deploy service — activating such a deployment restarts
the process serving it — and automating it from CI would mean giving GitHub a
key with root on both droplets, which is a much larger blast radius than the
thing it automates.

Note the binary currently binds `0.0.0.0`, inherited from the old server. That
is against the do2 convention of binding `127.0.0.1` behind nginx, and there is
no flag to change it yet; on a droplet with no general firewall the port is
reachable directly. Keep that in mind when picking a port.

## 6. Keys and project registration

Every key is created with `auth-setup`, which runs from a laptop holding no
credential: it prints an approval URL and a confirmation code, opens a browser,
and blocks until an admin approves. There is no `deploy-server create-key` and
no local key table; a key cannot be minted on the host.

A scope is `<resource>:<action>` — exactly two segments, action last. The
strings this instance needs:

- `do2-deploy:create-project` — to register projects on do2 (`dohl-deploy:…`
  on dohl). This is the instance's `DEPLOY_ADMIN_RESOURCE`.
- `<resource>:deploy` — a CI key that ships to one resource.
- `<resource>:read` — an on-call key that can only look.
- `<resource>:*` — everything on one resource, including `execute-sql`. `*`
  does not cross `:`, so it cannot reach another resource.

Do not write these by hand. `deploy auth-scopes <config.qc> --resource <name>`
prints the exact `auth-setup create-role` / `create-key` lines for a project;
`deploy create-project` prints the same block after registering. The reason is
specific: auth-center rejects a three-segment scope like
`deploy:hotlaps-api-staging:deploy` and *suggests* `deploy-hotlaps-api-staging:deploy`,
which is valid, mints cleanly, and names a resource this server will never ask
about. See [auth-integration.md](auth-integration.md).

The instance's administration resource belongs to the "Server Admin" auth-center
project; each application's deploy resources belong to that application's own
project. Resource names are global, so run `auth-service check-conflicts` on the
auth host after creating them.

With an admin key in hand, register every project on the instance:

```bash
deploy create-project hotlaps-api --resource hotlaps-api-staging --override-dest https://apf1.dev
```

Resources are per-project-per-environment, and the binding lives in this
instance's database: do2's `hotlaps-api` binds to `hotlaps-api-staging`, dohl's
to `hotlaps-api-prod`. Until a project is registered and bound, every call
naming it is denied — including reads. Repointing an already-bound project at a
different resource needs `--rebind` and is written to the instance's binding
history.

## 7. Cutover

### The database upgrades in place — this is the tested path

The rollout notes describe rebuilding an instance's database. Do not: pointing
this server at the existing `db.sqlite` is what do2 actually did, and it keeps
the operational state a rebuild throws away. On do2 that was 407 `deployment`
rows and 27 `active_deployment` rows.

On first start the server creates `project_resource_binding` and
`project_resource_binding_history`, and adds `authorized_by_key_id` /
`authorized_by_key_name` to the inherited `deployment` table. `project`,
`deployment`, `active_deployment` and `next_deploy_id` keep their contents and
their shape. The old `secret_key` table is left in place and never read — the
keys in it are dead against this server.

Back the database up first anyway:

```bash
cp /root/.local/state/deploy/db.sqlite /root/backups/db.sqlite.pre-rust-cutover
cp /etc/systemd/system/deploy.service /root/backups/deploy.service.node-original
```

Rolling back is restoring that unit file and `systemctl daemon-reload &&
systemctl restart deploy`. The two added tables and two added columns are inert
to the old server.


There is no legacy fallback, so enabling this on an instance is a cutover, not
a gradual migration. From the Rollout section of the auth-center requirements,
per instance:

1. Land the resource model and the resolution path.
2. Create the projects, roles and keys with `auth-setup`, including this
   instance's own `auth:introspect` service key. Generate the scope strings with
   `deploy auth-scopes`, and run `auth-service check-conflicts` afterwards.
3. Register every project against its resource, and distribute the new keys to
   every caller that needs one — CI secrets, `~/secrets/deploy.env`, and so on.
   Do this **before** the cutover. A caller holding no valid key at cutover
   simply stops being able to deploy, and this is the step most likely to be
   left incompletely done.
4. Cut over: rebuild or import the database (below), write the three variables
   into `/root/secrets/deploy.env`, restart.
5. Verify with a real deploy of a low-stakes project before relying on it.

Do do2 first and let it run clean for a while before cutting dohl over.
Production is under no deadline.

### If you rebuild instead, you lose live state

Kept for the case where a rebuild is genuinely wanted; the section above is the
path do2 took and the one to prefer.

The deploy database holds operational state as well as key material.
`active_deployment` records which deployment is currently serving each project,
and the `deployment` rows describe what is on disk. **Rebuilding discards
both** — the files stay on disk, but the server no longer knows which
deployment is live, so static-web serving and `deploy start-services` have
nothing to read and `deploy rollback` has no history.

The key material is what is being thrown away deliberately; the deployment
bookkeeping is not. Two recoveries:

- Redeploy every project on the instance, which regenerates the state, or
- Do a one-off import of `project`, `deployment` and `active_deployment` from
  the old database. Those three tables keep the shape the old tool gave them
  precisely so the import is a plain `insert into … select`.

An imported project has rows but no binding, so it is registered but unbound
and still denies every call until `deploy create-project` binds it (reported as
`created`, since there was no previous binding). Take a copy of the old
database before touching anything.

### The circular dependency, which is real

Fail-closed is correct: a deploy that fails because auth-center is unreachable
is right, one that succeeds because it was unreachable is not. But it means
auth-center downtime is deploy downtime for every instance pointed at it — and
**auth-center is itself deployed by this tool**. If auth-center is down and the
fix is to deploy auth-center, there is no way in.

Before any cutover, have a tested way back in. Decide deliberately whether
auth-center's own deploys stay on a separate path so the two cannot deadlock.
`--disable-api-key-check` behind an SSH-only restart is one answer, but it has
to be a decision that was made and tested, not one discovered during an outage.

Do not point `DEPLOY_AUTH_URL` at a host still running the **old** server: its
interim code treats a missing `allowed` as permission granted.

## Verify

```bash
ssh root@host 'systemctl is-active deploy-server'
ssh root@host 'journalctl -u deploy-server -n 50 --no-pager'   # check the auth summary
ssh root@host 'ss -ltnp | grep 4715'
deploy history <some-project>.qc                                # end-to-end read
deploy preview <some-project>.qc                                # before any real deploy
```

A denied call answers HTTP 401 and logs `[deploy auth] denied "<method>": …` on
the server. The full reason stays in the journal. The client is told only
`Unauthorized`, except in two cases: a key that is real but lacks the scope is
told which scope (`this key does not hold hotlaps-api-staging:deploy`), and an
unreachable auth-center is named coarsely so a cutover failure is not silent. An
unknown or revoked key learns nothing, so it cannot enumerate this instance's
project-to-resource bindings.
