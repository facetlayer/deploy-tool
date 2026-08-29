//! JSON-RPC wire types, shared by the CLI and the server.
//!
//! Ported from src/shared/rpc-types.ts, with docs/ClientServerAPI.md as the
//! authority on each method's shape. The wire format must stay byte-compatible
//! with the old tool for the length of the migration: an old TypeScript CLI has
//! to be able to talk to this server, and this CLI to the old server. That is
//! why nearly everything here is `rename_all = "camelCase"` — and why
//! `DeploymentInfo`, which the old server emitted in snake_case, is not.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub rel_path: String,
    pub sha: String,
}

/// Arbitrary key/value metadata attached to a deployment by the client.
/// Well-known keys are in [`tag_names`].
pub type DeploymentTags = BTreeMap<String, String>;

pub mod tag_names {
    pub const GIT_COMMIT: &str = "git-commit";
    pub const GIT_BRANCH: &str = "git-branch";
}

// ---------------------------------------------------------------------------
// createDeployment
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDeploymentParams {
    pub project_name: String,
    /// Full manifest, or empty when the client is using the batched
    /// `addManifestFiles` flow.
    #[serde(default)]
    pub source_file_manifest: Vec<FileEntry>,
    pub source_file_config: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<DeploymentTags>,
}

pub const EVENT_DEPLOYMENT_CREATED: &str = "deployment_created";

/// The old server answered `createDeployment` with a tagged event object rather
/// than a bare deploy name, so the `t` discriminator has to stay on the wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentCreatedEvent {
    pub t: String,
    pub deploy_name: String,
}

impl DeploymentCreatedEvent {
    pub fn new(deploy_name: impl Into<String>) -> Self {
        DeploymentCreatedEvent {
            t: EVENT_DEPLOYMENT_CREATED.to_string(),
            deploy_name: deploy_name.into(),
        }
    }
}

pub type CreateDeploymentResult = DeploymentCreatedEvent;

