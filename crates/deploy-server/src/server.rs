//! HTTP + JSON-RPC transport. Ported from `~/tools/deploy/rust/src/server.rs`,
//! which is itself a port of src/server/main.ts and rpc-server.ts.
//!
//! Same endpoint, same error codes and the same 50MB body limit as the old
//! server, so an old CLI can still talk to this one during the migration. The
//! one behavioral change is that `validate_api_key` is gone: authorization is
//! now the full R2 decision in `authz`, and the key it returns is handed to the
//! handlers for attribution.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value as Json};

use crate::auth_center;
use crate::authz::{self, AuthorizedKey, AuthzContext, Denial};
use crate::db;
use crate::handlers;
use crate::state::AppState;

/// json-rpc-2.0's DefaultErrorCode, used for errors thrown by a method.
const DEFAULT_ERROR_CODE: i64 = 0;
const METHOD_NOT_FOUND: i64 = -32601;
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const INTERNAL_ERROR: i64 = -32603;
const UNAUTHORIZED: i64 = -32001;

pub struct StartServerOptions {
    pub disable_api_key_check: bool,
    pub port: u16,
}

fn error_response(id: Json, code: i64, message: &str, data: Option<Json>) -> Json {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

fn success_response(id: Json, result: Json) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Runs the authorization decision against the process-wide auth-center client.
///
/// Blocking: it holds the database mutex and may make an HTTP call, so the
/// caller keeps it off the async runtime's worker threads.
fn authorize_request(
    state: &AppState,
    api_key: Option<&str>,
    method: &str,
    params: &Json,
) -> Result<AuthorizedKey, Denial> {
    // `serve` installs the client before it binds a port, so this is
    // unreachable in practice — and a denial rather than an assumption, because
    // no missing piece of configuration may ever result in an allow (R2).
    let Some(auth) = auth_center::global() else {
        return Err(Denial::AuthUnavailable(
            "the auth-center client is not installed".to_string(),
        ));
    };

    let conn = state.db();
    let ctx = AuthzContext {
        conn: &conn,
        auth,
        disable_api_key_check: state.disable_api_key_check,
    };
    authz::authorize(&ctx, api_key, method, params)
}

async fn handle_json_rpc(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request: Json = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::OK,
                axum::Json(error_response(Json::Null, PARSE_ERROR, "Parse error", None)),
            )
                .into_response()
        }
    };

    let id = request.get("id").cloned().unwrap_or(Json::Null);

    // The method name is needed before authorization, because which resource
    // and action the call requires is a property of the method.
    let method = match request.get("method").and_then(|v| v.as_str()) {
        Some(method) => method.to_string(),
        None => {
            return (
                StatusCode::OK,
                axum::Json(error_response(id, INVALID_REQUEST, "Invalid Request", None)),
            )
                .into_response()
        }
    };

    let params = request.get("params").cloned().unwrap_or(json!({}));

    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());

    // Authorization touches SQLite and may do blocking HTTP, so keep it off the
    // runtime's worker threads.
    let auth_state = state.clone();
    let auth_method = method.clone();
    let auth_params = params.clone();
    let decision = tokio::task::spawn_blocking(move || {
        authorize_request(&auth_state, api_key.as_deref(), &auth_method, &auth_params)
    })
    .await
    .unwrap_or_else(|join_error| {
        // A panic in the decision denies, like every other failure (R4).
        Err(Denial::AuthUnavailable(format!(
            "authorization task failed: {join_error}"
        )))
    });

    let authorized_key = match decision {
        Ok(key) => key,
        Err(denial) => {
            eprintln!("[deploy auth] denied \"{}\": {}", method, denial.reason());
            // The journal always gets the full reason; the caller gets it only
            // when the key was active. See Denial::client_detail.
            let data = denial.client_detail().map(Json::from);
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(error_response(id, UNAUTHORIZED, "Unauthorized", data)),
            )
                .into_response();
        }
    };

    // Handlers are synchronous and can do blocking IO (hashing, shell hooks),
    // so run them off the async runtime's worker threads too.
    let handler_state = state.clone();
    let method_for_task = method.clone();
    let outcome: std::result::Result<Result<Json>, tokio::task::JoinError> =
        tokio::task::spawn_blocking(move || {
            handlers::dispatch(&handler_state, &method_for_task, &params, &authorized_key)
        })
        .await;

    match outcome {
        Ok(Ok(result)) => {
            (StatusCode::OK, axum::Json(success_response(id, result))).into_response()
        }
        Ok(Err(error)) => {
            let message = error.to_string();
            if message == "__method_not_found__" {
                return (
                    StatusCode::OK,
                    axum::Json(error_response(
                        id,
                        METHOD_NOT_FOUND,
                        "Method not found",
                        None,
                    )),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                axum::Json(error_response(id, DEFAULT_ERROR_CODE, &message, None)),
            )
                .into_response()
        }
        Err(join_error) => {
            // A panic inside a handler. Mirrors the TS catch-all, which
            // responds 500 with "Internal error".
            eprintln!(
                "[deploy error] Unexpected error executing JSON-RPC method \"{}\": {}",
                method, join_error
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(error_response(
                    id,
                    INTERNAL_ERROR,
                    "Internal error",
                    Some(json!(join_error.to_string())),
                )),
            )
                .into_response()
        }
    }
}

pub async fn start_server(options: StartServerOptions) -> Result<()> {
    let conn = db::open_database()?;
    let deploy_dir = db::get_deployments_dir(&conn)?;

    if !deploy_dir.exists() {
        return Err(anyhow!(
            "Deployments directory does not exist: {}",
            deploy_dir.display()
        ));
    }
    if !deploy_dir.is_dir() {
        return Err(anyhow!(
            "Deployments directory path is not a directory: {}",
            deploy_dir.display()
        ));
    }

    println!("Using deploy directory: {}", deploy_dir.display());

    let state = Arc::new(AppState::new(conn, options.disable_api_key_check));

    let app = Router::new()
        .route("/json-rpc", post(handle_json_rpc))
        // The client sends whole files as base64, so allow large bodies.
        .layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", options.port)).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("[deploy error] Port {} is already in use!", options.port);
            std::process::exit(1);
        }
        Err(e) => return Err(e.into()),
    };

    println!("Server listening on port {}", options.port);
    println!(
        "Listening for deployments at: localhost:{}/json-rpc",
        options.port
    );
    // Tests watch stdout for the line above, so make sure it is not buffered.
    use std::io::Write;
    std::io::stdout().flush().ok();

    axum::serve(listener, app).await?;
    Ok(())
}
