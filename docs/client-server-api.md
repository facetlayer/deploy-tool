# Client–server API

The RPC surface between `deploy` and `deploy-server`. Types are defined once in
`crates/deploy-core/src/rpc.rs`, which both binaries use; this document
describes what is there.

The wire format is deliberately unchanged from the old tool. During the
migration an old TypeScript CLI has to be able to talk to this server, and this
CLI to an old server, so field names, error codes and limits all match. New
fields are additive and optional so an old peer ignores them.

## Protocol

- **Transport:** JSON-RPC 2.0 over HTTP POST.
- **Endpoint:** `POST <dest-url>/json-rpc`.
- **Authentication:** `x-api-key` header, on every call. See
  [Authorization](#authorization).
- **Body limit:** 50 MB.
- **Errors:** a failing method answers HTTP 200 with a JSON-RPC `error` object,
  code `0` and the error message. An unknown method is `-32601`, unparseable
  JSON `-32700`, a request with no `method` `-32600`, a panicking handler HTTP
  500 with `-32603`. A denied call is **HTTP 401** with code `-32001`
  ("Unauthorized"), and the reason is logged server-side, not returned.

## Types

```typescript
interface FileEntry {
    relPath: string;   // "src/index.ts", always '/'-separated
    sha: string;       // SHA of the file contents
}

// Arbitrary per-deployment metadata. Well-known keys: "git-commit", "git-branch".
type DeploymentTags = Record<string, string>;
```

## Methods

Every method's required action and how the call resolves to a project come from
`METHOD_TABLE` in `rpc.rs`, which is the same table the server authorizes
against. The scope a key must hold is `<resource>:<action>` — exactly two segments,
action last — where `<resource>` is the project's binding on that instance (see
[auth-integration.md](auth-integration.md)).

| Method | Action | Resolved by |
|---|---|---|
| `createProject` | `create-project` | instance administration resource |
| `createDeployment` | `deploy` | `projectName` |
| `addManifestFiles` | `deploy` | `deployName` |
| `finalizeManifest` | `deploy` | `deployName` |
| `getNeededFiles` | `deploy` | `deployName` |
| `uploadOneFile` | `deploy` | `deployName` |
| `startMultiPartUpload` | `deploy` | `deployName` |
| `uploadFilePart` | `deploy` | `deployName` |
| `finishMultiPartUpload` | `deploy` | `deployName` |
| `finishUploads` | `deploy` | `deployName` |
| `verifyDeployment` | `deploy` | `deployName` |
| `activateDeployment` | `deploy` | `deployName` |
| `listDeployments` | `read` | `projectName` |
| `getDeploymentTags` | `read` | `projectName` |
| `previewDeployment` | `read` | `projectName` |
| `previewByDeployName` | `read` | `deployName` |
| `downloadFile` | `read` | `projectName` |
| `listDatabases` | `read` | `projectName` |
| `executeSql` | `execute-sql` | `projectName` |
| `rollback` | `rollback` | `projectName` |

A `deployName` resolves through `deployment.deploy_name → project_name`. A
method not in this table cannot be checked, and is therefore denied rather than
dispatched.

### createProject

New in this version; no old server answers it. Registers a project on this
instance and binds it to an auth-center resource.

```typescript
// params
{ projectName: string; resourceName: string; rebind?: boolean }

// result
{
    projectName: string;
    resourceName: string;
    outcome: 'created' | 'rebound' | 'unchanged';
    previousResourceName?: string;
}
```

`created` — there was no binding, whether or not a `project` row already
existed (a project imported from an old database lands here). `rebound` — it
was bound elsewhere and `rebind: true` was passed. `unchanged` — already bound
to this exact resource, which writes no history row. Rebinding an already-bound
project without `rebind` is an error: repointing a project hands its deploy
rights to a different set of keys, so it is never implicit. Every change writes
a row to `project_resource_binding_history`, with the key that made it.

`resourceName` must not contain `:`. A scope is `<resource>:<action>`, so a
colon here would produce a three-segment scope that auth-center refuses to mint
a key for; the server rejects it rather than registering a project no key can
ever satisfy. The CLI checks the same thing locally first.

The server does **not** verify that the resource exists in auth-center. There is
no service-to-service endpoint that could answer, and resources are derived
rather than declared, so at registration time the resource usually does not
exist yet. See "R1's resource-existence check" in
[auth-integration.md](auth-integration.md). After registering, the CLI prints
the `auth-setup` commands that create the role and key for this resource.

