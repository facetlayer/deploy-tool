//! Process-wide server state shared by every handler.

use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

/// The key that authorized the current call, as resolved by the transport
/// before it dispatches to a handler.
///
/// Handlers never authorize anything themselves; they only record who did (R7).
/// It lives here rather than in `authz` because it is part of what a handler is
/// given, not part of how the decision is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedKey {
    /// auth-center's `key_id`, or `legacy:<id>` for a local `secret_key` row.
    pub key_id: String,
    pub key_name: Option<String>,
}

impl AuthorizedKey {
    pub fn new(key_id: impl Into<String>, key_name: Option<String>) -> AuthorizedKey {
        AuthorizedKey {
            key_id: key_id.into(),
            key_name,
        }
    }
}

pub struct AppState {
    /// A single connection behind a mutex. Handlers hold the lock only for the
    /// span of their queries, and the old TypeScript server was effectively
    /// single-threaded against SQLite as well.
    conn: Mutex<Connection>,

    /// `serve --disable-api-key-check`. Read by the transport, never by a
    /// handler.
    pub disable_api_key_check: bool,

    /// True when this instance checks against auth-center at all. Handlers use
    /// it for exactly one decision: R1 says an unregistered project cannot be
    /// deployed to once checking is on, while a legacy-only instance keeps the
    /// old implicit-create behavior — which is what makes rollout step 1
    /// ("land the resource model with DEPLOY_AUTH_URL unset") a no-op.
    pub auth_center_enabled: bool,
}

impl AppState {
    pub fn new(conn: Connection, disable_api_key_check: bool) -> AppState {
        AppState {
            conn: Mutex::new(conn),
            disable_api_key_check,
            auth_center_enabled: auth_center_configured(),
        }
    }

    /// A poisoned mutex is recovered rather than propagated: the panic that
    /// poisoned it happened inside a handler, and the connection itself is
    /// still usable.
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// Same rule the authorization layer uses: `DEPLOY_AUTH_URL` unset (or empty)
/// means legacy-only, no auth-center, no resource model enforcement.
fn auth_center_configured() -> bool {
    matches!(std::env::var("DEPLOY_AUTH_URL"), Ok(url) if !url.trim().is_empty())
}
