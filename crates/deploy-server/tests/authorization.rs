//! End-to-end authorization tests — the reason this rewrite exists.
//!
//! Normative sources: `~/auth-center/docs/deploy-service-requirements.md`
//! (R1–R7) and docs/auth-integration.md (D1, D2). Everything here runs against
//! a real server process and a stub auth-center, so the assertions cover the
//! whole decision: transport → method table → resource resolution →
//! introspection.

mod common;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value as Json};

use common::*;

/// A CI key scoped to one resource, exactly as it would be minted for do2.
const CI_KEY: &str = "ci-staging";
/// A key scoped to a *different* resource. It is active and valid; it just has
/// no business touching the first one.
const OTHER_KEY: &str = "ci-other";
/// Holds the instance administration action, and nothing else.
const ADMIN_KEY: &str = "instance-admin";

const RESOURCE: &str = "hotlaps-staging";
const ADMIN_RESOURCE: &str = "deploy-test";

fn standard_grants() -> Vec<KeyGrant> {
    vec![
        grant(
            ADMIN_KEY,
            &[
                "deploy:*:create-project",
                // So the fixtures can be set up without a second flow.
                "deploy:hotlaps-staging:**",
            ],
        ),
        grant(CI_KEY, &["deploy:hotlaps-staging:deploy"]),
        grant(OTHER_KEY, &["deploy:unrelated-prod:deploy"]),
    ]
}

fn start(root: &TempRoot, responder: Responder) -> (StubAuthCenter, DeployServer) {
    let stub = start_stub_auth_center(responder);
    let server = DeployServer::start(
        root,
        ServerOptions {
            auth_url: Some(stub.base_url.clone()),
            admin_resource: ADMIN_RESOURCE.to_string(),
            // Legacy keys are the migration fallback; the authorization tests
            // must not be able to pass through it by accident.
            disable_legacy_keys: true,
            ..ServerOptions::default()
        },
    );
    (stub, server)
}

// ---------------------------------------------------------------------------
// The accept path
// ---------------------------------------------------------------------------

#[test]
fn a_key_holding_the_deploy_action_can_run_the_whole_flow() {
    let root = TempRoot::new("authz-accept");
    let (stub, server) = start(&root, granting(standard_grants()));

    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );

    let ci = server.client(CI_KEY);
    let big = large_content(90 * 1024);
    let files = vec![
        file("server.js", "listen(3000)\n"),
        binary_file("assets/bundle.js", big.clone()),
    ];

    // Every step of upload and activation, on a key that carries only
    // `deploy:hotlaps-staging:deploy`.
    let deploy_name = deploy(
        &ci,
        "hotlaps-api",
        &config_for("hotlaps-api", true, ""),
        &files,
    );

    let dir = server.project_dir("hotlaps-api");
    assert_eq!(read_file(&dir.join("server.js")), "listen(3000)\n");
    assert_eq!(std::fs::read(dir.join("assets/bundle.js")).unwrap(), big);

    // Every scope this flow asked about names the bound resource — never the
    // client-supplied project name.
    let asked = stub.scopes_asked();
    assert!(asked.iter().any(|s| s == "deploy:hotlaps-staging:deploy"));
    assert!(
        !asked.iter().any(|s| s.contains("hotlaps-api")),
        "the project name must never reach auth-center as a resource: {asked:?}"
    );

    assert!(!deploy_name.is_empty());
}

#[test]
fn positive_verdicts_are_cached_so_a_deploy_is_not_one_call_per_file() {
    let root = TempRoot::new("authz-cache");
    let (stub, server) = start(&root, granting(standard_grants()));
    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );

    let ci = server.client(CI_KEY);
    let files: Vec<SourceFile> = (0..40)
        .map(|i| file(&format!("f{i}.txt"), &format!("{i}\n")))
        .collect();

    stub.clear();
    deploy(
        &ci,
        "hotlaps-api",
        &config_for("hotlaps-api", true, ""),
        &files,
    );

    // 40 uploads plus create/needed/finish/verify/activate is well over 45
    // authorized calls; R5's positive cache should collapse them.
    let deploy_scope_calls = stub
        .scopes_asked()
        .iter()
        .filter(|s| *s == "deploy:hotlaps-staging:deploy")
        .count();
    assert!(
        deploy_scope_calls < 10,
        "expected the positive cache to absorb most introspections, saw {deploy_scope_calls}"
    );
}