### createDeployment

Creates the deployment record and its directory.

```typescript
// params
{
    projectName: string;
    sourceFileManifest: FileEntry[];  // empty when using the batched flow
    sourceFileConfig: string;         // the .qc file text
    tags?: DeploymentTags;
}

// result
{ t: 'deployment_created'; deployName: string }   // e.g. "my-app-42"
```

The manifest is stored as JSON and empty subdirectories are created
immediately; with an empty manifest that happens at `finalizeManifest` instead.
`tags` land in `deployment.tags_json`. The key that authorized the call is
recorded on the deployment row (R7).

Deploying to a project that is not registered, or is registered with no
resource binding, is refused. There is no implicit create: the handler answers
"Project '<name>' is not registered on this server. Run: deploy create-project
…", and authorization has already denied the call for the same reason on any
method that reaches auth-center.

### addManifestFiles / finalizeManifest

For manifests too large to send in one request.

```typescript
addManifestFiles  { deployName: string; files: FileEntry[] }   // → null
finalizeManifest  { deployName: string }                       // → null
```

`finalizeManifest` signals the manifest is complete and sets up the directory
structure.

### getNeededFiles

```typescript
// params
{ deployName: string }

// result — a bare array, not an object wrapper
FileEntry[]
```

Files the server does not already have with a matching SHA. Content-hash dedup
across deployments is what makes a redeploy upload only what changed.

### uploadOneFile / startMultiPartUpload / uploadFilePart / finishMultiPartUpload

```typescript
uploadOneFile          { deployName, relPath, contentBase64 }                 // → null
startMultiPartUpload   { deployName, relPath }                                // → null (no-op)
uploadFilePart         { deployName, relPath, chunkStartsAt, chunkBase64 }    // → null
finishMultiPartUpload  { deployName, relPath }                                // → null
```

`chunkStartsAt` is the byte offset of the chunk in the original file. Chunks
are stored in a table and assembled in offset order by
`finishMultiPartUpload`, which then writes the file and clears the chunk and
needed-file rows.

`relPath` is client-supplied and is joined into the deployment directory
through a containment check; anything resolving outside it (`../…`, or an
absolute path) is rejected.

### finishUploads

```typescript
{ deployName: string }   // → null
```

