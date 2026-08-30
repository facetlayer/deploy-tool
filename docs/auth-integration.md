# auth-center integration design

How `deploy-server` authenticates and authorizes callers against
[auth-center](../../auth-center). Normative source:
`~/auth-center/docs/deploy-service-requirements.md` (R1–R7). This document
records the decisions that doc left open, and the one requirement (R1's
resource-existence check) that turned out not to be implementable, with what
replaces it.

## What auth-center actually offers today

Surveyed 2026-08-29 against `~/auth-center/backend/src/api.rs`. The
service-to-service surface is:

```
GET  /api/v1/whoami
POST /api/v1/check       {"scope": "..."}
POST /api/v1/introspect  {"token": "...", "scope": "..."}   # optional scope
GET  /api/v1/secrets[/*path]
PUT  /api/v1/secrets/*path
```

`POST /api/v1/introspect` requires the caller's own key to hold
`auth:introspect`, and answers:

```json
{"active": true, "token_type": "api_key", "key_id": "...", "project": "...",
 "name": "...", "service": "...", "scopes": [...], "roles": [...],
 "allowed": true, "scope": "<the scope asked about>"}
```

`allowed`/`scope` appear **only** when the request carried a `scope`. An
unknown or revoked token gets `{"active": false}`. That distinction is what
lets a denial tell an active key which scope it lacks while telling an unknown
one nothing — see "What a denial tells the caller".

Admin mutations (creating projects, roles and keys) are not on this surface at
all. They go through `auth-setup` and a human approval; see "Creating projects,
roles and keys" below.

**Resources are derived, not declared.** There is no resources table, no
registration endpoint, and no service-to-service way to look one up. A resource
exists once some key, role or secret names it. The only listing is
`/admin/api/resources`, gated on `auth:admin` — a scope that can mint any key
in any project, which a deploy instance must never hold. Authorization itself is
a single flat scope string matched against the key's granted patterns, where `*`
matches one segment (not `:` or `/`) and `**` matches everything.

The requirements doc's resource model therefore has to be built *on top of*
flat scope strings. Decisions D1 and D2 below are how.

## D1 — A scope is `<resource>:<action>`

```
<resource>:<action>
```

**Exactly two segments, the action last.** auth-center's `validate_scope`
(`~/auth-center/backend/src/scopes.rs`) enforces it and refuses a third
segment; `secrets:<path>:<action>` is the sole exception, and it is not ours.

The five actions from R3, and the methods each covers (generated from
`METHOD_TABLE` in `crates/deploy-core/src/rpc.rs`, which is what the server
actually authorizes against):

| Action | Methods |
|---|---|
| `deploy` | `createDeployment`, `addManifestFiles`, `finalizeManifest`, `getNeededFiles`, `uploadOneFile`, `startMultiPartUpload`, `uploadFilePart`, `finishMultiPartUpload`, `finishUploads`, `verifyDeployment`, `activateDeployment` |
| `read` | `listDeployments`, `getDeploymentTags`, `previewDeployment`, `previewByDeployName`, `downloadFile`, `listDatabases` |
| `execute-sql` | `executeSql` |
| `rollback` | `rollback` |
| `create-project` | `createProject` (instance administration) |

Examples:

- `hotlaps-api-staging:deploy` — a CI key that ships to staging.
- `hotlaps-api-prod:read` — an on-call key that can only look.
- `hotlaps-api-staging:*` — everything on the staging resource, including
  `execute-sql`. Note `*` does not cross `:`, so this grants every action on
  that one resource and cannot reach another.
- `**` — a laptop key for every resource everywhere. Issue sparingly.

R3's "`execute-sql` must be grantable independently" falls out of auth-center's
existing matcher with no server-side change: a key granted
`hotlaps-api-staging:deploy` does not match `hotlaps-api-staging:execute-sql`.

**This format is a cross-repo contract.** Keys must be minted with exactly
these strings, or every deploy is denied. Do not write them by hand — run
`deploy auth-scopes <config.qc> --resource <name>` and paste what it prints.

### Two ways to get this wrong, one of which auth-center cannot catch

There is no `deploy:` namespace to put grouping in. Grouping goes into the
resource *name*: `do2-deploy`, not `deploy:do2`.

1. **Three segments.** `deploy:hotlaps-api-staging:deploy` is rejected at write
   time, so no such key can be minted. That much is safe. What is not safe is
   the rejection's *suggested fix*: `validate_scope` joins all but the last
   segment with `-` and proposes `deploy-hotlaps-api-staging:deploy`. That is
   valid, mints cleanly, and names a resource this server will never ask about.
   Take the suggestion and you get a key that is denied on every call, with a
   scope string that looks right at a glance.
