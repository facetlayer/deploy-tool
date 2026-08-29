//! JSON-RPC method implementations.
//!
//! Handlers contain no authorization logic. The transport in `server.rs` has
//! already decided the call is allowed and resolved which key allowed it; all a
//! handler does with that key is record it (R7).

pub mod activate;
pub mod cleanup;
pub mod deployments;
pub mod files;
pub mod sql_methods;
pub mod tags;
pub mod uploads;

use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use serde_json::Value as Json;

use deploy_core::rpc::methods;

use crate::state::{AppState, AuthorizedKey};

/// Reads the JSON-RPC params into a typed struct from `deploy_core::rpc`.
///
/// The rpc types are the wire contract shared with the CLI, so this is the one
/// place params are interpreted — a handler never reaches into the raw JSON.
pub fn parse_params<T: DeserializeOwned>(params: &Json) -> Result<T> {
    serde_json::from_value(params.clone()).map_err(|err| anyhow!("{}", err))
}

/// Sentinel the transport turns into a JSON-RPC "Method not found" response.
/// The authorization layer rejects unknown methods before this, so reaching it
/// means a method is in `METHOD_TABLE` but has no implementation.
pub const METHOD_NOT_FOUND: &str = "__method_not_found__";

/// Dispatches an already-authorized JSON-RPC call.
pub fn dispatch(
    state: &AppState,
    method: &str,
    params: &Json,
    authorized_by: &AuthorizedKey,
) -> Result<Json> {
    let authorized_by = Some(authorized_by);

    match method {
        methods::CREATE_PROJECT => deployments::create_project(state, params, authorized_by),
        methods::CREATE_DEPLOYMENT => deployments::create_deployment(state, params, authorized_by),
        methods::ADD_MANIFEST_FILES => deployments::add_manifest_files(state, params),
        methods::FINALIZE_MANIFEST => deployments::finalize_manifest(state, params),
        methods::LIST_DEPLOYMENTS => deployments::list_deployments(state, params),
        methods::ROLLBACK => deployments::rollback(state, params),

        methods::GET_NEEDED_FILES => files::get_needed_files(state, params),
        methods::VERIFY_DEPLOYMENT => files::verify_deployment(state, params),
        methods::PREVIEW_DEPLOYMENT => files::preview_deployment_method(state, params),
        methods::PREVIEW_BY_DEPLOY_NAME => files::preview_by_deploy_name(state, params),
        methods::DOWNLOAD_FILE => files::download_file(state, params),

        methods::UPLOAD_ONE_FILE => uploads::upload_one_file(state, params),
        // The old server had nothing to do here: chunks carry their own offset,
        // so a multi-part upload needs no setup. Kept because the client calls it.
        methods::START_MULTIPART_UPLOAD => Ok(Json::Null),
        methods::UPLOAD_FILE_PART => uploads::upload_file_part(state, params),
        methods::FINISH_MULTIPART_UPLOAD => uploads::finish_multipart_upload(state, params),
        methods::FINISH_UPLOADS => uploads::finish_uploads(state, params),

        methods::ACTIVATE_DEPLOYMENT => activate::activate_deployment(state, params),

        methods::EXECUTE_SQL => sql_methods::execute_sql(state, params),
        methods::LIST_DATABASES => sql_methods::list_databases(state, params),

        methods::GET_DEPLOYMENT_TAGS => tags::get_deployment_tags(state, params),

        _ => Err(anyhow!(METHOD_NOT_FOUND)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use rusqlite::Connection;
    use serde_json::json;

    use deploy_core::hash::get_file_hash;

    use crate::db;

    struct TestServer {
        root: PathBuf,
        deploys_dir: PathBuf,
        state: AppState,
    }

    /// Every test gets its own directory and database file, so they can run in
    /// parallel and a failure leaves its state behind for inspection.
    fn setup(name: &str) -> TestServer {
        let root = std::env::temp_dir().join(format!("deploy-server-handlers-{}", name));
        let _ = fs::remove_dir_all(&root);
        let deploys_dir = root.join("deploys");
        fs::create_dir_all(&deploys_dir).unwrap();

        let conn = Connection::open(root.join("db.sqlite")).unwrap();
        db::init_connection(&conn).unwrap();
        conn.execute(
            "insert into deployments_dir (deployments_dir, created_at) values (?, ?)",
            rusqlite::params![deploys_dir.to_string_lossy().to_string(), db::now_iso()],
        )
        .unwrap();

        TestServer {
            root,
            deploys_dir,
            state: AppState::new(conn, true),
        }
    }

    fn test_key() -> AuthorizedKey {
        AuthorizedKey::new("key-abc", Some("ci-staging".to_string()))
    }

    impl TestServer {
        fn call(&self, method: &str, params: Json) -> Result<Json> {
            dispatch(&self.state, method, &params, &test_key())
        }

        fn deploy_dir(&self, project: &str) -> PathBuf {
            self.deploys_dir.join(project)
        }

        /// Creates a deployment and returns its deploy name.
        fn create_deployment(&self, project: &str, manifest: Json, config: &str) -> String {
            let result = self
                .call(
                    methods::CREATE_DEPLOYMENT,
                    json!({
                        "projectName": project,
                        "sourceFileManifest": manifest,
                        "sourceFileConfig": config,
                    }),
                )
                .unwrap();
            result["deployName"].as_str().unwrap().to_string()
        }

        fn activate(&self, deploy_name: &str) {
            self.call(
                methods::ACTIVATE_DEPLOYMENT,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn config_for(project: &str, extra: &str) -> String {
        format!(
            "deploy-settings\n  project-name={project}\n  dest-url=http://localhost:9999\n  \
             update-in-place\n\ninclude **\n{extra}\n"
        )
    }

    fn sha_of(path: &Path) -> String {
        get_file_hash(path).unwrap().unwrap()
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    // --- createDeployment ---

    #[test]
    fn create_deployment_makes_a_record_and_its_directories() {
        let server = setup("create");
        let config = config_for("test-project", "");

        let result = server
            .call(
                methods::CREATE_DEPLOYMENT,
                json!({
                    "projectName": "test-project",
                    "sourceFileManifest": [
                        { "relPath": "src/index.js", "sha": "abc123" },
                        { "relPath": "package.json", "sha": "def456" },
                    ],
                    "sourceFileConfig": config,
                }),
            )
            .unwrap();

        assert_eq!(result["t"], "deployment_created");
        let deploy_name = result["deployName"].as_str().unwrap();
        assert!(deploy_name.starts_with("test-project-"), "{deploy_name}");

        let deploy_dir = server.deploy_dir("test-project");
        assert!(deploy_dir.is_dir());
        // The manifest's parent directories are laid out up front.
        assert!(deploy_dir.join("src").is_dir());
    }

    #[test]
    fn deploy_names_increment() {
        let server = setup("increment");
        let config = config_for("incrementing-project", "");
        let manifest = json!([{ "relPath": "file.txt", "sha": "aaa" }]);

        let first = server.create_deployment("incrementing-project", manifest.clone(), &config);
        let second = server.create_deployment("incrementing-project", manifest, &config);

        assert_ne!(first, second);
    }

    #[test]
    fn create_deployment_records_the_authorizing_key() {
        let server = setup("attribution");
        let config = config_for("attributed-project", "");
        let deploy_name =
            server.create_deployment("attributed-project", json!([]), &config);

        let listed = server
            .call(
                methods::LIST_DEPLOYMENTS,
                json!({ "projectName": "attributed-project" }),
            )
            .unwrap();

        let entry = &listed["deployments"][0];
        assert_eq!(entry["deploy_name"], deploy_name);
        assert_eq!(entry["authorized_by_key_id"], "key-abc");
        assert_eq!(entry["authorized_by_key_name"], "ci-staging");
    }

    // --- createProject / R1 ---

    #[test]
    fn create_project_registers_and_binds_a_resource() {
        let server = setup("create-project");

        let result = server
            .call(
                methods::CREATE_PROJECT,
                json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
            )
            .unwrap();

        assert_eq!(result["outcome"], "created");
        assert_eq!(result["resourceName"], "hotlaps-staging");
        // No resource registry exists to confirm the name against yet.
        assert_eq!(result["resourceVerified"], false);

        let conn = server.state.db();
        let bound: String = conn
            .query_row(
                "select resource_name from project where project_name = 'hotlaps-api'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bound, "hotlaps-staging");

        let audit: (Option<String>, String, Option<String>) = conn
            .query_row(
                "select old_resource_name, new_resource_name, changed_by from project_resource_audit",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(audit, (None, "hotlaps-staging".to_string(), Some("key-abc".to_string())));
    }

    #[test]
    fn re_registering_the_same_resource_is_a_no_op() {
        let server = setup("create-project-repeat");
        let params = json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" });

        server.call(methods::CREATE_PROJECT, params.clone()).unwrap();
        let result = server.call(methods::CREATE_PROJECT, params).unwrap();

        assert_eq!(result["outcome"], "unchanged");

        // Unchanged means unchanged: no second audit row.
        let conn = server.state.db();
        let audit_rows: i64 = conn
            .query_row("select count(*) from project_resource_audit", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(audit_rows, 1);
    }

    #[test]
    fn rebinding_to_another_resource_requires_asking_for_it() {
        let server = setup("create-project-rebind");
        server
            .call(
                methods::CREATE_PROJECT,
                json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
            )
            .unwrap();

        let err = server
            .call(
                methods::CREATE_PROJECT,
                json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-prod" }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("already bound to resource 'hotlaps-staging'"), "{err}");

        let result = server
            .call(
                methods::CREATE_PROJECT,
                json!({
                    "projectName": "hotlaps-api",
                    "resourceName": "hotlaps-prod",
                    "rebind": true
                }),
            )
            .unwrap();
        assert_eq!(result["outcome"], "rebound");
        assert_eq!(result["previousResourceName"], "hotlaps-staging");

        let conn = server.state.db();
        let audit: (Option<String>, String) = conn
            .query_row(
                "select old_resource_name, new_resource_name from project_resource_audit
                 order by audit_id desc limit 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(audit, (Some("hotlaps-staging".to_string()), "hotlaps-prod".to_string()));
    }

    #[test]
    fn create_project_refuses_an_empty_resource_name() {
        let server = setup("create-project-empty");
        let err = server
            .call(
                methods::CREATE_PROJECT,
                json!({ "projectName": "hotlaps-api", "resourceName": "  " }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("resourceName is required"), "{err}");
    }

    #[test]
    fn deploying_to_an_unregistered_project_is_refused_when_auth_center_is_on() {
        let mut server = setup("r1-unregistered");
        server.state.auth_center_enabled = true;

        let config = config_for("unregistered", "");
        let err = server
            .call(
                methods::CREATE_DEPLOYMENT,
                json!({
                    "projectName": "unregistered",
                    "sourceFileManifest": [],
                    "sourceFileConfig": config,
                }),
            )
            .unwrap_err()
            .to_string();

        assert!(err.contains("is not registered on this server"), "{err}");
    }

    #[test]
    fn deploying_to_a_registered_project_is_allowed_when_auth_center_is_on() {
        let mut server = setup("r1-registered");
        server.state.auth_center_enabled = true;

        server
            .call(
                methods::CREATE_PROJECT,
                json!({ "projectName": "registered", "resourceName": "registered-staging" }),
            )
            .unwrap();

        let config = config_for("registered", "");
        let deploy_name = server.create_deployment("registered", json!([]), &config);
        assert!(deploy_name.starts_with("registered-"));
    }

    #[test]
    fn a_project_carried_over_without_a_resource_cannot_be_deployed_to() {
        let mut server = setup("r1-unbound");

        // A project row from the old tool, which had no resource column.
        {
            let conn = server.state.db();
            conn.execute(
                "insert into project (project_name, created_at) values ('legacy-project', ?)",
                rusqlite::params![db::now_iso()],
            )
            .unwrap();
        }
        server.state.auth_center_enabled = true;

        let config = config_for("legacy-project", "");
        let err = server
            .call(
                methods::CREATE_DEPLOYMENT,
                json!({
                    "projectName": "legacy-project",
                    "sourceFileManifest": [],
                    "sourceFileConfig": config,
                }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("not bound to an auth-center resource"), "{err}");
    }

    // --- getNeededFiles ---

    #[test]
    fn needed_files_lists_everything_when_nothing_is_on_disk() {
        let server = setup("needed-all");
        let config = config_for("needed-files-project", "");
        let deploy_name = server.create_deployment(
            "needed-files-project",
            json!([
                { "relPath": "a.txt", "sha": "sha-a" },
                { "relPath": "b.txt", "sha": "sha-b" },
            ]),
            &config,
        );

        let needed = server
            .call(
                methods::GET_NEEDED_FILES,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();

        let mut rel_paths: Vec<&str> = needed
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["relPath"].as_str().unwrap())
            .collect();
        rel_paths.sort();
        assert_eq!(rel_paths, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn needed_files_skips_content_the_server_already_has() {
        let server = setup("needed-dedup");
        let config = config_for("needed-files-project", "");
        let deploy_dir = server.deploy_dir("needed-files-project");
        fs::create_dir_all(&deploy_dir).unwrap();
        write(&deploy_dir.join("a.txt"), "content for a");
        let actual_sha = sha_of(&deploy_dir.join("a.txt"));

        let deploy_name = server.create_deployment(
            "needed-files-project",
            json!([
                { "relPath": "a.txt", "sha": actual_sha },
                { "relPath": "b.txt", "sha": "sha-b-different" },
            ]),
            &config,
        );

        let needed = server
            .call(
                methods::GET_NEEDED_FILES,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();

        assert_eq!(needed.as_array().unwrap().len(), 1);
        assert_eq!(needed[0]["relPath"], "b.txt");
    }

    #[test]
    fn needed_files_rejects_an_unknown_deployment() {
        let server = setup("needed-unknown");
        let err = server
            .call(
                methods::GET_NEEDED_FILES,
                json!({ "deployName": "nonexistent-99" }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("Deployment not found"), "{err}");
    }

    // --- finishUploads ---

    #[test]
    fn finish_uploads_deletes_leftovers_not_in_the_manifest() {
        let server = setup("finish-leftovers");
        let config = config_for("cleanup-project", "");
        let deploy_name = server.create_deployment(
            "cleanup-project",
            json!([{ "relPath": "keep.txt", "sha": "aaa" }]),
            &config,
        );

        let deploy_dir = server.deploy_dir("cleanup-project");
        write(&deploy_dir.join("keep.txt"), "keep me");
        write(&deploy_dir.join("leftover.txt"), "delete me");

        server
            .call(
                methods::FINISH_UPLOADS,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();

        assert!(deploy_dir.join("keep.txt").exists());
        assert!(!deploy_dir.join("leftover.txt").exists());
    }

    #[test]
    fn an_ignore_rule_protects_a_destination_file_from_deletion() {
        // Regression guard for the 2026-05-23 incident: a live database under
        // the deployment directory must survive an update-in-place deploy that
        // does not ship it.
        let server = setup("finish-ignore");
        let config = config_for("ignore-project", "ignore backend/data/**");
        let deploy_name = server.create_deployment(
            "ignore-project",
            json!([{ "relPath": "server.js", "sha": "aaa" }]),
            &config,
        );

        let deploy_dir = server.deploy_dir("ignore-project");
        write(&deploy_dir.join("server.js"), "code");
        write(&deploy_dir.join("backend/data/app.sqlite"), "live data");

        server
            .call(
                methods::FINISH_UPLOADS,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();

        assert!(deploy_dir.join("backend/data/app.sqlite").exists());
    }

    #[test]
    fn finish_uploads_keeps_files_matching_preserve_existing_files() {
        let server = setup("finish-preserve");
        let config = config_for("preserve-project", "preserve-existing-files _next/static/**");
        let deploy_name = server.create_deployment(
            "preserve-project",
            json!([{ "relPath": "index.html", "sha": "aaa" }]),
            &config,
        );

        let deploy_dir = server.deploy_dir("preserve-project");
        write(&deploy_dir.join("index.html"), "page");
        // An old hashed asset from a previous deploy, not in the new manifest.
        write(&deploy_dir.join("_next/static/css/old-hash.css"), "old css");
        // A leftover that is NOT under the preserved glob.
        write(&deploy_dir.join("stale.txt"), "delete me");

        server
            .call(
                methods::FINISH_UPLOADS,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();

        assert!(deploy_dir.join("_next/static/css/old-hash.css").exists());
        assert!(!deploy_dir.join("stale.txt").exists());
    }

    #[test]
    fn preserved_files_older_than_the_max_age_are_pruned() {
        let server = setup("finish-preserve-gc");
        let config = config_for(
            "preserve-gc-project",
            "preserve-existing-files _next/static/**\npreserve-existing-files-max-age 7d",
        );
        let deploy_name = server.create_deployment(
            "preserve-gc-project",
            json!([{ "relPath": "index.html", "sha": "aaa" }]),
            &config,
        );

        let deploy_dir = server.deploy_dir("preserve-gc-project");
        write(&deploy_dir.join("index.html"), "page");
        let recent_asset = deploy_dir.join("_next/static/css/recent.css");
        let old_asset = deploy_dir.join("_next/static/css/old.css");
        write(&recent_asset, "recent");
        write(&old_asset, "old");

        // Age the old asset past the 7d window.
        let long_ago = SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
        let handle = fs::File::options().write(true).open(&old_asset).unwrap();
        handle
            .set_times(fs::FileTimes::new().set_accessed(long_ago).set_modified(long_ago))
            .unwrap();
        drop(handle);

        server
            .call(
                methods::FINISH_UPLOADS,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();

        assert!(recent_asset.exists());
        assert!(!old_asset.exists());
    }

    // --- verifyDeployment ---

    #[test]
    fn verify_succeeds_when_every_file_matches() {
        let server = setup("verify-ok");
        let config = config_for("verify-project", "");
        let deploy_dir = server.deploy_dir("verify-project");
        fs::create_dir_all(&deploy_dir).unwrap();
        write(&deploy_dir.join("file.txt"), "hello verify");
        let sha = sha_of(&deploy_dir.join("file.txt"));

        let deploy_name = server.create_deployment(
            "verify-project",
            json!([{ "relPath": "file.txt", "sha": sha }]),
            &config,
        );

        let result = server
            .call(
                methods::VERIFY_DEPLOYMENT,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();
        assert_eq!(result["status"], "success");
    }

    #[test]
    fn verify_reports_wrong_contents_in_band() {
        let server = setup("verify-wrong");
        let config = config_for("verify-project", "");
        let deploy_dir = server.deploy_dir("verify-project");
        fs::create_dir_all(&deploy_dir).unwrap();
        write(&deploy_dir.join("file.txt"), "wrong content");

        let deploy_name = server.create_deployment(
            "verify-project",
            json!([{ "relPath": "file.txt", "sha": "expected-sha-that-wont-match" }]),
            &config,
        );

        let result = server
            .call(
                methods::VERIFY_DEPLOYMENT,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["error"].as_str().unwrap().contains("wrong contents"));
    }

    #[test]
    fn verify_reports_a_missing_file() {
        let server = setup("verify-missing");
        let config = config_for("verify-project", "");
        let deploy_name = server.create_deployment(
            "verify-project",
            json!([{ "relPath": "nonexistent.txt", "sha": "abc" }]),
            &config,
        );

        let result = server
            .call(
                methods::VERIFY_DEPLOYMENT,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["error"].as_str().unwrap().contains("missing"));
    }

    // --- previewDeployment ---

    /// A project with an active deployment holding `existing.txt` and a
    /// `server-only.txt` that no manifest mentions.
    fn setup_preview(name: &str) -> (TestServer, String, String) {
        let server = setup(name);
        let config = config_for("preview-project", "");
        let deploy_dir = server.deploy_dir("preview-project");
        fs::create_dir_all(&deploy_dir).unwrap();
        write(&deploy_dir.join("existing.txt"), "existing content");
        write(&deploy_dir.join("server-only.txt"), "only on server");
        let existing_sha = sha_of(&deploy_dir.join("existing.txt"));

        let deploy_name = server.create_deployment(
            "preview-project",
            json!([{ "relPath": "existing.txt", "sha": existing_sha }]),
            &config,
        );
        server.activate(&deploy_name);

        (server, config, existing_sha)
    }

    #[test]
    fn preview_treats_everything_as_new_when_there_is_no_active_deployment() {
        let server = setup("preview-none");
        let manifest = json!([{ "relPath": "new.txt", "sha": "abc" }]);

        let result = server
            .call(
                methods::PREVIEW_DEPLOYMENT,
                json!({
                    "projectName": "no-such-project",
                    "sourceFileManifest": manifest,
                    "sourceFileConfig": config_for("no-such-project", ""),
                }),
            )
            .unwrap();

        assert_eq!(result["filesToUpload"], manifest);
        assert_eq!(result["filesToDelete"], json!([]));
    }

    #[test]
    fn preview_reports_files_that_need_uploading() {
        let (server, config, _) = setup_preview("preview-upload");

        let result = server
            .call(
                methods::PREVIEW_DEPLOYMENT,
                json!({
                    "projectName": "preview-project",
                    "sourceFileManifest": [{ "relPath": "existing.txt", "sha": "different-sha" }],
                    "sourceFileConfig": config,
                }),
            )
            .unwrap();

        assert_eq!(result["filesToUpload"].as_array().unwrap().len(), 1);
        assert_eq!(result["filesToUpload"][0]["relPath"], "existing.txt");
    }

    #[test]
    fn preview_reports_no_drift_when_contents_match() {
        let (server, config, existing_sha) = setup_preview("preview-nodrift");

        let result = server
            .call(
                methods::PREVIEW_DEPLOYMENT,
                json!({
                    "projectName": "preview-project",
                    "sourceFileManifest": [{ "relPath": "existing.txt", "sha": existing_sha }],
                    "sourceFileConfig": config,
                }),
            )
            .unwrap();

        assert_eq!(result["filesToUpload"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn preview_reports_server_files_that_would_be_deleted() {
        let (server, config, existing_sha) = setup_preview("preview-delete");

        let result = server
            .call(
                methods::PREVIEW_DEPLOYMENT,
                json!({
                    "projectName": "preview-project",
                    "sourceFileManifest": [{ "relPath": "existing.txt", "sha": existing_sha }],
                    "sourceFileConfig": config,
                }),
            )
            .unwrap();

        let to_delete: Vec<&str> = result["filesToDelete"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert!(to_delete.contains(&"server-only.txt"), "{to_delete:?}");
    }

    // --- uploads ---

    #[test]
    fn uploading_a_file_writes_it_and_clears_its_needed_row() {
        let server = setup("upload-one");
        let config = config_for("upload-project", "");
        let deploy_name = server.create_deployment(
            "upload-project",
            json!([{ "relPath": "src/app.js", "sha": "aaa" }]),
            &config,
        );
        server
            .call(
                methods::GET_NEEDED_FILES,
                json!({ "deployName": deploy_name }),
            )
            .unwrap();

        server
            .call(
                methods::UPLOAD_ONE_FILE,
                json!({
                    "deployName": deploy_name,
                    "relPath": "src/app.js",
                    "contentBase64": "aGVsbG8=",
                }),
            )
            .unwrap();

        let written = fs::read_to_string(server.deploy_dir("upload-project").join("src/app.js"))
            .unwrap();
        assert_eq!(written, "hello");

        let remaining: i64 = server
            .state
            .db()
            .query_row(
                "select count(*) from deployment_needed_file where deploy_name = ?",
                [&deploy_name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn uploading_outside_the_deployment_directory_is_refused() {
        let server = setup("upload-traversal");
        let config = config_for("upload-project", "");
        let deploy_name = server.create_deployment("upload-project", json!([]), &config);

        let err = server
            .call(
                methods::UPLOAD_ONE_FILE,
                json!({
                    "deployName": deploy_name,
                    "relPath": "../../escaped.txt",
                    "contentBase64": "aGVsbG8=",
                }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid path"), "{err}");
    }

    #[test]
    fn multipart_chunks_are_reassembled_in_offset_order() {
        let server = setup("upload-multipart");
        let config = config_for("multipart-project", "");
        let deploy_name = server.create_deployment(
            "multipart-project",
            json!([{ "relPath": "big.bin", "sha": "aaa" }]),
            &config,
        );

        // Sent out of order, as concurrent uploads arrive.
        for (offset, chunk) in [(5, "d29ybGQ="), (0, "aGVsbG8=")] {
            server
                .call(
                    methods::UPLOAD_FILE_PART,
                    json!({
                        "deployName": deploy_name,
                        "relPath": "big.bin",
                        "chunkStartsAt": offset,
                        "chunkBase64": chunk,
                    }),
                )
                .unwrap();
        }

        server
            .call(
                methods::FINISH_MULTIPART_UPLOAD,
                json!({ "deployName": deploy_name, "relPath": "big.bin" }),
            )
            .unwrap();

        let written =
            fs::read_to_string(server.deploy_dir("multipart-project").join("big.bin")).unwrap();
        assert_eq!(written, "helloworld");
    }

    // --- downloadFile ---

    fn setup_download(name: &str) -> TestServer {
        let server = setup(name);
        let config = config_for("download-project", "");
        let deploy_dir = server.deploy_dir("download-project");
        fs::create_dir_all(&deploy_dir).unwrap();
        write(&deploy_dir.join("hello.txt"), "hello world");
        write(&deploy_dir.join("subdir/nested.txt"), "nested content");

        let deploy_name = server.create_deployment(
            "download-project",
            json!([
                { "relPath": "hello.txt", "sha": "aaa" },
                { "relPath": "subdir/nested.txt", "sha": "bbb" },
            ]),
            &config,
        );
        server.activate(&deploy_name);
        server
    }

    fn decode(result: &Json) -> String {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result["contentBase64"].as_str().unwrap())
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn download_returns_file_contents_as_base64() {
        let server = setup_download("download-basic");
        let result = server
            .call(
                methods::DOWNLOAD_FILE,
                json!({ "projectName": "download-project", "relPath": "hello.txt" }),
            )
            .unwrap();

        assert_eq!(decode(&result), "hello world");
        assert_eq!(result["relPath"], "hello.txt");
    }

    #[test]
    fn download_handles_nested_files() {
        let server = setup_download("download-nested");
        let result = server
            .call(
                methods::DOWNLOAD_FILE,
                json!({ "projectName": "download-project", "relPath": "subdir/nested.txt" }),
            )
            .unwrap();

        assert_eq!(decode(&result), "nested content");
    }

    #[test]
    fn download_refuses_path_traversal() {
        let server = setup_download("download-traversal");
        let err = server
            .call(
                methods::DOWNLOAD_FILE,
                json!({ "projectName": "download-project", "relPath": "../../../etc/passwd" }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid path"), "{err}");
    }

    #[test]
    fn download_requires_an_active_deployment() {
        let server = setup("download-inactive");
        let err = server
            .call(
                methods::DOWNLOAD_FILE,
                json!({ "projectName": "nonexistent-project", "relPath": "file.txt" }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("No active deployment found"), "{err}");
    }

    // --- tags / history ---

    #[test]
    fn deployment_tags_round_trip_through_the_database() {
        let server = setup("tags");
        let config = config_for("tags-project", "");
        let deploy_name = server
            .call(
                methods::CREATE_DEPLOYMENT,
                json!({
                    "projectName": "tags-project",
                    "sourceFileManifest": [],
                    "sourceFileConfig": config,
                    "tags": { "git-commit": "abc123", "git-branch": "main" },
                }),
            )
            .unwrap()["deployName"]
            .as_str()
            .unwrap()
            .to_string();
        server.activate(&deploy_name);

        let result = server
            .call(
                methods::GET_DEPLOYMENT_TAGS,
                json!({ "projectName": "tags-project" }),
            )
            .unwrap();

        assert_eq!(result["deployName"], deploy_name);
        assert_eq!(result["isActive"], true);
        assert_eq!(result["tags"]["git-commit"], "abc123");
        assert_eq!(result["tags"]["git-branch"], "main");
    }

    #[test]
    fn list_deployments_marks_the_active_one() {
        let server = setup("list");
        let config = config_for("list-project", "");
        let first = server.create_deployment("list-project", json!([]), &config);
        let second = server.create_deployment("list-project", json!([]), &config);
        server.activate(&first);

        let result = server
            .call(
                methods::LIST_DEPLOYMENTS,
                json!({ "projectName": "list-project" }),
            )
            .unwrap();

        assert_eq!(result["activeDeployName"], first);
        let entries = result["deployments"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        for entry in entries {
            let expected_active = entry["deploy_name"] == json!(first);
            assert_eq!(entry["is_active"], json!(expected_active));
        }
        assert!(entries.iter().any(|entry| entry["deploy_name"] == json!(second)));
    }

    #[test]
    fn rollback_refuses_a_deployment_from_another_project() {
        let server = setup("rollback-mismatch");
        let config = config_for("rollback-project", "");
        server.create_deployment("rollback-project", json!([]), &config);

        let other_config = config_for("other-project", "");
        let other = server.create_deployment("other-project", json!([]), &other_config);

        let err = server
            .call(
                methods::ROLLBACK,
                json!({ "projectName": "rollback-project", "deployName": other }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found for project"), "{err}");
    }

    #[test]
    fn rollback_activates_an_earlier_deployment() {
        let server = setup("rollback");
        let config = config_for("rollback-project", "");
        let first = server.create_deployment("rollback-project", json!([]), &config);
        let second = server.create_deployment("rollback-project", json!([]), &config);
        server.activate(&second);

        server
            .call(
                methods::ROLLBACK,
                json!({ "projectName": "rollback-project", "deployName": first }),
            )
            .unwrap();

        let active: String = server
            .state
            .db()
            .query_row(
                "select deploy_name from active_deployment where project_name = 'rollback-project'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, first);
    }

    // --- executeSql ---

    fn setup_sql(name: &str) -> TestServer {
        let server = setup(name);
        let config = "deploy-settings\n  project-name=sql-agent-project\n  \
             dest-url=http://localhost:9999\n  update-in-place\n\n\
             database data/blocked.sqlite\n  agent-sql-access-blocked\n\
             database data/open.sqlite\n";

        let deploy_dir = server.deploy_dir("sql-agent-project");
        fs::create_dir_all(deploy_dir.join("data")).unwrap();
        for (file, table) in [("blocked.sqlite", "secrets"), ("open.sqlite", "widgets")] {
            let conn = Connection::open(deploy_dir.join("data").join(file)).unwrap();
            conn.execute_batch(&format!("create table {table} (id integer)"))
                .unwrap();
        }

        let deploy_name = server.create_deployment(
            "sql-agent-project",
            json!([{ "relPath": "data/blocked.sqlite", "sha": "aaa" }]),
            config,
        );
        server.activate(&deploy_name);
        server
    }

    #[test]
    fn agent_queries_against_a_flagged_database_are_blocked() {
        let server = setup_sql("sql-blocked");
        let err = server
            .call(
                methods::EXECUTE_SQL,
                json!({
                    "projectName": "sql-agent-project",
                    "sql": "select * from secrets",
                    "callerIsAgent": true,
                }),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked when running inside a coding agent"), "{err}");
    }

    #[test]
    fn agent_queries_against_an_unflagged_database_are_allowed() {
        let server = setup_sql("sql-open");
        let result = server
            .call(
                methods::EXECUTE_SQL,
                json!({
                    "projectName": "sql-agent-project",
                    "sql": "select * from widgets",
                    "callerIsAgent": true,
                }),
            )
            .unwrap();
        assert_eq!(result["columns"], json!(["id"]));
    }

    #[test]
    fn non_agent_queries_against_a_flagged_database_are_allowed() {
        let server = setup_sql("sql-nonagent");
        let result = server
            .call(
                methods::EXECUTE_SQL,
                json!({
                    "projectName": "sql-agent-project",
                    "sql": "select * from secrets",
                    "callerIsAgent": false,
                }),
            )
            .unwrap();
        assert_eq!(result["columns"], json!(["id"]));
    }

    #[test]
    fn list_databases_reports_each_configured_database_and_its_tables() {
        let server = setup_sql("sql-list");
        let result = server
            .call(
                methods::LIST_DATABASES,
                json!({ "projectName": "sql-agent-project" }),
            )
            .unwrap();

        let databases = result["databases"].as_array().unwrap();
        assert_eq!(databases.len(), 2);
        assert_eq!(databases[0]["path"], "data/blocked.sqlite");
        assert_eq!(databases[0]["tables"], json!(["secrets"]));
    }

    // --- dispatch ---

    #[test]
    fn an_unknown_method_reports_method_not_found() {
        let server = setup("unknown-method");
        let err = server.call("deleteEverything", json!({})).unwrap_err();
        assert_eq!(err.to_string(), METHOD_NOT_FOUND);
    }
}
