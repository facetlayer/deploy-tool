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
| `DEPLOY_AUTH_URL` | auth-center base URL, e.g. `https://auth.apf1.dev`. Never hardcoded. Unset ⇒ legacy-only, no network calls. |
| `DEPLOY_AUTH_KEY` | This instance's own auth-center service key, holding `auth:introspect`. Each instance gets its own so they revoke independently. |
| `DEPLOY_ADMIN_RESOURCE` | Per D2. Required when `DEPLOY_AUTH_URL` is set. |
| `DEPLOY_DISABLE_LEGACY_KEYS` | Set to `1` to turn off the local `secret_key` table on an instance whose keys have all migrated (R6). |

All live in the instance's `EnvironmentFile` (`/root/secrets/deploy.env`),
`0600` root-owned — not in the unit file.

## Authorization flow (R2)

Every RPC, in order:

1. **Resolve the call to a project.** Methods carrying `projectName` use it
   directly. Methods carrying only `deployName` join
   `deployment.deploy_name → deployment.project_name`. `createProject` resolves
   to the instance administration resource instead.
2. **Look up the project's bound resource** (`project.resource_name`).
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

## Legacy keys (R6)

The local `secret_key` table is checked first, so it costs no network call, and
grants everything. It is disabled per instance with
`DEPLOY_DISABLE_LEGACY_KEYS=1`. `deploy-server list-legacy-keys` reports what
remains (id, created-at, last-seen) so a migration can actually be finished.

## Attribution (R7)

`deployment.authorized_by_key_id` and `authorized_by_key_name` record the
auth-center key that authorized the deployment (or `legacy:<id>` for a local
key). `deploy history` surfaces it.

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

Per the requirements doc, unchanged:

1. Land the resource model with `DEPLOY_AUTH_URL` unset everywhere. No
   behavior change.
2. Register resources and issue keys in auth-center.
3. Enable on do2 with the legacy table still active; confirm real deploys work.
4. Enable on dohl only after do2 has run clean for a while.
5. Disable the legacy table per instance once its keys are migrated.

Do not enable `DEPLOY_AUTH_URL` on any instance still running the old server —
the interim code's `allowed ?? true` makes that strictly worse than legacy-only.
