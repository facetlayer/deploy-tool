# Getting started

Walking a new project from nothing to a deployed service. Assumes a
`deploy-server` instance already exists; standing one up is
[server-setup.md](server-setup.md).

If you have used the old tool, read [step 3](#3-register-the-project) first.
`deploy create-project` is new and is not optional: projects are no longer
created implicitly by the first deploy, and every call for an unregistered or
unbound project is denied. There is no legacy key path and no fallback that
lets a deploy through without it.

The whole sequence, once a server exists:

1. Set the instance's deployments directory (`set-deployments-dir`).
2. Configure `DEPLOY_AUTH_URL`, `DEPLOY_AUTH_KEY` and `DEPLOY_ADMIN_RESOURCE`
   on the instance; it will not start without all three.
3. `deploy create-project <name> --resource <resource>`, then create the role
   and key it prints with `auth-setup`.
4. `deploy run <config>.qc`.

Steps 1 and 2 are done once per instance and are in
[server-setup.md](server-setup.md). Steps 3 and 4 are per project and are what
the rest of this page walks through.

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

Keys are issued in auth-center; the server has no local key table. A key is a
set of `<resource>:<action>` grants — two segments, action last. See
[auth-integration.md](auth-integration.md) for the format, and for which action
each command needs.

You will not have a key until someone creates one with `auth-setup`, which is
step 3's job; `deploy auth-scopes` prints the exact commands. Come back here
once you have the key it mints.

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
`my-app-staging:<action>`. The binding lives in the instance's database and is
never taken from a client, which is what lets the same project name require
different resources on staging and production hosts.

Resource names are per-project-per-environment for that reason: `my-app-staging`
and `my-app-prod` are separate resources in the same auth-center project, so a
staging key presented to production is denied. `--resource` must not contain a
`:` and both the CLI and the server refuse one.

Skipping this step is the failure most likely to catch out someone used to the
old tool: without a binding there is no resource to check a key against, so
every call naming the project answers HTTP 401, reads included. The server's
journal names the reason; the client sees a bare `Unauthorized`, because an
unrecognized key is told nothing about this instance's bindings.

There is no config file yet, so the destination has to be given with
`--override-dest`. Registering requires a key holding
`<instance-admin-resource>:create-project` — e.g. `do2-deploy:create-project` —
which is a different key from the one that deploys.

The command finishes by printing the scopes this project needs and the
ready-to-run `auth-setup` commands that create them. Nothing verifies that the
resource exists: auth-center has no endpoint that could answer, and resources
are derived rather than declared, so at this point the resource usually does not
exist yet — the `auth-setup` commands are what bring it into being. A typo shows
up at the first call as `denied: this key does not hold my-app-stagign:deploy`,
which is why the generated commands are worth pasting rather than retyping.

Re-running with the same resource is a no-op. Pointing an already-bound project
at a different resource requires `--rebind`, and is recorded in the instance's
binding history.

### 3b. Create the role and the key

The same block `create-project` prints is available on its own, before you have
an admin key or a server to talk to:

```bash
deploy auth-scopes my-app.qc --resource my-app-staging
```

It contacts nothing and needs no key. It reads the config for the project name
and destination, derives the scopes from the same method table the server
authorizes against, and prints the `auth-setup` commands to run — roughly:

```bash
auth-setup create-role my-app-staging-deployer \
    --project <auth-center-project-id> \
    --scope my-app-staging:deploy \
    --scope my-app-staging:read \
    --description 'Ship and inspect deployments of my-app-staging'

auth-setup create-key my-app-staging-ci \
    --project <auth-center-project-id> \
    --service github-actions \
    --role my-app-staging-deployer
```

`auth-setup` holds no credential: each command prints an approval URL and a
confirmation code, opens a browser, and waits for an admin to approve. The
request is validated when it is made, so a malformed scope fails immediately
rather than after someone approves it, and it expires after 15 minutes. A new
key's plaintext is printed once and then wiped from the server — put it in
`~/secrets/deploy.env` (step 2) straight away.

`my-app-staging:execute-sql` and `my-app-staging:rollback` are printed
*separately*, not folded into that role. That is deliberate: the actions are
split so a CI key that ships builds cannot run arbitrary SQL against the
project's database. Grant them to the keys that need them, as their own role.

`--project` is the auth-center project that owns the application — resource
names are global across auth-center projects, so `my-app-staging` must be
declared in exactly one. Do not claim a bare application name as a resource
(`hotlaps` is where `hotlaps:admin`, the SSO client's required scope, lives).
`auth-service check-conflicts` on the auth host reports any name owned by two
projects.

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
