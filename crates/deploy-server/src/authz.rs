//! The authorization decision for one JSON-RPC call (R2).
//!
//! Every call resolves to a concrete resource and is checked against it:
//!
//! 1. Resolve the call to a project — `projectName` directly, or `deployName`
//!    joined through the `deployment` table, or the instance administration
//!    resource for `createProject`.
//! 2. Look up the project's binding in `project_resource_binding`.
//! 3. Introspect the presented key against `<resource>:<action>`.
//! 4. Deny on any failure.
//!
//! There is deliberately no branch that allows a call because no resource could
//! be determined, and no local key table to short-circuit any of it (R6). That
//! branch is what made the old server's entire upload and activation path
//! unguarded.

use deploy_core::rpc::{lookup_method, MethodSpec, ProjectResolution};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as Json;

use crate::auth_center::{AuthCenter, Introspection};
// The handlers take this type, so it lives with the state they are given rather
// than here; re-exported so callers of `authorize` need only this module.
pub use crate::state::AuthorizedKey;

/// Everything the decision needs. Deliberately narrower than `AppState` so the
/// decision can be unit-tested against a bare in-memory database.
pub struct AuthzContext<'a> {
    pub conn: &'a Connection,
    pub auth: &'a AuthCenter,
    /// `serve --disable-api-key-check`. Local development only.
    pub disable_api_key_check: bool,
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
    /// auth-center answered, and the answer was no. `active` is true when the
    /// key itself is real and unrevoked and merely lacks the scope, which is
    /// the only case where the scope may be echoed back to the caller.
    NotAuthorized {
        active: bool,
        scope: String,
        detail: String,
    },
    /// auth-center could not be asked.
    AuthUnavailable(String),
}

impl Denial {
    pub fn reason(&self) -> String {
        match self {
            Denial::MissingKey => "no x-api-key header was presented".to_string(),
            Denial::UnknownMethod(method) => {
                format!("method '{method}' has no entry in the authorization table")
            }
            Denial::Unresolved(detail) => detail.clone(),
            Denial::NotAuthorized { detail, .. } => detail.clone(),
            Denial::AuthUnavailable(detail) => detail.clone(),
        }
    }

    /// What may be told to the caller, beyond a bare "Unauthorized".
    ///
    /// Naming the scope is the whole diagnostic: a caller whose key is real
    /// learns it needs `hotlaps-api-staging:deploy` and sees the typo
    /// immediately, without auth-center needing a resource registry to check
    /// against at registration time. It is withheld from an unknown or revoked
    /// key, because that would let anyone holding a random string enumerate
    /// this instance's project-to-resource bindings.
    ///
    /// `AuthUnavailable` is told too, and deliberately: it is not a verdict
    /// about the caller at all, and it names no binding, so the reasoning above
    /// does not apply. It is also the failure an operator is most likely to
    /// meet during a cutover, where the alternative is staring at a bare
    /// "Unauthorized" while the real cause sits in a journal on a host they may
    /// not be able to read. The detail is deliberately coarse — that the
    /// instance got no verdict, not the URL, the status or the error. Note it
    /// covers a misconfigured instance (auth-center answered 401 or 403 for
    /// this server's own service key) as well as an unreachable one, because
    /// the caller can act on neither and both mean the same thing to them.
    pub fn client_detail(&self) -> Option<String> {
        match self {
            Denial::NotAuthorized {
                active: true,
                scope,
                ..
            } => Some(format!("this key does not hold {scope}")),
            // Resolution failures name only things the caller just supplied — the
            // project or deploy name in their own request — so echoing them back
            // discloses nothing they did not already know, and it is the
            // difference between a diagnosable cutover and an opaque one: an
            // instance that has not yet registered a project refuses every call
            // for it, and "Unauthorized" alone gives an operator nothing to act
            // on.
            Denial::Unresolved(detail) => Some(detail.clone()),
            Denial::AuthUnavailable(_) => Some(
                "this deploy server could not get a verdict from auth-center, so it denied \
                 the call; it is misconfigured or auth-center is unreachable. See the \
                 server's journal for which."
                    .to_string(),
            ),
            _ => None,
        }
    }
}

