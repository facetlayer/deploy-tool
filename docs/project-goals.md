# System overview

`deploy-tool` is a small deployment service for hosts that run applications
directly rather than through a container orchestrator. The CLI uploads a
declared set of files; the server verifies them, activates the deployment, and
runs configured post-deploy actions.

## Components

| Crate | Purpose |
|---|---|
| `deploy-core` | Shared `.qc` parsing, file selection, hashing, RPC types, and authorization method table. |
| `deploy-cli` | The `deploy` command. Builds manifests, uploads missing content, activates releases, and provides history, rollback, copy-back, and SQL commands. |
| `deploy-server` | The JSON-RPC service. Stores deployment state in SQLite and files in a configured deployment directory. |

Shared behavior belongs in `deploy-core`, so the client and server cannot
interpret the config or wire protocol differently.

## Deployment model

Each project has a `.qc` file containing its destination, file rules, and
hooks. A deployment follows this sequence:

1. The CLI runs local `before-deploy` commands and builds a content-hash
   manifest.
2. The server creates a deployment record and reports which files it does not
   already have.
3. The CLI uploads only the missing content.
4. The server removes untracked destination files according to the config,
   verifies hashes, and activates the deployment.
5. Server-side `after-deploy` commands run.

Deployments are versioned by default. `update-in-place` instead reuses one
directory; it is appropriate for services whose process configuration points
at a fixed path, but it cannot restore an earlier directory during rollback.

## Authorization model

Every remote operation is authorized by auth-center. A project must first be
registered on each deploy-server instance and bound to a resource stored in
that instance's database. The same project can therefore use different
resources in different environments.

Scopes have the form `<resource>:<action>`. The supported actions are
`deploy`, `read`, `execute-sql`, `rollback`, and `create-project`. The server
has no local API-key table and fails closed if auth-center cannot return an
explicit positive decision.

See [auth-integration.md](auth-integration.md) for the complete model.

## Storage and compatibility

The server stores metadata in `db.sqlite` and payloads under its configured
deployments directory. Its schema can open the old service's database and adds
the resource-binding tables and deployment-attribution columns when missing.
Existing deployment and active-deployment rows remain intact.

The JSON-RPC wire format remains compatible with the previous client and
server where methods overlap. New fields are optional. `createProject` is new
and is not supported by the old server.

## Operating principles

- Preview every `update-in-place` deployment. Server-generated files require
  `ignore` or `ignore-destination` rules or they will be deleted.
- Give CI only `deploy` and, when needed, `read`; grant SQL and rollback
  separately.
- Keep auth-center credentials in the server environment file and client keys
  in a secrets file or environment variable.
- Use `--disable-api-key-check` only for local development.