// ---------------------------------------------------------------------------
// Defect 1: the deployName-only methods
// ---------------------------------------------------------------------------

/// Every method a caller can reach, with params that resolve. `deployName`
/// points at a real deployment of a project bound to `hotlaps-staging`, so
/// nothing here is denied for want of a resolvable resource — the only reason
/// the other-resource key can be refused is that the check actually happened.
fn every_method(deploy_name: &str) -> Vec<(&'static str, Json)> {
    let by_deploy = json!({ "deployName": deploy_name });
    vec![
        (
            "createDeployment",
            json!({
                "projectName": "hotlaps-api",
                "sourceFileManifest": [],
                "sourceFileConfig": "deploy-settings\n  project-name=hotlaps-api\n",
            }),
        ),
        (
            "addManifestFiles",
            json!({ "deployName": deploy_name, "files": [] }),
        ),
        ("finalizeManifest", by_deploy.clone()),
        ("getNeededFiles", by_deploy.clone()),
        (
            "uploadOneFile",
            json!({
                "deployName": deploy_name, "relPath": "pwned.txt", "contentBase64": "cHduZWQ=",
            }),
        ),
        (
            "startMultiPartUpload",
            json!({ "deployName": deploy_name, "relPath": "pwned.txt" }),
        ),
        (
            "uploadFilePart",
            json!({
                "deployName": deploy_name, "relPath": "pwned.txt",
                "chunkStartsAt": 0, "chunkBase64": "cHduZWQ=",
            }),
        ),
        (
            "finishMultiPartUpload",
            json!({ "deployName": deploy_name, "relPath": "pwned.txt" }),
        ),
        ("finishUploads", by_deploy.clone()),
        ("verifyDeployment", by_deploy.clone()),
        ("activateDeployment", by_deploy.clone()),
        ("listDeployments", json!({ "projectName": "hotlaps-api" })),
        ("getDeploymentTags", json!({ "projectName": "hotlaps-api" })),
        (
            "previewDeployment",
            json!({
                "projectName": "hotlaps-api", "sourceFileManifest": [], "sourceFileConfig": "",
            }),
        ),
        ("previewByDeployName", by_deploy.clone()),
        (
            "downloadFile",
            json!({ "projectName": "hotlaps-api", "relPath": "server.js" }),
        ),
        ("listDatabases", json!({ "projectName": "hotlaps-api" })),
        (
            "executeSql",
            json!({ "projectName": "hotlaps-api", "sql": "select 1" }),
        ),
        (
            "rollback",
            json!({ "projectName": "hotlaps-api", "deployName": deploy_name }),
        ),
        (
            "createProject",
            json!({
                "projectName": "hotlaps-api", "resourceName": "attacker-resource", "rebind": true,
            }),
        ),
    ]
}

/// Defect 1, stated end to end.
///
/// The upload path and `activateDeployment` carry only a `deployName`. The old
/// server sent no scope for those calls, got no `allowed` back, and its
/// `allowed ?? true` fallback accepted every active key — so any valid key from
/// any project could overwrite files in, and activate, someone else's
/// deployment. Here a key that holds `deploy:unrelated-prod:deploy` and nothing
/// on `hotlaps-staging` must be refused by all twenty methods.
#[test]
fn a_key_for_another_resource_is_denied_on_every_method_including_deploy_name_only_ones() {
    let root = TempRoot::new("authz-defect-1");
    let (stub, server) = start(&root, granting(standard_grants()));

    let admin = server.client(ADMIN_KEY);
    admin.ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );
    let files = vec![file("server.js", "v1\n")];
    let deploy_name = deploy(
        &admin,
        "hotlaps-api",
        &config_for("hotlaps-api", true, ""),
        &files,
    );

    let intruder = server.client(OTHER_KEY);
    stub.clear();
    let methods = every_method(&deploy_name);
    assert_eq!(
        methods.len(),
        20,
        "the whole method surface must be covered"
    );

    for (method, params) in methods {
        intruder.denied(method, params);
    }

    // The deployName-only methods were refused *after* resolving to the bound
    // resource and asking auth-center — not for want of a resolvable project,
    // which would make this test pass for the wrong reason.
    let asked = stub.scopes_asked();
    assert!(
        asked
            .iter()
            .filter(|scope| *scope == "deploy:hotlaps-staging:deploy")
            .count()
            >= 9,
        "every deployName-only method should have been checked against the \
         project's bound resource: {asked:?}"
    );

    // Nothing the intruder sent reached the disk, and the deployment is intact.
    let dir = server.project_dir("hotlaps-api");
    assert!(!dir.join("pwned.txt").exists());
    assert_eq!(read_file(&dir.join("server.js")), "v1\n");

    // And the project is still bound where it was: the rebind attempt failed.
    let listed = admin.ok("listDeployments", json!({ "projectName": "hotlaps-api" }));
    assert_eq!(listed["activeDeployName"], deploy_name);
}

