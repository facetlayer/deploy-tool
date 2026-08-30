//! End-to-end tests for the `deploy` CLI against a real `deploy-server`.
//!
//! The old tool had no Rust CLI, so this is new coverage for the port of
//! `src/client/*.ts`: the same ground as `test/deployment.test.ts`,
//! `test/multipartUpload.test.ts` and `test/rollback.test.ts`, but driven
//! through the command line rather than through the RPC surface directly.

mod common;

use serde_json::json;

use common::*;

const CI_KEY: &str = "ci-staging";
const OTHER_KEY: &str = "ci-elsewhere";
const ADMIN_KEY: &str = "instance-admin";
const RESOURCE: &str = "basic-staging";

fn grants() -> Vec<KeyGrant> {
    vec![
        grant(ADMIN_KEY, &["deploy-test:create-project"]),
        grant(CI_KEY, &["basic-staging:**"]),
        grant(OTHER_KEY, &["somewhere-else:**"]),
    ]
}

fn start(root: &TempRoot) -> (StubAuthCenter, DeployServer) {
    let stub = start_stub_auth_center(granting(grants()));
    let server = DeployServer::start(
        root,
        ServerOptions {
            auth_url: stub.base_url.clone(),
            admin_resource: "deploy-test".to_string(),
            ..ServerOptions::default()
        },
    );
    (stub, server)
}

/// The whole CLI flow: register the project, ship it, and confirm what landed.
#[test]
fn create_project_then_deploy_run_ships_the_whole_file_set() {
    let root = TempRoot::new("cli-run");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());

    // Big enough that its base64 clears the client's 80KB ceiling, so `deploy
    // run` has to take the multipart path.
    let bundle = large_content(120 * 1024);
    workspace.write("assets/bundle.js", &bundle);

    workspace
        .run(
            ADMIN_KEY,
            &[
                "create-project",
                "basic-app",
                "--resource",
                RESOURCE,
                "--override-dest",
                &server.url(),
            ],
        )
        .expect_ok("create-project");

    let run = workspace.deploy(CI_KEY).expect_ok("deploy run");
    assert!(
        run.stdout.contains("Deployment is active"),
        "{}",
        run.output()
    );

    let dir = server.project_dir("basic-app");
    assert!(read_file(&dir.join("index.js")).contains("Hello from Basic App v1!"));
    assert!(read_file(&dir.join("public/index.html")).contains("Welcome to Basic App"));
    assert_eq!(
        read_file(&dir.join("public/styles.css")),
        "body { background: #f6f6f6; }\n"
    );
    assert!(read_file(&dir.join("package.json")).contains("\"version\": \"1.0.0\""));
    assert_eq!(
        std::fs::read(dir.join("assets/bundle.js")).unwrap(),
        bundle,
        "the multipart upload must reassemble byte for byte"
    );

    // R7 attribution reaches `deploy history`.
    let history = workspace
        .run(CI_KEY, &["history", "deploy.qc"])
        .expect_ok("history");
    assert!(
        history.stdout.contains("basic-app-"),
        "{}",
        history.output()
    );
    assert!(history.stdout.contains("<- active"), "{}", history.output());
    assert!(history.stdout.contains(CI_KEY), "{}", history.output());

    // A second run with nothing changed uploads nothing.
    let again = workspace.deploy(CI_KEY).expect_ok("second deploy run");
    assert!(
        again.stdout.contains("Server has requested 0 files"),
        "{}",
        again.output()
    );
}

#[test]
fn a_redeploy_updates_changed_files_and_removes_dropped_ones() {
    let root = TempRoot::new("cli-redeploy");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());
    create_project(&server.client(ADMIN_KEY), "basic-app", RESOURCE);

    workspace.deploy(CI_KEY).expect_ok("first deploy");

    workspace.write("index.js", b"console.log('Hello from Basic App v2!');\n");
    workspace.write("settings.json", br#"{"version":"2.0.0"}"#);
    workspace.remove("README.md");

    workspace.deploy(CI_KEY).expect_ok("second deploy");

    let dir = server.project_dir("basic-app");
    assert!(read_file(&dir.join("index.js")).contains("v2"));
    assert!(read_file(&dir.join("settings.json")).contains("2.0.0"));
    assert!(
        !dir.join("README.md").exists(),
        "a file dropped from the local set should be removed on the server"
    );
}

/// The fixture's `ignore server-data` rule, exercised through the CLI: a
/// directory that only ever exists on the server survives a deploy that knows
/// nothing about it. This is the client-side half of the 2026-05-23
/// production-database-wipe regression.
#[test]
fn an_ignored_server_side_directory_survives_a_cli_deploy() {
    let root = TempRoot::new("cli-ignore");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());
    create_project(&server.client(ADMIN_KEY), "basic-app", RESOURCE);

    workspace.deploy(CI_KEY).expect_ok("first deploy");

    let live_db = server
        .project_dir("basic-app")
        .join("server-data/app.sqlite");
    write_file(&live_db, b"REAL PRODUCTION DATA");

    workspace.write("index.js", b"console.log('v2');\n");
    workspace.deploy(CI_KEY).expect_ok("second deploy");

    assert_eq!(read_file(&live_db), "REAL PRODUCTION DATA");
}

#[test]
fn preview_reports_drift_and_then_reports_none() {
    let root = TempRoot::new("cli-preview");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());
    create_project(&server.client(ADMIN_KEY), "basic-app", RESOURCE);

    workspace.deploy(CI_KEY).expect_ok("deploy");

    let clean = workspace
        .run(CI_KEY, &["preview", "deploy.qc"])
        .expect_ok("preview after a clean deploy");
    assert!(
        !clean.stdout.contains("index.js"),
        "nothing should be pending: {}",
        clean.output()
    );

    workspace.write("index.js", b"console.log('changed');\n");
    let dirty = workspace
        .run(CI_KEY, &["preview", "deploy.qc"])
        .expect_ok("preview after an edit");
    assert!(dirty.stdout.contains("index.js"), "{}", dirty.output());
}

