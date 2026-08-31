# `.qc` configuration reference

A `.qc` file defines one project, its destination, the files to deploy, and
local/server hooks. The CLI uploads the config with each deployment so the
server can apply activation, database, route, and preservation settings.

## Settings

```text
deploy-settings
  project-name=my-app
  dest-url=https://deploy.example.com
  update-in-place
```

| Setting | Meaning |
|---|---|
| `project-name=<name>` | Registered project name. Required for remote commands. |
| `dest-url=<url>` | Server base URL; commands may override it with `--override-dest`. |
| `local-dir=<path>` | Local root for file rules and client hooks, relative to the config directory. |
| `secrets-file=<path>` | Env file from which to read `DEPLOY_API_KEY`; `~` is expanded. |
| `update-in-place` | Reuse one destination directory and delete untracked files. |
| `web-static-dir=<path>` | Static content directory recorded for `static-web-server`. |
| `candle-config=<path>` | Candle config used by `deploy start-services`. |
| `track-git-commit` | Record the current commit and branch; requires a clean worktree. |
| `allow-dirty-git-tree` | Permit a dirty worktree with `track-git-commit`. |
| `ignore-security-scan(<path>)` | Allowlist a path from the client credential scan; repeatable. |

Client key lookup order is the configured `secrets-file`, the
`DEPLOY_API_KEY` or legacy `GOOBERNETES_API_KEY` environment variable, then
`~/secrets/deploy.env`.

## Hooks

```text
before-deploy
  shell(pnpm build)

after-deploy
  shell(systemctl restart my-app)
  candle-restart(my-app)
```

`before-deploy` shell commands run on the client in the local root before the
manifest is built. `after-deploy` actions run on the server in the activated
deployment directory. Actions run in declaration order and a failure stops the
operation.

## File rules

```text
include dist
include package.json
exclude dist/**/*.map
ignore data
ignore-destination logs
```

| Directive | Source selection | Destination cleanup |
|---|---|---|
| `include <glob>` | Include matching files. | No effect. |
| `exclude <glob>` | Remove matching files. | No effect. |
| `ignore <glob>` | Exclude matching files. | Preserve matching paths. |
| `ignore-destination <glob>` | No effect. | Preserve matching paths. |

Patterns use `/`-separated, picomatch-compatible globs. `*` stays within a
path segment; `**` crosses directories. Directory traversal is sorted and
symlinked directories are treated as files rather than followed.

`update-in-place` deletes every destination path not present in the manifest
or protected by a destination rule. Add protection for databases, uploads,
caches, generated assets, and any other server-owned data. Always run `deploy
preview` before the first deployment and after changing file rules.

## Databases

```text
database data/app.db
  agent-sql-access-blocked
```

Each `database` path is relative to the active deployment. It becomes
available to `deploy list-databases` and `deploy sql`.
`agent-sql-access-blocked` rejects SQL when the client reports that it is
running in a coding agent. Explicit `--database` selections must name a
configured database and remain inside the deployment directory.

## Dynamic routes

```text
dynamic-route from=/i/:code to=/i/_/index.html
dynamic-route from=/post/:id to=post.html metadata-source=meta.json metadata-cache-ttl=60
```

These routes are consumed by `static-web-server`. `from` and `to` are
required. `metadata-source` is optional; `metadata-cache-ttl` is a positive
integer number of seconds.

## Preserving generated files

```text
preserve-existing-files public/assets/**
preserve-existing-files-max-age 7d
```

`preserve-existing-files` retains matching destination files that are not in
the new manifest. Uploaded files still replace same-named files.
`preserve-existing-files-max-age` removes preserved files older than the
specified duration; supported suffixes are `s`, `m`, `h`, `d`, and `w`.

## Deployment modes

- Versioned mode is the default. Each deployment has a separate directory and
  an earlier deployment can be reactivated.
- `update-in-place` keeps a stable directory for process managers but has no
  previous directory to restore. Recover by deploying an older revision.