#[test]
fn a_deploy_key_cannot_run_sql_because_the_action_is_separately_grantable() {
    let root = TempRoot::new("authz-sql");
    let (stub, server) = start(&root, granting(standard_grants()));
    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );

    let ci = server.client(CI_KEY);
    ci.ok(
        "createDeployment",
        json!({
            "projectName": "hotlaps-api",
            "sourceFileManifest": [],
            "sourceFileConfig": config_for("hotlaps-api", true, ""),
        }),
    );

    stub.clear();
    ci.denied(
        "executeSql",
        json!({ "projectName": "hotlaps-api", "sql": "select 1" }),
    );
    ci.denied("listDatabases", json!({ "projectName": "hotlaps-api" }));

    // The refusal came from auth-center answering "no" to a distinct scope, not
    // from the server failing to ask.
    assert_eq!(
        stub.scopes_asked(),
        vec![
            "deploy:hotlaps-staging:sql".to_string(),
            "deploy:hotlaps-staging:read".to_string(),
        ]
    );
}

#[test]
fn create_project_is_checked_against_the_instance_administration_resource() {
    let root = TempRoot::new("authz-admin");
    let (stub, server) = start(&root, granting(standard_grants()));

    // A key with every action on the project's own resource still cannot
    // register a project: D2 checks registration against the instance.
    server.client(CI_KEY).denied(
        "createProject",
        json!({ "projectName": "new-project", "resourceName": RESOURCE }),
    );
    assert_eq!(
        stub.scopes_asked(),
        vec!["deploy:deploy-test:create-project".to_string()]
    );

    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "new-project", "resourceName": RESOURCE }),
    );
}

// ---------------------------------------------------------------------------
// R2: nothing is allowed for want of a resource
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_deploy_name_denies_rather_than_falling_through() {
    let root = TempRoot::new("authz-unknown-deploy");
    let (stub, server) = start(&root, granting(standard_grants()));
    let ci = server.client(CI_KEY);

    for method in [
        "getNeededFiles",
        "uploadOneFile",
        "uploadFilePart",
        "finishUploads",
        "verifyDeployment",
        "activateDeployment",
        "previewByDeployName",
    ] {
        ci.denied(
            method,
            json!({
                "deployName": "no-such-deployment-9999",
                "relPath": "x.txt",
                "contentBase64": "eA==",
                "chunkStartsAt": 0,
                "chunkBase64": "eA==",
            }),
        );
        // And with no deployName at all.
        ci.denied(method, json!({}));
    }

    assert!(
        stub.scopes_asked().is_empty(),
        "a call that cannot be resolved must be denied before any scope is built"
    );
}

#[test]
fn an_unregistered_project_denies() {
    let root = TempRoot::new("authz-unregistered");
    let (_stub, server) = start(&root, granting(standard_grants()));

    server.client(CI_KEY).denied(
        "createDeployment",
        json!({
            "projectName": "never-registered",
            "sourceFileManifest": [],
            "sourceFileConfig": "",
        }),
    );
}

