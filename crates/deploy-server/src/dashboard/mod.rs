//! The read-only web dashboard: a React SPA plus the handful of routes that
//! authenticate it.
//!
//! Everything here is visibility. There is no route that deploys, rolls back,
//! activates or runs SQL, and no path from a dashboard session to any of those
//! — a session is checked against `admin-read` and nothing else. That is what
//! makes it acceptable for the dashboard to depend on auth-center even though
//! this server deploys auth-center: when the circular dependency bites, the
//! CLI over SSH still does everything the dashboard could have.

pub mod api;
pub mod oauth;
pub mod session;

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_embed::RustEmbed;

use crate::state::AppState;

pub const SESSION_COOKIE: &str = "deploy_session";

/// The built SPA, compiled into the binary.
///
/// The dashboard ships with the server rather than as a separate static
/// deploy: one origin means the session cookie, the OAuth callback and the
/// JSON API need no CORS and no second nginx upstream, and it removes the
/// bootstrap problem of the deploy tool having to deploy its own dashboard
/// before it can show you anything.
#[derive(RustEmbed)]
#[folder = "dashboard/dist"]
struct Assets;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/oauth/login", get(login))
        .route("/oauth/callback", get(callback))
        .route("/dashboard/api/me", get(api::me))
        .route("/dashboard/api/logout", post(api::logout))
        .route("/dashboard/api/projects", get(api::projects))
        .route("/dashboard/api/projects/:name", get(api::project))
        .route("/", get(index))
        .route("/assets/*path", get(asset))
        .fallback(get(index))
}

// --- cookies -------------------------------------------------------------------

pub fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        for part in text.split(';') {
            if let Some(found) = part
                .trim()
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
            {
                return Some(found.to_string());
            }
        }
    }
    None
}

/// `Secure` is unconditional: the redirect URI registered at auth-center must
/// be https except on localhost, and a cookie that a plain-http origin can
/// read is not worth the local-development convenience.
fn set_cookie(value: &str, max_age: i64) -> String {
    format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Secure; Max-Age={max_age}")
}

pub fn clear_cookie() -> String {
    set_cookie("", 0)
}

// --- sign-in -------------------------------------------------------------------

fn unavailable() -> Response {
    (
        StatusCode::NOT_FOUND,
        "this deploy server does not serve a dashboard",
    )
        .into_response()
}

async fn login(State(state): State<Arc<AppState>>) -> Response {
    let Some(config) = state.dashboard.clone() else {
        return unavailable();
    };
    let Some(auth) = crate::auth_center::global() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "auth-center is not configured").into_response();
    };

    let pkce = oauth::new_pkce();
    let login_state = oauth::new_state();
    {
        let conn = state.db();
        if session::start_login(&conn, &login_state, &pkce.verifier, "/").is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "could not start a login").into_response();
        }
    }
    Redirect::to(&oauth::authorize_url(
        auth.base_url(),
        &config,
        auth.admin_resource(),
        &login_state,
        &pkce.challenge,
    ))
    .into_response()
}

#[derive(serde::Deserialize)]
struct CallbackParams {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: String,
}

async fn callback(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<CallbackParams>,
) -> Response {
    let Some(config) = state.dashboard.clone() else {
        return unavailable();
    };
    let Some(auth) = crate::auth_center::global() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "auth-center is not configured").into_response();
    };

    if !params.error.is_empty() {
        return sign_in_failed(&format!("auth-center refused the sign-in: {}", params.error));
    }

    // The state must be one this server issued and has not already redeemed.
    // Without that check a callback URL from anywhere would log the visitor in
    // as whoever the attacker had a code for.
    let pending = {
        let conn = state.db();
        session::finish_login(&conn, &params.state).ok().flatten()
    };
    let Some((verifier, return_to)) = pending else {
        return sign_in_failed("this sign-in has expired or was already used. Try again.");
    };

    let token = {
        let base = auth.base_url().to_string();
        match oauth::exchange_code(&base, &config, &params.code, &verifier) {
            Ok(token) => token,
            Err(error) => return sign_in_failed(&format!("{error:#}")),
        }
    };

    // Confirm the user actually holds admin-read before issuing a session, so
    // an unauthorized sign-in fails at the door with a message rather than
    // landing on a dashboard where every panel says "denied".
    let identity = match auth.introspect(
        &token.access_token,
        auth.admin_resource(),
        deploy_core::rpc::Action::AdminRead,
    ) {
        crate::auth_center::Introspection::Allowed(identity) => identity,
        crate::auth_center::Introspection::Denied { .. } => {
            oauth::revoke(auth.base_url(), &config, &token.access_token);
            return sign_in_failed(&format!(
                "your account does not hold {}:admin-read, which this dashboard requires.",
                auth.admin_resource()
            ));
        }
        crate::auth_center::Introspection::Unavailable { .. } => {
            return sign_in_failed("auth-center could not be reached to check your access.");
        }
    };

    let cookie_value = oauth::new_state();
    {
        let conn = state.db();
        if session::create(
            &conn,
            &cookie_value,
            &token.access_token,
            identity.name.as_deref().unwrap_or("signed in"),
            &identity.key_id,
            token.expires_at,
        )
        .is_err()
        {
            return sign_in_failed("could not record the session");
        }
    }

    let max_age = (token.expires_at
        - std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0))
    .max(60);

    let mut response = Redirect::to(&return_to).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(&cookie_value, max_age).parse().unwrap(),
    );
    response
}

/// A sign-in that fails has no SPA to render the error, because the SPA is
/// what the sign-in was for. So it gets a page of its own.
fn sign_in_failed(message: &str) -> Response {
    let body = format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Sign-in failed</title><style>
body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;background:#0f1216;color:#e6e8eb;font:15px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif}}
.card{{background:#171b21;border:1px solid #2a3038;border-radius:12px;padding:32px;width:420px;max-width:92vw}}
h1{{font-size:18px;margin:0 0 10px}} p{{margin:0 0 18px;color:#9aa4b2}}
a{{color:#4f8cff}}</style>
<div class="card"><h1>Sign-in failed</h1><p>{}</p><p><a href="/oauth/login">Try again</a></p></div>"#,
        html_escape(message)
    );
    (
        StatusCode::FORBIDDEN,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// --- static assets -------------------------------------------------------------

async fn index(State(state): State<Arc<AppState>>) -> Response {
    if state.dashboard.is_none() {
        return unavailable();
    }
    serve_asset("index.html")
}

async fn asset(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> Response {
    if state.dashboard.is_none() {
        return unavailable();
    }
    serve_asset(&format!("assets/{path}"))
}

fn serve_asset(path: &str) -> Response {
    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_string();
            // index.html names the hashed asset files, so it must never be
            // cached; the hashed files themselves can be cached forever.
            let cache = if path == "index.html" {
                "no-store"
            } else {
                "public, max-age=31536000, immutable"
            };
            (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, cache.to_string()),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
