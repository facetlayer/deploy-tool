# Implementation plan — aligning with the deployed auth service

Written after `~/auth-center/docs/deploy-service-requirements.md` was revised
(2026-08-29) following the auth service's deployment and the settling of the
scope grammar. The workspace is otherwise complete: `cargo test --workspace` is
green at 267 tests and the whole flow has been exercised against the real
binaries. This plan covers only the delta.

## What changed

Three things, in descending order of impact.

### 1. The scope grammar settled, and what we emit is now invalid

A scope is `<resource>:<action>` — **exactly two segments, action last**
(auth-service commit `ec2d4a0`). `validate_scope` enforces it, with
`secrets:<path>:<action>` the sole three-segment exception.

This repo currently emits `deploy:<resource>:<action>`. That was decision D1 in
`docs/auth-integration.md`, made when the auth service had no resource model at
all and a namespace looked like the safe way to avoid collisions. The revised
requirements name that exact shape as one of two invalid forms: it parses as a
resource literally named `deploy:hotlaps-staging`, which nothing else
understands, and the service rejects it at write time.

So no key can be minted that our server would ever accept. **This gates
everything else** — nothing can be tested against the live service until it is
fixed.

The other invalid shape is worth remembering because it *does* validate:
`deploy:hotlaps-staging` (the old interim form) is structurally fine and simply
means the wrong thing — resource `deploy`, action `hotlaps-staging`. The
service cannot catch it. Correctness here is our responsibility, not the
validator's.

### 2. R1's resource-existence check has a decided answer, and it is not ours

The requirements now state plainly that the check is **not currently
implementable**: resources are listed only by `GET /admin/api/resources`, which
needs `auth:admin` — a scope that can drive the entire dashboard API including
minting keys. The doc's words: giving a deploy instance that scope to
spell-check a string is a trade not to make.

There is also a deeper reason. Resources are *implied*, not declared: one
exists once some key, role or secret names it. At registration time the binding
is normally set up before the keys are issued, so "does `hotlaps-staging`
exist?" correctly answers "not yet".

We currently call a speculative `GET /api/v1/resources/<name>` that does not
exist, and warn when it 404s. That endpoint is not coming without auth-service
work, so the call is dead weight that prints a warning on every registration.

### 3. The auth-service project topology is decided

Confirmed for this integration:

- **One "Server Admin" auth-service project** owns the deploy instances'
  administration resources and the keys that create deployment targets.
- **Each app is its own auth-service project**, declaring its own deploy
  resources.

This also resolves one of the doc's open questions. The naming
`hotlaps-api-staging` (rather than a shared `hotlaps-staging` covering both api
and frontend) means resources are **per project per environment**, not shared
within an environment — so a compromised frontend CI key cannot touch the API.

## Decisions settled

| | Decision |
|---|---|
| Scope format | `<resource>:<action>`, two segments, action last |
| Actions | `deploy`, `read`, `execute-sql`, `rollback`, `create-project` |
| Admin resource | `do2-deploy`, `dohl-deploy` — suffixed, so the bare hostname stays free |
| Resource granularity | Per project per environment (`hotlaps-api-staging`) |
| R1 existence check | Check at first use, not at registration |

`execute-sql` rather than the doc's `sql`: the auth service has no vocabulary of
known actions, so any last segment works, and `execute-sql` reads unambiguously
in the dashboard's resource inventory.

### The intended topology, concretely

Auth-service project **Server Admin**:

```
resources:  do2-deploy, dohl-deploy
key:        andy-laptop-admin  → do2-deploy:create-project
                                 dohl-deploy:create-project
```

Auth-service project **hotlaps**:

```
resources:  hotlaps-api-staging, hotlaps-api-prod,
            hotlaps-frontend-staging, hotlaps-frontend-prod
role:       deployer  → hotlaps-api-staging:deploy
                        hotlaps-frontend-staging:deploy
keys:       hotlaps-ci   (service github-actions) → role deployer
            andy-laptop                           → role deployer
```

Bindings stored in each deploy instance's own database:

| Instance | Deploy project | Bound resource |
|---|---|---|
| do2 | `hotlaps-api` | `hotlaps-api-staging` |
| dohl | `hotlaps-api` | `hotlaps-api-prod` |

Two names the deploy side must **not** claim, per the uniqueness rule:

- Bare `hotlaps` — that is where `hotlaps:admin` lives, the planned
  `required_scope` for the hotlaps admin console's SSO client.
- Bare `do2` / `dohl` — hence the `-deploy` suffix.

Resource names are globally unique across auth-service projects, and a name is
owned by the project that first declares it. Declaring `hotlaps-api-staging`
from both project `hotlaps` and project `auth` is refused.

## Work items, in dependency order

### W1 — Emit `<resource>:<action>` (gates everything)

- `deploy_core::rpc::scope_string`: drop the `deploy:` prefix.
- `Action::Sql.as_str()`: `"sql"` → `"execute-sql"`. Rename the variant to
  `ExecuteSql` so the code reads the way the wire does.
- Reject a `--resource` containing `:` at registration, in both the CLI and the
  server. A colon there silently produces a three-segment scope that the auth
  service will refuse for every key — cheap to catch, expensive to debug.
- Update the ~25 test assertions and the four docs that pin the old form.

### W2 — Drop the speculative existence check; report the resource on denial

