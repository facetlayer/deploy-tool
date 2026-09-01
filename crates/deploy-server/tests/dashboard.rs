//! The web dashboard: sign-in, session handling, and the `admin-read` reads.
//!
//! The dashboard is a second front door onto the same server, so the thing
//! worth testing is that it is not a second *authorization path*. Every case
//! below drives the real server process against the stub auth-center, exactly
//! as `authorization.rs` does for API keys.

mod common;

use serde_json::{json, Value as Json};

use common::*;

const ADMIN_RESOURCE: &str = "deploy-test";
/// Holds the instance administration actions, including the dashboard's read.
const ADMIN_KEY: &str = "instance-admin";
/// A perfectly good project key. It can deploy, and it must still not be able
/// to enumerate the instance.
const PROJECT_KEY: &str = "ci-staging";
/// Stands in for the OAuth access token a dashboard session carries.
const DASHBOARD_TOKEN: &str = "at_dashboard_user";

fn grants() -> Vec<KeyGrant> {
    vec![
        grant(ADMIN_KEY, &["deploy-test:**", "hotlaps-staging:**"]),
        grant(
            PROJECT_KEY,
            &["hotlaps-staging:deploy", "hotlaps-staging:read"],
        ),
        grant(DASHBOARD_TOKEN, &["deploy-test:admin-read"]),
    ]
}

fn dashboard_env() -> Vec<(String, String)> {
    vec![
        ("DEPLOY_PUBLIC_URL".into(), "https://deploy.test".into()),
        ("DEPLOY_OAUTH_CLIENT_ID".into(), "oc_test".into()),
        ("DEPLOY_OAUTH_CLIENT_SECRET".into(), "cs_test".into()),
    ]
}

fn start(root: &TempRoot, extra_env: Vec<(String, String)>) -> (StubAuthCenter, DeployServer) {
    let stub = start_stub_auth_center(granting(grants()));
    let server = DeployServer::start(
        root,
        ServerOptions {
            auth_url: stub.base_url.clone(),
            admin_resource: ADMIN_RESOURCE.to_string(),
            extra_env,
            ..ServerOptions::default()
        },
    );
    (stub, server)
}

/// A raw GET that does not follow redirects, so a 302 can be asserted on.
fn get(server: &DeployServer, path: &str, cookie: Option<&str>) -> (u16, String, String) {
    let mut request = ureq::builder()
        .redirects(0)
        .build()
        .get(&format!("http://127.0.0.1:{}{path}", server.port));
    if let Some(cookie) = cookie {
        request = request.set("cookie", cookie);
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("request to {path} failed: {error}"),
    };
    let status = response.status();
    let location = response.header("location").unwrap_or("").to_string();
    let body = response.into_string().unwrap_or_default();
    (status, location, body)
}