/// A project carried over from the old tool, which created projects implicitly
/// and had no resource column. Once auth-center checking is on, it cannot be
/// deployed to until an administrator binds it.
#[test]
fn a_project_with_no_bound_resource_denies() {
    let root = TempRoot::new("authz-unbound");

    // Phase 1: a legacy-only instance, where projects still spring into
    // existence on first deploy.
    let deploy_name;
    let state_dir;
    let deploys_dir;
    {
        let legacy_server = DeployServer::start(&root, ServerOptions::default());
        let legacy_key = legacy_server.create_legacy_key();
        let rpc = legacy_server.client(&legacy_key);
        deploy_name = deploy(
            &rpc,
            "old-project",
            &config_for("old-project", true, ""),
            &[file("app.js", "v1\n")],
        );
        state_dir = legacy_server.state_dir.clone();
        deploys_dir = legacy_server.deploys_dir.clone();
    }

    let bound: Option<String> = {
        let conn = rusqlite::Connection::open(state_dir.join("db.sqlite")).unwrap();
        conn.query_row(
            "select resource_name from project where project_name = 'old-project'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(bound, None, "the legacy path leaves the project unbound");

    // Phase 2: the same instance, now checking against auth-center.
    let stub = start_stub_auth_center(granting(standard_grants()));
    let server = DeployServer::start_with_existing_state(
        &root,
        state_dir,
        deploys_dir,
        ServerOptions {
            auth_url: Some(stub.base_url.clone()),
            admin_resource: ADMIN_RESOURCE.to_string(),
            disable_legacy_keys: true,
            ..ServerOptions::default()
        },
    );

    let ci = server.client(CI_KEY);
    ci.denied("listDeployments", json!({ "projectName": "old-project" }));
    ci.denied("activateDeployment", json!({ "deployName": deploy_name }));
    assert!(
        stub.scopes_asked().is_empty(),
        "an unbound project is refused before auth-center is consulted"
    );

    // Binding it is the migration step, and needs no `rebind` flag.
    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "old-project", "resourceName": RESOURCE }),
    );
    server
        .client(CI_KEY)
        .ok("activateDeployment", json!({ "deployName": deploy_name }));
}

#[test]
fn a_call_with_no_api_key_is_denied() {
    let root = TempRoot::new("authz-no-key");
    let (_stub, server) = start(&root, granting(standard_grants()));
    server
        .anonymous_client()
        .denied("listDeployments", json!({ "projectName": "hotlaps-api" }));
}

#[test]
fn an_unknown_method_is_denied_rather_than_left_unguarded() {
    let root = TempRoot::new("authz-unknown-method");
    let (_stub, server) = start(&root, granting(standard_grants()));
    server
        .client(ADMIN_KEY)
        .denied("deleteEverything", json!({ "projectName": "hotlaps-api" }));
}

// ---------------------------------------------------------------------------
// R4: fail closed
// ---------------------------------------------------------------------------

/// Sets up an instance whose auth-center answers with `reply` for every
/// introspection, with one project registered beforehand by a working
/// auth-center.
fn instance_with_failing_auth(
    root: &TempRoot,
    reply: impl Fn() -> StubReply + Send + Sync + 'static,
) -> DeployServer {
    // Registration and one deployment first, so later denials cannot be blamed
    // on an unresolvable project.
    let (_stub, setup_server) = start(root, granting(standard_grants()));
    let admin = setup_server.client(ADMIN_KEY);
    admin.ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );
    deploy(
        &admin,
        "hotlaps-api",
        &config_for("hotlaps-api", true, ""),
        &[file("app.js", "v1\n")],
    );

    let state_dir = setup_server.state_dir.clone();
    let deploys_dir = setup_server.deploys_dir.clone();
    drop(setup_server);

    let broken = start_stub_auth_center(Arc::new(move |_: &Json| reply()));
    DeployServer::start_with_existing_state(
        root,
        state_dir,
        deploys_dir,
        ServerOptions {
            auth_url: Some(broken.base_url.clone()),
            admin_resource: ADMIN_RESOURCE.to_string(),
            disable_legacy_keys: true,
            ..ServerOptions::default()
        },
    )
    // The stub is leaked deliberately: it must outlive this function, and its
    // listener thread ends with the test process.
}

