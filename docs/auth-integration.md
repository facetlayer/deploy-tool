# auth-center integration design

How `deploy-server` authenticates and authorizes callers against
[auth-center](../../auth-center). Normative source:
`~/auth-center/docs/deploy-service-requirements.md` (R1–R7). This document
records the decisions that doc left open, and the gaps that are blocked on
auth-center work.

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
unknown or revoked token gets `{"active": false}`.

**There is no resource entity in auth-center.** No resources table, no
registration endpoint, no way to list or look up a resource. Authorization is a
single flat scope string matched against the key's granted patterns, where `*`
matches one segment (not `:` or `/`) and `**` matches everything.

The requirements doc's resource model therefore has to be built *on top of*
flat scope strings. Decisions D1 and D2 below are how.

## D1 — Resource and action encode into one scope string

```
deploy:<resource>:<action>
```

The five actions from R3:

| Action | Methods |
|---|---|
| `deploy` | `createDeployment`, `addManifestFiles`, `finalizeManifest`, `getNeededFiles`, `uploadOneFile`, `startMultiPartUpload`, `uploadFilePart`, `finishMultiPartUpload`, `finishUploads`, `verifyDeployment`, `activateDeployment` |
| `read` | `listDeployments`, `getDeploymentTags`, `previewDeployment`, `previewByDeployName`, `downloadFile`, `listDatabases` |
| `sql` | `executeSql` |
| `rollback` | `rollback` |
| `create-project` | `createProject` (instance administration) |

Examples:

- `deploy:hotlaps-staging:deploy` — a CI key that ships to staging.
- `deploy:hotlaps-prod:read` — an on-call key that can only look.
- `deploy:hotlaps-staging:*` — everything on the staging resource, including
  `sql`. Note `*` does not cross `:`, so this does not grant another resource.
- `deploy:**` — a laptop key for every resource on every instance. Issue
  sparingly.

This layout means R3's "`sql` must be grantable independently" falls out of
auth-center's existing matcher with no server-side change: a key granted
`deploy:hotlaps-staging:deploy` does not match `deploy:hotlaps-staging:sql`.

**This format is a cross-repo contract.** Keys minted in the auth-center
dashboard must use exactly these strings, or every deploy is denied.

### Why not `deploy:<project>`

That is the interim format, and it is what R1 and defect 2 exist to kill: the
project name is client-supplied and identical for `hotlaps-api` staging and
production. The resource is server-side and per instance, so the same project
name maps to `hotlaps-staging` on do2 and `hotlaps-prod` on dohl.

## D2 — Instance administration resource

`create-project` is checked against a resource naming the *instance*, not any
project. It is configured per instance:

| Variable | Meaning |
|---|---|
| `DEPLOY_ADMIN_RESOURCE` | Resource name for this instance's administration, e.g. `deploy-do2`. Required whenever `DEPLOY_AUTH_URL` is set. |

So registering a project on do2 requires a key holding
`deploy:deploy-do2:create-project`. There is deliberately no default and no
derivation from the hostname — an instance that forgets to set this refuses
`createProject` rather than falling back to something guessable.

## Configuration

| Variable | Meaning |
|---|---|
| `DEPLOY_AUTH_URL` | auth-center base URL, e.g. `https://auth.apf1.dev`. **Required** — there is no legacy fallback, so an instance without it cannot authenticate anyone. Never hardcoded. |
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
3. **Introspect** the presented key against `deploy:<resource>:<action>`.
4. **Deny on any failure** — unknown deploy name, unregistered project, project
   with no bound resource, key lacking the action.

There is no path that allows a call because no resource could be determined.
Concretely, the interim `allowed ?? true` fallback is gone: a response without
an explicit `allowed: true` is a denial. Since we always send a scope, a
well-behaved auth-center always answers with `allowed`.

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

## Known gap — resource existence is not verified at registration

R1 asks that `create-project` fail if auth-center does not recognize the named
resource, so a typo surfaces at registration rather than as a mass denial at
the next deploy.

**auth-center has no endpoint that can answer this** — there is no resource
registry to query. `deploy-server` therefore attempts
`GET {DEPLOY_AUTH_URL}/api/v1/resources/<name>` and:

- `200` ⇒ verified, proceed.
- `404` on the *resource* ⇒ reject the registration.
- `404` because the *endpoint* does not exist, or any other error ⇒ log a
  loud warning that the resource could not be verified, and proceed.

Until auth-center grows a resource registry, every registration takes the third
branch. This is the one requirement in the doc that cannot be satisfied today;
it is not a stub in the authorization path, only in the typo check.

## Rollout

There is no legacy fallback, so enabling this on an instance is a cutover.
Per instance:

1. Land the resource model and the resolution path.
2. Create the resources and issue keys in auth-center, including the instance's
   own `auth:introspect` service key.
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
