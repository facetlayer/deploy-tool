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
`DEPLOY_ADMIN_RESOURCE`, for example `host-deploy:create-project`. All other
methods resolve a project directly from `projectName` or indirectly from a
`deployName`, then use that project's stored binding.

## Creating roles and keys

`deploy auth-scopes <config.qc> --resource <resource>` prints the current
scope set and ready-to-run `auth-setup` commands. It is local-only and needs no
API key.

The suggested deployer role contains `deploy` and `read`. Grant
`execute-sql` and `rollback` separately. Instance administration keys should
hold only `<admin-resource>:create-project`.

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
