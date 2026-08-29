//! `executeSql` / `listDatabases`, run against the SQLite files a deployment
//! ships. Note these are separate actions in the authorization table: a key
//! that can deploy must not be able to run SQL.

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value as Json;

use deploy_core::config::{parse_deploy_database_configs, parse_deploy_databases};
use deploy_core::rpc::{
    DatabaseInfo, ExecuteSqlParams, ListDatabasesParams, ListDatabasesResult,
};
use deploy_core::sqlnames::parse_sql_table_names;

use crate::handlers::parse_params;
use crate::paths::{get_active_deployment_dir, get_safe_path_in_dir};
use crate::sql;
use crate::state::AppState;

/// Reads the `.deploy` config recorded for a project's active deployment. The
/// config is the only source of which databases a project has; a client cannot
/// name an arbitrary path.
fn get_project_config(conn: &Connection, project_name: &str) -> Result<String> {
    let active: Option<String> = conn
        .query_row(
            "select deploy_name from active_deployment where project_name = ?",
            [project_name],
            |row| row.get(0),
        )
        .optional()?;

    let active = active
        .ok_or_else(|| anyhow!("No active deployment found for project: {}", project_name))?;

    let config: Option<Option<String>> = conn
        .query_row(
            "select source_config_file from deployment where deploy_name = ?",
            [active],
            |row| row.get(0),
        )
        .optional()?;

    config.flatten().ok_or_else(|| {
        anyhow!(
            "Deployment config not found for project: {}",
            project_name
        )
    })
}

pub fn list_databases(state: &AppState, params: &Json) -> Result<Json> {
    let params: ListDatabasesParams = parse_params(params)?;
    let project_name = params.project_name;

    let (deploy_dir, config_text) = {
        let conn = state.db();
        let deploy_dir = get_active_deployment_dir(&conn, &project_name)?
            .ok_or_else(|| anyhow!("No active deployment found for project: {}", project_name))?;
        let config_text = get_project_config(&conn, &project_name)?;
        (deploy_dir, config_text)
    };

    let databases: Vec<DatabaseInfo> = parse_deploy_databases(&config_text)
        .iter()
        .map(|rel_path| {
            let (absolute_path, tables) = match get_safe_path_in_dir(&deploy_dir, rel_path) {
                Ok(path) => {
                    let tables = sql::get_table_names_in_db(&path).unwrap_or_default();
                    (path.to_string_lossy().to_string(), tables)
                }
                Err(_) => (String::new(), Vec::new()),
            };
            DatabaseInfo {
                path: rel_path.clone(),
                absolute_path,
                tables,
            }
        })
        .collect();

    Ok(serde_json::to_value(ListDatabasesResult { databases })?)
}

pub fn execute_sql(state: &AppState, params: &Json) -> Result<Json> {
    let params: ExecuteSqlParams = parse_params(params)?;
    let project_name = params.project_name;
    let sql_text = params.sql;
    let caller_is_agent = params.caller_is_agent.unwrap_or(false);

    let (deploy_dir, config_text) = {
        let conn = state.db();
        let deploy_dir = get_active_deployment_dir(&conn, &project_name)?
            .ok_or_else(|| anyhow!("No active deployment found for project: {}", project_name))?;
        let config_text = get_project_config(&conn, &project_name)?;
        (deploy_dir, config_text)
    };

    let db_configs = parse_deploy_database_configs(&config_text);
    let db_paths: Vec<String> = db_configs.iter().map(|db| db.path.clone()).collect();

    if db_paths.is_empty() {
        return Err(anyhow!(
            "No databases configured for project: {}. Add 'database <path>' to the .deploy config file.",
            project_name
        ));
    }

    // Resolve the target as a relative path first, so the per-database config
    // can be applied before the file is opened.
    let target_rel_path = if let Some(database) = params.database {
        database
    } else if db_paths.len() == 1 {
        db_paths[0].clone()
    } else {
        let table_names = parse_sql_table_names(&sql_text);

        if table_names.is_empty() {
            let db_info = sql::build_database_info_list(&deploy_dir, &db_paths);
            return Err(anyhow!(
                "Cannot determine which database to use for this query. Use --database to specify one.\nAvailable databases:\n{}",
                sql::format_database_list(&db_info)
            ));
        }

        sql::find_database_for_tables(&deploy_dir, &db_paths, &table_names)?
    };

    if caller_is_agent {
        let blocked = db_configs
            .iter()
            .find(|db| db.path == target_rel_path)
            .map(|db| db.agent_sql_access_blocked)
            .unwrap_or(false);

        if blocked {
            return Err(anyhow!(
                "SQL access to database '{}' is blocked when running inside a coding agent.\nThis database is marked 'agent-sql-access-blocked' in the deploy config.",
                target_rel_path
            ));
        }
    }

    let target_db_path = get_safe_path_in_dir(&deploy_dir, &target_rel_path)?;
    Ok(serde_json::to_value(sql::run_sql(
        &target_db_path,
        &sql_text,
    )?)?)
}
