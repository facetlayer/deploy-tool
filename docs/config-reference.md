# `.qc` configuration reference

A `.qc` file (historically `.deploy`) describes one deploy project: where it
goes, which local files it contains, and what runs before and after. It is read
by the CLI, uploaded verbatim with the deployment, and re-read on the server at
activation time — so client-side and server-side directives live in the same
file.

The directives below are the complete set the code reads; they come from
`crates/deploy-core/src/config.rs` (settings, hooks, databases, routes,
preserve) and `crates/deploy-core/src/filelist.rs` (include/exclude/ignore).
Anything else in the file is parsed and ignored.

```
deploy-settings
  project-name=my-app
  dest-url=https://apf1.dev
  update-in-place

include dist
include package.json

exclude dist/**/*.map

ignore data
```

## `deploy-settings`

One block, with each setting on its own indented line.

| Setting | Read by | Meaning |
|---|---|---|
| `project-name=<name>` | both | The project. Must already be registered on the destination with `deploy create-project`, or every call naming it is denied (see [getting-started.md](getting-started.md#3-register-the-project)). The name is *not* what authorizes the deploy — the instance's resource binding is. |
| `dest-url=<url>` | client | Base URL of the deploy server, e.g. `https://apf1.dev`. The CLI posts to `<dest-url>/json-rpc`. Overridable per command with `--override-dest`. |
| `local-dir=<path>` | client | Moves the local root that file rules and hooks resolve against. See [local-dir](#local-dir) below. |
| `secrets-file=<path>` | client | Read `DEPLOY_API_KEY` from this env file instead of the default `~/secrets/deploy.env`. `~` is expanded. Use it when one machine deploys to instances that require different keys. Precedence: this file (if it exists and has a key) → `DEPLOY_API_KEY` / `GOOBERNETES_API_KEY` in the environment → `~/secrets/deploy.env`. |
| `update-in-place` | both | Overwrite the same project directory on every deploy instead of creating a new versioned one. **Deletes destination files that are not in the upload** — read [ignore](#ignore-and-server-generated-paths). |
| `web-static-dir=<path>` | both | Records which directory holds the static site, for `static-web-server` to serve at `/web/<project-name>/`. No restart or registration needed; static-web reads the active deployment from the deploy database on each request. |
| `candle-config=<path>` | both | Path to a Candle config inside the deployment. `deploy start-services` runs `candle check-start` in every active deployment that declares one. |
| `track-git-commit` | client | Record the local `HEAD` commit and branch as deployment tags. The deploy is refused if the working tree is dirty. Read back with `deploy check-deployed-commit`. |
| `allow-dirty-git-tree` | client | With `track-git-commit`: still record the commit, but do not require a clean tree. |
| `ignore-security-scan(<path>)` | client | Allowlist one path out of the security scan. Repeatable — each occurrence adds one path. |

The security scan runs on the client for every command that reads a config, not
just `run`, and refuses file sets containing `.env` files, `.pem`/`.key`/`.p12`
material, SSH keys, `.git`/`.ssh`/`.aws` contents, and basenames containing
`secret`, `credential` or `password`. `ignore-security-scan` is the only way
past it.

## Hooks

```
before-deploy
  shell(pnpm build)
  shell(node tools/bundle-check.ts)

after-deploy
  shell(bash release-api/post-deploy.sh)
  candle-restart(my-app)
```

- `before-deploy` — `shell(...)` commands run **on the client**, in the local
  root, before anything is uploaded. Used for builds. A non-zero exit stops the
  deploy.
- `after-deploy` — run **on the server**, in the deployment directory, at
  activation. `shell(...)` runs a command; `candle-restart(<service>)` restarts
  a Candle-managed service.
- Commands run in the order written. Anything other than `shell` or
  `candle-restart` in these blocks is ignored.

`deploy run` restarts nothing by itself. If the `after-deploy` hook does not
call `systemctl restart` (or `candle-restart`), the old process keeps serving
the old binary.

## File rules

```
include release-api
include web/**/*.html
exclude web/node_modules
ignore backend/data
ignore-destination logs
```

| Directive | Applies to source | Applies to destination |
|---|---|---|
| `include <pattern>` | selects files | — |
| `exclude <pattern>` | drops files | — |
| `ignore <pattern>` | drops files | protects from deletion |
| `ignore-destination <pattern>` | — | protects from deletion |

A directive with no pattern is an error ("Missing pattern for … rule"), which
is checked before the command name is recognised — so a bare `include` fails
whatever else is on the line.

Patterns are picomatch-compatible globs, matched against `/`-separated paths
relative to the local root:

- `*` matches within one path segment and never crosses `/`.
- `**` crosses segments: `web/**/*.ts`.
- A wildcard does **not** match a leading dot in a segment (picomatch's
  `dot: false`), so `include *` does not pick up `.env` or `.git`. A pattern
  containing `**` is exempt from that guard, matching picomatch.
- Directory entries are `lstat`ed, so a symlink to a directory counts as a file
  and is not descended into.
- Directory listings are sorted, so a deployment's file list is reproducible.

### `ignore` and server-generated paths

**This is the directive that has caused real data loss.** `update-in-place`
deletes anything in the destination directory that is not part of the upload.
Every path the server generates — SQLite databases, uploaded media, pipeline
artifacts, caches — exists on the server and not in the local tree, so a deploy
sees it as an orphan and deletes it.

`~/biz/hotlaps/deploy/api-staging.qc` records the incident: the `ignore` line
protecting the database was lost during the Node→Rust rewrite of that project's
config, and the next `update-in-place` deploy wiped the production database
(2026-05-23). The config now carries the rule with a comment saying exactly
that:

```
# CRITICAL: the sqlite DB lives at backend/data/app.db on the server and
# must not be touched by deploys. Dropping this `ignore` caused a full
# DB wipe (2026-05-23).
ignore backend/data

# Papermap pipeline artifacts are generated on the server and don't exist
# in the local tree — without these ignores, update-in-place treats them
# as orphaned and deletes them on every deploy.
ignore release-api/config/resorts/*/papermap
ignore release-api/config/resorts/papermap-fleet.json
```

Before the first deploy of any config that points at an existing directory, run
`deploy preview <config>.qc`: it lists exactly which server-side files the
deploy would delete. If it names something that should survive, add an `ignore`
for it — and rescue anything already at risk with
`deploy copy-back <config> <path>` first.

### `local-dir`

Every file rule and every hook's working directory resolves against the local
root, which by default is the directory holding the config file. `local-dir`
moves it, and is itself relative to the config file's directory:

```
# deploy/api-staging.qc
deploy-settings
  local-dir=..
```

That is the hotlaps arrangement: configs live in `deploy/`, but every path in
them is written relative to the project root. Without `local-dir=..` the CLI
resolves every `include` against `deploy/` and ships an empty bundle. No
command exposes a flag to override it. Resolution is lexical — the path need
not exist yet, so a `before-deploy` step may create it.

## Databases

```
database backend/data/app.db
  agent-sql-access-blocked

database backend/data/cache.db
```

- `database <path>` registers a SQLite file for `deploy sql` and
  `deploy list-databases`. The path is relative to the deployment directory on
  the server. Repeat for each file. The server reads this list from the
  **active** deployment's stored config, so queries hit the same files the
  running application sees.
- `agent-sql-access-blocked` (as a child line) makes the server refuse
  `deploy sql` queries the client reports as coming from a coding agent. Use it
  on production databases. It is a guardrail, not a security boundary: agent
  detection is client-side and a caller that controls its environment can
  bypass it.

Queries against a multi-database project are routed by extracting table names
from the SQL. If routing is ambiguous or finds nothing, the command fails and
lists the databases; `--database <path>` picks one explicitly. That path must
be one listed here, and must resolve inside the deployment directory.

## Dynamic routes

```
dynamic-route from=/i/:code to=/i/_/index.html
dynamic-route from=/post/detail to=post.html metadata-source=meta.json metadata-cache-ttl=60
```

Express-style patterns served from one HTML shell by `static-web-server`, for
routes whose parameter is only known at runtime. `from` and `to` are both
required — a route missing either is dropped silently. `metadata-source` names
a JSON file supplying per-route metadata; `metadata-cache-ttl` is its cache
lifetime in seconds and must parse as a positive integer, otherwise it is
ignored.

## Preserving files across deploys

```
preserve-existing-files release-api/static/**
preserve-existing-files-max-age 7d
```

- `preserve-existing-files <glob>` keeps destination files matching the glob
  even when they are not in the new upload. Repeatable. Files in the upload
  still overwrite same-named destination files — a no-op for content-hashed
  assets, whose names change each build.
- `preserve-existing-files-max-age <duration>` garbage-collects preserved files
  older than the cutoff during a deploy, so the directory does not grow without
  bound. Units: `s`, `m`, `h`, `d`, `w` (e.g. `7d`). A bare number or an
  unknown unit is a hard error.

This is what lets an already-open browser tab still fetch its old Next.js
`_next/static/` chunk after a redeploy.

## Deployment modes

- **Versioned (default)** — each deploy gets its own directory
  (`my-app-1`, `my-app-2`, …), so `deploy rollback` can re-activate an earlier
  one.
- **`update-in-place`** — one directory, overwritten each time. Simpler, and
  what every service on do2/dohl uses because their systemd `ExecStart` points
  at a fixed path. There is no previous copy to roll back to; rolling back
  means redeploying an older build.