#[test]
fn a_non_2xx_from_auth_center_denies() {
    let root = TempRoot::new("authz-500");
    let server = instance_with_failing_auth(&root, || StubReply::json(500, r#"{"error":"boom"}"#));
    server
        .client(CI_KEY)
        .denied("listDeployments", json!({ "projectName": "hotlaps-api" }));
}

/// The exact shape a scope-less introspect returns, and the exact shape the old
/// server's `allowed ?? true` read as permission granted.
#[test]
fn a_response_without_allowed_denies() {
    let root = TempRoot::new("authz-no-allowed");
    let server = instance_with_failing_auth(&root, || {
        StubReply::json(
            200,
            r#"{"active":true,"key_id":"key_1","scopes":["deploy:**"]}"#,
        )
    });
    server.client(CI_KEY).denied(
        "createDeployment",
        json!({
            "projectName": "hotlaps-api",
            "sourceFileManifest": [],
            "sourceFileConfig": "",
        }),
    );
}

#[test]
fn an_unparseable_body_denies() {
    let root = TempRoot::new("authz-garbage");
    let server = instance_with_failing_auth(&root, || StubReply::json(200, "<html>nope</html>"));
    server
        .client(CI_KEY)
        .denied("listDeployments", json!({ "projectName": "hotlaps-api" }));
}

#[test]
fn a_timeout_denies() {
    let root = TempRoot::new("authz-timeout");
    // The server gives auth-center 5 seconds (R4); stall past that.
    let server = instance_with_failing_auth(&root, || {
        StubReply::json(200, r#"{"active":true,"allowed":true,"key_id":"k"}"#)
            .after(Duration::from_secs(8))
    });
    server
        .client(CI_KEY)
        .denied("listDeployments", json!({ "projectName": "hotlaps-api" }));
}

#[test]
fn an_unreachable_auth_center_denies() {
    let root = TempRoot::new("authz-unreachable");
    let (_stub, setup_server) = start(&root, granting(standard_grants()));
    setup_server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );
    let state_dir = setup_server.state_dir.clone();
    let deploys_dir = setup_server.deploys_dir.clone();
    drop(setup_server);

    let server = DeployServer::start_with_existing_state(
        &root,
        state_dir,
        deploys_dir,
        ServerOptions {
            // Port 1 is reserved; nothing listens there.
            auth_url: Some("http://127.0.0.1:1".to_string()),
            admin_resource: ADMIN_RESOURCE.to_string(),
            disable_legacy_keys: true,
            ..ServerOptions::default()
        },
    );

    server
        .client(CI_KEY)
        .denied("listDeployments", json!({ "projectName": "hotlaps-api" }));
}

// ---------------------------------------------------------------------------
// The staging/production separation from docs/project-goals.md
// ---------------------------------------------------------------------------

/// `api-staging.qc` and `api-prod.qc` both say `project-name=hotlaps-api`; they
/// differ only in `dest-url`. Because the resource binding lives in each
/// instance's own database, the same project name gates on different resources
/// on do2 and dohl — and a staging key presented to dohl is denied.
#[test]
fn the_same_project_name_on_two_instances_gives_different_verdicts() {
    let staging_root = TempRoot::new("authz-do2");
    let prod_root = TempRoot::new("authz-dohl");

    let stub = start_stub_auth_center(granting(vec![
        grant(ADMIN_KEY, &["deploy:*:create-project"]),
        grant("staging-ci", &["deploy:hotlaps-staging:deploy"]),
        grant("prod-ci", &["deploy:hotlaps-prod:deploy"]),
    ]));

    let do2 = DeployServer::start(
        &staging_root,
        ServerOptions {
            auth_url: Some(stub.base_url.clone()),
            admin_resource: "deploy-do2".to_string(),
            disable_legacy_keys: true,
            ..ServerOptions::default()
        },
    );
    let dohl = DeployServer::start(
        &prod_root,
        ServerOptions {
            auth_url: Some(stub.base_url.clone()),
            admin_resource: "deploy-dohl".to_string(),
            disable_legacy_keys: true,
            ..ServerOptions::default()
        },
    );

    do2.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
    );
    dohl.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-prod" }),
    );

    let create = json!({
        "projectName": "hotlaps-api",
        "sourceFileManifest": [],
        "sourceFileConfig": config_for("hotlaps-api", true, ""),
    });

    // Same key, same project name, opposite verdicts.
    do2.client("staging-ci")
        .ok("createDeployment", create.clone());
    dohl.client("staging-ci")
        .denied("createDeployment", create.clone());

    dohl.client("prod-ci")
        .ok("createDeployment", create.clone());
    do2.client("prod-ci").denied("createDeployment", create);
}