2. **The reversed two-segment form.** `deploy:hotlaps-api-staging` is
   structurally valid — resource `deploy`, action `hotlaps-api-staging`. It
   simply means the wrong thing, and auth-center has no way to detect that. The
   ordering is our responsibility, which is the other reason to generate the
   string rather than type it.

`--resource` values containing `:` are rejected by both the CLI and the server,
so the three-segment scope cannot be produced from a resource name here.

### Why not `deploy:<project>`

That was the interim format, and it is what R1 and defect 2 exist to kill: the
project name is client-supplied and identical for `hotlaps-api` staging and
production. The resource is server-side and per instance, so the same project
name maps to `hotlaps-api-staging` on do2 and `hotlaps-api-prod` on dohl.

## D2 — Instance administration resource

`create-project` is checked against a resource naming the *instance*, not any
project. It is configured per instance:

| Variable | Meaning |
|---|---|
| `DEPLOY_ADMIN_RESOURCE` | Resource name for this instance's administration, e.g. `do2-deploy`. Required. |

So registering a project on do2 requires a key holding
`do2-deploy:create-project`. There is deliberately no default and no derivation
from the hostname — an instance that forgets to set this refuses `createProject`
rather than falling back to something guessable.

`DEPLOY_ADMIN_RESOURCE` is a **deliberate addition** to the requirements' config
table, which omits it. `createProject` resolves to no project, so without this
variable there is nothing to authorize it against; deriving it from a hostname
would make the check guessable, which defeats it.

The `-deploy` suffix is not decoration — see "Reserved names" below.

## Project topology in auth-center

A resource name is global across auth-center projects, and is owned by the
project that first declares one. The layout:

- **One "Server Admin" project** owns the deploy instances' administration
  resources: `do2-deploy` and `dohl-deploy`. Its keys hold
  `do2-deploy:create-project` (and `dohl-deploy:create-project`) — the keys that
  register projects on an instance, and nothing else.
- **Each application is its own auth-center project**, owning its own deploy
  resources: `hotlaps-api-staging`, `hotlaps-api-prod`, and so on. Resources
  are per-project-*per-environment*, so a compromised frontend CI key cannot
  touch the API, and a staging key cannot touch production.
- **The binding project → resource lives in each deploy instance's own
  database**, not in auth-center. That is what separates staging from
  production: do2's `hotlaps-api` binds to `hotlaps-api-staging`, dohl's
  `hotlaps-api` binds to `hotlaps-api-prod`. Same project name, same `.qc`
  file, different resource, different keys.

### Reserved names

Because the namespace is global, two families of name must not be claimed as
deploy resources:

