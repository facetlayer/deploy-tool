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

**`auth-setup`** is the answer, and it is already built and on PATH
(`~/.cargo/bin/auth-setup`, workspace member `~/auth-center/setup-tool`):

```bash
export AUTH_URL=https://$AUTH_HOST
auth-setup create-project server-admin --name 'Server Admin'
auth-setup create-role deployer --project server-admin \
    --scope do2-deploy:create-project --scope dohl-deploy:create-project
auth-setup create-key andy-laptop-admin --project server-admin --role deployer
```

It holds **no credential**. Each command posts the proposed change, prints an
approval URL and a confirmation code, opens the browser, and blocks; an admin
signs in with their own session, types the code, and the change runs *as them* —
the audit log and the key's `created_by` name the human, not the tool. This is
RFC 8628's device authorization grant pointed at admin mutations, and it is
strictly better than the `auth:admin` laptop key this plan previously
contemplated.

Details that matter for scripting it:

- `--json` puts one result object on stdout and the approval link on stderr.
  That is the form to use from an agent or a script.
- `--no-open` skips launching a browser; `--timeout` overrides the 900s wait.
- Requests are validated when made, so a bad scope or unknown project fails
  immediately rather than after a wasted trip to the browser.
- Requests expire after 15 minutes, and a `create-key` plaintext is wiped the
  moment the tool collects it. A tool that dies in that window has lost the
  key — revoke it and ask again.
- Anything but approval exits non-zero.

**It is live and usable now.** `auth-setup` is a *client* that runs on a laptop;
only the server half ever ships to do2, and it has — `auth-service-api` build
407 from auth-center `bf6dd5e`. Verified rather than assumed:

```
$ curl -s -X POST https://$AUTH_HOST/setup/api/requests \
    -H 'content-type: application/json' \
    -d '{"kind":"create-role","project":"__nope__","name":"x","scopes":[],
         "token":"rq_00000000000000000000000000000000"}'
HTTP 400  {"error":"unknown project \"__nope__\""}
```

A backend validation error means the route reaches the backend. A 404 with
"File not found" would have meant nginx never got there — which is exactly what
happened at first: the vhost proxies an explicit prefix list, so `/setup` had to
be added to the location regex in `/etc/nginx/sites-enabled/$AUTH_HOST` before
it worked. That is worth remembering for any future path this service adds; a
deploy alone is not enough.

`~/auth-center/docs/handoff.md` still says "Not yet deployed". It is stale.

One gap remains:

- **Resources cannot be created at all**, by design — a resource exists once
  something names it. So there is nothing to pre-declare, and a typo in a scope
  string silently creates a *new* resource rather than failing.

That is the real hazard. `hotlaps-api-stagng:deploy` is a perfectly valid
scope naming a resource that now exists and that nothing will ever check
against. Nothing catches it at write time; it surfaces as a deploy
that is denied for no visible reason.

Two tiers of tooling, in the order worth building them.

**Tier 1 — generate the strings, never hand-write them.** No new privilege, no
auth-service work, useful immediately.

- `deploy auth-scopes <config.qc>` prints exactly the scopes a given `.qc`
  needs, derived from the same `METHOD_TABLE` the server authorizes against —
  so what you paste into the dashboard is what the server will ask for, by
  construction.
- `deploy create-project` ends by printing the ready-to-run `auth-setup
  create-role` and `auth-setup create-key` commands, naming which auth-service
  project to declare them in.

This closes the typo hazard for the common path, because the string is copied
rather than retyped.

**Tier 2 — drive the admin API from the deploy CLI.** A `deploy auth create-key`
that calls `POST /admin/api/keys` directly. Convenient, and a real privilege
trade: it needs `auth:admin`, which can mint any key in any project. The
requirements are explicit that a deploy *instance* must never hold that scope.
A human's laptop key is a different question, but it is still a credential that
can grant anything, so it should be a deliberate choice rather than a
convenience that arrives with a subcommand. `auth-setup` does the same job
without the privilege, which is the argument for not building this at all.

Recommendation: build Tier 1 as part of this work, use `auth-setup` for the
rest, and leave Tier 2 alone unless the approval flow proves genuinely
painful.

## Next steps, in order

Steps 1–4 are this repo and can proceed now. Step 5 is a different repo and can
run in parallel. Steps 6–8 depend on both.