// ---------------------------------------------------------------------------
// R6: legacy keys
// ---------------------------------------------------------------------------

/// Rollout step 3: auth-center is on, but the local `secret_key` table is still
/// accepted so existing keys keep working while they are migrated.
#[test]
fn a_legacy_key_is_accepted_while_the_table_is_enabled_and_stamps_last_used_at() {
    let root = TempRoot::new("legacy-enabled");

    // An auth-center that denies everything except registration, so a legacy
    // key that works can only have worked through the local table.
    let stub = start_stub_auth_center(granting(vec![grant(
        ADMIN_KEY,
        &["deploy:deploy-test:create-project"],
    )]));
    let server = DeployServer::start(
        &root,
        ServerOptions {
            auth_url: Some(stub.base_url.clone()),
            admin_resource: ADMIN_RESOURCE.to_string(),
            disable_legacy_keys: false,
            ..ServerOptions::default()
        },
    );

    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );

    let legacy_key = server.create_legacy_key();
    let before: Option<String> = server
        .open_db()
        .query_row("select last_used_at from secret_key", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, None, "a fresh key has never been used");

    let rpc = server.client(&legacy_key);
    let deploy_name = deploy(
        &rpc,
        "hotlaps-api",
        &config_for("hotlaps-api", true, ""),
        &[file("app.js", "v1\n")],
    );
    assert_eq!(
        read_file(&server.project_dir("hotlaps-api").join("app.js")),
        "v1\n"
    );

    // R6: `deploy-server list-legacy-keys` needs this to report what still has
    // to migrate.
    let after: Option<String> = server
        .open_db()
        .query_row("select last_used_at from secret_key", [], |row| row.get(0))
        .unwrap();
    assert!(after.is_some(), "last_used_at should have been stamped");

    // R7: the attribution namespace makes it obvious this went through the
    // table rather than auth-center.
    let listed = rpc.ok("listDeployments", json!({ "projectName": "hotlaps-api" }));
    let entry = listed["deployments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["deploy_name"] == deploy_name.as_str())
        .unwrap();
    assert!(entry["authorized_by_key_id"]
        .as_str()
        .unwrap()
        .starts_with("legacy:"));

    // An unknown key is still refused: the table is a list, not a bypass.
    server
        .client("not-a-key")
        .denied("listDeployments", json!({ "projectName": "hotlaps-api" }));
}

/// Rollout step 5: once an instance's keys have migrated, the table is turned
/// off and the keys still sitting in it stop working.
#[test]
fn a_legacy_key_is_rejected_once_the_table_is_disabled() {
    let root = TempRoot::new("legacy-disabled");
    let stub = start_stub_auth_center(granting(standard_grants()));

    let legacy_key;
    let state_dir;
    let deploys_dir;
    {
        let server = DeployServer::start(
            &root,
            ServerOptions {
                auth_url: Some(stub.base_url.clone()),
                admin_resource: ADMIN_RESOURCE.to_string(),
                disable_legacy_keys: false,
                ..ServerOptions::default()
            },
        );
        server.client(ADMIN_KEY).ok(
            "createProject",
            json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
        );
        legacy_key = server.create_legacy_key();
        server
            .client(&legacy_key)
            .ok("listDeployments", json!({ "projectName": "hotlaps-api" }));
        state_dir = server.state_dir.clone();
        deploys_dir = server.deploys_dir.clone();
    }

    let server = DeployServer::start_with_existing_state(
        &root,
        state_dir,
        deploys_dir,
        ServerOptions {
            auth_url: Some(stub.base_url.clone()),
            admin_resource: ADMIN_RESOURCE.to_string(),
            disable_legacy_keys: true,
            ..ServerOptions::default()
        },
    );

    // The row is still in the table; it is simply no longer honored.
    server
        .client(&legacy_key)
        .denied("listDeployments", json!({ "projectName": "hotlaps-api" }));
    // An auth-center key on the same instance still works.
    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": RESOURCE }),
    );
}
