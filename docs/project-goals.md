# deploy-tool — project goals

Written 2026-08-29, at the start of the rewrite.

## What this is

A deployment service and its companion CLI. A deployment is: upload a set of
files to a server, then trigger server-side actions (restarts, SQL migrations,
static-site swaps). This repo replaces the existing tool at `~/tools/deploy`.

Two artifacts ship from here:

- **`deploy-server`** — the backend. One instance per host. Owns a deployments
  directory on disk and a SQLite database, serves the RPC surface the CLI
  calls, and performs the activation actions.
- **`deploy`** — the CLI. Reads a project's `.qc` file, computes the file set,
  uploads what the server does not already have, and drives the deployment
  through to activation.

## Why rewrite rather than extend

Three reasons, in order of weight:

1. **Auth.** The current server authenticates against a local `secret_key`
   table: a flat list of key strings with no owner, scope, expiry or audit
   trail. Every key can do everything on the instance. The interim
   auth-service integration that exists today has a fallback
   (`allowed ?? true`) that makes the entire file-upload and activation path
   effectively unguarded. That has to go, and it is not a patch — it is a
   different authorization model.
2. **One language.** The current tool is split: a Rust server under `rust/`
   and a TypeScript CLI under `src/`. The RPC types, the `.qc` parser and the
   file-list logic exist in both, and drift between them. Everything here is
   Rust, with the shared pieces in one crate used by both binaries.
3. **Staging and production are not distinguishable today.** See below.

## Goal 1 — everything in Rust

A Cargo workspace. No TypeScript, no Node in the build or the runtime.

- A shared crate holding the RPC types, the `.qc` parser, and the file-list /
  hashing logic — defined once, used by both binaries.
- The `.qc` parser is copied into this repo (the existing Rust implementation
  under `~/tools/deploy/rust/src/qc/` is the starting point). `.qc` stays the
  project definition format; the file format itself is not changing.
- The CLI is a Rust binary, distributable as a single static-ish executable,
  with no `node_modules` on the deploying machine.

## Goal 2 — tight integration with auth-center

`~/auth-center` is the account and permission service. The deploy service
becomes a client of it, per
`~/auth-center/docs/deploy-service-requirements.md`, which is the normative
spec for this part. In summary:

- **A project is bound to a named auth-center resource**, stored server-side
  in the deploy instance's own database. The binding is never derived from a
  naming convention and never taken from the client.
- **Projects are created explicitly**: `deploy create-project <name>
  --resource <resourceName>`. This is a behavior change from the old tool,
  where a project came into existence implicitly on first deploy. Creating a
  project requires a key holding `create-project` on the instance's
  administration resource, and registration fails if auth-center does not
  recognize the named resource — so a typo surfaces at registration, not as a
  mass denial at the next deploy.
- **Every authenticated call resolves to a concrete resource and is checked
  against it.** Calls carrying only a deploy name resolve through the
  deployment table to their project. A call that cannot be resolved to a
  resource is *denied*, never allowed.
- **Actions are distinguished**, not collapsed into "can deploy": at minimum
  `deploy`, `read`, `sql`, `rollback`, and `create-project`. `sql` in
  particular must be grantable independently — a CI key that ships builds
  should not be able to run arbitrary SQL.
- **Fail closed.** Network error, timeout, non-2xx or a malformed
  introspection response all deny.
- **Cache positive results briefly**, keyed by key hash + resource + action,
  so one deploy does not make one introspection call per uploaded file. Never
  cache negative results.
- **Record who authorized each deployment**, so `list-deployments` can answer
  "who shipped this".

### The staging/production problem this solves

`hotlaps/deploy/api-staging.qc` and `api-prod.qc` both declare
`project-name=hotlaps-api`; they differ only in `dest-url`. Any scheme
deriving permission from the project name grants both identically. Because the
resource binding lives in each deploy instance's own database, do2 and dohl can
require different resources for the same project name:

| Instance | Project | Required resource |
|---|---|---|
| do2 | `hotlaps-api` | `hotlaps-staging` |
| dohl | `hotlaps-api` | `hotlaps-prod` |

A staging key presented to dohl is checked against `hotlaps-prod`, does not
hold it, and is denied.

## Goal 3 — same deployment flow

The flow the CLI drives is unchanged, and existing `.qc` files keep working:

1. Read the `.qc` file; resolve include/exclude globs into a file list.
2. Run `before-deploy` actions locally.
3. Create a deployment; ask the server which files it does not already have
   (content-hash dedup); upload only those, single-shot or multipart.
4. Build and finalize the manifest; verify.
5. Activate — swap the live directory, run `after-deploy` server actions.
6. Rollback remains available as a distinct action.

## Goal 4 — server instance setup

Setting up an instance configures:

- the **deployments directory** on disk (as before), and
- the **auth-center URL**, plus this instance's own service key.

The auth-center URL is configuration, not a constant. It is usually
`https://auth.apf1.dev`, but it is **never hardcoded** — do2 and dohl each get
their own setting and their own service key, so the keys can be revoked
independently. Both live in the instance's environment file, not in the unit
file.

There is deliberately no "environment" or "server name" setting. The
staging/production distinction is carried entirely by the per-project resource
binding.

## Goal 5 — replace the old tool

The end state is the old service and CLI retired, and this one installed on
**do2** and **dohl** (see `~/biz/do2` and `~/biz/dohl`). Rollout follows the
staged plan in the requirements doc: land the resource model with auth
disabled, register resources and issue keys, enable on do2 first with the
legacy key table still active as a fallback, then dohl, then disable the
legacy table per instance once its keys are migrated.

The legacy `secret_key` table therefore stays supported during migration, but
must be disableable per instance, and the CLI needs a way to list what legacy
keys remain so the migration can actually be finished.

## Current constraint

auth-center is still being built. Work proceeds against the documented
contract as far as it can go; the parts that need a real running auth service
to verify will block until one exists.
