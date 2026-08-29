//! SQLite schema and access.
//!
//! Compatibility with the old tool's database is explicitly not a requirement
//! (R6), so this schema is authoritative: an instance is cut over by rebuilding
//! its database, not by migrating one in place.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use rusqlite::Connection;

/// `project`, `deployment` and `active_deployment` deliberately keep the shape
/// the old tool gave them. Rebuilding an instance's database discards live
/// operational state — which deployment is serving each project, and what is on
/// disk — and the Rollout section's documented recovery is a one-off import of
/// exactly those three tables from the old database. Keeping their shape keeps
/// that import a plain `insert into ... select`.
const SCHEMA: &str = r#"
create table if not exists deployments_dir(
  deployments_dir text primary key,
  created_at datetime not null
);

create table if not exists next_deploy_id(
  value integer not null
);

create table if not exists project(
  project_name text primary key,
  created_at datetime not null
);

create table if not exists deployment(
  deploy_name text primary key,
  deploy_dir text not null,
  project_name text not null,
  created_at datetime not null,
  source_config_file text,
  manifest_json text,
  web_static_dir text,
  dynamic_routes_json text,
  tags_json text,
  -- R7: which auth-center key authorized this deployment.
  authorized_by_key_id text,
  authorized_by_key_name text
);

create table if not exists deployment_needed_file(
  deploy_name text not null,
  rel_path text not null,
  sha text not null,
  created_at datetime not null
);

create table if not exists deployment_pending_multi_part_file_chunk(
  deploy_name text not null,
  rel_path text not null,
  chunk_start_at integer not null,
  chunk_base64 text not null,
  created_at datetime not null
);

create table if not exists active_deployment(
  project_name text primary key,
  deploy_name text not null,
  updated_at datetime not null
);

-- R1: the resource a project's keys are checked against. Its own table rather
-- than a column on `project`, so the binding can be rebound and audited
-- independently of the project row.
create table if not exists project_resource_binding(
  project_name text primary key,
  resource_name text not null,
  bound_at datetime not null,
  bound_by_key_id text,
  bound_by_key_name text
);

-- R1: every bind and rebind, with the key that made the change. Rebinding a
-- project to a resource the caller controls is a privilege-escalation path, so
-- the history is the record of who took it.
create table if not exists project_resource_binding_history(
  history_id integer primary key autoincrement,
  project_name text not null,
  previous_resource_name text,
  resource_name text not null,
  changed_at datetime not null,
  changed_by_key_id text,
  changed_by_key_name text
);
"#;

/// Where the state directory lives. Matches the old tool's resolution order so
/// an installed instance keeps its existing database file location.
pub fn state_directory() -> PathBuf {
    if let Ok(dir) = std::env::var("DEPLOY_STATE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("deploy");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("deploy")
}

pub fn database_path() -> PathBuf {
    state_directory().join("db.sqlite")
}

pub fn open_database() -> Result<Connection> {
    let state_dir = state_directory();
    std::fs::create_dir_all(&state_dir)?;
    let conn = Connection::open(database_path())?;
    init_connection(&conn)?;
    Ok(conn)
}

/// Applies pragmas and the schema. Split out so tests can drive an in-memory or
/// temp-file database through the same path.
pub fn init_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// ISO-8601 with milliseconds and a `Z` suffix, matching the timestamps the old
/// tool wrote, so an imported `deployment` row sorts against a new one.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn take_next_deploy_id(conn: &Connection) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row("select value from next_deploy_id", [], |row| row.get(0))
        .ok();

    match existing {
        Some(value) => {
            conn.execute("update next_deploy_id set value = value + 1", [])?;
            Ok(value)
        }
        None => {
            conn.execute("insert into next_deploy_id (value) values (?)", [2])?;
            Ok(1)
        }
    }
}

pub fn get_deployments_dir(conn: &Connection) -> Result<PathBuf> {
    let found: Option<String> = conn
        .query_row("select deployments_dir from deployments_dir", [], |row| {
            row.get(0)
        })
        .ok();

    found
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("Deployments directory has not been configured"))
}
