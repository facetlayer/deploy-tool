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
    /// auth-center's `key_id`.
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

    /// `None` when this instance serves no dashboard, which is every instance
    /// that has not been given the three `DEPLOY_OAUTH_*`/`DEPLOY_PUBLIC_URL`
    /// variables. The dashboard routes are not mounted at all in that case.
    pub dashboard: Option<crate::dashboard::oauth::DashboardConfig>,
}

impl AppState {
    pub fn new(conn: Connection, disable_api_key_check: bool) -> AppState {
        AppState {
            conn: Mutex::new(conn),
            disable_api_key_check,
            dashboard: None,
        }
    }

    pub fn with_dashboard(
        mut self,
        dashboard: Option<crate::dashboard::oauth::DashboardConfig>,
    ) -> AppState {
        self.dashboard = dashboard;
        self
    }

    /// A poisoned mutex is recovered rather than propagated: the panic that
    /// poisoned it happened inside a handler, and the connection itself is
    /// still usable.
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|err| err.into_inner())
    }
}
