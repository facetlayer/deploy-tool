# Implementation status

The Rust rewrite is implemented as a Cargo workspace with a shared core,
client, and server. This page records the current architecture and remaining
operational constraints; it is not a work plan.

## Implemented

- One shared `.qc` parser and file-selection implementation in `deploy-core`.
- A compatible JSON-RPC client and server with content-hash deduplication,
  batched manifests, multipart uploads, verification, and activation.
- Versioned and update-in-place deployments, preview, history, rollback,
  copy-back, deployment tags, SQLite queries, and Candle service startup.
- auth-center introspection with project-to-resource bindings, distinct action
  scopes, positive-result caching, fail-closed behavior, and deployment
  attribution.
- Additive opening of the previous server's SQLite database. Resource-binding
  tables and attribution columns are created when absent.
- Linux release build and guarded remote-upgrade scripts under `install/`.

## Source of truth

| Concern | Source |
|---|---|
| Config directives | `deploy-core/src/config.rs` and `filelist.rs` |
| RPC types and authorization actions | `deploy-core/src/rpc.rs` |
| Database schema and upgrades | `deploy-server/src/db.rs` |
| Authorization flow | `deploy-server/src/authz.rs` and `auth_center.rs` |
| CLI behavior | `deploy-cli/src/main.rs` and `commands/` |
| Host installation | `install/` |

## Current constraints

- `deploy-server serve` binds `0.0.0.0`; there is no bind-address option.
  Restrict exposure at the host or network layer.
- auth-center is a runtime dependency. If it is unavailable, uncached
  authorization requests fail. Operators need a tested recovery path if
  auth-center itself is deployed through this service.
- `update-in-place` has no prior directory to reactivate. Recover by deploying
  a known-good revision.
- `agent-sql-access-blocked` depends on client-reported agent detection. It is
  a guardrail, not an authorization boundary.

## Verification

```bash
cargo test --workspace
cargo check --workspace
```

Server integration tests exercise authorization and filesystem deployment
flows against temporary SQLite databases and deployment directories. CLI
end-to-end tests exercise command behavior against a test server.
