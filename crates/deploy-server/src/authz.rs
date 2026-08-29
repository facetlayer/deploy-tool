//! The authorization decision for one JSON-RPC call (R2).
//!
//! Every call resolves to a concrete resource and is checked against it:
//!
//! 1. Resolve the call to a project — `projectName` directly, or `deployName`
//!    joined through the `deployment` table, or the instance administration
//!    resource for `createProject`.
//! 2. Look up `project.resource_name`.
//! 3. Introspect the presented key against `deploy:<resource>:<action>`.
//! 4. Deny on any failure.
//!
//! There is deliberately no branch that allows a call because no resource could
//! be determined. That branch is what made the old server's entire upload and
//! activation path unguarded.

use deploy_core::rpc::{lookup_method, MethodSpec, ProjectResolution};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as Json;

use crate::auth_center::{AuthCenter, Introspection};
use crate::db;
// The handlers take this type, so it lives with the state they are given rather
// than here; re-exported so callers of `authorize` need only this module.
pub use crate::state::AuthorizedKey;

/// Everything the decision needs. Deliberately narrower than `AppState` so the
/// decision can be unit-tested against a bare in-memory database.
pub struct AuthzContext<'a> {
    pub conn: &'a Connection,
    /// `None` means legacy-only: no auth-center is configured on this instance.
    pub auth: Option<&'a AuthCenter>,
    /// `serve --disable-api-key-check`. Development only.
    pub disable_api_key_check: bool,
}

/// R7 attribution for a local `secret_key` row. The `legacy:` namespace cannot
/// collide with a real auth-center key id, and the row's label is carried
/// through so history can say which laptop key shipped a deployment.
fn legacy_attribution(row_id: i64, label: Option<String>) -> AuthorizedKey {
    AuthorizedKey::new(format!("legacy:{row_id}"), label)
}

/// Used only under `--disable-api-key-check`, so history still shows that the
/// deployment went through a server that was not checking anything.
fn unchecked_attribution() -> AuthorizedKey {
    AuthorizedKey::new("api-key-check-disabled", None)
}

/// Why a call was refused. Every variant denies; they are separate so the
/// journal distinguishes a bad key from an unreachable auth-center (R4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Denial {
    MissingKey,
    /// Not in `METHOD_TABLE`. An unknown method has no action and no
    /// resolution, so it cannot be checked and therefore cannot be allowed.
    UnknownMethod(String),
    /// The call could not be resolved to a bound resource.
    Unresolved(String),
    /// auth-center answered, and the answer was no.
    NotAuthorized(String),
    /// auth-center could not be asked.
    AuthUnavailable(String),
    /// The key is not in the local table and there is no auth-center to ask.
    NoAuthConfigured,
}

impl Denial {
    pub fn reason(&self) -> String {
        match self {
            Denial::MissingKey => "no x-api-key header was presented".to_string(),
            Denial::UnknownMethod(method) => {
                format!("method '{method}' has no entry in the authorization table")
            }
            Denial::Unresolved(detail) => detail.clone(),
            Denial::NotAuthorized(detail) => detail.clone(),
            Denial::AuthUnavailable(detail) => detail.clone(),
            Denial::NoAuthConfigured => {
                "key is not a local secret key and DEPLOY_AUTH_URL is not set".to_string()
            }
        }
    }
}

pub type Decision = Result<AuthorizedKey, Denial>;

/// True unless the operator has turned the local `secret_key` table off for
/// this instance (R6).
pub fn legacy_keys_enabled() -> bool {
    !matches!(std::env::var("DEPLOY_DISABLE_LEGACY_KEYS"), Ok(value) if value.trim() == "1")
}