/// Files a session row straight into the server's database, standing in for a
/// completed OAuth callback. The exchange itself is auth-center's to test; what
/// matters here is what a session is allowed to do once it exists.
fn plant_session(server: &DeployServer, cookie_value: &str, token: &str, expires_in: i64) {
    use sha2::{Digest, Sha256};
    let conn = rusqlite::Connection::open(server.state_dir.join("db.sqlite")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
        "insert or replace into dashboard_session
           (session_id, access_token, username, subject, created_at, expires_at)
         values (?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            hex::encode(Sha256::digest(cookie_value.as_bytes())),
            token,
            "andy",
            "7",
            "2026-08-31T00:00:00Z",
            now + expires_in
        ],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------

#[test]
fn the_dashboard_api_refuses_a_caller_with_no_session() {
    let root = TempRoot::new("dash-anon");
    let (_stub, server) = start(&root, dashboard_env());

    for path in [
        "/dashboard/api/me",
        "/dashboard/api/projects",
        "/dashboard/api/projects/anything",
    ] {
        let (status, _, _) = get(&server, path, None);
        assert_eq!(status, 401, "{path} answered an anonymous caller");
    }
}

#[test]
fn login_redirects_to_auth_center_with_pkce_and_the_admin_read_scope() {
    let root = TempRoot::new("dash-login");
    let (stub, server) = start(&root, dashboard_env());

    let (status, location, _) = get(&server, "/oauth/login", None);
    assert_eq!(status, 303, "expected a redirect to auth-center");
    assert!(
        location.starts_with(&format!("{}/oauth/authorize", stub.base_url)),
        "{location}"
    );
    assert!(location.contains("client_id=oc_test"), "{location}");
    assert!(
        location.contains("code_challenge_method=S256"),
        "{location}"
    );
    // The redirect URI is derived from DEPLOY_PUBLIC_URL and has to match what
    // auth-center registered, exactly.
    assert!(
        location.contains(&urlencoding::encode("https://deploy.test/oauth/callback").into_owned()),
        "{location}"
    );
    assert!(
        location.contains(&urlencoding::encode("deploy-test:admin-read").into_owned()),
        "{location}"
    );
}

#[test]
fn a_callback_with_a_state_this_server_never_issued_is_refused() {
    let root = TempRoot::new("dash-callback-state");
    let (_stub, server) = start(&root, dashboard_env());

    // Without this check, a callback URL from anywhere would sign the visitor
    // in as whoever the attacker holds a code for.
    let (status, _, body) = get(&server, "/oauth/callback?code=abc&state=never-issued", None);
    assert_eq!(status, 403);
    assert!(body.contains("expired or was already used"), "{body}");
    // And no session cookie came back with it.
    assert!(!body.contains("deploy_session"));
}

#[test]
fn an_instance_with_no_dashboard_configuration_serves_no_dashboard() {
    let root = TempRoot::new("dash-off");
    // No dashboard env at all: the deploy API still works, the dashboard is
    // simply not there. This is every existing instance.
    let (_stub, server) = start(&root, Vec::new());

    for path in ["/", "/oauth/login", "/dashboard/api/projects"] {
        let (status, _, _) = get(&server, path, None);
        assert!(
            status == 404 || status == 401,
            "{path} answered {status} on an instance with no dashboard"
        );
    }

    // The JSON-RPC surface is untouched by any of it.
    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
    );
}

// ---------------------------------------------------------------------------
// What a session may read
// ---------------------------------------------------------------------------

/// The SPA is compiled into the binary, so a built server serves the real page
/// rather than depending on a second static deploy of its own dashboard.
#[test]
fn the_spa_is_served_from_the_binary() {
    let root = TempRoot::new("dash-spa");
    let (_stub, server) = start(&root, dashboard_env());

    let (status, _, body) = get(&server, "/", None);
    assert_eq!(status, 200);
    assert!(body.contains("<div id=\"root\">"), "{body}");
    // A hash-routed deep link has no server-side route; it must still land on
    // the app rather than a 404.
    let (status, _, _) = get(&server, "/projects/hotlaps-api", None);
    assert_eq!(status, 200);
}

#[test]
fn a_session_reads_every_project_on_the_instance() {
    let root = TempRoot::new("dash-projects");
    let (_stub, server) = start(&root, dashboard_env());

    let admin = server.client(ADMIN_KEY);
    admin.ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
    );
    admin.ok(
        "createProject",
        json!({ "projectName": "hotlaps-web", "resourceName": "hotlaps-staging" }),
    );

    plant_session(&server, "cookie-a", DASHBOARD_TOKEN, 3600);
    let (status, _, body) = get(
        &server,
        "/dashboard/api/projects",
        Some("deploy_session=cookie-a"),
    );
    assert_eq!(status, 200, "{body}");

    let payload: Json = serde_json::from_str(&body).unwrap();
    let projects = payload["projects"].as_array().unwrap();
    let names: Vec<&str> = projects
        .iter()
        .map(|p| p["projectName"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["hotlaps-api", "hotlaps-web"]);
    // The binding is what the dashboard exists to make visible.
    assert_eq!(projects[0]["resourceName"], "hotlaps-staging");
    assert_eq!(projects[0]["deploymentCount"], 0);
}

#[test]
fn a_session_can_read_one_projects_history() {
    let root = TempRoot::new("dash-project-detail");
    let (_stub, server) = start(&root, dashboard_env());

    let admin = server.client(ADMIN_KEY);
    admin.ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
    );
    let files = vec![file("server.js", "listen(3000)\n")];
    let deploy_name = deploy(
        &server.client(PROJECT_KEY),
        "hotlaps-api",
        &config_for("hotlaps-api", true, ""),
        &files,
    );

    plant_session(&server, "cookie-b", DASHBOARD_TOKEN, 3600);
    let (status, _, body) = get(
        &server,
        "/dashboard/api/projects/hotlaps-api",
        Some("deploy_session=cookie-b"),
    );
    assert_eq!(status, 200, "{body}");

    let payload: Json = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["project"]["projectName"], "hotlaps-api");
    assert_eq!(payload["project"]["activeDeployName"], deploy_name);
    let deployments = payload["deployments"].as_array().unwrap();
    assert_eq!(deployments.len(), 1);
    assert_eq!(deployments[0]["deploy_name"], deploy_name);
    assert_eq!(deployments[0]["is_active"], true);
    // R7 attribution reaches the dashboard: this is the "who shipped this"
    // column, and it is the project key that shipped it, not the admin key.
    assert_eq!(deployments[0]["authorized_by_key_name"], PROJECT_KEY);
}