// ---------------------------------------------------------------------------
// Manifest batching
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddManifestFilesParams {
    pub deploy_name: String,
    pub files: Vec<FileEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeManifestParams {
    pub deploy_name: String,
}

// ---------------------------------------------------------------------------
// Uploads
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetNeededFilesParams {
    pub deploy_name: String,
}

/// `getNeededFiles` answers with a bare array, not an object wrapper.
pub type GetNeededFilesResult = Vec<FileEntry>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadOneFileParams {
    pub deploy_name: String,
    pub rel_path: String,
    pub content_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartMultiPartUploadParams {
    pub deploy_name: String,
    pub rel_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFilePartParams {
    pub deploy_name: String,
    pub rel_path: String,
    /// Byte offset of this chunk within the original file.
    pub chunk_starts_at: i64,
    pub chunk_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishMultiPartUploadParams {
    pub deploy_name: String,
    pub rel_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishUploadsParams {
    pub deploy_name: String,
}

// ---------------------------------------------------------------------------
// Verify / activate
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyDeploymentParams {
    pub deploy_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyStatus {
    Success,
    Error,
}

/// Verification failure is reported in-band as `status: "error"`, not as a
/// JSON-RPC error, so the CLI can print the reason and stop before activating.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyDeploymentResult {
    pub status: VerifyStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateDeploymentParams {
    pub deploy_name: String,
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDeploymentParams {
    pub project_name: String,
    pub source_file_manifest: Vec<FileEntry>,
    pub source_file_config: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewByDeployNameParams {
    pub deploy_name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDeploymentResult {
    pub files_to_upload: Vec<FileEntry>,
    pub files_to_delete: Vec<String>,
}

// ---------------------------------------------------------------------------
// downloadFile
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadFileParams {
    pub project_name: String,
    pub rel_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadFileResult {
    pub content_base64: String,
    pub rel_path: String,
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteSqlParams {
    pub project_name: String,
    pub sql: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Set by the client when it detects it is running inside a coding agent.
    /// The server uses this to enforce per-database `agent-sql-access-blocked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_is_agent: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteSqlResult {
    pub columns: Vec<String>,
    /// Cell values keep their SQLite types, so they stay untyped JSON here.
    pub rows: Vec<Vec<Json>>,
    pub rows_affected: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDatabasesParams {
    pub project_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInfo {
    pub path: String,
    pub absolute_path: String,
    pub tables: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDatabasesResult {
    pub databases: Vec<DatabaseInfo>,
}

// ---------------------------------------------------------------------------
// Deployment history
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDeploymentsParams {
    pub project_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// Deliberately NOT camelCase. The old server emitted these three fields as
/// raw SQLite column names, and an old CLI reads them by those names.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeploymentInfo {
    pub deploy_name: String,
    pub created_at: String,
    pub is_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<DeploymentTags>,
    /// R7 attribution. New in this version, so an old CLI simply ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_by_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_by_key_name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDeploymentsResult {
    pub deployments: Vec<DeploymentInfo>,
    pub active_deploy_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDeploymentTagsParams {
    pub project_name: String,
    /// Which deployment to read. Omit to read the project's active deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDeploymentTagsResult {
    pub deploy_name: String,
    pub created_at: String,
    pub is_active: bool,
    /// `{}` when the deployment recorded no tags.
    pub tags: DeploymentTags,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackParams {
    pub project_name: String,
    pub deploy_name: String,
}

// ---------------------------------------------------------------------------
// createProject (new in this version)
// ---------------------------------------------------------------------------

/// R1: projects are registered explicitly and bound to an auth-center resource,
/// instead of springing into existence on first deploy. The binding lives in
/// this instance's database and is never taken from a later client.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectParams {
    pub project_name: String,
    pub resource_name: String,
    /// Repointing an existing project at a different resource is a
    /// privilege-escalation path, so it has to be asked for explicitly.
    #[serde(default)]
    pub rebind: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CreateProjectOutcome {
    /// The project did not exist and was registered.
    Created,
    /// The project existed with no binding, or with a different one and
    /// `rebind` set; it now points at `resourceName`.
    Rebound,
    /// The project was already bound to this exact resource.
    Unchanged,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectResult {
    pub project_name: String,
    pub resource_name: String,
    pub outcome: CreateProjectOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_resource_name: Option<String>,
    /// False when auth-center could not confirm the resource exists. Today this
    /// is always false: auth-center has no resource registry to ask, so a typo
    /// cannot be caught at registration. See docs/auth-integration.md.
    pub resource_verified: bool,
}

// ---------------------------------------------------------------------------
// Method names
// ---------------------------------------------------------------------------

pub mod methods {
    pub const CREATE_DEPLOYMENT: &str = "createDeployment";
    pub const GET_DEPLOYMENT_TAGS: &str = "getDeploymentTags";
    pub const ADD_MANIFEST_FILES: &str = "addManifestFiles";
    pub const FINALIZE_MANIFEST: &str = "finalizeManifest";
    pub const GET_NEEDED_FILES: &str = "getNeededFiles";
    pub const UPLOAD_ONE_FILE: &str = "uploadOneFile";
    pub const START_MULTIPART_UPLOAD: &str = "startMultiPartUpload";
    pub const UPLOAD_FILE_PART: &str = "uploadFilePart";
    pub const FINISH_MULTIPART_UPLOAD: &str = "finishMultiPartUpload";
    pub const FINISH_UPLOADS: &str = "finishUploads";
    pub const VERIFY_DEPLOYMENT: &str = "verifyDeployment";
    pub const ACTIVATE_DEPLOYMENT: &str = "activateDeployment";
    pub const PREVIEW_DEPLOYMENT: &str = "previewDeployment";
    pub const PREVIEW_BY_DEPLOY_NAME: &str = "previewByDeployName";
    pub const DOWNLOAD_FILE: &str = "downloadFile";
    pub const EXECUTE_SQL: &str = "executeSql";
    pub const LIST_DATABASES: &str = "listDatabases";
    pub const LIST_DEPLOYMENTS: &str = "listDeployments";
    pub const ROLLBACK: &str = "rollback";
    /// New in this version; no old server answers it.
    pub const CREATE_PROJECT: &str = "createProject";
}

// ---------------------------------------------------------------------------
// Authorization table
// ---------------------------------------------------------------------------

/// The five actions from R3. `sql` is separate from `deploy` on purpose: a CI
/// key that ships builds must not be able to run arbitrary SQL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Deploy,
    Read,
    Sql,
    Rollback,
    CreateProject,
}

impl Action {
    /// The action segment of the scope string. This spelling is a cross-repo
    /// contract with the auth-center dashboard — keys are minted with these
    /// exact strings, and a mismatch denies every call.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Deploy => "deploy",
            Action::Read => "read",
            Action::Sql => "sql",
            Action::Rollback => "rollback",
            Action::CreateProject => "create-project",
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// D1: resource and action encode into one flat auth-center scope string,
/// `deploy:<resource>:<action>`. auth-center's matcher stops `*` at a `:`, so
/// `deploy:hotlaps-staging:*` cannot reach another resource.
pub fn scope_string(resource_name: &str, action: Action) -> String {
    format!("deploy:{}:{}", resource_name, action.as_str())
}

/// How a call is resolved to the project whose resource binding gates it.
///
/// There is deliberately no "unresolved ⇒ allow" variant: a call that cannot be
/// resolved to a resource is denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectResolution {
    /// The params carry `projectName`, used directly.
    ByProjectName,
    /// The params carry only `deployName`; join
    /// `deployment.deploy_name → deployment.project_name`.
    ByDeployName,
    /// Checked against this instance's administration resource
    /// (`DEPLOY_ADMIN_RESOURCE`) rather than any project.
    InstanceAdministration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    pub action: Action,
    pub resolution: ProjectResolution,
}

/// The one place both binaries agree on what each method requires. Normative
/// source: the action table in docs/auth-integration.md (D1).
pub const METHOD_TABLE: &[MethodSpec] = &[
    // deploy
    spec(methods::CREATE_DEPLOYMENT, Action::Deploy, ProjectResolution::ByProjectName),
    spec(methods::ADD_MANIFEST_FILES, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::FINALIZE_MANIFEST, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::GET_NEEDED_FILES, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::UPLOAD_ONE_FILE, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::START_MULTIPART_UPLOAD, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::UPLOAD_FILE_PART, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::FINISH_MULTIPART_UPLOAD, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::FINISH_UPLOADS, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::VERIFY_DEPLOYMENT, Action::Deploy, ProjectResolution::ByDeployName),
    spec(methods::ACTIVATE_DEPLOYMENT, Action::Deploy, ProjectResolution::ByDeployName),
    // read
    spec(methods::LIST_DEPLOYMENTS, Action::Read, ProjectResolution::ByProjectName),
    spec(methods::GET_DEPLOYMENT_TAGS, Action::Read, ProjectResolution::ByProjectName),
    spec(methods::PREVIEW_DEPLOYMENT, Action::Read, ProjectResolution::ByProjectName),
    spec(methods::PREVIEW_BY_DEPLOY_NAME, Action::Read, ProjectResolution::ByDeployName),
    spec(methods::DOWNLOAD_FILE, Action::Read, ProjectResolution::ByProjectName),
    spec(methods::LIST_DATABASES, Action::Read, ProjectResolution::ByProjectName),
    // sql
    spec(methods::EXECUTE_SQL, Action::Sql, ProjectResolution::ByProjectName),
    // rollback: params carry both names, and projectName is authoritative —
    // the handler still checks the deployment belongs to that project.
    spec(methods::ROLLBACK, Action::Rollback, ProjectResolution::ByProjectName),
    // create-project
    spec(methods::CREATE_PROJECT, Action::CreateProject, ProjectResolution::InstanceAdministration),
];

const fn spec(name: &'static str, action: Action, resolution: ProjectResolution) -> MethodSpec {
    MethodSpec { name, action, resolution }
}

/// Looks up what a method requires. `None` means the method is unknown, which
/// the server must treat as a denial and not as an unguarded call.
pub fn lookup_method(name: &str) -> Option<&'static MethodSpec> {
    METHOD_TABLE.iter().find(|spec| spec.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_constant_is_in_the_table() {
        let names = [
            methods::CREATE_DEPLOYMENT,
            methods::GET_DEPLOYMENT_TAGS,
            methods::ADD_MANIFEST_FILES,
            methods::FINALIZE_MANIFEST,
            methods::GET_NEEDED_FILES,
            methods::UPLOAD_ONE_FILE,
            methods::START_MULTIPART_UPLOAD,
            methods::UPLOAD_FILE_PART,
            methods::FINISH_MULTIPART_UPLOAD,
            methods::FINISH_UPLOADS,
            methods::VERIFY_DEPLOYMENT,
            methods::ACTIVATE_DEPLOYMENT,
            methods::PREVIEW_DEPLOYMENT,
            methods::PREVIEW_BY_DEPLOY_NAME,
            methods::DOWNLOAD_FILE,
            methods::EXECUTE_SQL,
            methods::LIST_DATABASES,
            methods::LIST_DEPLOYMENTS,
            methods::ROLLBACK,
            methods::CREATE_PROJECT,
        ];
        for name in names {
            assert!(lookup_method(name).is_some(), "{name} missing from METHOD_TABLE");
        }
        assert_eq!(METHOD_TABLE.len(), names.len());
    }

    #[test]
    fn table_has_no_duplicate_names() {
        let mut seen = std::collections::HashSet::new();
        for spec in METHOD_TABLE {
            assert!(seen.insert(spec.name), "duplicate entry for {}", spec.name);
        }
    }

    #[test]
    fn actions_match_the_auth_design_doc() {
        assert_eq!(lookup_method(methods::CREATE_DEPLOYMENT).unwrap().action, Action::Deploy);
        assert_eq!(lookup_method(methods::ACTIVATE_DEPLOYMENT).unwrap().action, Action::Deploy);
        assert_eq!(lookup_method(methods::LIST_DEPLOYMENTS).unwrap().action, Action::Read);
        assert_eq!(lookup_method(methods::DOWNLOAD_FILE).unwrap().action, Action::Read);
        assert_eq!(lookup_method(methods::LIST_DATABASES).unwrap().action, Action::Read);
        // A deploy key must not reach SQL or rollback.
        assert_eq!(lookup_method(methods::EXECUTE_SQL).unwrap().action, Action::Sql);
        assert_eq!(lookup_method(methods::ROLLBACK).unwrap().action, Action::Rollback);
        assert_eq!(
            lookup_method(methods::CREATE_PROJECT).unwrap().action,
            Action::CreateProject
        );
    }

    #[test]
    fn resolution_matches_the_params_each_method_carries() {
        assert_eq!(
            lookup_method(methods::UPLOAD_ONE_FILE).unwrap().resolution,
            ProjectResolution::ByDeployName
        );
        assert_eq!(
            lookup_method(methods::PREVIEW_BY_DEPLOY_NAME).unwrap().resolution,
            ProjectResolution::ByDeployName
        );
        assert_eq!(
            lookup_method(methods::EXECUTE_SQL).unwrap().resolution,
            ProjectResolution::ByProjectName
        );
        assert_eq!(
            lookup_method(methods::CREATE_PROJECT).unwrap().resolution,
            ProjectResolution::InstanceAdministration
        );
    }

    #[test]
    fn unknown_method_has_no_spec() {
        assert!(lookup_method("deleteEverything").is_none());
        assert!(lookup_method("").is_none());
    }

    #[test]
    fn scope_strings_match_the_documented_format() {
        assert_eq!(
            scope_string("hotlaps-staging", Action::Deploy),
            "deploy:hotlaps-staging:deploy"
        );
        assert_eq!(scope_string("hotlaps-prod", Action::Read), "deploy:hotlaps-prod:read");
        assert_eq!(
            scope_string("deploy-do2", Action::CreateProject),
            "deploy:deploy-do2:create-project"
        );
    }

    #[test]
    fn create_deployment_params_are_camel_case_on_the_wire() {
        let params = CreateDeploymentParams {
            project_name: "hotlaps-api".to_string(),
            source_file_manifest: vec![FileEntry {
                rel_path: "src/index.ts".to_string(),
                sha: "abc".to_string(),
            }],
            source_file_config: "deploy-settings\n".to_string(),
            tags: None,
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["projectName"], "hotlaps-api");
        assert_eq!(json["sourceFileManifest"][0]["relPath"], "src/index.ts");
        assert_eq!(json["sourceFileConfig"], "deploy-settings\n");
        assert!(json.get("tags").is_none());
    }

    #[test]
    fn deployment_created_event_keeps_its_discriminator() {
        let json = serde_json::to_value(DeploymentCreatedEvent::new("hotlaps-api-42")).unwrap();
        assert_eq!(json["t"], "deployment_created");
        assert_eq!(json["deployName"], "hotlaps-api-42");
    }

    #[test]
    fn deployment_info_stays_snake_case() {
        let json = serde_json::to_value(DeploymentInfo {
            deploy_name: "sample-7".to_string(),
            created_at: "2026-08-29T00:00:00.000Z".to_string(),
            is_active: true,
            tags: None,
            authorized_by_key_id: None,
            authorized_by_key_name: None,
        })
        .unwrap();
        assert_eq!(json["deploy_name"], "sample-7");
        assert_eq!(json["created_at"], "2026-08-29T00:00:00.000Z");
        assert_eq!(json["is_active"], true);
    }

    #[test]
    fn list_deployments_result_wrapper_is_camel_case() {
        let json = serde_json::to_value(ListDeploymentsResult {
            deployments: vec![],
            active_deploy_name: Some("sample-7".to_string()),
        })
        .unwrap();
        assert_eq!(json["activeDeployName"], "sample-7");
    }

    #[test]
    fn verify_result_status_is_lowercase() {
        let json = serde_json::to_value(VerifyDeploymentResult {
            status: VerifyStatus::Error,
            error: Some("2 files are missing".to_string()),
        })
        .unwrap();
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"], "2 files are missing");
    }

    #[test]
    fn old_server_upload_params_deserialize() {
        let json = r#"{"deployName":"a-1","relPath":"x.txt","chunkStartsAt":40960,
                       "chunkBase64":"aGk="}"#;
        let params: UploadFilePartParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.deploy_name, "a-1");
        assert_eq!(params.chunk_starts_at, 40960);
    }

    #[test]
    fn create_project_result_reports_what_happened() {
        let json = serde_json::to_value(CreateProjectResult {
            project_name: "hotlaps-api".to_string(),
            resource_name: "hotlaps-staging".to_string(),
            outcome: CreateProjectOutcome::Rebound,
            previous_resource_name: Some("hotlaps-prod".to_string()),
            resource_verified: false,
        })
        .unwrap();
        assert_eq!(json["projectName"], "hotlaps-api");
        assert_eq!(json["resourceName"], "hotlaps-staging");
        assert_eq!(json["outcome"], "rebound");
        assert_eq!(json["previousResourceName"], "hotlaps-prod");
        assert_eq!(json["resourceVerified"], false);
    }
}