pub type Decision = Result<AuthorizedKey, Denial>;

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

    let Some(spec) = lookup_method(method) else {
        return Err(Denial::UnknownMethod(method.to_string()));
    };

    let resource = resolve_resource(ctx.conn, spec, params, ctx.auth)?;

    match ctx.auth.introspect(api_key, &resource, spec.action) {
        Introspection::Allowed(identity) => Ok(AuthorizedKey::new(identity.key_id, identity.name)),
        Introspection::Denied {
            active,
            scope,
            detail,
        } => Err(Denial::NotAuthorized {
            active,
            scope,
            detail,
        }),
        Introspection::Unavailable { detail } => Err(Denial::AuthUnavailable(detail)),
    }
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
    let registered: bool = conn
        .query_row(
            "select 1 from project where project_name = ?",
            params![project_name],
            |_| Ok(true),
        )
        .optional()
        .unwrap_or(None)
        .unwrap_or(false);

    if !registered {
        return Err(Denial::Unresolved(format!(
            "{}: project '{project_name}' is not registered on this instance; \
             register it with `auth-setup`, binding '{project_name}' to a resource",
            spec.name
        )));
    }

    let bound: Option<String> = conn
        .query_row(
            "select resource_name from project_resource_binding where project_name = ?",
            params![project_name],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap_or(None);

    match bound {
        Some(resource) if !resource.trim().is_empty() => Ok(resource),
        Some(_) => Err(Denial::Unresolved(format!(
            "{}: project '{project_name}' has an empty resource binding",
            spec.name
        ))),
        None => Err(Denial::Unresolved(format!(
            "{}: project '{project_name}' has no bound auth-center resource; \
             bind it to one with `auth-setup`",
            spec.name
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_center::tests::{start_stub, StubReply};
    use crate::auth_center::AuthCenterConfig;
    use crate::db;
    use deploy_core::rpc::{methods, ProjectResolution, METHOD_TABLE};
    use serde_json::json;
    use std::time::Duration;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // WAL is not available in-memory; everything else in init_connection is.
        conn.execute_batch(
            r#"create table project(project_name text primary key, created_at datetime not null);
               create table deployment(deploy_name text primary key, deploy_dir text,
                                       project_name text not null, created_at datetime);
               create table project_resource_binding(project_name text primary key,
                                                     resource_name text not null,
                                                     bound_at datetime not null,
                                                     bound_by_key_id text,
                                                     bound_by_key_name text);"#,
        )
        .unwrap();
        conn
    }

    fn register(conn: &Connection, project: &str, resource: Option<&str>) {
        conn.execute(
            "insert into project (project_name, created_at) values (?, ?)",
            params![project, db::now_iso()],
        )
        .unwrap();
        if let Some(resource) = resource {
            conn.execute(
                "insert into project_resource_binding
                    (project_name, resource_name, bound_at) values (?, ?, ?)",
                params![project, resource, db::now_iso()],
            )
            .unwrap();
        }
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

    fn ctx<'a>(conn: &'a Connection, auth: &'a AuthCenter) -> AuthzContext<'a> {
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
            &ctx(&conn, &auth),
            Some("presented"),
            methods::CREATE_DEPLOYMENT,
            &json!({ "projectName": "hotlaps-api" }),
        );

        let key = decision.expect("should be allowed");
        assert_eq!(key.key_id, "key_1");
        assert_eq!(key.key_name.as_deref(), Some("ci"));

        let sent = stub.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["scope"], "hotlaps-staging:deploy");
        assert_eq!(sent[0]["token"], "presented");
    }

    #[test]
    fn deploy_name_methods_resolve_through_the_deployment_table() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        add_deployment(&conn, "hotlaps-api-7", "hotlaps-api");
        let (stub, auth) = allow_everything();

        authorize(
            &ctx(&conn, &auth),
            Some("presented"),
            methods::ACTIVATE_DEPLOYMENT,
            &json!({ "deployName": "hotlaps-api-7" }),
        )
        .expect("should be allowed");

        assert_eq!(stub.requests()[0]["scope"], "hotlaps-staging:deploy");
    }

    #[test]
    fn create_project_is_checked_against_the_instance_administration_resource() {
        let conn = test_db();
        let (stub, auth) = allow_everything();

        authorize(
            &ctx(&conn, &auth),
            Some("presented"),
            methods::CREATE_PROJECT,
            &json!({ "projectName": "new", "resourceName": "whatever" }),
        )
        .expect("should be allowed");

        assert_eq!(stub.requests()[0]["scope"], "deploy-test:create-project");
    }

    #[test]
    fn the_action_is_part_of_the_scope_so_execute_sql_is_separately_grantable() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        let (stub, auth) = allow_everything();

        for (method, expected) in [
            (methods::LIST_DEPLOYMENTS, "hotlaps-staging:read"),
            (methods::EXECUTE_SQL, "hotlaps-staging:execute-sql"),
            (methods::ROLLBACK, "hotlaps-staging:rollback"),
        ] {
            authorize(
                &ctx(&conn, &auth),
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
                &ctx(&conn, &auth),
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
            let decision = authorize(&ctx(&conn, &auth), Some("presented"), spec.name, &json!({}));
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
                let decision = authorize(&ctx(&conn, &auth), Some("presented"), spec.name, &params);
                assert!(
                    matches!(decision, Err(Denial::Unresolved(_))),
                    "{} must be denied, got {decision:?}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn a_project_with_no_resource_binding_is_denied() {
        let conn = test_db();
        register(&conn, "unbound-project", None);
        add_deployment(&conn, "unbound-project-1", "unbound-project");
        let (_stub, auth) = allow_everything();

        let by_project = authorize(
            &ctx(&conn, &auth),
            Some("presented"),
            methods::LIST_DEPLOYMENTS,
            &json!({ "projectName": "unbound-project" }),
        );
        assert!(matches!(by_project, Err(Denial::Unresolved(_))));

        let by_deploy = authorize(
            &ctx(&conn, &auth),
            Some("presented"),
            methods::UPLOAD_ONE_FILE,
            &json!({ "deployName": "unbound-project-1" }),
        );
        assert!(matches!(by_deploy, Err(Denial::Unresolved(_))));
    }

    #[test]
    fn an_unknown_method_is_denied_rather_than_left_unguarded() {
        let conn = test_db();
        let (_stub, auth) = allow_everything();
        let decision = authorize(
            &ctx(&conn, &auth),
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
                &ctx(&conn, &auth),
                None,
                methods::LIST_DEPLOYMENTS,
                &json!({})
            ),
            Err(Denial::MissingKey)
        );
        assert_eq!(
            authorize(
                &ctx(&conn, &auth),
                Some(""),
                methods::LIST_DEPLOYMENTS,
                &json!({})
            ),
            Err(Denial::MissingKey)
        );
    }

    /// R6: there is no local key table and no environment variable that brings
    /// one back, so a key auth-center rejects is denied, full stop.
    #[test]
    fn a_key_auth_center_rejects_is_denied_with_no_local_table_to_fall_back_on() {
        let conn = test_db();
        register(&conn, "hotlaps-api", Some("hotlaps-staging"));
        let stub = start_stub(|_, _, _| StubReply::json(200, r#"{"active":false}"#));
        let auth = auth_for(&stub);

        // The old server's bypass: a row in a local table checked before
        // auth-center. There is no such table to put one in.
        assert!(!table_exists(&conn, "secret_key"));

        let decision = authorize(
            &ctx(&conn, &auth),
            Some("a-key-that-used-to-be-in-the-local-table"),
            methods::EXECUTE_SQL,
            &json!({ "projectName": "hotlaps-api" }),
        );
        assert!(
            matches!(decision, Err(Denial::NotAuthorized { .. })),
            "got {decision:?}"
        );
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        let count: i64 = conn
            .query_row(
                "select count(*) from sqlite_master where type = 'table' and name = ?",
                params![name],
                |row| row.get(0),
            )
            .unwrap();
        count > 0
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
            &ctx(&conn, &auth),
            Some("presented"),
            methods::CREATE_DEPLOYMENT,
            &json!({ "projectName": "hotlaps-api" }),
        );
        assert!(
            matches!(decision, Err(Denial::NotAuthorized { .. })),
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
            &ctx(&conn, &auth),
            Some("staging-key"),
            methods::CREATE_DEPLOYMENT,
            &json!({ "projectName": "hotlaps-api" }),
        );
        assert!(matches!(decision, Err(Denial::NotAuthorized { .. })));
    }

    #[test]
    fn an_inactive_token_is_a_denial() {
        let conn = test_db();
        register(&conn, "p", Some("r"));
        let stub = start_stub(|_, _, _| StubReply::json(200, r#"{"active":false}"#));
        let auth = auth_for(&stub);
        assert!(matches!(
            authorize(
                &ctx(&conn, &auth),
                Some("k"),
                methods::LIST_DEPLOYMENTS,
                &json!({ "projectName": "p" })
            ),
            Err(Denial::NotAuthorized { .. })
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
                &ctx(&conn, &auth),
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
                &ctx(&conn, &auth),
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
                &ctx(&conn, &auth),
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
                &ctx(&conn, &auth),
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
        let ctx = ctx(&conn, &auth);

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
        let ctx = ctx(&conn, &auth);

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
}
