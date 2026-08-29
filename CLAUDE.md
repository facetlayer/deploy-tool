# deploy-tool — working in this repo

A Cargo workspace holding a deployment server and its CLI. Read
[docs/project-goals.md](docs/project-goals.md) and
[docs/auth-integration.md](docs/auth-integration.md) first — they are
normative, and most design questions are already answered there.

## Layout

```
crates/deploy-core/    shared library, no binary
  qc/                  the .qc parser, copied verbatim from the old tool
  config.rs            every .qc directive both binaries read
  filelist.rs          include/exclude/ignore rules, directory walks, leftovers
  glob.rs              picomatch-compatible matching
  hash.rs              file content hashes
  security.rs          client-side scan that blocks .env / keys / credentials
  sqlnames.rs          table-name extraction, for routing `deploy sql`
  rpc.rs               wire types, method names, the authorization action table

crates/deploy-server/  binary `deploy-server`
  server.rs            HTTP + JSON-RPC transport; authorizes, then dispatches
  authz.rs             the R2 decision: resolve → look up resource → introspect
  auth_center.rs       auth-center client, cache, fail-closed behavior
  db.rs                SQLite schema + migrations (the contract with live instances)
  handlers/            one module per group of RPC methods
  paths.rs             path-traversal guards for client-supplied relPath
  preserve.rs, manifest.rs, sql.rs, state.rs

crates/deploy-cli/     binary `deploy`
  main.rs              clap command tree
  commands/            one module per subcommand
  rpc_client.rs        JSON-RPC client
  api_key.rs           key resolution: secrets-file → env → ~/secrets/deploy.env
```

## Shared logic lives in deploy-core

The thing this rewrite exists to avoid: the old repo had the RPC types, the
`.qc` parser and the file-list logic implemented twice — once in TypeScript
under `src/`, once in Rust under `rust/` — and they drifted. Do not recreate
that shape here.

Concretely: anything both binaries need to agree on goes in `deploy-core`. That
includes the parts that are asymmetric in use — `config.rs` holds the
client-only settings *and* the server-only activation directives, because a
disagreement about what a config means is exactly the bug class being
eliminated. `rpc.rs::METHOD_TABLE` is the single place that says which action
and which project resolution each RPC method requires; the server reads it to
authorize, and it is the doc source for the action table.

`deploy-server` is a binary-only crate, so the CLI cannot depend on it. Where
the CLI genuinely needs server behavior (`start-services` resolves the server's
state directory), the logic is duplicated with a comment saying so. Prefer
moving such code into `deploy-core` over adding a second copy.

Do not modify `crates/deploy-core/src/qc/` or `glob.rs` — both are ports whose
value is being byte-identical to the originals.

## The RPC wire format is frozen during migration

An old TypeScript CLI has to be able to talk to this server, and this CLI to
the old Rust/TS server, for as long as both exist. So:

- Field names stay as they are. Most types are `rename_all = "camelCase"`;
  `DeploymentInfo` is deliberately snake_case because the old server emitted
  raw column names. The tests in `rpc.rs` pin this.
- New fields are additive and optional (`#[serde(default, skip_serializing_if)]`),
  so an old peer ignores them — that is how the R7 attribution fields were
  added.
- The endpoint (`POST /json-rpc`), the `x-api-key` header, the JSON-RPC error
  codes and the 50MB body limit all match the old server.
- `createProject` is the one method no old server answers. That is fine: it is
  new, and a client calling it against an old server gets a method-not-found.

The same applies to `db.rs`: do2 and dohl have live `db.sqlite` files this
server opens in place. Add columns via `ADDED_COLUMNS` migrations, never by
changing an existing `CREATE TABLE`.

## Tests

```bash
cargo test                      # whole workspace
cargo test -p deploy-core       # parser, config, file list, rpc shapes
cargo test -p deploy-server     # handlers against a temp DB + temp deploy dir
cargo test -p deploy-cli
cargo check -p <crate>          # while iterating
```

Server handler tests stand up a real SQLite database and a real deployments
directory in a temp dir and drive the handlers through `dispatch`, so they
cover the on-disk effects (leftover deletion, preserve rules, path traversal)
rather than just the return values. Follow that pattern for new handlers.

## Style

- `anyhow::Result` in library and handler code; `thiserror` only where a caller
  matches on the variant.
- Hand-written `rusqlite` SQL with `params!`. No ORM, no query builder.
- No async in the storage layer. The server's async surface is the axum
  transport only; anything touching SQLite or making an outbound HTTP call runs
  under `spawn_blocking`.
- Comments explain why, not what. The hazards worth a comment are the ones that
  have already cost something: the path-traversal guards in `paths.rs`, the
  missing-`ignore` DB wipe (hotlaps, 2026-05-23), picomatch's dotfile
  semantics, and the `allowed ?? true` fallback the auth rewrite removed.

## Things that are deliberately not done

- No resource-existence check at `createProject` beyond a best-effort probe —
  auth-center has no resource registry yet. See "Known gap" in
  docs/auth-integration.md before trying to fix it.
- The legacy `secret_key` table still grants everything. It stays until each
  instance's keys have migrated, then is turned off per instance with
  `DEPLOY_DISABLE_LEGACY_KEYS=1`.
- `deploy-server serve` binds `0.0.0.0`, inherited from the old server. The do2
  convention is `127.0.0.1`; there is no bind flag yet.