### 1. Fix the scope grammar (W1) — nothing can be tested until this lands

34 sites across `rpc.rs`, `authz.rs`, `auth_center.rs`, `main.rs` and the two
integration test files. One production function, one constant, the rest
assertions and fixtures. Note that the test fixtures use `deploy:**` and
`deploy:*:create-project`, which are three-segment and equally invalid; they
become `**` and `*:create-project`.

### 2. Drop the speculative resource check (W2)

Now settled by evidence rather than assumption: the survey confirms there is
**no resources table**, resources are a derived read-only view assembled per
request from scopes on keys, roles, secrets and usage rows, and they are exposed
only under `/admin/api`, gated on `auth:admin`. The narrow
`auth:list-resources` endpoint is a proposal in a requirements doc, not code.

So delete `verify_resource_exists`, and make denials name the scope they
checked. That is the doc's "check at first use", and it is now the only option
that does not over-privilege the instance.

### 3. Docs and configuration (W3)

### 4. Tier-1 tooling (W6)

`deploy auth-scopes <config.qc>` prints the scopes a config needs, derived from
`METHOD_TABLE`. `deploy create-project` ends by printing ready-to-run
`auth-setup create-role` / `create-key` commands.

This matters more than it looks, for a reason the survey turned up: when
`validate_scope` rejects a three-segment scope it **auto-suggests a fix** by
joining all but the last segment with `-`. So `deploy:hotlaps-staging:deploy` is
rejected with the suggestion `deploy-hotlaps-staging:deploy` — which is valid,
and names a resource that is *not* the one the deploy server will ask about.
Anyone following that suggestion gets a key that validates, mints cleanly, and
is denied on every call. Generating the string removes the chance to take that
bait.

`auth-setup` validates a request when it is *made*, not at approval, so a bad
scope or an unknown project comes back immediately with the real error and
costs nobody a trip to the browser. That makes it a cheap way to check a scope
string before anyone is asked to approve anything — though it still cannot
answer "does this resource exist", since nothing can (step 2).

### 5. Prerequisites — DONE

Nothing is left in front of the auth-center side. The server half is deployed
and `/setup` is reachable (see W6), and the `andy` account's one-time password
has been changed, so setup requests can be approved in the browser.

### 6. Create the topology with `auth-setup`

```bash
export AUTH_URL=https://$AUTH_HOST

auth-setup create-project server-admin --name 'Server Admin'
auth-setup create-role deploy-admin --project server-admin \
    --scope do2-deploy:create-project --scope dohl-deploy:create-project
auth-setup create-key andy-laptop-admin --project server-admin --role deploy-admin

auth-setup create-role deployer --project hotlaps \
    --scope hotlaps-api-staging:deploy --scope hotlaps-frontend-staging:deploy
auth-setup create-key hotlaps-ci --project hotlaps --role deployer \
    --service github-actions
```

Each blocks for a browser approval. Capture each `ak_…` at the moment it is
printed — it is shown once, and the plaintext is wiped from the server the
instant the tool collects it. A tool that dies in that window has lost the key.

Then `auth-service check-conflicts` on the auth host to confirm no resource name
is owned by two projects.

### 7. Verify against the live auth service

Point a local `deploy-server` at the real host with do2's existing service key
(`deploy-server-do2`, `b11b8ad7b30d2f73`, in `/root/secrets/` on do2) and
confirm: a real key accepted for `deploy`, denied for `execute-sql`, plus the
three misconfiguration responses — `401` (bad `DEPLOY_AUTH_KEY`), `403` (key
lacks `auth:introspect`), and `{"active": false}` for an unknown token. The
first two must deny *and* log loudly; they mean the instance is broken, not the
caller.

### 8. Cut do2 over

Per the rollout sequence, once every caller has a working key.

Keep the old deploy tool working until this is done and proven. It is the only
thing that can deploy auth-center without auth-center being up, which is the
"tested way back in" the requirements ask for against the circular dependency —
and it stops being available the moment do2 is cut over. Deciding whether
auth-center's own deploys stay on a separate path (W5) is due before this step,
not after it.

## Out of scope

- Adding `GET /api/v1/resources` behind an `auth:list-resources` scope. That is
  the only option that actually satisfies R1, but it is auth-service work.
- Any change to `~/tools/deploy`. R6 drops legacy compatibility, so the interim
  introspection path goes away with the old tool rather than being fixed.
