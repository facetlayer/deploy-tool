//! `deploy sql` support: database routing and query execution against a
//! deployment's own SQLite files.
//!
//! Port of the old daemon's `sql.rs`. The SQL tokenizer that used to live here
//! is now `deploy_core::sqlnames`, shared with the CLI.

use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value as Json};

use deploy_core::rpc::ExecuteSqlResult;
use deploy_core::sqlnames::is_query_statement;

use crate::paths::get_safe_path_in_dir;

/// Opens a database that belongs to a deployment, not the server's own.
fn open_external_db(db_path: &Path) -> Result<Connection> {
    Ok(Connection::open(db_path)?)
}

pub fn get_table_names_in_db(db_path: &Path) -> Result<Vec<String>> {
    let conn = open_external_db(db_path)?;
    let mut stmt = conn.prepare("select name from sqlite_master where type='table'")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

    let mut names = Vec::new();
    for name in rows {
        names.push(name?.to_lowercase());
    }
    Ok(names)
}

fn value_ref_to_json(value: ValueRef<'_>) -> Json {
    match value {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(bytes) => json!(String::from_utf8_lossy(bytes).to_string()),
        // Matches how a Node Buffer serialized through JSON.stringify, which is
        // what the old TypeScript server put on the wire.
        ValueRef::Blob(bytes) => json!({ "type": "Buffer", "data": bytes.to_vec() }),
    }
}

pub fn run_sql(db_path: &Path, sql: &str) -> Result<ExecuteSqlResult> {
    let conn = open_external_db(db_path)?;

    if is_query_statement(sql) {
        let mut stmt = conn.prepare(sql)?;
        let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        let column_count = columns.len();

        let mut rows_out = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(column_count);
            for index in 0..column_count {
                values.push(value_ref_to_json(row.get_ref(index)?));
            }
            rows_out.push(values);
        }

        Ok(ExecuteSqlResult {
            columns,
            rows: rows_out,
            rows_affected: 0,
        })
    } else {
        let changes = conn.execute(sql, [])?;
        Ok(ExecuteSqlResult {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: changes as i64,
        })
    }
}

pub struct DatabaseSummary {
    pub path: String,
    pub tables: Vec<String>,
}

pub fn build_database_info_list(deploy_dir: &Path, db_paths: &[String]) -> Vec<DatabaseSummary> {
    db_paths
        .iter()
        .map(|rel_path| {
            let tables = get_safe_path_in_dir(deploy_dir, rel_path)
                .ok()
                .and_then(|abs| get_table_names_in_db(&abs).ok())
                .unwrap_or_default();
            DatabaseSummary {
                path: rel_path.clone(),
                tables,
            }
        })
        .collect()
}

pub fn format_database_list(db_info: &[DatabaseSummary]) -> String {
    db_info
        .iter()
        .map(|db| {
            let table_list = if db.tables.is_empty() {
                "(none)".to_string()
            } else {
                db.tables.join(", ")
            };
            format!("  {} (tables: {})", db.path, table_list)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolves which configured database contains all of the given tables.
///
/// Ambiguity is an error rather than a guess: picking the wrong database for an
/// `update` would silently write to the wrong deployment data.
pub fn find_database_for_tables(
    deploy_dir: &Path,
    db_paths: &[String],
    table_names: &[String],
) -> Result<String> {
    let mut matches: Vec<String> = Vec::new();

    for rel_path in db_paths {
        let abs_path = match get_safe_path_in_dir(deploy_dir, rel_path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if let Ok(tables) = get_table_names_in_db(&abs_path) {
            let has_all = table_names
                .iter()
                .all(|name| tables.contains(&name.to_lowercase()));
            if has_all {
                matches.push(rel_path.clone());
            }
        }
    }

    if matches.is_empty() {
        let db_info = build_database_info_list(deploy_dir, db_paths);
        return Err(anyhow!(
            "No database found containing tables: {}\nAvailable databases:\n{}",
            table_names.join(", "),
            format_database_list(&db_info)
        ));
    }

    if matches.len() > 1 {
        let db_info = build_database_info_list(deploy_dir, db_paths);
        return Err(anyhow!(
            "Ambiguous query: tables [{}] found in multiple databases: {}\nUse --database to specify which one.\nAvailable databases:\n{}",
            table_names.join(", "),
            matches.join(", "),
            format_database_list(&db_info)
        ));
    }

    Ok(matches.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("deploy-server-sql-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_db(dir: &Path, file: &str, table: &str) {
        let conn = Connection::open(dir.join(file)).unwrap();
        conn.execute_batch(&format!("create table {table} (id integer)"))
            .unwrap();
    }

    #[test]
    fn select_returns_columns_and_rows() {
        let dir = temp_dir("select");
        make_db(&dir, "a.sqlite", "widgets");
        let path = dir.join("a.sqlite");
        run_sql(&path, "insert into widgets (id) values (7)").unwrap();

        let result = run_sql(&path, "select * from widgets").unwrap();
        assert_eq!(result.columns, vec!["id".to_string()]);
        assert_eq!(result.rows, vec![vec![json!(7)]]);
        assert_eq!(result.rows_affected, 0);
    }

    #[test]
    fn non_query_statements_report_rows_affected() {
        let dir = temp_dir("update");
        make_db(&dir, "a.sqlite", "widgets");
        let path = dir.join("a.sqlite");
        run_sql(&path, "insert into widgets (id) values (1)").unwrap();

        let result = run_sql(&path, "update widgets set id = 2").unwrap();
        assert_eq!(result.rows_affected, 1);
        assert!(result.columns.is_empty());
    }

    #[test]
    fn routes_a_query_to_the_database_holding_its_tables() {
        let dir = temp_dir("routing");
        make_db(&dir, "one.sqlite", "widgets");
        make_db(&dir, "two.sqlite", "secrets");

        let db_paths = vec!["one.sqlite".to_string(), "two.sqlite".to_string()];
        let target = find_database_for_tables(&dir, &db_paths, &["secrets".to_string()]).unwrap();
        assert_eq!(target, "two.sqlite");
    }

    #[test]
    fn a_table_in_two_databases_is_ambiguous_rather_than_guessed() {
        let dir = temp_dir("ambiguous");
        make_db(&dir, "one.sqlite", "widgets");
        make_db(&dir, "two.sqlite", "widgets");

        let db_paths = vec!["one.sqlite".to_string(), "two.sqlite".to_string()];
        let err = find_database_for_tables(&dir, &db_paths, &["widgets".to_string()]).unwrap_err();
        assert!(err.to_string().contains("Ambiguous query"));
    }

    #[test]
    fn an_unknown_table_reports_the_available_databases() {
        let dir = temp_dir("unknown-table");
        make_db(&dir, "one.sqlite", "widgets");

        let db_paths = vec!["one.sqlite".to_string()];
        let err = find_database_for_tables(&dir, &db_paths, &["nope".to_string()]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("No database found containing tables: nope"));
        assert!(message.contains("one.sqlite (tables: widgets)"));
    }
}
