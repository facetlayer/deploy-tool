//! SQLite schema and access.
//!
//! Compatibility with the old tool's database is not a *requirement* (R6), but
//! it turns out to be worth having: do2 was cut over by pointing this server at
//! the existing database, which kept 27 live deployments that a rebuild would
//! have discarded. So the schema is authoritative and additive — see
//! `ADDED_COLUMNS`.

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
-- Dashboard browser sessions and the half-finished logins that produce them.
-- Neither is deployment state: dropping both tables signs everyone out and
-- costs nothing else, which is the intended blast radius for a visibility-only
-- feature that is explicitly not on the recovery path.
create table if not exists dashboard_session(
  session_id text primary key,
  access_token text not null,
  username text not null,
  subject text not null,
  created_at datetime not null,
  expires_at integer not null
);

create table if not exists dashboard_login(
  state text primary key,
  code_verifier text not null,
  return_to text not null,
  created_at integer not null
);

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
    add_missing_columns(conn)?;
    Ok(())
}

/// Columns this version adds to a table the old tool also had.
///
/// `create table if not exists` does nothing to a table that already exists, so
/// these have to be added explicitly. That matters because cutting an instance
/// over is done by pointing this server at the existing database — which keeps
/// the 27 live `active_deployment` and `deployment` rows that a rebuild would
/// have thrown away. Without this, every `createDeployment` and
/// `listDeployments` fails on `no such column` the moment the server starts.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("deployment", "authorized_by_key_id", "text"),
    ("deployment", "authorized_by_key_name", "text"),
];

fn add_missing_columns(conn: &Connection) -> Result<()> {
    for (table, column, col_type) in ADDED_COLUMNS {
        let present: i64 = conn.query_row(
            "select count(*) from pragma_table_info(?1) where name = ?2",
            rusqlite::params![table, column],
            |row| row.get(0),
        )?;
        if present == 0 {
            conn.execute_batch(&format!(
                "alter table {table} add column {column} {col_type}"
            ))?;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact `deployment` table the old tool created. Reproduced here
    /// because opening a database of this shape is how an instance is cut over,
    /// and the columns R7 added are not in it.
    const OLD_DEPLOYMENT_TABLE: &str = r#"
        create table deployment(
          deploy_name text primary key,
          deploy_dir text not null,
          project_name text not null,
          created_at datetime not null,
          source_config_file text,
          manifest_json text,
          web_static_dir text,
          dynamic_routes_json text,
          tags_json text
        );
    "#;

    /// Regression: `create table if not exists` silently does nothing to a
    /// table that already exists, so the R7 attribution columns never appeared
    /// on a cut-over instance. Every createDeployment and listDeployments then
    /// failed with `no such column: authorized_by_key_id` — which is exactly
    /// what happened on do2, in production, minutes after the cutover.
    #[test]
    fn opening_an_old_database_adds_the_columns_r7_needs() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(OLD_DEPLOYMENT_TABLE).unwrap();
        conn.execute_batch(
            "insert into deployment (deploy_name, deploy_dir, project_name, created_at)
             values ('envscore-api-41', 'envscore-api', 'envscore-api', '2026-01-01T00:00:00.000Z');",
        )
        .unwrap();

        init_connection(&conn).unwrap();

        // The queries that broke, run against the upgraded table.
        let listed: String = conn
            .query_row(
                "select deploy_name from deployment
                 where authorized_by_key_id is null and authorized_by_key_name is null",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(listed, "envscore-api-41", "the existing row must survive");

        conn.execute_batch(
            "insert into deployment (deploy_name, deploy_dir, project_name, created_at,
                                     authorized_by_key_id, authorized_by_key_name)
             values ('envscore-api-42', 'envscore-api', 'envscore-api',
                     '2026-01-02T00:00:00.000Z', 'key_1', 'hotlaps-ci');",
        )
        .unwrap();

        // Idempotent: restarting the server must not fail on a second pass.
        init_connection(&conn).unwrap();
        let count: i64 = conn
            .query_row("select count(*) from deployment", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
}
