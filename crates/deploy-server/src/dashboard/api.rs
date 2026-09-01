//! The dashboard's JSON API.
//!
//! Every one of these resolves the session cookie to the access token it was
//! issued for and then runs the *same* `authz::authorize` decision an API key
//! would. There is deliberately no second authorization path: a dashboard
//! request is an `admin-read` call carrying a different kind of token, not a
//! different kind of request.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use serde_json::Value as Json_;

use deploy_core::rpc::methods;

use crate::authz::{self, AuthzContext, Denial};
use crate::dashboard::{clear_cookie, oauth, read_cookie, session, SESSION_COOKIE};
use crate::handlers;
use crate::state::AppState;

/// A dashboard call that could not be authorized. The SPA turns a 401 into the
/// sign-in prompt, so the distinction between "no session" and "session no
/// longer allowed" matters to it and both arrive as 401.
fn unauthorized(detail: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": detail }))).into_response()
}

/// Resolves the cookie to the access token behind it, then authorizes one
/// method exactly as the JSON-RPC transport would.
///
/// Blocking: it holds the database mutex and may call auth-center.
fn authorized_call(
    state: &AppState,
    headers: &HeaderMap,
    method: &str,
) -> Result<session::Session, Response> {
    let Some(cookie) = read_cookie(headers, SESSION_COOKIE) else {
        return Err(unauthorized("not signed in"));
    };
    let Some(auth) = crate::auth_center::global() else {
        return Err(unauthorized("auth-center is not configured"));
    };

    let conn = state.db();
    let Some(found) = session::resolve(&conn, &cookie).ok().flatten() else {
        return Err(unauthorized("your session has expired"));
    };
    let ctx = AuthzContext {
        conn: &conn,
        auth,
        disable_api_key_check: state.disable_api_key_check,
    };
    match authz::authorize(&ctx, Some(&found.access_token), method, &json!({})) {
        Ok(_) => Ok(found),
        // A session whose token auth-center no longer accepts is a session, so
        // say so rather than leaving the SPA to guess from an empty payload.
        Err(Denial::NotAuthorized { .. }) => Err(unauthorized("your access has been withdrawn")),
        Err(denial) => Err(unauthorized(
            &denial
                .client_detail()
                .unwrap_or_else(|| "not authorized".to_string()),
        )),
    }
}

/// Runs an already-authorized read through the ordinary handler dispatch, so
/// the dashboard and the CLI cannot drift on what a project looks like.
fn dispatch(state: &AppState, method: &str, params: Json_) -> Response {
    let key = crate::state::AuthorizedKey::new("dashboard-session", None);
    match handlers::dispatch(state, method, &params, &key) {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("{error:#}") })),
        )
            .into_response(),
    }
}

pub async fn me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        match authorized_call(&state, &headers, methods::LIST_PROJECTS) {
            Ok(found) => {
                // The URL the SPA sends the browser to when someone signs out.
                // Ending the auth-center session too is the difference between
                // "signed out" and "signed out until you click sign in again".
                let logout_url = match (state.dashboard.clone(), crate::auth_center::global()) {
                    (Some(config), Some(auth)) => oauth::logout_url(auth.base_url(), &config),
                    _ => String::new(),
                };
                Json(json!({
                    "username": found.username,
                    "subject": found.subject,
                    "logoutUrl": logout_url,
                }))
                .into_response()
            }
            Err(response) => response,
        }
    })
    .await;
    result.unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "panicked").into_response())
}

pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let _ = tokio::task::spawn_blocking(move || {
        let Some(cookie) = read_cookie(&headers, SESSION_COOKIE) else {
            return;
        };
        let token = {
            let conn = state.db();
            let token = session::resolve(&conn, &cookie)
                .ok()
                .flatten()
                .map(|found| found.access_token);
            let _ = session::delete(&conn, &cookie);
            token
        };
        // Drop the token at auth-center too, so signing out here is not merely
        // forgetting a cookie while a live token sits in the database.
        if let (Some(token), Some(config), Some(auth)) = (
            token,
            state.dashboard.clone(),
            crate::auth_center::global(),
        ) {
            oauth::revoke(auth.base_url(), &config, &token);
        }
    })
    .await;

    let mut response = Json(json!({ "ok": true })).into_response();
    response
        .headers_mut()
        .insert(axum::http::header::SET_COOKIE, clear_cookie().parse().unwrap());
    response
}

pub async fn projects(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        if let Err(response) = authorized_call(&state, &headers, methods::LIST_PROJECTS) {
            return response;
        }
        dispatch(&state, methods::LIST_PROJECTS, json!({}))
    })
    .await;
    result.unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "panicked").into_response())
}

pub async fn project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        if let Err(response) = authorized_call(&state, &headers, methods::GET_PROJECT) {
            return response;
        }
        dispatch(&state, methods::GET_PROJECT, json!({ "projectName": name }))
    })
    .await;
    result.unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "panicked").into_response())
}
