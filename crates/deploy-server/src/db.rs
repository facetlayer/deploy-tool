//! SQLite schema and access.
//!
//! The table shapes are inherited from the old tool (`~/tools/deploy`), because
//! do2 and dohl each have a live `db.sqlite` this server has to open in place.
//! New columns are added by migration rather than by a schema bump, so an
//! existing instance keeps its deployment history.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use rusqlite::Connection;

/// Tables inherited verbatim from the old tool. The text is kept identical so
/// that a database created here is byte-compatible with one created there
/// during the migration window.
const SCHEMA: &[(&str, &str)] = &[
    (
        "deployments_dir",
        r#"create table deployments_dir(
      deployments_dir text primary key,
      created_at datetime not null
    )"#,
    ),
    (
        "next_deploy_id",
        r#"create table next_deploy_id(
      value integer not null
    )"#,
    ),
    (
        "project",
        r#"create table project(
      project_name text primary key,
      created_at datetime not null
    )"#,
    ),
    (
        "deployment",
        r#"create table deployment(
      deploy_name text primary key,
      deploy_dir text not null,
      project_name text not null,
      created_at datetime not null,
      source_config_file text,
      manifest_json text,
      web_static_dir text,
      dynamic_routes_json text,
      tags_json text
    )"#,
    ),
    (
        "deployment_needed_file",
        r#"create table deployment_needed_file(
      deploy_name text not null,
      rel_path text not null,
      sha text not null,
      created_at datetime not null
    )"#,
    ),
    (
        "deployment_pending_multi_part_file_chunk",
        r#"create table deployment_pending_multi_part_file_chunk(
      deploy_name text not null,
      rel_path text not null,
      chunk_start_at integer not null,
      chunk_base64 text not null,
      created_at datetime not null
    )"#,
    ),
    (
        "active_deployment",
        r#"create table active_deployment(
      project_name text primary key,
      deploy_name text not null,
      updated_at datetime not null
    )"#,
    ),
    (
        "secret_key",
        r#"create table secret_key(
      key_id integer primary key autoincrement,
      key_text text not null,
      created_at datetime not null
    )"#,
    ),
    // New in this version: an audit trail for resource rebinding. Rebinding a
    // project to a resource the caller controls is a privilege-escalation path,
    // so every change is recorded (R1).
    (
        "project_resource_audit",
        r#"create table project_resource_audit(
      audit_id integer primary key autoincrement,
      project_name text not null,
      old_resource_name text,
      new_resource_name text not null,
      changed_at datetime not null,
      changed_by text
    )"#,
    ),
];

/// Columns added to inherited tables. `(table, column, type)`. Applied only if
/// the column is missing, so opening an old database upgrades it in place.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    // R1: the auth-center resource this project's keys are checked against.
    // Null means the project predates registration and cannot be deployed to
    // while auth-center checking is enabled.
    ("project", "resource_name", "text"),
    ("project", "resource_bound_at", "datetime"),
    // R7: which key authorized this deployment.
    ("deployment", "authorized_by_key_id", "text"),
    ("deployment", "authorized_by_key_name", "text"),
    // R6: lets `list-legacy-keys` show which legacy keys are still in use.
    ("secret_key", "last_used_at", "datetime"),
    ("secret_key", "label", "text"),
];

/// Where the state directory lives. Matches the old tool's resolution order so
/// an installed instance keeps its existing database.
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

/// Applies pragmas and migrations. Split out so tests can drive an in-memory or
/// temp-file database through the same path.
pub fn init_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    for (table, statement) in SCHEMA {
        if !table_exists(conn, table)? {
            conn.execute_batch(statement)?;
        }
    }

    for (table, column, col_type) in ADDED_COLUMNS {
        if !column_exists(conn, table, column)? {
            conn.execute_batch(&format!(
                "alter table {table} add column {column} {col_type}"
            ))?;
        }
    }

    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "select count(*) from sqlite_master where type = 'table' and name = ?",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
    let names: HashSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(names.contains(column))
}

/// ISO-8601 with milliseconds and a `Z` suffix, matching the timestamps the old
/// tool wrote.
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
