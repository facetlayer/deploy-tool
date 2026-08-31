# Getting started

This guide assumes a running deploy-server instance. See
[server-setup.md](server-setup.md) to install one.

## Install the CLI

```bash
cargo build --release
install target/release/deploy /usr/local/bin/deploy
```

## Create a project config

Create `my-app.qc` in the project repository:

```text
deploy-settings
  project-name=my-app
  dest-url=https://deploy.example.com
  update-in-place

before-deploy
  shell(pnpm build)

after-deploy
  shell(systemctl restart my-app)

include dist
include package.json
exclude dist/**/*.map

# Preserve data created on the server.
ignore data
```

Paths are relative to the config directory unless `local-root` changes the
root; the root it resolves to must be at or below the directory you run the
command from. See [config-reference.md](config-reference.md) for all
directives.

## Register and authorize the project

Each project must be registered once on each server instance:

```bash
deploy create-project my-app \
  --resource my-app-staging \
  --override-dest https://deploy.example.com
```

This command requires a key with
`<instance-admin-resource>:create-project`. It binds `my-app` to
`my-app-staging` on that server and prints `auth-setup` commands for the
project role and key.

You can generate the same authorization setup independently:

```bash
deploy auth-scopes my-app.qc --resource my-app-staging
```

Store the resulting client key as `DEPLOY_API_KEY` in the environment or in
`~/secrets/deploy.env`. A config's `secrets-file` setting takes precedence.
The legacy `GOOBERNETES_API_KEY` environment name is also accepted.

## Preview and deploy

Inspect both the local file set and server-side changes:

```bash
deploy preview-deploy-files my-app.qc
deploy preview my-app.qc
```

Pay particular attention to deletions. An update-in-place deployment deletes
destination files absent from the manifest unless an `ignore`,
`ignore-destination`, or preservation rule protects them.

Deploy after the preview is safe:

```bash
deploy run my-app.qc
```

The CLI runs local hooks, scans for likely credentials, uploads missing
content, verifies it, activates it, and then the server runs `after-deploy`.
Multiple config files may be passed to `run`; they execute serially and stop at
the first failure.

## Inspect and recover

```bash
deploy history my-app.qc
deploy check-deployed-commit my-app.qc
deploy rollback my-app.qc
deploy copy-back my-app.qc path/to/file
```

`check-deployed-commit` requires `track-git-commit` in the config. Rollback
reactivates a previous versioned directory; with `update-in-place`, deploy a
known-good revision instead.

## Databases

Declare SQLite files in the config:

```text
database data/app.db
  agent-sql-access-blocked
```

Then query the active deployment:

```bash
deploy list-databases my-app.qc
deploy sql my-app.qc 'select count(*) from users'
deploy sql my-app.qc 'select id, name from users' --json
```

Use `--database <path>` when table-based routing is ambiguous. SQL writes
commit immediately. `agent-sql-access-blocked` is a client-detection
guardrail, not a security boundary.
