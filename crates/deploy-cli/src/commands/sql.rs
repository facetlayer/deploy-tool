//! `deploy sql` and `deploy list-databases`. Port of src/client/sqlCommand.ts.

use std::path::Path;

use anyhow::Result;
use deploy_core::rpc::*;
use serde_json::{json, Value};

use crate::client_setup::setup_client;
use crate::detect_coding_agent::detect_coding_agent;

pub fn run_sql(
    config_file: &Path,
    sql: &str,
    database: Option<&str>,
    json_output: bool,
    override_dest: Option<&str>,
) -> Result<()> {
    let setup = setup_client(config_file, override_dest)?;

    let result = setup.client.execute_sql(&ExecuteSqlParams {
        project_name: setup.project_name.clone(),
        sql: sql.to_string(),
        database: database.map(str::to_string),
        // The server refuses this call for databases marked
        // agent-sql-access-blocked. Reporting it honestly is the whole point of
        // the guardrail.
        caller_is_agent: Some(detect_coding_agent().is_agent),
    })?;

    if json_output {
        print_sql_result_json(&result);
    } else {
        print_sql_result(&result);
    }

    Ok(())
}

pub fn list_databases(config_file: &Path, override_dest: Option<&str>) -> Result<()> {
    let setup = setup_client(config_file, override_dest)?;

    let result = setup.client.list_databases(&ListDatabasesParams {
        project_name: setup.project_name.clone(),
    })?;

    if result.databases.is_empty() {
        println!("No databases configured for this project.");
        println!("Add 'database <path>' entries to the .deploy config file.");
        return Ok(());
    }

    println!("Databases for project '{}':", setup.project_name);
    for db in &result.databases {
        println!();
        println!("  {}", db.path);
        if db.tables.is_empty() {
            println!("  Tables: (none or file not found)");
        } else {
            println!("  Tables: {}", db.tables.join(", "));
        }
    }

    Ok(())
}

/// Emits SELECT results as an array of row objects and write statements as
/// `{ rowsAffected: n }`. Values keep their native SQLite types, so callers can
/// parse the output without re-parsing a table.
fn print_sql_result_json(result: &ExecuteSqlResult) {
    if result.columns.is_empty() {
        println!("{}", json!({ "rowsAffected": result.rows_affected }));
        return;
    }

    let rows: Vec<Value> = result
        .rows
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::new();
            for (index, column) in result.columns.iter().enumerate() {
                object.insert(
                    column.clone(),
                    row.get(index).cloned().unwrap_or(Value::Null),
                );
            }
            Value::Object(object)
        })
        .collect();

    println!("{}", Value::Array(rows));
}

fn print_sql_result(result: &ExecuteSqlResult) {
    if result.columns.is_empty() {
        println!("{} row(s) affected", result.rows_affected);
        return;
    }

    if result.rows.is_empty() {
        println!("{}", result.columns.join("\t"));
        println!("(0 rows)");
        return;
    }

    let widths: Vec<usize> = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let widest_cell = result
                .rows
                .iter()
                .map(|row| cell_text(row.get(index)).chars().count())
                .max()
                .unwrap_or(0);
            column.chars().count().max(widest_cell)
        })
        .collect();

    let header = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| pad_end(column, widths[index]))
        .collect::<Vec<_>>()
        .join("  ");
    let separator = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>()
        .join("  ");

    println!("{header}");
    println!("{separator}");

    for row in &result.rows {
        let line = widths
            .iter()
            .enumerate()
            .map(|(index, width)| pad_end(&cell_text(row.get(index)), *width))
            .collect::<Vec<_>>()
            .join("  ");
        println!("{line}");
    }

    println!();
    println!(
        "({} row{})",
        result.rows.len(),
        if result.rows.len() == 1 { "" } else { "s" }
    );
}

/// Renders one cell the way `String(value ?? '')` did: NULL is blank, and
/// strings print without their JSON quotes.
fn cell_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn pad_end(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_cells_render_blank() {
        assert_eq!(cell_text(None), "");
        assert_eq!(cell_text(Some(&Value::Null)), "");
    }

    #[test]
    fn string_cells_lose_their_json_quotes() {
        assert_eq!(cell_text(Some(&json!("hello"))), "hello");
        assert_eq!(cell_text(Some(&json!(42))), "42");
    }

    #[test]
    fn padding_counts_characters_not_bytes() {
        assert_eq!(pad_end("ab", 4), "ab  ");
        assert_eq!(pad_end("abcd", 2), "abcd");
    }
}