pub fn authorize(
    ctx: &AuthzContext,
    api_key: Option<&str>,
    method: &str,
    params: &Json,
) -> Decision {
    if ctx.disable_api_key_check {
        return Ok(unchecked_attribution());
    }

    let api_key = match api_key {
        Some(key) if !key.is_empty() => key,
        _ => return Err(Denial::MissingKey),
    };

    // Checked first so a legacy key costs no network call (R6). A legacy key
    // grants everything, which is exactly why the table has to go away.
    if legacy_keys_enabled() {
        if let Some(authorized) = check_legacy_key(ctx.conn, api_key) {
            return Ok(authorized);
        }
    }

    let Some(spec) = lookup_method(method) else {
        return Err(Denial::UnknownMethod(method.to_string()));
    };

    let Some(auth) = ctx.auth else {
        return Err(Denial::NoAuthConfigured);
    };

    let resource = resolve_resource(ctx.conn, spec, params, auth)?;

    match auth.introspect(api_key, &resource, spec.action) {
        Introspection::Allowed(identity) => Ok(AuthorizedKey::new(identity.key_id, identity.name)),
        Introspection::Denied { detail } => Err(Denial::NotAuthorized(detail)),
        Introspection::Unavailable { detail } => Err(Denial::AuthUnavailable(detail)),
    }
}