- Delete `AuthCenter::verify_resource_exists` and the `resourceVerified` field
  it feeds. It calls an endpoint that does not exist and warns on every
  registration.
- Make a denial name the scope it checked, all the way out to the CLI. This is
  the doc's "check at first use": a typo shows up as
  `denied: key does not hold hotlaps-api-stagng:deploy` rather than a bare 401,
  which makes it self-diagnosing without any new API surface.
- `create-project` output should state the scopes callers will now need, and in
  which auth-service project to declare them.

### W3 — Configuration and docs

- Keep `DEPLOY_ADMIN_RESOURCE`. The doc's configuration table lists only
  `DEPLOY_AUTH_URL` and `DEPLOY_AUTH_KEY`, but `create-project` cannot be
  authorized without knowing this instance's administration resource, and the
  requirements are explicit that it must never be derived from a hostname. This
  is a deliberate addition, recorded here rather than left implicit.
- Rewrite decision D1 in `docs/auth-integration.md`: the format, why the
  three-segment form was wrong, and the fact that the reversed two-segment form
  validates but silently means the wrong thing.
- Add the topology above to the docs, with the reserved-name warnings.
- `docs/server-setup.md`: fold in `auth-service check-conflicts`, to be run on
  the auth host after issuing keys.

### W4 — Verify against the live auth service

Newly unblocked: the service is deployed and do2's service key already exists
(`deploy-server-do2`, key id `b11b8ad7b30d2f73`). Until now everything has been
tested against a stub.

- Point a local `deploy-server` at the real auth service and confirm a real key
  is accepted for `deploy` and denied for `execute-sql`.
- Confirm the three documented misconfiguration responses behave as specified:
  `401` (bad `DEPLOY_AUTH_KEY`), `403` (key lacks `auth:introspect`), and
  `{"active": false}` for an unknown subject token — the first two deny *and*
  log loudly, because they mean the instance is broken rather than the caller.

This needs the actual key material, which lives in `/root/secrets/` on do2.

### W5 — Cutover prerequisites (not code)

Both are called out in the requirements as things to settle before any cutover,
and neither is resolved yet:

- **The circular dependency.** Fail-closed means auth-service downtime is
  deploy downtime, and the auth service is itself deployed by this tool. Decide
  deliberately whether its own deploys stay on a separate path.
- **Key distribution.** Step 3 of the rollout is the step most likely to be
  left half-done, and there is no fallback to catch it.

### W6 — Tooling for creating scopes and keys

How this is done today, on the auth host:

```bash
auth-service create-project server-admin --name 'Server Admin'
auth-service create-key --project server-admin --name andy-laptop-admin \
  --scope do2-deploy:create-project --scope dohl-deploy:create-project
```

`create-key` prints the `ak_…` secret once and never again. The full CLI is
`serve`, `gen-secrets-key`, `create-admin`, `create-project`, `create-key`,
`list-keys`, `revoke-key`, `check-conflicts`.

Two gaps make the topology above tedious to stand up:

- **There is no `create-role` command.** Roles exist only through the dashboard
  (`PUT /admin/api/roles/:project/:name`), so the tidy way to bundle scopes is
  the one way you cannot script.
- **Resources cannot be created at all**, by design — a resource exists once
  something names it. So there is nothing to pre-declare, and a typo in a scope
  string silently creates a *new* resource rather than failing.

That second point is the real hazard. `hotlaps-api-stagng:deploy` is a
perfectly valid scope naming a resource that now exists and that nothing will
ever check against. Nothing catches it at write time; it surfaces as a deploy
that is denied for no visible reason.

Three tiers of tooling, in the order worth building them.

**Tier 1 — generate the strings, never hand-write them.** No new privilege, no
auth-service work, useful immediately.

- `deploy auth-scopes <config.qc>` prints exactly the scopes a given `.qc`
  needs, derived from the same `METHOD_TABLE` the server authorizes against —
  so what you paste into the dashboard is what the server will ask for, by
  construction.
- `deploy create-project` ends by printing the ready-to-run `auth-service
  create-key` command and the dashboard steps for the role, naming which
  auth-service project to declare them in.

This closes the typo hazard for the common path, because the string is copied
rather than retyped.

**Tier 2 — make the topology declarative.** Small auth-service additions:
`create-role`, and a `bootstrap --from-file` taking a YAML/JSON description of
projects, roles, keys and their scopes. The topology then lives in git, is
reviewable, and is reproducible on a rebuilt auth host. This is where the
lasting value is, and it is auth-service work rather than deploy work.

**Tier 3 — drive the admin API from the deploy CLI.** A `deploy auth create-key`
that calls `POST /admin/api/keys` directly. Convenient, and a real privilege
trade: it needs `auth:admin`, which can mint any key in any project. The
requirements are explicit that a deploy *instance* must never hold that scope.
A human's laptop key is a different question, but it is still a credential that
can grant anything, so it should be a deliberate choice rather than a
convenience that arrives with a subcommand.

Recommendation: build Tier 1 as part of this work, propose Tier 2 to
auth-center, and leave Tier 3 alone unless the manual path proves genuinely
painful.

## Out of scope

- Adding `GET /api/v1/resources` behind an `auth:list-resources` scope. That is
  the only option that actually satisfies R1, but it is auth-service work.
- Any change to `~/tools/deploy`. R6 drops legacy compatibility, so the interim
  introspection path goes away with the old tool rather than being fixed.
