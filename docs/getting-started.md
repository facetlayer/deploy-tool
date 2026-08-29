# Getting started

Walking a new project from nothing to a deployed service. Assumes a
`deploy-server` instance already exists; standing one up is
[server-setup.md](server-setup.md).

If you have used the old tool: the one new step is
[step 3](#3-register-the-project), `deploy create-project`. Projects are no
longer created implicitly by the first deploy.

## Model

- The **server** runs on the deployment target. It receives files, stores them
  under its deployments directory, and runs your `after-deploy` hook.
- The **client** (`deploy`) runs wherever you trigger deploys — your laptop or
  CI. It reads a `.qc` file describing what to ship and where.

## 1. Install the CLI

```bash
cargo build --release        # target/release/deploy
```

Put it on your `PATH`.

## 2. Set up the client's key

The CLI looks for a key in this order:

1. The file named by `secrets-file=` in the config, if it has a key.
2. `DEPLOY_API_KEY` (or `GOOBERNETES_API_KEY`) in the environment.
3. `~/secrets/deploy.env`.

The env-file format is lenient: `KEY=value`, optional `export ` prefix,
optional quotes, `#` comments.

```bash
mkdir -p ~/secrets
echo 'DEPLOY_API_KEY=<your key>' >> ~/secrets/deploy.env
chmod 600 ~/secrets/deploy.env
```

Use `secrets-file=` when one machine deploys to instances that require
different keys — a hardened production instance that rejects the shared key,
for example.

## 3. Register the project

```bash
deploy create-project my-app --resource my-app-staging --override-dest https://apf1.dev
```

This binds the project name to an auth-center resource **on that instance**.
Every later call for this project is checked against
`deploy:my-app-staging:<action>`. The binding lives in the instance's database
and is never taken from a client, which is what lets the same project name
require different resources on staging and production hosts.

There is no config file yet, so the destination has to be given with
`--override-dest`. Registering requires a key holding
`deploy:<instance-admin-resource>:create-project`.

The command prints the scope strings keys for this project must hold. It also
warns that it could not verify the resource exists — auth-center has no
resource registry yet, so a typo in the resource name cannot be caught here and
would show up later as every deploy being denied. Check the spelling against
the dashboard.

Re-running with the same resource is a no-op. Pointing an already-bound project
at a different resource requires `--rebind`.

## 4. Write the config

`my-app.qc` in the project repo:

```
deploy-settings
  project-name=my-app
  dest-url=https://apf1.dev
  update-in-place

before-deploy
  shell(pnpm build)

after-deploy
  shell(systemctl restart my-app)

include dist
include package.json

exclude dist/**/*.map

# Server-generated. Without this, update-in-place deletes it.
ignore data
```

- `include` / `exclude` / `ignore` take picomatch globs relative to the config
  file's directory (or to `local-dir`, if set).
- `before-deploy` runs on your machine before upload; `after-deploy` runs on the
  server after the files land.
- `update-in-place` overwrites one directory instead of creating a versioned
  one — necessary when a systemd unit's `ExecStart` points at a fixed path.

Every directive is in [config-reference.md](config-reference.md).

## 5. Look before you deploy

```bash
deploy preview-deploy-files my-app.qc   # local files that would be included
deploy preview my-app.qc                # what would upload, and what would be DELETED
```

Read the delete list. `update-in-place` removes anything in the destination
that is not part of the upload, and every server-generated path — databases,
uploaded media, generated artifacts — is exactly that. A missing `ignore` for a
database directory wiped a production database on 2026-05-23; the details are
in [config-reference.md](config-reference.md#ignore-and-server-generated-paths).

If `preview` names a file that must survive, either add an `ignore` rule or
rescue it first:

```bash
deploy copy-back my-app.qc path/to/file
```

## 6. Deploy

```bash
deploy run my-app.qc
```

Which does:

1. Run `before-deploy` locally.
2. Resolve the file list and run the security scan (it refuses to upload
   `.env` files, keys, and credential files; `ignore-security-scan(<path>)`
   allowlists one).
3. Create the deployment and send the manifest.
4. Ask which files the server lacks, and upload only those.
5. Delete leftovers, verify every file's hash.
6. Activate — swap the live directory and run `after-deploy` on the server.

Several configs can be given at once; they run serially and the first failure
stops the rest.

`deploy run` restarts nothing on its own. If your `after-deploy` hook does not
call `systemctl restart` (or `candle-restart(...)`), the old process keeps
running the old code.

## 7. Afterwards

```bash
deploy history my-app.qc                    # deployments, which is active, who shipped it
deploy check-deployed-commit my-app.qc      # needs track-git-commit
deploy rollback my-app.qc                   # pick an earlier deployment
```

Rollback re-activates an earlier deployment directory. With `update-in-place`
there is no earlier directory to go back to, so rolling back means redeploying
an older build — plan on rolling forward.

## Databases

Register each SQLite file the project owns, so it can be queried without
SSHing in:

```
database data/app.db
  agent-sql-access-blocked
```

```bash
deploy list-databases my-app.qc
deploy sql my-app.qc 'select count(*) from users'
deploy sql my-app.qc 'select id, name from users' --json
```

The server reads the database list from the *active* deployment's stored
config, so redeploy after adding an entry. With more than one database, queries
are routed by the table names in the SQL; `--database <path>` overrides that.

`agent-sql-access-blocked` makes the server refuse queries the client reports
as coming from a coding agent — a guardrail for production data, not a security
boundary.

Writes commit immediately and there is no transaction wrapper. Run the `SELECT`
first.

## Next

- [config-reference.md](config-reference.md) — every directive.
- [client-server-api.md](client-server-api.md) — the RPC surface.
- [auth-integration.md](auth-integration.md) — which scope a key needs.
