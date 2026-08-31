# Client-server API

The CLI talks to `POST /json-rpc` using JSON-RPC 2.0. Requests use
`Content-Type: application/json` and carry the client key in `x-api-key`.
The server body limit is 50 MB.

Most field names are camelCase. Deployment-history rows retain snake_case for
compatibility with the previous server. Existing wire shapes are stable;
additions must be optional so old peers can ignore them.

## Shared types

```typescript
type FileEntry = { relPath: string; sha: string };
type DeploymentTags = Record<string, string>;
type Preview = { filesToUpload: FileEntry[]; filesToDelete: string[] };
```

## Methods

| Method | Parameters | Result | Action |
|---|---|---|---|
| `createProject` | `{ projectName, resourceName, rebind? }` | Binding status and resource | `create-project` |
| `createDeployment` | `{ projectName, sourceFileManifest, sourceFileConfig, tags? }` | `{ t: "deployment_created", deployName }` | `deploy` |
| `addManifestFiles` | `{ deployName, files }` | `null` | `deploy` |
| `finalizeManifest` | `{ deployName }` | `null` | `deploy` |
| `getNeededFiles` | `{ deployName }` | `FileEntry[]` | `deploy` |
| `uploadOneFile` | `{ deployName, relPath, contentBase64 }` | `null` | `deploy` |
| `startMultiPartUpload` | `{ deployName, relPath }` | `null` | `deploy` |
| `uploadFilePart` | `{ deployName, relPath, chunkStartsAt, chunkBase64 }` | `null` | `deploy` |
| `finishMultiPartUpload` | `{ deployName, relPath }` | `null` | `deploy` |
| `finishUploads` | `{ deployName }` | `null` | `deploy` |
| `verifyDeployment` | `{ deployName }` | `{ status: "success" | "error", error? }` | `deploy` |
| `activateDeployment` | `{ deployName }` | `null` | `deploy` |
| `rollback` | `{ projectName, deployName }` | `null` | `rollback` |
| `previewDeployment` | `{ projectName, sourceFileManifest, sourceFileConfig }` | `Preview` | `read` |
| `previewByDeployName` | `{ deployName }` | `Preview` | `read` |
| `listDeployments` | `{ projectName, limit? }` | Deployment list and active name | `read` |
| `getDeploymentTags` | `{ projectName, deployName? }` | Deployment tags and status | `read` |
| `downloadFile` | `{ projectName, relPath }` | `{ contentBase64, relPath }` | `read` |
| `listDatabases` | `{ projectName }` | Configured databases and tables | `read` |
| `executeSql` | `{ projectName, sql, database?, callerIsAgent? }` | Columns, rows, and affected count | `execute-sql` |

`createProject` is the only method without an equivalent in the previous
server, and the only one the `deploy` CLI never calls — projects are registered
with `auth-setup`. `getNeededFiles` returns a bare array. `verifyDeployment` reports hash
failure in its result rather than as a JSON-RPC error.

All client-supplied relative paths are checked to remain inside their
deployment directory.

## Deployment flow

For up to 500 manifest entries, the CLI sends the manifest with
`createDeployment`. Larger manifests are sent in batches of 500 through
`addManifestFiles`, followed by `finalizeManifest`.

The remaining flow is:

1. `getNeededFiles`
2. upload missing files
3. `finishUploads`
4. `verifyDeployment`
5. `activateDeployment`

Files with base64 content below 80 KiB use `uploadOneFile`; larger files use
40 KiB multipart chunks. The CLI runs at most 50 upload workers. Its request
timeout is 600 seconds and it warns for request bodies over 512 KiB.

## Authorization and errors

`METHOD_TABLE` in `deploy-core/src/rpc.rs` is the authoritative mapping from
method to action and project-resolution strategy. The server authorizes before
dispatch and denies calls it cannot resolve to a stored resource.

Authorization failures return HTTP 401 with JSON-RPC code `-32001` and message
`Unauthorized`. An active key missing a scope may receive the missing scope in
`data`; invalid or inactive keys receive no binding details. See
[auth-integration.md](auth-integration.md).
