# Authorization

`deploy-server` delegates authentication and authorization to auth-center. It
does not store or accept local API keys.

## Scope model

A deploy scope is exactly `<resource>:<action>`:

| Action | Grants |
|---|---|
| `deploy` | Create, upload, verify, and activate deployments. |
| `read` | Preview, history, tags, downloads, and database discovery. |
| `execute-sql` | Execute SQL against configured project databases. |
| `rollback` | Reactivate an earlier versioned deployment. |
| `create-project` | Register or rebind projects on an instance. |
| `admin-read` | Read every project on the instance. Used by the web dashboard. |

The action is the final segment. Generate scopes with `deploy auth-scopes`
instead of constructing them manually. A resource must not contain `:`.

Resources should be specific to an application and environment, such as
`my-app-staging` and `my-app-prod`. The same deploy project can then be bound
to different resources on separate server instances.

## Project bindings

Before first use, register the project on each destination. Registration is
done with `auth-setup`, which calls the server's `createProject` method; the
`deploy` CLI has no command for it.

The server stores the binding in `project_resource_binding`; clients cannot
choose a resource per request. Rebinding requires `--rebind`, and every bind or
rebind is recorded in `project_resource_binding_history` with the authorizing
key when available.

`createProject` is authorized against the instance's
`DEPLOY_ADMIN_RESOURCE`, for example `host-deploy:create-project`. So are
`listProjects` and `getProject`, the dashboard's two reads, against
`host-deploy:admin-read`. All other methods resolve a project directly from
`projectName` or indirectly from a `deployName`, then use that project's stored
binding.

`admin-read` exists rather than reusing `read` because the dashboard's question
is "what is on this server", which has no project to resolve against. A
per-project `read` grant cannot answer it: a viewer would need one grant per
project and would still never learn about a project nobody had thought to grant
them. Note that `getProject` takes a project name as a *filter*, not as the
thing that gates it — it is checked against the instance either way. Holding
`admin-read` grants no deploy, no rollback and no SQL, on this or any
project.

## Creating roles and keys

`deploy auth-scopes <config.qc> --resource <resource>` prints the current
scope set and ready-to-run `auth-setup` commands. It is local-only and needs no
API key.

The suggested deployer role contains `deploy` and `read`. Grant
`execute-sql` and `rollback` separately. Instance administration keys should
hold only `<admin-resource>:create-project`.

Dashboard viewers are admin *users* rather than keys, and hold
`<admin-resource>:admin-read`. Grant it through a role so that adding a viewer
is a role assignment rather than a scope edit:

```bash
auth-setup create-role deploy-viewer --project deploy \
  --scope do2-deploy:admin-read
```

Resources are derived from scopes in auth-center rather than explicitly
registered. The deploy server therefore validates the resource name's shape
but cannot check its existence during project registration. The first
authorized call is the effective check.

## Server configuration

All three variables are required for an authenticated server:

| Variable | Purpose |
|---|---|
| `DEPLOY_AUTH_URL` | auth-center base URL. |
| `DEPLOY_AUTH_KEY` | This deploy instance's service key; it must hold `auth:introspect`. |
| `DEPLOY_ADMIN_RESOURCE` | Resource used to authorize `createProject`. |

Keep them in the root-owned environment file used by the systemd unit. The
server refuses to start when one is missing. `--disable-api-key-check` bypasses
all authorization and is only for local development and tests.

## Request authorization

For each JSON-RPC request, the server:

1. Looks up the method in `deploy-core/src/rpc.rs::METHOD_TABLE`.
2. Resolves the instance admin resource or the request's project binding.
3. Introspects the presented `x-api-key` for `<resource>:<action>`.
4. Dispatches only when auth-center returns an active key and explicit
   `allowed: true`.

Unknown methods, missing bindings, invalid keys, timeouts, non-success HTTP
responses, and malformed introspection responses are denied. Positive
decisions are cached for 30 seconds by key hash, resource, and action;
negative decisions are not cached.

An active key missing a scope receives a useful detail such as
`this key does not hold my-app-staging:deploy`. Unknown, revoked, and expired
keys receive only `Unauthorized`, which avoids exposing project bindings. The
server journal contains the full denial reason.

Successful deployments record the authorizing key ID and name. `deploy
history` displays that attribution.
