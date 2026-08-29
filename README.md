# deploy-tool

A deployment service and its CLI. A deployment is: upload a set of files to a
server, then trigger server-side actions (restarts, SQL migrations, static-site
swaps). A bare-bones alternative to a container orchestrator.

This repo replaces `~/tools/deploy`, which was split between a TypeScript CLI
and a Rust daemon. Everything here is Rust, in one workspace:

| Crate | Binary | What it is |
|---|---|---|
| `crates/deploy-core` | — | Shared logic: the `.qc` parser, config reading, file list + hashing, RPC wire types, the authorization action table. |
| `crates/deploy-server` | `deploy-server` | The backend. One instance per host. Owns a deployments directory and a SQLite database, serves JSON-RPC, runs activation actions. |
| `crates/deploy-cli` | `deploy` | The CLI. Reads a `.qc` file, computes the file set, uploads what the server does not have, drives the deployment to activation. |

Why the rewrite, and what it changes, is in
[docs/project-goals.md](docs/project-goals.md).

## Setup, in order

An instance is not usable until all four of these are done, and skipping step 3
means every deploy of that project is denied.

1. **Deployments directory** — `deploy-server set-deployments-dir /root/deploys`
   on the host. The path lives in the server's database, not the environment.
2. **auth-center configuration** — `DEPLOY_AUTH_URL`, `DEPLOY_AUTH_KEY` and
   `DEPLOY_ADMIN_RESOURCE` in the instance's environment file. All three are
   required; the server refuses to start without them.
3. **Register each project**, binding it to an auth-center resource:

   ```bash
   deploy create-project my-app --resource my-app-staging --override-dest https://apf1.dev
   ```

4. **Deploy** — `deploy run my-app.qc`.

Steps 1 and 2 are per instance ([docs/server-setup.md](docs/server-setup.md));
steps 3 and 4 are per project ([docs/getting-started.md](docs/getting-started.md)).

## `create-project` is a required first step

In the old tool a project came into existence implicitly on its first
successful deploy. It does not any more. A project must be registered on the
target instance and bound to an [auth-center](docs/auth-integration.md)
resource before anything can be deployed to it. A deploy to an unregistered or
unbound project is refused — there is no fallback and no implicit create.

The resource binding lives in that instance's own database and is never
supplied by a client. That is what lets the same project name — `hotlaps-api`,
say — require `hotlaps-staging` on do2 and `hotlaps-prod` on dohl, so a staging
key presented to production is denied.

Registering requires a key holding `deploy:<admin-resource>:create-project` on
the instance (see [docs/auth-integration.md](docs/auth-integration.md), D2).

## Install

Build both binaries from the workspace:

```bash
cargo build --release
# target/release/deploy         — the CLI
# target/release/deploy-server  — the backend
```

For the droplets, cross-compile a Linux binary with
[`install/build-release.sh`](install/build-release.sh); they run Ubuntu 24.04
and have no Rust toolchain. Standing up an instance is
[docs/server-setup.md](docs/server-setup.md).

On the client, put the auth-center key in `~/secrets/deploy.env` as
`DEPLOY_API_KEY=…`, or in the environment variable of the same name.

## CLI commands

| Command | What it does |
|---|---|
| `deploy create-project <name> --resource <r> [--rebind] --override-dest <url>` | Register a project on an instance and bind it to an auth-center resource. Required before the first deploy. |
| `deploy run <config.qc>…` | Deploy. Several config files run serially; the first failure stops the rest. |
| `deploy preview <config.qc>` | Ask the server what would be uploaded and what server-side files would be deleted. |
| `deploy preview-deploy-files <config.qc>` | List the local files a deploy would include. Does not contact the server. |
| `deploy history <config.qc> [--limit N]` | Deployment history and which one is active. |
| `deploy check-deployed-commit <config.qc> [--deploy-name N] [--json]` | Which git commit the server is running (needs `track-git-commit`). |
| `deploy rollback <config.qc> [deploy-name] [--limit N]` | Activate an earlier deployment. Omit the name to choose from a list. |
| `deploy copy-back <config.qc> <path>` | Copy a file from the server's active deployment back to the local tree. |
| `deploy sql <config.qc> <query> [--database P] [--json]` | Run SQL against a database declared in the config. |
| `deploy list-databases <config.qc>` | The databases registered for a project, with their tables. |
| `deploy start-services` / `deploy preview-start-services` | Runs on a deploy host: `candle check-start` in every active deployment that declares a `candle-config`. Brings services back after a reboot. |

Every command that talks to a server takes `--override-dest <url>` to override
the config's `dest-url`.

Server-side commands are in [docs/server-setup.md](docs/server-setup.md):
`deploy-server serve` and `set-deployments-dir`. There is no key-management
subcommand — keys live in auth-center, and the server has no local key table.

## A minimal config

`.qc` is the project definition format; it is unchanged from the old tool, and
existing files keep working.

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

# Every path the server generates needs an explicit ignore, or an
# update-in-place deploy treats it as an orphan and deletes it.
ignore data
```

Then:

```bash
deploy create-project my-app --resource my-app-staging --override-dest https://apf1.dev
deploy preview my-app.qc
deploy run     my-app.qc
```

## Documentation

- [docs/getting-started.md](docs/getting-started.md) — first deployment, end to end.
- [docs/config-reference.md](docs/config-reference.md) — every `.qc` directive.
- [docs/client-server-api.md](docs/client-server-api.md) — the RPC surface.
- [docs/server-setup.md](docs/server-setup.md) — standing up an instance.
- [docs/project-goals.md](docs/project-goals.md) — what this is and why it exists (normative).
- [docs/auth-integration.md](docs/auth-integration.md) — the authorization model (normative).
- [CLAUDE.md](CLAUDE.md) — orientation for working in this repo.