- **Bare `hotlaps`** (and any other application's bare name). That is where
  `hotlaps:admin` lives — the SSO client's `required_scope`. Claiming it in
  another project would collide with the thing that gates sign-in.
- **Bare `do2` / `dohl`.** Those name the hosts, not their deploy services;
  hence the `-deploy` suffix on the administration resources.

`auth-service check-conflicts`, run on the auth host, reports any resource name
owned by two projects. Run it after creating resources.

## Creating projects, roles and keys: `auth-setup`

`auth-setup` (`~/auth-center/setup-tool`, already on `PATH`) is how the
topology above gets created. It runs on a laptop **holding no credential**: it
proposes the change, prints an approval URL and a confirmation code, opens a
browser, and blocks until an admin signs in and approves. The change then runs
as that admin. It is RFC 8628's device authorization grant pointed at admin
mutations, which is why there is no `auth:admin` key on any developer machine.

```
auth-setup create-project <id> [--name <display>] [--description <text>]
auth-setup create-role <name> --project <id> [--scope <s>]… [--description <text>]
auth-setup create-key <name> --project <id> [--scope <s>]… [--role <r>]…
                      [--service <svc>] [--description <text>] [--expires-in-days <n>]

COMMON: --url <base> (default $AUTH_URL), --json, --no-open, --timeout <secs>
```

Three properties worth knowing:

- **Requests are validated when they are made**, before anyone is asked to
  approve. A malformed scope fails immediately and costs nobody an approval —
  which also makes `auth-setup create-role` a cheap way to check a scope
  string's shape against the live service, Ctrl-C before approving.
- **Requests expire after 15 minutes.** An unapproved request is not a pending
  change.
- **A created key's plaintext is shown once** and is wiped from the server the
  moment the tool collects it. There is no second chance to read it.

`deploy auth-scopes <config.qc> --resource <name>` prints ready-to-run
`auth-setup` lines for a project. It contacts no server and needs no API key —
everything it prints is derived from the config file and from `METHOD_TABLE`,
so what you paste is what the server will ask for, by construction.

The suggested role it prints grants only `deploy` and `read`. `execute-sql` and
`rollback` are printed separately, as a deliberate second step: R3 splits the
actions precisely so that a CI key which ships builds cannot run arbitrary SQL,
and a copy-paste line bundling them would make the split decorative.
`deploy create-project` prints the same block after registering.

## Configuration

| Variable | Meaning |
|---|---|
| `DEPLOY_AUTH_URL` | auth-center base URL, `https://$AUTH_HOST`. **Required** — there is no legacy fallback, so an instance without it cannot authenticate anyone. Never hardcoded in the binary, and never written into a doc: the hostname is a deployment detail. |
| `DEPLOY_AUTH_KEY` | This instance's own auth-center service key, holding `auth:introspect`. Each instance gets its own so they revoke independently. |
| `DEPLOY_ADMIN_RESOURCE` | Per D2. Required. |

All live in the instance's `EnvironmentFile` (`/root/secrets/deploy.env`),
`0600` root-owned — not in the unit file. The server refuses to start if any of
the three is missing, rather than starting in a half-configured state, and it
names the missing one.

`--disable-api-key-check` still exists for local development and for the test
suite, and a server started with it needs no auth-center configuration at all —
it never introspects anything, so requiring the three variables would be
friction with no security value. The bypass is explicit in that flag and is
never a consequence of configuration having gone missing, which is the property
the refusal above protects. It is not a migration path and must never be set on
do2 or dohl; the server logs a prominent warning at startup when it is on.

## Authorization flow (R2)

Every RPC, in order:

1. **Resolve the call to a project.** Methods carrying `projectName` use it
   directly. Methods carrying only `deployName` join
   `deployment.deploy_name → deployment.project_name`. `createProject` resolves
   to the instance administration resource instead.
2. **Look up the project's bound resource** (`project_resource_binding`).
3. **Introspect** the presented key against `<resource>:<action>`.
4. **Deny on any failure** — unknown deploy name, unregistered project, project
   with no bound resource, key lacking the action.

There is no path that allows a call because no resource could be determined.
Concretely, the interim `allowed ?? true` fallback is gone: a response without
an explicit `allowed: true` is a denial. Since we always send a scope, a
well-behaved auth-center always answers with `allowed`.

## What a denial tells the caller

The server's journal always gets the full reason. What goes back over the wire
in the JSON-RPC error's `data` depends on what the key turned out to be
(`Denial::client_detail` in `crates/deploy-server/src/authz.rs`):

- **An active key lacking the scope** is told which scope it lacks:
  `this key does not hold hotlaps-api-staging:deploy`. The CLI surfaces it as
  `JSON-RPC createDeployment denied: this key does not hold …`. That is the
  whole diagnostic: a caller with a real key sees the typo immediately, which
  is what replaces the registration-time existence check auth-center cannot
  offer.
- **An unknown, revoked or expired key** gets a bare `Unauthorized` with no
  detail at all. Naming the scope there would let anyone holding a random
  string enumerate this instance's project → resource bindings, one guess at a
  time.
- **Everything else** — no key presented, unknown method, unresolvable deploy
  name, unregistered or unbound project — also gets a bare `Unauthorized`.

The one deliberate exception is an unreachable auth-center, which is told to
the caller in coarse terms ("could not reach auth-center; see the server's
journal"). It is not a verdict about the caller and it names no binding, and it
is the failure an operator is most likely to meet during a cutover — where the
alternative is staring at a bare `Unauthorized` while the real cause sits in a
journal on a host they may not be able to read.

## Fail closed (R4)

Network error, timeout (5s), non-2xx, missing `active`, missing `allowed`, or
an unparseable body all deny, and log the reason. A deploy that fails because
auth-center is unreachable is correct; one that succeeds because it was
unreachable is not.

## Caching (R5)

Positive verdicts only, keyed by `sha256(key) | resource | action`, 30s TTL,
capped entry count. Negative verdicts are never cached, so revocation takes
effect within the positive-cache window at worst.

## No legacy keys (R6)

The old server authenticated against a local `secret_key` table: a flat list of
key strings with no owner, scope, expiry or audit trail, where every key could
do everything on the instance. **That table does not exist in this
implementation.** It is removed, not deprecated — there is no fallback path, no
per-instance flag to disable it, and therefore no bypass left behind. Every
caller authenticates against auth-center.

This is the single largest security win available in the rewrite, and it is
only available because backwards compatibility was dropped. An instance is no
longer only as strong as its weakest forgotten legacy key.

The consequence is that enabling this on an instance is a **cutover, not a
gradual migration**. See "Rollout" below.

Because compatibility is not a requirement, the schema is modelled properly
rather than bolted onto the old one. The resource binding lives in its own
table with its own history, rather than as a column on `project`:

- `project_resource_binding` — the current binding for each project.
- `project_resource_binding_history` — every bind and rebind, with the key that
  made the change. R1 requires this: rebinding a project to a resource the
  caller controls is a privilege-escalation path, so it is audited.

## Attribution (R7)

`deployment.authorized_by_key_id` and `authorized_by_key_name` record the
auth-center key that authorized the deployment. `deploy history` surfaces it.
Every key is an auth-center key, so there is no second attribution form.

## R1's resource-existence check: the answer is "check at first use"

R1 asks that `create-project` fail if auth-center does not recognize the named
resource, so a typo surfaces at registration rather than as a mass denial at
the next deploy. **It is not implementable, and the code that tried has been
removed** (`verify_resource_exists`, the `ResourceCheck` enum, and the
`resourceVerified` field on `createProject`). Two reasons, both structural:

- The only listing of resources is `/admin/api/resources`, gated on
  `auth:admin` — a scope that can mint any key in any project. A deploy
  instance holding that would be a far worse problem than the typo it was
  checking for. There is no service-to-service endpoint that can answer the
  question.
- Resources are *derived*, not declared. One exists as soon as some key, role
  or secret names it. At registration time the honest answer is usually "not
  yet" — the resource is created by the very `auth-setup create-role` /
  `create-key` calls that follow registration — so an existence check would
  reject the correct order of operations.

What replaces it:

1. **`create-project` refuses a `:` in `--resource`**, which is the one
   malformed shape that can be detected locally.
2. **It prints the exact `auth-setup` commands** for the resource it just
   bound, so the resource is created from a generated string rather than a
   typed one.
3. **The first denied call names the scope that was checked** — see "What a
   denial tells the caller". A typo shows up as
   `this key does not hold hotlpas-api-staging:deploy`, which is legible
   without server access.

This is a deliberate decision, not an outstanding gap. Nothing in the
authorization path is stubbed.

## Rollout

There is no legacy fallback, so enabling this on an instance is a cutover.
Per instance:

1. Land the resource model and the resolution path.
2. Create the projects, roles and keys with `auth-setup`, including the
   instance's own `auth:introspect` service key. Use
   `deploy auth-scopes <config.qc> --resource <name>` to generate the commands,
   and `auth-service check-conflicts` afterwards to confirm no resource name is
   owned by two projects.
3. Register every existing project on the instance against its resource, and
   distribute the new keys to every caller that needs one — CI secrets,
   `~/secrets/deploy.env`, and so on. Do this **before** the cutover; a caller
   holding no valid key at cutover simply stops being able to deploy.
4. Cut over: rebuild or migrate the database, set the three environment
   variables, restart.
5. Verify with a real deploy of a low-stakes project before relying on it.

do2 first, and let it run clean for a while before cutting dohl over.
Production is under no deadline.

### Rebuilding the database discards live state

The deploy database holds operational state as well as key material:
`active_deployment` records which deployment is currently serving each project,
and the `deployment` rows describe what is on disk. Rebuilding discards that.
Before rebuilding an instance, either redeploy every project on it so the state
regenerates, or do a one-off import of `project`, `deployment` and
`active_deployment` from the old database. The key material is the part being
thrown away deliberately; the deployment bookkeeping is not.

### The circular dependency, which is real

Fail-closed (R4) is correct, but it means auth-center downtime is deploy
downtime for every instance pointed at it — and **auth-center is itself
deployed by this tool**. That is a genuine deadlock, not a hypothetical: if
auth-center is down and the fix is to deploy auth-center, there is no way in.

Two things must be ready before any cutover, since there is no fallback to
catch a mistake:

- A tested way back in. Decide deliberately whether auth-center's own deploys
  stay on a separate path so the two cannot deadlock. `--disable-api-key-check`
  behind an SSH-only restart is one answer, but it has to be a decision that
  was made and tested, not one discovered during an outage.
- Confirmation that every caller has a working key. Step 3's distribution is
  the step most likely to be left incompletely done.