#[test]
fn preview_deploy_files_lists_the_local_set_without_a_server() {
    let root = TempRoot::new("cli-file-list");
    let workspace = CliWorkspace::new(&root, "basic-app", "http://127.0.0.1:9");

    let listed = workspace
        .run("unused", &["preview-deploy-files", "deploy.qc"])
        .expect_ok("preview-deploy-files");

    assert!(listed.stdout.contains("public/index.html"));
    assert!(listed.stdout.contains("package.json"));
}

#[test]
fn rollback_repoints_the_active_deployment() {
    let root = TempRoot::new("cli-rollback");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());
    create_project(&server.client(ADMIN_KEY), "basic-app", RESOURCE);

    // Versioned deploys, so rolling back actually swaps directories.
    let config = read_file(&workspace.config()).replace("  update-in-place\n", "");
    std::fs::write(workspace.config(), config).unwrap();

    workspace.deploy(CI_KEY).expect_ok("first deploy");
    workspace.write("index.js", b"console.log('v2');\n");
    workspace.deploy(CI_KEY).expect_ok("second deploy");

    // ADMIN_KEY holds only create-project, so reading history uses the CI key.
    let reader = server.client(CI_KEY);
    let listed = reader.ok("listDeployments", json!({ "projectName": "basic-app" }));
    let names: Vec<String> = listed["deployments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["deploy_name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 2);
    let active = listed["activeDeployName"].as_str().unwrap().to_string();
    let previous = names.iter().find(|name| **name != active).unwrap().clone();

    workspace
        .run(CI_KEY, &["rollback", "deploy.qc", &previous])
        .expect_ok("rollback");

    let listed = reader.ok("listDeployments", json!({ "projectName": "basic-app" }));
    assert_eq!(listed["activeDeployName"], previous);
}

// ---------------------------------------------------------------------------
// Authorization, from the client's side of the wire
// ---------------------------------------------------------------------------

#[test]
fn deploy_run_fails_when_the_key_does_not_hold_the_projects_resource() {
    let root = TempRoot::new("cli-denied");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());
    create_project(&server.client(ADMIN_KEY), "basic-app", RESOURCE);

    let run = workspace
        .deploy(OTHER_KEY)
        .expect_failure("deploy run with a key for another resource");
    // The denial names the scope it checked, which is what makes a mistyped
    // resource obvious at first use rather than a wall of bare 401s.
    assert!(
        run.output()
            .contains("denied: this key does not hold basic-staging:deploy"),
        "the CLI should surface the denial: {}",
        run.output()
    );
    assert!(!server.project_dir("basic-app").join("index.js").exists());
}

/// Half a deploy is worse than none: once the upload path is denied, nothing
/// from the intruder may be left behind for a later, legitimate deploy to
/// activate.
#[test]
fn a_denied_key_cannot_upload_into_someone_elses_deployment() {
    let root = TempRoot::new("cli-denied-upload");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());
    create_project(&server.client(ADMIN_KEY), "basic-app", RESOURCE);
    workspace.deploy(CI_KEY).expect_ok("legitimate deploy");

    // The intruder holds a valid key — for a different resource.
    let intruder = server.client(OTHER_KEY);
    let listed = server
        .client(CI_KEY)
        .ok("listDeployments", json!({ "projectName": "basic-app" }));
    let deploy_name = listed["activeDeployName"].as_str().unwrap();

    intruder.denied(
        "uploadOneFile",
        json!({
            "deployName": deploy_name,
            "relPath": "index.js",
            "contentBase64": base64_of(b"console.log('pwned');\n"),
        }),
    );

    assert!(read_file(&server.project_dir("basic-app").join("index.js")).contains("v1"));
}

#[test]
fn create_project_needs_the_instance_administration_action() {
    let root = TempRoot::new("cli-create-project-denied");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());

    let run = workspace
        .run(
            CI_KEY,
            &[
                "create-project",
                "basic-app",
                "--resource",
                RESOURCE,
                "--override-dest",
                &server.url(),
            ],
        )
        .expect_failure("create-project with a project-scoped key");
    assert!(
        run.output()
            .contains("denied: this key does not hold deploy-test:create-project"),
        "{}",
        run.output()
    );
}

#[test]
fn deploying_an_unregistered_project_is_refused() {
    let root = TempRoot::new("cli-unregistered");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());

    // No create-project call at all.
    let run = workspace
        .deploy(CI_KEY)
        .expect_failure("deploy run against an unregistered project");
    // No binding to name, so the denial stays bare — an unregistered project
    // must not be a way to learn anything about this instance.
    assert!(
        run.output().contains("denied: Unauthorized"),
        "{}",
        run.output()
    );
}

/// A dropped or missing key must not fall back to anything. The workspace's
/// HOME is a temp directory, so there is no `~/secrets/deploy.env` to find.
#[test]
fn deploy_run_with_no_api_key_is_refused() {
    let root = TempRoot::new("cli-no-key");
    let (_stub, server) = start(&root);
    let workspace = CliWorkspace::new(&root, "basic-app", &server.url());
    create_project(&server.client(ADMIN_KEY), "basic-app", RESOURCE);

    let run = workspace
        .run("", &["run", "deploy.qc"])
        .expect_failure("deploy run with an empty key");
    assert!(
        run.output().contains("denied: Unauthorized"),
        "{}",
        run.output()
    );
}
