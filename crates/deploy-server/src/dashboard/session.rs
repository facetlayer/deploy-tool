//! Browser sessions for the dashboard.
//!
//! A session is a row keyed by the sha256 of the cookie value, exactly as
//! auth-center stores its own — the cookie is the only copy of the secret, so a
//! stolen database yields no usable session.
//!
//! The row holds the auth-center access token the login produced. That is a
//! deliberate choice: it means every dashboard request goes through the same
//! `auth_center::introspect` path an API key does, so the dashboard inherits
//! fail-closed behavior and the 30-second revocation bound rather than
//! trusting a local row until it expires. The cost is that the token sits in
//! the server's SQLite file, which is root-owned `0600` on the deploy hosts
//! and already gates every deployment on the machine.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::db::now_iso;

/// Sessions expire with the access token they carry, so a dashboard session
/// never outlives the credential that justifies it.
pub struct Session {
    pub access_token: String,
    pub username: String,
    pub subject: String,
}

pub fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn create(
    conn: &Connection,
    cookie_value: &str,
    access_token: &str,
    username: &str,
    subject: &str,
    expires_at: i64,
) -> Result<()> {
    conn.execute(
        "insert or replace into dashboard_session
           (session_id, access_token, username, subject, created_at, expires_at)
         values (?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            hash(cookie_value),
            access_token,
            username,
            subject,
            now_iso(),
            expires_at
        ],
    )?;
    // Cheap enough to do on every login, and it keeps the table from growing
    // one row per sign-in forever.
    conn.execute(
        "delete from dashboard_session where expires_at < ?",
        rusqlite::params![now_unix()],
    )?;
    Ok(())
}

pub fn resolve(conn: &Connection, cookie_value: &str) -> Result<Option<Session>> {
    Ok(conn
        .query_row(
            "select access_token, username, subject from dashboard_session
             where session_id = ? and expires_at > ?",
            rusqlite::params![hash(cookie_value), now_unix()],
            |row| {
                Ok(Session {
                    access_token: row.get(0)?,
                    username: row.get(1)?,
                    subject: row.get(2)?,
                })
            },
        )
        .optional()?)
}

pub fn delete(conn: &Connection, cookie_value: &str) -> Result<()> {
    conn.execute(
        "delete from dashboard_session where session_id = ?",
        rusqlite::params![hash(cookie_value)],
    )?;
    Ok(())
}

// --- in-flight logins ----------------------------------------------------------

/// How long a half-finished login may sit before its `state` is refused. Long
/// enough to type a password, short enough that a leaked authorize URL is not
/// useful later.
const LOGIN_TTL_SECS: i64 = 10 * 60;

pub fn start_login(
    conn: &Connection,
    state: &str,
    code_verifier: &str,
    return_to: &str,
) -> Result<()> {
    conn.execute(
        "insert or replace into dashboard_login (state, code_verifier, return_to, created_at)
         values (?, ?, ?, ?)",
        rusqlite::params![state, code_verifier, return_to, now_unix()],
    )?;
    conn.execute(
        "delete from dashboard_login where created_at < ?",
        rusqlite::params![now_unix() - LOGIN_TTL_SECS],
    )?;
    Ok(())
}

/// Consumes a pending login. Single-use: a `state` that comes back twice is a
/// replayed callback, and the second attempt finds nothing.
pub fn finish_login(conn: &Connection, state: &str) -> Result<Option<(String, String)>> {
    let row = conn
        .query_row(
            "select code_verifier, return_to from dashboard_login
             where state = ? and created_at >= ?",
            rusqlite::params![state, now_unix() - LOGIN_TTL_SECS],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    conn.execute(
        "delete from dashboard_login where state = ?",
        rusqlite::params![state],
    )?;
    Ok(row)
}