Deletes destination files that are not in the manifest, honoring `ignore`,
`ignore-destination` and `preserve-existing-files` from the uploaded config,
and prunes preserved files past `preserve-existing-files-max-age`. This is the
step that deletes a server-generated file lacking an `ignore` rule — see
[config-reference.md](config-reference.md#ignore-and-server-generated-paths).

### verifyDeployment

```typescript
// params
{ deployName: string }

// result
{ status: 'success' | 'error'; error?: string }
```

Checks that every manifest file is on disk with the right hash. A failure is
reported in-band, not as a JSON-RPC error, so the CLI can print the reason and
stop before activating.

### activateDeployment

```typescript
{ deployName: string }   // → null
```

Marks the deployment active, swaps the live directory, and runs the config's
`after-deploy` actions (`shell(...)`, `candle-restart(...)`) on the server.

### rollback

```typescript
{ projectName: string; deployName: string }   // → null
```

Re-activates an earlier deployment. `projectName` is what authorizes the call,
so the handler additionally verifies that the named deployment belongs to that
project.

### previewDeployment / previewByDeployName

```typescript
previewDeployment      { projectName, sourceFileManifest, sourceFileConfig }
previewByDeployName    { deployName }

// result, both
{ filesToUpload: FileEntry[]; filesToDelete: string[] }
```

Compares against the project's active deployment without creating anything.
`previewByDeployName` reads the manifest from an existing deployment record,
for projects whose manifest is too large to send inline.

### listDeployments

```typescript
// params
{ projectName: string; limit?: number }    // limit defaults to 10

// result
{
    deployments: Array<{
        deploy_name: string;      // snake_case: the old server emitted raw column names
        created_at: string;
        is_active: boolean;
        tags?: DeploymentTags;
        authorized_by_key_id?: string;    // new; an old CLI ignores it
        authorized_by_key_name?: string;
    }>;
    activeDeployName: string | null;
}
```

The `authorized_by_*` fields are the R7 attribution: which auth-center key
shipped the deployment. They are null for rows created before attribution
existed, or imported from an old database.

### getDeploymentTags

```typescript
// params
{ projectName: string; deployName?: string }   // omit deployName for the active one

// result
{ deployName: string; createdAt: string; isActive: boolean; tags: DeploymentTags }
```

Errors if the project has no active deployment (when `deployName` is omitted)
or the named deployment does not exist. Backs `deploy check-deployed-commit`.

### downloadFile

```typescript
// params
{ projectName: string; relPath: string }

// result
{ contentBase64: string; relPath: string }
```

Reads from the project's active deployment. Path traversal is refused.

### listDatabases

```typescript
// params
{ projectName: string }

// result
{ databases: Array<{ path: string; absolutePath: string; tables: string[] }> }
```

The `database` entries from the active deployment's stored config, opened
read-only to list their tables.

### executeSql

```typescript
// params
{ projectName: string; sql: string; database?: string; callerIsAgent?: boolean }

// result
{ columns: string[]; rows: unknown[][]; rowsAffected: number }
```

Cell values keep their SQLite types. `database` bypasses table-name routing and
must name one of the configured databases. `callerIsAgent` is set by the client
when it detects it is running inside a coding agent; the server refuses the
query if the target database is marked `agent-sql-access-blocked`.

`executeSql` requires the `execute-sql` action (scope
`<resource>:execute-sql`), which is granted separately from `deploy` — a CI key
that ships builds cannot run arbitrary SQL.

## Deployment flow

Small deploy (manifest ≤ 500 entries):

1. Resolve the local file list and compute the SHA manifest.
2. `createDeployment` with the full manifest → `deployName`.
3. `getNeededFiles`.
4. Upload each needed file (single-shot, or multipart when large).
5. `finishUploads`.
6. `verifyDeployment`.
7. `activateDeployment`.

Large deploy (over 500 entries): step 2 sends an empty manifest, then
`addManifestFiles` in batches of 500, then `finalizeManifest`, then continue at
step 3.

## Size thresholds

| Parameter | Value | Where |
|---|---|---|
| Single-file base64 threshold | 80 KB (81920) | `deploy-cli/src/commands/run.rs` |
| Multi-part chunk size | 40 KB (40960) | `deploy-cli/src/commands/run.rs` |
| Manifest batch size | 500 entries | `deploy-cli/src/commands/run.rs` |
| Upload concurrency | 50 parallel | `deploy-cli/src/commands/run.rs` |
| Client large-request warning | 512 KB | `deploy-cli/src/rpc_client.rs` |
| Client request timeout | 600 s | `deploy-cli/src/rpc_client.rs` |
| Server body limit | 50 MB | `deploy-server/src/server.rs` |

A file whose base64 length is 80 KB or more goes multipart; anything smaller is
a single `uploadOneFile`.

## Authorization

Every call carries `x-api-key` and is checked before dispatch. There is no local
key table and no path that skips the check. The decision, in order:

1. Look the method up in `METHOD_TABLE`. An unknown method is denied, not
   dispatched.
2. Resolve the call to a project — by `projectName`, or by joining
   `deployment.deploy_name → project_name` for a `deployName`. `createProject`
   resolves to the instance's administration resource instead.
3. Look up that project's row in `project_resource_binding`.
4. Introspect the key against `<resource>:<action>` at auth-center.

Anything that goes wrong denies: unknown method, unresolvable deploy name,
unregistered project, project with no binding, empty binding, network error,
timeout, non-2xx, or a response without an explicit `allowed: true`. There is no
branch that allows a call because no resource could be determined — the old
server's `allowed ?? true` fallback is gone.

Positive verdicts are cached for 30 s keyed by `sha256(key) | resource |
action`; negative verdicts are never cached, so a revocation takes effect
within that window at worst.

A denial answers HTTP 401 with a JSON-RPC error whose `message` is
`Unauthorized`. Its `data` field carries a detail string **only when the key was
active and merely lacked the scope**:

```json
{"code": -32001, "message": "Unauthorized",
 "data": "this key does not hold hotlaps-api-staging:deploy"}
```

An unknown, revoked or expired key gets no `data` at all, so it cannot
enumerate this instance's project → resource bindings by guessing. The full
reason is always in the server's journal.

`deploy-server serve --disable-api-key-check` skips all of it. Local
development only; the startup banner prints a boxed warning when it is set.

Full model, scope-string format and configuration variables:
[auth-integration.md](auth-integration.md).