#[test]
fn an_expired_session_is_signed_out_rather_than_served() {
    let root = TempRoot::new("dash-expired");
    let (_stub, server) = start(&root, dashboard_env());

    plant_session(&server, "cookie-old", DASHBOARD_TOKEN, -60);
    let (status, _, body) = get(
        &server,
        "/dashboard/api/projects",
        Some("deploy_session=cookie-old"),
    );
    assert_eq!(status, 401);
    assert!(body.contains("expired"), "{body}");
}

// ---------------------------------------------------------------------------
// What a session may NOT do
// ---------------------------------------------------------------------------

/// The point of a separate `admin-read` action. A project key is real, active
/// and able to deploy; it still must not enumerate the instance, because
/// `read` is per-project by construction and this is not a per-project call.
#[test]
fn a_project_key_cannot_read_the_instance() {
    let root = TempRoot::new("dash-project-key");
    let (stub, server) = start(&root, dashboard_env());

    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
    );

    server.client(PROJECT_KEY).denied("listProjects", json!({}));
    assert!(
        stub.scopes_asked()
            .contains(&"deploy-test:admin-read".to_string()),
        "listProjects must be checked against the instance resource, not a project: {:?}",
        stub.scopes_asked()
    );

    // Through the dashboard, the same key gets the same answer.
    plant_session(&server, "cookie-c", PROJECT_KEY, 3600);
    let (status, _, _) = get(
        &server,
        "/dashboard/api/projects",
        Some("deploy_session=cookie-c"),
    );
    assert_eq!(status, 401);
}

/// A dashboard session holds `admin-read` and nothing else, so it must not be
/// able to reach any method that writes — through the JSON-RPC surface either.
#[test]
fn a_dashboard_token_cannot_deploy_or_run_sql() {
    let root = TempRoot::new("dash-readonly");
    let (_stub, server) = start(&root, dashboard_env());

    server.client(ADMIN_KEY).ok(
        "createProject",
        json!({ "projectName": "hotlaps-api", "resourceName": "hotlaps-staging" }),
    );

    let dashboard = server.client(DASHBOARD_TOKEN);
    for (method, params) in [
        ("createDeployment", json!({ "projectName": "hotlaps-api" })),
        (
            "executeSql",
            json!({ "projectName": "hotlaps-api", "sql": "select 1" }),
        ),
        (
            "rollback",
            json!({ "projectName": "hotlaps-api", "deployName": "whatever" }),
        ),
        (
            "createProject",
            json!({ "projectName": "sneaky", "resourceName": "hotlaps-staging" }),
        ),
    ] {
        dashboard.denied(method, params);
    }

    // And it is still allowed to do the one thing it is for.
    dashboard.ok("listProjects", json!({}));
}