/// Looks the key up in the local table and, on a hit, stamps `last_used_at` so
/// `deploy-server list-legacy-keys` can show which keys still have to migrate.
fn check_legacy_key(conn: &Connection, api_key: &str) -> Option<AuthorizedKey> {
    let row: Option<(i64, Option<String>)> = conn
        .query_row(
            "select key_id, label from secret_key where key_text = ?",
            params![api_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap_or(None);

    let (key_id, label) = row?;

    // Best effort: failing to record the timestamp must not fail the call.
    if let Err(err) = conn.execute(
        "update secret_key set last_used_at = ? where key_id = ?",
        params![db::now_iso(), key_id],
    ) {
        eprintln!("[deploy warning] could not stamp secret_key.last_used_at: {err}");
    }

    Some(legacy_attribution(key_id, label))
}

/// Step 1 and 2 of R2. Every failure here is a denial.
fn resolve_resource(
    conn: &Connection,
    spec: &MethodSpec,
    params: &Json,
    auth: &AuthCenter,
) -> Result<String, Denial> {
    let project_name = match spec.resolution {
        ProjectResolution::InstanceAdministration => {
            // D2: administration is checked against the instance, not a
            // project. There is no default and no derivation from the hostname.
            return Ok(auth.admin_resource().to_string());
        }
        ProjectResolution::ByProjectName => required_param(params, "projectName", spec)?,
        ProjectResolution::ByDeployName => {
            let deploy_name = required_param(params, "deployName", spec)?;
            project_of_deployment(conn, &deploy_name, spec)?
        }
    };

    resource_of_project(conn, &project_name, spec)
}

fn required_param(params: &Json, field: &str, spec: &MethodSpec) -> Result<String, Denial> {
    match params.get(field).and_then(Json::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(Denial::Unresolved(format!(
            "{} requires a non-empty '{field}' to resolve a resource",
            spec.name
        ))),
    }
}

/// The join that fixes Defect 1: the upload and activation methods carry only a
/// deploy name, and that name is what ties them to a project.
fn project_of_deployment(
    conn: &Connection,
    deploy_name: &str,
    spec: &MethodSpec,
) -> Result<String, Denial> {
    conn.query_row(
        "select project_name from deployment where deploy_name = ?",
        params![deploy_name],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .unwrap_or(None)
    .filter(|name| !name.is_empty())
    .ok_or_else(|| {
        Denial::Unresolved(format!(
            "{}: no deployment named '{deploy_name}'",
            spec.name
        ))
    })
}

fn resource_of_project(
    conn: &Connection,
    project_name: &str,
    spec: &MethodSpec,
) -> Result<String, Denial> {
    let row: Option<Option<String>> = conn
        .query_row(
            "select resource_name from project where project_name = ?",
            params![project_name],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .unwrap_or(None);

    match row {
        None => Err(Denial::Unresolved(format!(
            "{}: project '{project_name}' is not registered on this instance",
            spec.name
        ))),
        // A project that predates registration has no binding. It cannot be
        // deployed to until an administrator runs `deploy create-project`.
        Some(None) => Err(Denial::Unresolved(format!(
            "{}: project '{project_name}' has no bound auth-center resource; \
             run `deploy create-project {project_name} --resource <name>`",
            spec.name
        ))),
        Some(Some(resource)) if resource.trim().is_empty() => Err(Denial::Unresolved(format!(
            "{}: project '{project_name}' has an empty resource binding",
            spec.name
        ))),
        Some(Some(resource)) => Ok(resource),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_center::tests::{start_stub, StubReply};
    use crate::auth_center::{AuthCenterConfig, ResourceCheck};
    use deploy_core::rpc::{methods, ProjectResolution, METHOD_TABLE};
    use serde_json::json;
    use std::time::Duration;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // WAL is not available in-memory; everything else in init_connection is.
        conn.execute_batch(
            r#"create table project(project_name text primary key, created_at datetime not null,
                                    resource_name text, resource_bound_at datetime);
               create table deployment(deploy_name text primary key, deploy_dir text,
                                       project_name text not null, created_at datetime);
               create table secret_key(key_id integer primary key autoincrement,
                                       key_text text not null, created_at datetime not null,
                                       last_used_at datetime, label text);"#,
        )
        .unwrap();
        conn
    }

    fn register(conn: &Connection, project: &str, resource: Option<&str>) {
        conn.execute(
            "insert into project (project_name, created_at, resource_name) values (?, ?, ?)",
            params![project, db::now_iso(), resource],
        )
        .unwrap();
    }

    fn add_deployment(conn: &Connection, deploy_name: &str, project: &str) {
        conn.execute(
            "insert into deployment (deploy_name, project_name, created_at) values (?, ?, ?)",
            params![deploy_name, project, db::now_iso()],
        )
        .unwrap();
    }

    fn allow_everything() -> (crate::auth_center::tests::StubAuthCenter, AuthCenter) {
        let stub = start_stub(|_, _, _| {
            StubReply::json(
                200,
                r#"{"active":true,"allowed":true,"key_id":"key_1","name":"ci"}"#,
            )
        });
        let auth = auth_for(&stub);
        (stub, auth)
    }

    fn auth_for(stub: &crate::auth_center::tests::StubAuthCenter) -> AuthCenter {
        AuthCenter::with_timeout(
            AuthCenterConfig {
                base_url: stub.base_url.clone(),
                service_key: "service-key".to_string(),
                admin_resource: "deploy-test".to_string(),
            },
            Duration::from_millis(500),
        )
    }

    fn ctx<'a>(conn: &'a Connection, auth: Option<&'a AuthCenter>) -> AuthzContext<'a> {
        AuthzContext {
            conn,
            auth,
            disable_api_key_check: false,
        }
    }

    // -- the accept path --------------------------------------------------

    #[test]
    fn a_key_holding_the_action_is_allowed_and_identified() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        let (stub, auth) = allow_everything();

        let decision = authorize(
            &ctx(&conn, Some(&auth)),
            Some("presented"),
            methods::CREATE_DEPLOYMENT,
            &json!({ "projectName": "hotlaps-api" }),
        );

        let key = decision.expect("should be allowed");
        assert_eq!(key.key_id, "key_1");
        assert_eq!(key.key_name.as_deref(), Some("ci"));

        let sent = stub.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["scope"], "deploy:hotlaps-staging:deploy");
        assert_eq!(sent[0]["token"], "presented");
    }

    #[test]
    fn deploy_name_methods_resolve_through_the_deployment_table() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        add_deployment(&conn, "hotlaps-api-7", "hotlaps-api");
        let (stub, auth) = allow_everything();

        authorize(
            &ctx(&conn, Some(&auth)),
            Some("presented"),
            methods::ACTIVATE_DEPLOYMENT,
            &json!({ "deployName": "hotlaps-api-7" }),
        )
        .expect("should be allowed");

        assert_eq!(stub.requests()[0]["scope"], "deploy:hotlaps-staging:deploy");
    }

    #[test]
    fn create_project_is_checked_against_the_instance_administration_resource() {
        let conn = test_db();
        let (stub, auth) = allow_everything();

        authorize(
            &ctx(&conn, Some(&auth)),
            Some("presented"),
            methods::CREATE_PROJECT,
            &json!({ "projectName": "new", "resourceName": "whatever" }),
        )
        .expect("should be allowed");

        assert_eq!(
            stub.requests()[0]["scope"],
            "deploy:deploy-test:create-project"
        );
    }

    #[test]
    fn the_action_is_part_of_the_scope_so_sql_is_separately_grantable() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        let (stub, auth) = allow_everything();

        for (method, expected) in [
            (methods::LIST_DEPLOYMENTS, "deploy:hotlaps-staging:read"),
            (methods::EXECUTE_SQL, "deploy:hotlaps-staging:sql"),
            (methods::ROLLBACK, "deploy:hotlaps-staging:rollback"),
        ] {
            authorize(
                &ctx(&conn, Some(&auth)),
                Some("presented"),
                method,
                &json!({ "projectName": "hotlaps-api", "deployName": "x" }),
            )
            .expect("allowed");
            let sent = stub.requests();
            assert_eq!(sent.last().unwrap()["scope"], expected, "for {method}");
        }
    }

    // -- R2: nothing is allowed for want of a resource ---------------------

    #[test]
    fn every_deploy_name_method_is_denied_when_the_deployment_is_unknown() {
        // This is Defect 1 stated as a test: the upload path and activation
        // carry no projectName, and under the old server every active key was
        // accepted for all of them.
        let conn = test_db();
        let (stub, auth) = allow_everything();

        let by_deploy_name: Vec<&MethodSpec> = METHOD_TABLE
            .iter()
            .filter(|spec| spec.resolution == ProjectResolution::ByDeployName)
            .collect();
        assert!(
            by_deploy_name.len() >= 10,
            "the deployName group should be large"
        );

        for spec in by_deploy_name {
            let decision = authorize(
                &ctx(&conn, Some(&auth)),
                Some("presented"),
                spec.name,
                &json!({ "deployName": "does-not-exist" }),
            );
            assert!(
                matches!(decision, Err(Denial::Unresolved(_))),
                "{} must be denied when its deployment is unknown, got {decision:?}",
                spec.name
            );

            // And with no deployName at all.
            let decision = authorize(
                &ctx(&conn, Some(&auth)),
                Some("presented"),
                spec.name,
                &json!({}),
            );
            assert!(
                matches!(decision, Err(Denial::Unresolved(_))),
                "{} must be denied with no deployName, got {decision:?}",
                spec.name
            );
        }

        // Not one of those calls should have reached auth-center: they were
        // denied before any scope could be built.
        assert!(stub.requests().is_empty());
    }

    #[test]
    fn every_project_name_method_is_denied_when_the_project_is_unregistered() {
        let conn = test_db();
        let (_stub, auth) = allow_everything();

        for spec in METHOD_TABLE
            .iter()
            .filter(|spec| spec.resolution == ProjectResolution::ByProjectName)
        {
            for params in [json!({ "projectName": "never-registered" }), json!({})] {
                let decision = authorize(
                    &ctx(&conn, Some(&auth)),
                    Some("presented"),
                    spec.name,
                    &params,
                );
                assert!(
                    matches!(decision, Err(Denial::Unresolved(_))),
                    "{} must be denied, got {decision:?}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn a_project_with_a_null_resource_binding_is_denied() {
        let conn = test_db();
        register(&conn, "legacy-project", None);
        add_deployment(&conn, "legacy-project-1", "legacy-project");
        let (_stub, auth) = allow_everything();

        let by_project = authorize(
            &ctx(&conn, Some(&auth)),
            Some("presented"),
            methods::LIST_DEPLOYMENTS,
            &json!({ "projectName": "legacy-project" }),
        );
        assert!(matches!(by_project, Err(Denial::Unresolved(_))));

        let by_deploy = authorize(
            &ctx(&conn, Some(&auth)),
            Some("presented"),
            methods::UPLOAD_ONE_FILE,
            &json!({ "deployName": "legacy-project-1" }),
        );
        assert!(matches!(by_deploy, Err(Denial::Unresolved(_))));
    }

    #[test]
    fn an_unknown_method_is_denied_rather_than_left_unguarded() {
        let conn = test_db();
        let (_stub, auth) = allow_everything();
        let decision = authorize(
            &ctx(&conn, Some(&auth)),
            Some("presented"),
            "deleteEverything",
            &json!({ "projectName": "x" }),
        );
        assert!(matches!(decision, Err(Denial::UnknownMethod(_))));
    }

    #[test]
    fn no_key_is_denied() {
        let conn = test_db();
        let (_stub, auth) = allow_everything();
        assert_eq!(
            authorize(
                &ctx(&conn, Some(&auth)),
                None,
                methods::LIST_DEPLOYMENTS,
                &json!({})
            ),
            Err(Denial::MissingKey)
        );
        assert_eq!(
            authorize(
                &ctx(&conn, Some(&auth)),
                Some(""),
                methods::LIST_DEPLOYMENTS,
                &json!({})
            ),
            Err(Denial::MissingKey)
        );
    }

    #[test]
    fn an_unknown_key_with_no_auth_center_is_denied() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        let decision = authorize(
            &ctx(&conn, None),
            Some("not-a-local-key"),
            methods::CREATE_DEPLOYMENT,
            &json!({ "projectName": "hotlaps-api" }),
        );
        assert_eq!(decision, Err(Denial::NoAuthConfigured));
    }

    // -- fail closed (R4) --------------------------------------------------

    #[test]
    fn a_response_without_allowed_is_a_denial() {
        // The exact shape auth-center returns for a scope-less introspect, and
        // the exact shape the old server treated as permission granted.
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        let stub = start_stub(|_, _, _| {
            StubReply::json(200, r#"{"active":true,"key_id":"key_1","scopes":[]}"#)
        });
        let auth = auth_for(&stub);

        let decision = authorize(
            &ctx(&conn, Some(&auth)),
            Some("presented"),
            methods::CREATE_DEPLOYMENT,
            &json!({ "projectName": "hotlaps-api" }),
        );
        assert!(
            matches!(decision, Err(Denial::NotAuthorized(_))),
            "got {decision:?}"
        );
    }

    #[test]
    fn allowed_false_is_a_denial() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-prod"));
        let stub = start_stub(|_, _, _| {
            StubReply::json(200, r#"{"active":true,"allowed":false,"key_id":"key_1"}"#)
        });
        let auth = auth_for(&stub);
        let decision = authorize(
            &ctx(&conn, Some(&auth)),
            Some("staging-key"),
            methods::CREATE_DEPLOYMENT,
            &json!({ "projectName": "hotlaps-api" }),
        );
        assert!(matches!(decision, Err(Denial::NotAuthorized(_))));
    }

    #[test]
    fn an_inactive_token_is_a_denial() {
        let conn = test_db();
        register(&conn, "p", Some("r"));
        let stub = start_stub(|_, _, _| StubReply::json(200, r#"{"active":false}"#));
        let auth = auth_for(&stub);
        assert!(matches!(
            authorize(
                &ctx(&conn, Some(&auth)),
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "p" })
            ),
            Err(Denial::NotAuthorized(_))
        ));
    }

    #[test]
    fn a_non_2xx_answer_is_a_denial() {
        let conn = test_db();
        register(&conn, "p", Some("r"));
        let stub = start_stub(|_, _, _| StubReply::json(500, r#"{"error":"boom"}"#));
        let auth = auth_for(&stub);
        assert!(matches!(
            authorize(
                &ctx(&conn, Some(&auth)),
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "p" })
            ),
            Err(Denial::AuthUnavailable(_))
        ));
    }

    #[test]
    fn an_unparseable_body_is_a_denial() {
        let conn = test_db();
        register(&conn, "p", Some("r"));
        let stub = start_stub(|_, _, _| StubReply::json(200, "<html>not json</html>"));
        let auth = auth_for(&stub);
        assert!(matches!(
            authorize(
                &ctx(&conn, Some(&auth)),
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "p" })
            ),
            Err(Denial::AuthUnavailable(_))
        ));
    }

    #[test]
    fn a_timeout_is_a_denial() {
        let conn = test_db();
        register(&conn, "p", Some("r"));
        let stub = start_stub(|_, _, _| {
            StubReply::json(200, r#"{"active":true,"allowed":true,"key_id":"k"}"#)
                .after(Duration::from_millis(1500))
        });
        let auth = AuthCenter::with_timeout(
            AuthCenterConfig {
                base_url: stub.base_url.clone(),
                service_key: "svc".to_string(),
                admin_resource: "deploy-test".to_string(),
            },
            Duration::from_millis(150),
        );
        assert!(matches!(
            authorize(
                &ctx(&conn, Some(&auth)),
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "p" })
            ),
            Err(Denial::AuthUnavailable(_))
        ));
    }

    #[test]
    fn an_unreachable_auth_center_is_a_denial() {
        let conn = test_db();
        register(&conn, "p", Some("r"));
        // Port 1 is reserved and nothing listens there.
        let auth = AuthCenter::with_timeout(
            AuthCenterConfig {
                base_url: "http://127.0.0.1:1".to_string(),
                service_key: "svc".to_string(),
                admin_resource: "deploy-test".to_string(),
            },
            Duration::from_millis(500),
        );
        assert!(matches!(
            authorize(
                &ctx(&conn, Some(&auth)),
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "p" })
            ),
            Err(Denial::AuthUnavailable(_))
        ));
    }

    // -- caching (R5) ------------------------------------------------------

    #[test]
    fn positive_verdicts_are_cached_per_key_resource_and_action() {
        let conn = test_db();
        register(&conn, "a", Some("res-a"));
        register(&conn, "b", Some("res-b"));
        let (stub, auth) = allow_everything();
        let ctx = ctx(&conn, Some(&auth));

        for _ in 0..5 {
            authorize(
                &ctx,
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "a" }),
            )
            .unwrap();
        }
        assert_eq!(
            stub.requests().len(),
            1,
            "repeat calls should hit the cache"
        );

        // A different action on the same resource is a different cache entry —
        // otherwise a cached `read` would satisfy `sql`.
        authorize(
            &ctx,
            Some("k"),
            methods::EXECUTE_SQL,
            &json!({ "projectName": "a" }),
        )
        .unwrap();
        assert_eq!(stub.requests().len(), 2);

        // A different resource is a different entry.
        authorize(
            &ctx,
            Some("k"),
            methods::LIST_DEPLOYMENTS,
            &json!({ "projectName": "b" }),
        )
        .unwrap();
        assert_eq!(stub.requests().len(), 3);

        // And a different key.
        authorize(
            &ctx,
            Some("k2"),
            methods::LIST_DEPLOYMENTS,
            &json!({ "projectName": "a" }),
        )
        .unwrap();
        assert_eq!(stub.requests().len(), 4);
    }

    #[test]
    fn denials_are_never_cached() {
        let conn = test_db();
        register(&conn, "p", Some("r"));
        let stub = start_stub(|_, _, _| {
            StubReply::json(200, r#"{"active":true,"allowed":false,"key_id":"k"}"#)
        });
        let auth = auth_for(&stub);
        let ctx = ctx(&conn, Some(&auth));

        for _ in 0..3 {
            assert!(authorize(
                &ctx,
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "p" })
            )
            .is_err());
        }
        assert_eq!(stub.requests().len(), 3, "every denial must be re-asked");
    }

    // -- legacy keys (R6) --------------------------------------------------

    #[test]
    fn a_legacy_key_grants_everything_without_a_network_call() {
        let conn = test_db();
        conn.execute(
            "insert into secret_key (key_text, created_at, label) values (?, ?, ?)",
            params!["local-key", db::now_iso(), "do2 laptop"],
        )
        .unwrap();
        let (stub, auth) = allow_everything();

        // Shares temp_env's lock with the test that disables legacy keys, so
        // the two cannot see each other's environment.
        crate::auth_center::tests::temp_env(&[("DEPLOY_DISABLE_LEGACY_KEYS", None)], || {
            let key = authorize(
                &ctx(&conn, Some(&auth)),
                Some("local-key"),
                methods::EXECUTE_SQL,
                // Not even a registered project: legacy keys bypass resolution.
                &json!({ "projectName": "unregistered" }),
            )
            .expect("legacy key grants everything");

            assert_eq!(key.key_id, "legacy:1");
            assert_eq!(key.key_name.as_deref(), Some("do2 laptop"));
            assert!(
                stub.requests().is_empty(),
                "legacy keys make no network call"
            );
        });

        let stamped: Option<String> = conn
            .query_row(
                "select last_used_at from secret_key where key_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            stamped.is_some(),
            "last_used_at must be stamped so the migration can finish"
        );
    }

    #[test]
    fn legacy_keys_can_be_disabled_per_instance() {
        let conn = test_db();
        conn.execute(
            "insert into secret_key (key_text, created_at) values (?, ?)",
            params!["local-key", db::now_iso()],
        )
        .unwrap();
        let stub = start_stub(|_, _, _| StubReply::json(200, r#"{"active":false}"#));
        let auth = auth_for(&stub);

        crate::auth_center::tests::temp_env(&[("DEPLOY_DISABLE_LEGACY_KEYS", Some("1"))], || {
            assert!(!legacy_keys_enabled());
            let decision = authorize(
                &ctx(&conn, Some(&auth)),
                Some("local-key"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "nope" }),
            );
            // Falls through to auth-center, which does not know this key.
            assert!(matches!(decision, Err(Denial::Unresolved(_))));
        });

        crate::auth_center::tests::temp_env(&[("DEPLOY_DISABLE_LEGACY_KEYS", None)], || {
            assert!(legacy_keys_enabled());
        });
    }

    // -- the resource existence probe (known gap) --------------------------

    #[test]
    fn resource_probe_accepts_a_200() {
        let stub = start_stub(|_, _, _| StubReply::json(200, r#"{"name":"hotlaps-staging"}"#));
        assert_eq!(
            auth_for(&stub).verify_resource_exists("hotlaps-staging"),
            ResourceCheck::Verified
        );
    }

    #[test]
    fn resource_probe_rejects_a_json_404() {
        let stub = start_stub(|_, _, _| StubReply::json(404, r#"{"error":"no such resource"}"#));
        assert_eq!(
            auth_for(&stub).verify_resource_exists("typoed-name"),
            ResourceCheck::NotFound
        );
    }

    #[test]
    fn resource_probe_proceeds_when_the_endpoint_is_missing() {
        // What auth-center actually does today: the route does not exist, so
        // the 404 carries no JSON body. Warn and proceed.
        let stub = start_stub(|_, _, _| StubReply::json(404, ""));
        assert!(matches!(
            auth_for(&stub).verify_resource_exists("hotlaps-staging"),
            ResourceCheck::Unverifiable { .. }
        ));

        let stub = start_stub(|_, _, _| StubReply::json(503, "upstream down"));
        assert!(matches!(
            auth_for(&stub).verify_resource_exists("hotlaps-staging"),
            ResourceCheck::Unverifiable { .. }
        ));
    }
}
